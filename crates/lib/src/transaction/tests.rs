//! Tests for the transaction module.

use super::*;
use serde::{Deserialize, Serialize};

use crate::{
    Instance,
    auth::crypto::generate_keypair,
    backend::database::InMemory,
    backend::{CacheScope, ProjectionDescriptor, StoreStateLifecycle, StoreStateRequest},
    crdt::{CRDT, Data},
    store::{DocStore, Registered},
};

#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MaxCounter(u64);

impl Data for MaxCounter {}

impl CRDT for MaxCounter {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self(self.0.max(other.0)))
    }
}

struct CounterStore {
    name: String,
    txn: Transaction,
}

impl Registered for CounterStore {
    fn type_id() -> &'static str {
        "test:max-counter"
    }
}

#[async_trait::async_trait]
impl Store for CounterStore {
    type Data = MaxCounter;

    async fn load(txn: &Transaction, name: String) -> Result<Self> {
        Ok(Self {
            name,
            txn: txn.clone(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn transaction(&self) -> &Transaction {
        &self.txn
    }
}

#[tokio::test]
async fn typed_store_state_folds_custom_crdt_without_doc_conversion() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    for value in [4, 9, 2] {
        let tx = db.new_transaction().await.unwrap();
        tx.get_store::<CounterStore>("counter").await.unwrap();
        tx.update_subtree("counter", serde_json::to_vec(&MaxCounter(value)).unwrap())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(
        db.get_store_state::<CounterStore>("counter").await.unwrap(),
        MaxCounter(9)
    );
    assert!(matches!(
        db.get_store_state::<DocStore>("counter").await.unwrap_err(),
        crate::Error::Store(ref error) if matches!(**error, StoreError::TypeMismatch { .. })
    ));
}

#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StagedRows(crate::crdt::LwwMap<String, String>);
impl Data for StagedRows {}
impl CRDT for StagedRows {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self(self.0.merge(&other.0)?))
    }
}

struct LegacyRowsProjection;
impl RecordProjection<Doc> for LegacyRowsProjection {
    fn descriptor(&self) -> ProjectionDescriptor {
        RowsProjection.descriptor()
    }
    fn mutations<'a>(
        &'a self,
        _: &'a Doc,
    ) -> Result<Box<dyn Iterator<Item = Result<RecordMutation>> + Send + 'a>> {
        unreachable!()
    }
}

struct RowsProjection;
impl RecordProjection<StagedRows> for RowsProjection {
    fn descriptor(&self) -> ProjectionDescriptor {
        ProjectionDescriptor {
            name: "test/rows".into(),
            version: 0,
        }
    }
    fn mutations<'a>(
        &'a self,
        delta: &'a StagedRows,
    ) -> Result<Box<dyn Iterator<Item = Result<RecordMutation>> + Send + 'a>> {
        Ok(Box::new(delta.0.operations().map(|(key, op)| {
            Ok(match op {
                crate::crdt::Lww::Set(value) => RecordMutation::Put {
                    key: key.as_bytes().to_vec(),
                    value: value.as_bytes().to_vec(),
                },
                crate::crdt::Lww::Delete => RecordMutation::Delete {
                    key: key.as_bytes().to_vec(),
                },
                crate::crdt::Lww::NoOp => unreachable!(),
            })
        })))
    }
}

fn row(key: &str, value: &str) -> StagedRows {
    let mut rows = crate::crdt::LwwMap::new();
    rows.set(key.to_string(), value.to_string());
    StagedRows(rows)
}

fn is_stale(
    result: crate::Result<(
        crate::backend::RecordPage,
        Option<crate::store::TableCursor>,
    )>,
) {
    assert!(
        matches!(result, Err(crate::Error::Store(error)) if matches!(*error, StoreError::StaleCursor { .. }))
    );
}

#[derive(Debug)]
struct RecordlessOps(std::sync::Arc<dyn crate::instance::backend::Backend>);

