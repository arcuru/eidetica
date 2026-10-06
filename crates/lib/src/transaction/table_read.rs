//! Table-owned physical planning over the existing transaction overlay scanner.
use super::record_read::RecordRead;
use crate::{
    Result, Transaction,
    backend::{BackendError, RecordPage, RecordRange},
    crdt::Codec,
    store::{RecordProjection, assistance::PrivateRepresentation},
};

impl Transaction {
    async fn table_committed_page(
        &self,
        store: &str,
        mut query: crate::store::table_query::TableQuery,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
        read: &mut RecordRead,
    ) -> Result<RecordPage> {
        use crate::store::{RawTable, Registered, table::TableProjection};
        if self.store_snapshot(store).await?.is_empty() {
            return Ok(RecordPage::default());
        }
        match &mut query {
            crate::store::table_query::TableQuery::Point { repair, .. }
            | crate::store::table_query::TableQuery::Page { repair, .. } => {
                *repair = read.budget.can_reconstruct()
            }
        }
        let outer = self.query_outer_type(store, RawTable::type_id())?;
        let descriptor = TableProjection.descriptor();
        self.query_record_page::<crate::store::TableData>(
            store,
            &outer,
            query.encode()?,
            PrivateRepresentation {
                format: descriptor.clone(),
                configuration: RawTable::type_id().as_bytes().to_vec(),
            },
            &TableProjection,
            range,
            after,
            limit,
            read,
            |entry, bytes| crate::store::state::decode_source(store, entry, bytes, &descriptor),
        )
        .await
    }

    pub(crate) async fn table_query_get(&self, store: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
        use crate::store::{StoreError, table::TableProjection, table_query::TableQuery};
        let mut read = RecordRead::default();
        loop {
            let stage = self.projected.lock().unwrap().get(store).cloned();
            let revision = stage.as_ref().map_or(0, |stage| stage.revision);
            let descriptor =
                self.encrypted_projection_descriptor(store, TableProjection.descriptor());
            if stage
                .as_ref()
                .is_some_and(|stage| stage.descriptor != descriptor)
            {
                return Err(BackendError::InvalidRawSource.into());
            }
            let value = if let Some(value) = stage.as_ref().and_then(|stage| stage.logical.get(key))
            {
                value.clone()
            } else {
                let physical = self.physical_record_key(store, key)?;
                let mut end = physical.clone();
                end.push(0);
                let range = RecordRange {
                    start: Some(physical.clone()),
                    end: Some(end),
                };
                let page = self
                    .table_committed_page(
                        store,
                        TableQuery::Point {
                            key: physical.clone(),
                            repair: true,
                        },
                        &range,
                        None,
                        1,
                        &mut read,
                    )
                    .await?;
                page.records
                    .first()
                    .map(|(key, value)| {
                        self.decode_projected_record(store, key, value)
                            .map(|(_, value)| value)
                    })
                    .transpose()?
            };
            if self.encrypted_projection_descriptor(store, TableProjection.descriptor())
                != descriptor
            {
                return Err(StoreError::StaleCursor {
                    store: store.into(),
                }
                .into());
            }
            if self
                .projected
                .lock()
                .unwrap()
                .get(store)
                .map_or(0, |stage| stage.revision)
                == revision
            {
                return Ok(value);
            }
        }
    }

