//! Table-owned planning; only the opaque physical point/page message is encoded.

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    backend::{BackendError, RecordRange},
    crdt::{Codec, Doc},
    store::{
        ExecuteQuery, RawTable, RowCodec, StoreError, Table, TableCursor, TablePage,
        query::{QueryOutcome, StoreQueryContext, StoreQueryHandler},
        table::TableProjection,
    },
};

/// Borrowed application point query; no Serde/Clone/Send/static requirement.
pub struct GetRow<'a>(pub &'a str);
/// A bounded physical-order page. Unreadable rows still advance its cursor.
pub struct ScanRows<'a> {
    pub cursor: Option<&'a TableCursor>,
    pub limit: usize,
}
/// Application predicate executed client-side over bounded opaque pages.
pub struct SearchRows<F>(pub F);

#[derive(Serialize, Deserialize)]
pub(crate) enum TableQuery {
    Point {
        key: Vec<u8>,
        repair: bool,
    },
    Page {
        after: Option<Vec<u8>>,
        limit: usize,
        repair: bool,
    },
}
impl Codec for TableQuery {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl<'q, T, C: RowCodec<T>> ExecuteQuery<GetRow<'q>> for Table<T, C> {
    type Output = T;
    async fn execute<'a>(&'a self, query: GetRow<'q>) -> Result<T>
    where
        GetRow<'q>: 'a,
    {
        self.get_planned(query.0).await
    }
}
impl<'q, T, C: RowCodec<T>> ExecuteQuery<ScanRows<'q>> for Table<T, C> {
    type Output = TablePage<T>;
    async fn execute<'a>(&'a self, query: ScanRows<'q>) -> Result<Self::Output>
    where
        ScanRows<'q>: 'a,
    {
        self.scan_planned(query.cursor, query.limit).await
    }
}
impl<T, C: RowCodec<T>, F: Fn(&T) -> bool> ExecuteQuery<SearchRows<F>> for Table<T, C> {
    type Output = Vec<(String, T)>;
    async fn execute<'a>(&'a self, query: SearchRows<F>) -> Result<Self::Output>
    where
        SearchRows<F>: 'a,
    {
        self.search_planned(query.0).await
    }
}
impl<'q> ExecuteQuery<GetRow<'q>> for RawTable {
    type Output = Vec<u8>;
    async fn execute<'a>(&'a self, query: GetRow<'q>) -> Result<Self::Output>
    where
        GetRow<'q>: 'a,
    {
        self.get_planned(query.0).await
    }
}
impl<'q> ExecuteQuery<ScanRows<'q>> for RawTable {
    type Output = TablePage<Vec<u8>>;
    async fn execute<'a>(&'a self, query: ScanRows<'q>) -> Result<Self::Output>
    where
        ScanRows<'q>: 'a,
    {
        self.scan_planned(query.cursor, query.limit).await
    }
}

impl StoreQueryHandler for RawTable {
    async fn handle_query(context: &StoreQueryContext<'_>, query: &[u8]) -> Result<QueryOutcome> {
        // Source/type validation preceded dispatch. Configuration selects LWW
        // interpretation; no application codec or T is installed on the daemon.
        let registration = Doc::decode(context.registration())?;
        if registration
            .get("config")
            .and_then(|v| v.as_doc())
            .and_then(|config| config.get("row_codec"))
            .and_then(|v| v.as_text())
            .is_none_or(|format| format.is_empty())
        {
            return Err(StoreError::InvalidConfiguration {
                store: context.store().into(),
                reason: "Table requires a non-empty row_codec identity".into(),
            }
            .into());
        }
        let (range, after, limit, repair) = match TableQuery::decode(query)? {
            TableQuery::Point { key, repair } => {
                if std::str::from_utf8(&key).is_err() {
                    return Err(BackendError::InvalidRawPage.into());
                }
                let mut end = key.clone();
                end.push(0);
                (
                    RecordRange {
                        start: Some(key),
                        end: Some(end),
                    },
                    None,
                    1,
                    repair,
                )
            }
            TableQuery::Page {
                after,
                limit,
                repair,
            } => (RecordRange::default(), after, limit, repair),
        };
        let page = context
            .records::<Self>(&TableProjection, &range, after.as_deref(), limit, repair)
            .await;
        let page = match page {
            Ok(page) => page,
            Err(error) if error.is_unsupported_store_state() => {
                return Ok(QueryOutcome::Unavailable);
            }
            Err(error) => return Err(error),
        };
        Ok(QueryOutcome::Result(serde_json::to_vec(&page)?))
    }
}