#[async_trait::async_trait]
impl crate::instance::backend::Backend for RecordlessOps {
    async fn get(&self, id: &ID) -> Result<Entry> {
        self.0.get(id).await
    }
    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        self.0.snapshot(tree).await
    }
    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        self.0.store_snapshot(tree, store).await
    }
    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        self.0.store_snapshot_at(tree, store, snapshot).await
    }
    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.0.store_at(tree, store, snapshot).await
    }
    async fn compute_merge_state(
        &self,
        tree: &ID,
        store: &str,
        ids: &[ID],
    ) -> Result<crate::instance::backend::MergeSlice> {
        self.0.compute_merge_state(tree, store, ids).await
    }
    async fn put(&self, entry: Entry) -> Result<()> {
        self.0.put(entry).await
    }
    async fn write_entry(
        &self,
        status: VerificationStatus,
        entry: Entry,
        source: WriteSource,
    ) -> Result<()> {
        self.0.write_entry(status, entry, source).await
    }
    async fn get_instance_metadata(&self) -> Result<Option<crate::backend::InstanceMetadata>> {
        self.0.get_instance_metadata().await
    }
    async fn set_instance_metadata(
        &self,
        metadata: &crate::backend::InstanceMetadata,
    ) -> Result<()> {
        self.0.set_instance_metadata(metadata).await
    }
}

#[tokio::test]
async fn projected_recordless_fallback_reduces_typed_history() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("a", "old"))
        .await
        .unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("b", "old"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut deletion = crate::crdt::LwwMap::new();
    deletion.delete("a".to_string());
    tx.stage_projected_delta("rows", &RowsProjection, StagedRows(deletion))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let recordless = db
        .clone()
        .with_test_ops(std::sync::Arc::new(RecordlessOps(db.backend().unwrap())));
    let tx = recordless.new_transaction().await.unwrap();
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"a")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"b")
            .await
            .unwrap(),
        Some(b"old".to_vec())
    );
    tx.stage_projected_delta("rows", &RowsProjection, row("c", "new"))
        .await
        .unwrap();
    let (page, next) = tx
        .projected_record_scan_page("rows", &RowsProjection, None, 1)
        .await
        .unwrap();
    assert_eq!(page.records, vec![(b"b".to_vec(), b"old".to_vec())]);
    let (page, end) = tx
        .projected_record_scan_page("rows", &RowsProjection, next.as_ref(), 1)
        .await
        .unwrap();
    assert_eq!(page.records, vec![(b"c".to_vec(), b"new".to_vec())]);
    assert!(end.is_none());
}

#[tokio::test]
async fn projected_streaming_history_applies_deletes_across_chunks() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut rows = crate::crdt::LwwMap::new();
    for i in 0..140 {
        rows.set(format!("{i:03}"), "old".to_string());
    }
    tx.stage_projected_delta("rows", &RowsProjection, StagedRows(rows))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut rows = crate::crdt::LwwMap::new();
    rows.delete("000".to_string());
    rows.delete("139".to_string());
    tx.stage_projected_delta("rows", &RowsProjection, StagedRows(rows))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"000")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"139")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"001")
            .await
            .unwrap(),
        Some(b"old".to_vec())
    );
    let view = tx.record_view("rows", &RowsProjection).await.unwrap();
    assert_eq!(
        db.ops()
            .store_state_record_get(&view, b"000")
            .await
            .unwrap(),
        None
    );
    let tx = db.new_transaction().await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("000", "new"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"000")
            .await
            .unwrap(),
        Some(b"new".to_vec())
    );
}