    pub(crate) async fn table_query_scan(
        &self,
        store: &str,
        cursor: Option<&crate::store::TableCursor>,
        limit: usize,
    ) -> Result<(RecordPage, Option<crate::store::TableCursor>)> {
        use crate::store::{
            StoreError, TableCursor,
            table::{CursorKind, TableProjection},
            table_query::TableQuery,
        };
        let format = self.table_format_stamp()?;
        let cursor = match cursor.map(|cursor| &cursor.0) {
            None => None,
            Some(CursorKind::Query {
                cursor,
                format: previous,
            }) if previous == &format => Some(cursor.as_ref()),
            _ => {
                return Err(StoreError::StaleCursor {
                    store: store.into(),
                }
                .into());
            }
        };
        let source = self.query_source()?;
        let read = std::sync::Arc::new(tokio::sync::Mutex::new(RecordRead::default()));
        let revision = self
            .projected
            .lock()
            .unwrap()
            .get(store)
            .map_or(0, |stage| stage.revision);
        let (page, next) = self
            .projected_scan_page(
                store,
                TableProjection.descriptor(),
                cursor,
                limit.min(128),
                Some(source.main.clone()),
                |after, count| {
                    let read = read.clone();
                    async move {
                        let mut read = read.lock().await;
                        self.table_committed_page(
                            store,
                            TableQuery::Page {
                                after: after.clone(),
                                limit: count,
                                repair: true,
                            },
                            &RecordRange::default(),
                            after.as_deref(),
                            count,
                            &mut read,
                        )
                        .await
                    }
                },
            )
            .await?;
        #[cfg(all(unix, feature = "service"))]
        if let Some(connection) = self.db.ops().remote_connection() {
            self.check_remote_scan_frontier(
                store,
                revision,
                &source.main,
                connection.get_verified_tips(
                    self.database_id().clone(),
                    self.db.auth_identity().cloned().unwrap_or_default(),
                ),
            )
            .await?;
        }
        #[cfg(not(all(unix, feature = "service")))]
        let _ = revision;
        if self.table_format_stamp()? != format {
            return Err(StoreError::StaleCursor {
                store: store.into(),
            }
            .into());
        }
        Ok((
            page,
            next.map(|cursor| {
                TableCursor(CursorKind::Query {
                    cursor: Box::new(cursor),
                    format: format.clone(),
                })
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn registered_record_read_budget_cannot_reconstruct_twice() -> Result<()> {
        use crate::{
            Instance, NewUser,
            crdt::Doc,
            store::{Registered, Table, TableData, table::TableProjection},
        };
        let (instance, mut owner) = Instance::create_backend(
            Box::new(crate::backend::database::InMemory::new()),
            NewUser::passwordless("owner"),
        )
        .await?;
        let db = owner
            .create_database(Doc::new(), &owner.get_default_key()?)
            .await?;
        let write = db.new_transaction().await?;
        write
            .get_store::<Table<String>>("rows")
            .await?
            .set("a", "canonical".into())
            .await?;
        write.commit().await?;
        let tx = db.new_transaction().await?;
        let rows = tx.get_store::<Table<String>>("rows").await?;
        rows.get("a").await?;
        let engine = instance.require_local_engine()?;
        let memory = engine
            .as_any()
            .downcast_ref::<crate::backend::database::InMemory>()
            .unwrap();
        {
            let mut inner = memory.inner.write().unwrap();
            let record = inner
                .store_state_namespaces
                .values_mut()
                .find(|namespace| {
                    namespace.ready
                        && namespace.request.store == "rows"
                        && namespace
                            .request
                            .projection
                            .name
                            .starts_with("eidetica/query-records/")
                })
                .unwrap();
            record.records.insert(b"a".to_vec(), Some(vec![0xff]));
        }
        let mut read = RecordRead::default();
        let range = RecordRange {
            start: Some(b"a".to_vec()),
            end: Some(b"a\0".to_vec()),
        };
        let query = br#"{"Point":{"key":[97],"repair":true}}"#.to_vec();
        let representation = PrivateRepresentation {
            format: TableProjection.descriptor(),
            configuration: Table::<String>::type_id().as_bytes().to_vec(),
        };
        for attempt in 0..2 {
            let result = tx
                .query_record_page::<TableData>(
                    "rows",
                    Table::<String>::type_id(),
                    query.clone(),
                    representation.clone(),
                    &TableProjection,
                    &range,
                    None,
                    1,
                    &mut read,
                    |_, bytes| Ok(Some(TableData::decode(bytes)?)),
                )
                .await;
            if attempt == 0 {
                assert!(result.is_ok());
            } else {
                assert!(
                    result.is_err(),
                    "nested registered helpers cannot replenish the logical-read repair allowance"
                );
            }
        }
        Ok(())
    }
}