#[tokio::test]
async fn projected_real_record_view_physical_scan_and_overlay() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("a", "old"))
        .await
        .unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("b", "old"))
        .await
        .unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("c", "old"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    // These rows are not in the new transaction's overlay: the first read
    // must materialize and address a real backend RecordView.
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"b")
            .await
            .unwrap(),
        Some(b"old".to_vec())
    );
    let view = tx.record_view("rows", &RowsProjection).await.unwrap();
    assert_eq!(
        db.ops().store_state_record_get(&view, b"b").await.unwrap(),
        Some(b"old".to_vec())
    );
    let mut deletion = crate::crdt::LwwMap::new();
    deletion.delete("b".to_string());
    tx.stage_projected_delta("rows", &RowsProjection, StagedRows(deletion))
        .await
        .unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("d", "new"))
        .await
        .unwrap();
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"b")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"d")
            .await
            .unwrap(),
        Some(b"new".to_vec())
    );
    let mut cursor = None;
    let mut rows = Vec::new();
    loop {
        let (page, next) = tx
            .projected_record_scan_page("rows", &RowsProjection, cursor.as_ref(), 1)
            .await
            .unwrap();
        rows.extend(page.records);
        cursor = next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        rows,
        vec![
            (b"a".to_vec(), b"old".to_vec()),
            (b"c".to_vec(), b"old".to_vec()),
            (b"d".to_vec(), b"new".to_vec())
        ]
    );
    assert_eq!(
        db.ops().store_state_record_get(&view, b"b").await.unwrap(),
        Some(b"old".to_vec())
    );
}

#[tokio::test]
async fn projected_real_backend_fetch_rejects_racing_overlay() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("a", "old"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let view = tx.record_view("rows", &RowsProjection).await.unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let reader = tx.clone();
    let database = db.clone();
    let task = tokio::spawn(async move {
        let mut entered = Some(entered_tx);
        let mut release = Some(release_rx);
        reader
            .projected_scan_page(
                "rows",
                RowsProjection.descriptor(),
                None,
                1,
                move |after, limit| {
                    entered.take().unwrap().send(()).unwrap();
                    let wait = release.take().unwrap();
                    let database = database.clone();
                    let view = view.clone();
                    async move {
                        wait.await.unwrap();
                        database
                            .ops()
                            .store_state_record_scan(
                                &view,
                                &RecordRange::default(),
                                after.as_deref(),
                                limit,
                            )
                            .await
                    }
                },
            )
            .await
    });
    entered_rx.await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("b", "new"))
        .await
        .unwrap();
    release_tx.send(()).unwrap();
    is_stale(task.await.unwrap());
}

#[tokio::test]
async fn projected_page_cursor_rejects_put_delete_and_other_view() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("b", "two"))
        .await
        .unwrap();
    let backend = || async {
        Ok(crate::backend::RecordPage {
            records: vec![
                (b"a".to_vec(), b"one".to_vec()),
                (b"c".to_vec(), b"three".to_vec()),
            ],
            next: None,
        })
    };
    let (first, cursor) = tx
        .projected_scan_page("rows", RowsProjection.descriptor(), None, 1, |_, _| {
            backend()
        })
        .await
        .unwrap();
    assert_eq!(first.records, vec![(b"a".to_vec(), b"one".to_vec())]);
    let cursor = cursor.unwrap();
    let physical = [
        (b"a".to_vec(), b"one".to_vec()),
        (b"c".to_vec(), b"three".to_vec()),
    ];
    let (second, next) = tx
        .projected_scan_page(
            "rows",
            RowsProjection.descriptor(),
            Some(&cursor),
            1,
            |after, _| {
                let records = physical
                    .iter()
                    .filter(|(key, _)| after.as_ref().is_none_or(|after| key > after))
                    .cloned()
                    .collect();
                async move {
                    Ok(crate::backend::RecordPage {
                        records,
                        next: None,
                    })
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(second.records, vec![(b"b".to_vec(), b"two".to_vec())]);
    assert!(next.is_some());
    let other = db.new_transaction().await.unwrap();
    other
        .stage_projected_delta("rows", &RowsProjection, row("b", "two"))
        .await
        .unwrap();
    is_stale(
        other
            .projected_scan_page(
                "rows",
                RowsProjection.descriptor(),
                Some(&cursor),
                1,
                |_, _| backend(),
            )
            .await,
    );
    let mut different = RowsProjection.descriptor();
    different.version += 1;
    is_stale(
        tx.projected_scan_page("rows", different, Some(&cursor), 1, |_, _| backend())
            .await,
    );
    tx.stage_projected_delta("rows", &RowsProjection, row("d", "four"))
        .await
        .unwrap();
    is_stale(
        tx.projected_scan_page(
            "rows",
            RowsProjection.descriptor(),
            Some(&cursor),
            1,
            |_, _| backend(),
        )
        .await,
    );
    let (_, cursor) = tx
        .projected_scan_page("rows", RowsProjection.descriptor(), None, 1, |_, _| {
            backend()
        })
        .await
        .unwrap();
    let mut deletion = crate::crdt::LwwMap::new();
    deletion.delete("b".to_string());
    tx.stage_projected_delta("rows", &RowsProjection, StagedRows(deletion))
        .await
        .unwrap();
    is_stale(
        tx.projected_scan_page(
            "rows",
            RowsProjection.descriptor(),
            cursor.as_ref(),
            1,
            |_, _| backend(),
        )
        .await,
    );
}

#[tokio::test]
async fn projected_page_discards_awaited_fetch_after_racing_mutation() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("a", "one"))
        .await
        .unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let reader = tx.clone();
    let task = tokio::spawn(async move {
        let mut entered = Some(entered_tx);
        let mut release = Some(release_rx);
        reader
            .projected_scan_page("rows", RowsProjection.descriptor(), None, 1, move |_, _| {
                entered.take().unwrap().send(()).unwrap();
                let wait = release.take().unwrap();
                async move {
                    wait.await.unwrap();
                    Ok(crate::backend::RecordPage {
                        records: vec![(b"b".to_vec(), b"old".to_vec())],
                        next: None,
                    })
                }
            })
            .await
    });
    entered_rx.await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("b", "new"))
        .await
        .unwrap();
    release_tx.send(()).unwrap();
    is_stale(task.await.unwrap());
}

#[tokio::test]
async fn projected_staging_installs_concurrent_writes_in_canonical_and_both_overlays() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    // Both writers must finish constructing from revision 0 before either installs.
    // A bounded barrier forces the lost-update interleaving of an unchecked snapshot.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let handles = [("a", "one"), ("b", "two")]
        .into_iter()
        .map(|(key, value)| {
            let tx = tx.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(tx.stage_projected_delta(
                    "rows",
                    &RacingProjection,
                    RacingRows {
                        rows: row(key, value),
                        barrier: Some(barrier),
                    },
                ))
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let state = tx.projected.lock().unwrap().get("rows").unwrap().clone();
    assert_eq!(state.revision, 2);
    assert_eq!(state.logical.len(), 2);
    assert_eq!(state.physical, state.logical);
    assert!(
        matches!(tx.stage_record("rows", &LegacyRowsProjection, b"x".to_vec(), Some(b"x".to_vec())),
        Err(crate::Error::Store(error)) if matches!(*error, StoreError::InvalidOperation { .. }))
    );
    let staged: RacingRows = tx.get_local_data("rows").unwrap().unwrap();
    assert_eq!(staged.rows.0.get(&"a".to_string()).unwrap(), "one");
    assert_eq!(staged.rows.0.get(&"b".to_string()).unwrap(), "two");
    tx.commit().await.unwrap();
    let next = db.new_transaction().await.unwrap();
    let persisted: RacingRows = next.get_full_state("rows").await.unwrap();
    assert_eq!(persisted.rows, staged.rows);
}

#[derive(Clone, Default, Deserialize)]
struct RacingRows {
    rows: StagedRows,
    #[serde(skip)]
    barrier: Option<std::sync::Arc<std::sync::Barrier>>,
}
impl Serialize for RacingRows {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        #[derive(Serialize)]
        struct Wire<'a> {
            rows: &'a StagedRows,
        }
        Wire { rows: &self.rows }.serialize(serializer)
    }
}
impl Data for RacingRows {}
impl CRDT for RacingRows {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self {
            rows: self.rows.merge(&other.rows)?,
            barrier: None,
        })
    }
}
struct RacingProjection;
impl RecordProjection<RacingRows> for RacingProjection {
    fn descriptor(&self) -> ProjectionDescriptor {
        RowsProjection.descriptor()
    }
    fn mutations<'a>(
        &'a self,
        delta: &'a RacingRows,
    ) -> Result<Box<dyn Iterator<Item = Result<RecordMutation>> + Send + 'a>> {
        RowsProjection.mutations(&delta.rows)
    }
}

#[derive(Clone, Default, Deserialize)]
struct FallibleRows {
    rows: StagedRows,
    fail: bool,
}
impl Serialize for FallibleRows {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        if self.fail {
            return Err(serde::ser::Error::custom("injected serialization failure"));
        }
        #[derive(Serialize)]
        struct Wire<'a> {
            rows: &'a StagedRows,
            fail: bool,
        }
        Wire {
            rows: &self.rows,
            fail: self.fail,
        }
        .serialize(serializer)
    }
}
impl Data for FallibleRows {}
impl CRDT for FallibleRows {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self {
            rows: self.rows.merge(&other.rows)?,
            fail: other.fail,
        })
    }
}
struct FallibleProjection;
impl RecordProjection<FallibleRows> for FallibleProjection {
    fn descriptor(&self) -> ProjectionDescriptor {
        RowsProjection.descriptor()
    }
    fn mutations<'a>(
        &'a self,
        delta: &'a FallibleRows,
    ) -> Result<Box<dyn Iterator<Item = Result<RecordMutation>> + Send + 'a>> {
        RowsProjection.mutations(&delta.rows)
    }
}

struct FailingEncryptor;
impl Encryptor for FailingEncryptor {
    fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }
    fn encrypt(&self, _: &[u8]) -> Result<Vec<u8>> {
        Err(StoreError::SerializationFailed {
            store: "rows".into(),
            reason: "injected encryption failure".into(),
        }
        .into())
    }
}

#[tokio::test]
async fn projected_staging_failures_leave_canonical_and_overlays_unchanged() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("a", "one"))
        .await
        .unwrap();
    let before = tx.projected.lock().unwrap().get("rows").unwrap().clone();
    struct BadProjection;
    impl RecordProjection<StagedRows> for BadProjection {
        fn descriptor(&self) -> ProjectionDescriptor {
            RowsProjection.descriptor()
        }
        fn mutations<'a>(
            &'a self,
            _: &'a StagedRows,
        ) -> Result<Box<dyn Iterator<Item = Result<RecordMutation>> + Send + 'a>> {
            Err(StoreError::SerializationFailed {
                store: "rows".into(),
                reason: "injected projection failure".into(),
            }
            .into())
        }
    }
    assert!(
        tx.stage_projected_delta("rows", &BadProjection, row("b", "two"))
            .await
            .is_err()
    );
    let serial_tx = db.new_transaction().await.unwrap();
    let baseline = FallibleRows {
        rows: row("a", "one"),
        fail: false,
    };
    serial_tx
        .stage_projected_delta("rows", &FallibleProjection, baseline.clone())
        .await
        .unwrap();
    let prior = serial_tx
        .projected
        .lock()
        .unwrap()
        .get("rows")
        .unwrap()
        .clone();
    assert!(
        serial_tx
            .stage_projected_delta(
                "rows",
                &FallibleProjection,
                FallibleRows {
                    rows: row("b", "two"),
                    fail: true
                }
            )
            .await
            .is_err()
    );
    let unchanged = serial_tx
        .projected
        .lock()
        .unwrap()
        .get("rows")
        .unwrap()
        .clone();
    assert_eq!(prior.canonical, unchanged.canonical);
    assert_eq!(prior.logical, unchanged.logical);
    assert_eq!(prior.physical, unchanged.physical);
    assert_eq!(prior.revision, unchanged.revision);
    assert_eq!(
        serial_tx
            .get_local_data::<FallibleRows>("rows")
            .unwrap()
            .unwrap()
            .rows,
        baseline.rows
    );
    let encrypted_tx = db.new_transaction().await.unwrap();
    encrypted_tx
        .register_encryptor("rows", Box::new(FailingEncryptor))
        .unwrap();
    let prior_error = encrypted_tx
        .stage_projected_delta("rows", &RowsProjection, row("a", "one"))
        .await;
    assert!(prior_error.is_err());
    assert!(encrypted_tx.projected.lock().unwrap().get("rows").is_none());
    assert!(
        encrypted_tx
            .get_local_data::<StagedRows>("rows")
            .unwrap()
            .is_none()
    );
    let after = tx.projected.lock().unwrap().get("rows").unwrap().clone();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.canonical, before.canonical);
    assert_eq!(after.logical, before.logical);
    assert_eq!(after.physical, before.physical);
    assert!(
        tx.register_encryptor("rows", Box::new(FailingEncryptor))
            .is_err()
    );
    assert_eq!(
        tx.get_local_data::<StagedRows>("rows").unwrap().unwrap(),
        row("a", "one")
    );
}

/// Test that corrupted auth configuration prevents commit
///
/// Validates that transactions reject changes that would corrupt the auth configuration,
/// preventing corrupted entries from entering the Merkle DAG.
#[tokio::test]
async fn test_prevent_auth_corruption() {
    let backend = InMemory::new();
    let (instance, _admin) =
        Instance::create_backend(Box::new(backend), crate::NewUser::passwordless("admin"))
            .await
            .unwrap();
    let (private_key, _) = generate_keypair();

    // Create database with the test key
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    // Initial operation should work
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store.set("initial", "value").await.unwrap();
    tx.commit().await.expect("Initial operation should succeed");

    // Test corruption path 1: Set auth to wrong type (String instead of Doc)
    let tx = database.new_transaction().await.unwrap();
    let settings = tx.get_store::<DocStore>("_settings").await.unwrap();
    settings.set("auth", "corrupted_string").await.unwrap();

    let result = tx.commit().await;
    assert!(
        result.is_err(),
        "Corruption commit (wrong type) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Test corruption path 2: Delete auth (creates CRDT tombstone)
    let tx = database.new_transaction().await.unwrap();
    let settings = tx.get_store::<DocStore>("_settings").await.unwrap();
    settings.delete("auth").await.unwrap();

    let result = tx.commit().await;
    assert!(
        result.is_err(),
        "Deletion commit (tombstone) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Verify database is still functional after preventing corruption
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store
        .set("after_prevented_corruption", "value")
        .await
        .unwrap();
    tx.commit()
        .await
        .expect("Normal operations should still work");
}

#[tokio::test]
async fn opaque_non_doc_state_materializes_cold_warm_and_after_clear() {
    let backend = InMemory::new();
    let (instance, _admin) =
        Instance::create_backend(Box::new(backend), crate::NewUser::passwordless("admin"))
            .await
            .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let tx = database.new_transaction().await.unwrap();
    tx.update_subtree("counter", serde_json::to_vec(&MaxCounter(7)).unwrap())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let tx = database.new_transaction().await.unwrap();
    let cold = tx.get_full_state::<MaxCounter>("counter").await.unwrap();
    let warm = tx.get_full_state::<MaxCounter>("counter").await.unwrap();
    assert_eq!(cold, MaxCounter(7));
    assert_eq!(warm, cold);

    let backend = database.backend().unwrap();
    let entry_id = backend
        .store_snapshot(database.root_id(), "counter")
        .await
        .unwrap()
        .into_tips()
        .pop()
        .unwrap();
    let request = StoreStateRequest {
        database: database.root_id().clone(),
        store: "counter".to_string(),
        lifecycle: StoreStateLifecycle::Derived,
        scope: CacheScope::Shared,
        projection: ProjectionDescriptor {
            name: "eidetica/opaque".to_string(),
            version: 0,
        },
        source_key: entry_id.to_string().into_bytes(),
    };
    assert!(
        backend
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_some()
    );
    backend.clear_derived_store_state().await.unwrap();
    assert!(
        backend
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_none()
    );

    let rebuilt = tx.get_full_state::<MaxCounter>("counter").await.unwrap();
    assert_eq!(rebuilt, cold);
    assert!(
        backend
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        CounterStore::state_model().descriptor().name,
        "eidetica/opaque"
    );
}
