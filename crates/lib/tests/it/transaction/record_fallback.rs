//! A backend written before the record-based Store-state substrate keeps
//! working: its default record methods report "unsupported", and transaction
//! reads fall back to folding history instead of failing.
//!
//! `Recordless` wraps a real engine and forwards every entry-addressed method,
//! deliberately leaving the record methods at their defaults — the shape of an
//! old custom [`BackendImpl`](eidetica::backend::BackendImpl).

use std::{
    any::Any,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use eidetica::{
    Database, Instance, NewUser, Result, Snapshot,
    auth::crypto::generate_keypair,
    backend::database::InMemory,
    backend::{
        BackendError, BackendImpl, InstanceMetadata, InstanceSecrets, RecordMutations, RecordPage,
        RecordRange, RecordView, StagingToken, StoreStateRequest, VerificationStatus,
    },
    crdt::Doc,
    entry::{Entry, ID},
    store::{DocStore, PasswordStore, Table},
};

static ENCRYPTED_FALLBACK_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn recordless_backend_can_serve_a_client_without_reclamation() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("recordless.sock");
    let (server, _admin) = Instance::create_backend(
        Box::new(Recordless(InMemory::new(), None)),
        NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (shutdown, receiver) = tokio::sync::watch::channel(());
    let service = eidetica::service::ServiceServer::bind(server.clone(), &socket)
        .await
        .unwrap();
    let task = tokio::spawn(service.run(receiver));
    let client = Instance::connect(format!("unix://{}", socket.display()))
        .await
        .unwrap();
    assert_eq!(client.id(), server.id());
    let user = client.login_user("admin", None).await.unwrap();
    assert!(user.get_default_key().is_ok());
    let mut admin = server.login_user("admin", None).await.unwrap();
    let key = admin.get_default_key().unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("key", "value")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let identity = Database::find_sigkeys(&server, db.root_id(), &key)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .0;
    let conn = client.remote_connection().unwrap();
    let state = conn
        .get_store_state::<DocStore>(db.root_id().clone(), identity, "data".into())
        .await
        .unwrap();
    assert_eq!(state.get_as::<&str>("key"), Some("value"));
    shutdown.send(()).unwrap();
    task.await.unwrap().unwrap();
}

/// A pre-record-substrate backend: full entry storage, no record support.
struct Recordless<B>(B, Option<Arc<StaleThenUnsupported>>);

struct StaleThenUnsupported {
    phase: AtomicU8,
    get: AtomicU8,
    scan: AtomicU8,
}

#[async_trait::async_trait]
impl<B: BackendImpl> BackendImpl for Recordless<B> {
    async fn get(&self, id: &ID) -> Result<Entry> {
        self.0.get(id).await
    }

    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        self.0.get_verification_status(id).await
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        self.0.put(entry).await
    }

    async fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> Result<()> {
        self.0
            .update_verification_status(id, verification_status)
            .await
    }

    async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        self.0.get_entries_by_verification_status(status).await
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
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        self.0.store_snapshot_at(tree, store, main_snapshot).await
    }

    async fn all_roots(&self) -> Result<Vec<ID>> {
        self.0.all_roots().await
    }

    async fn find_merge_base(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<Option<ID>> {
        self.0.find_merge_base(tree, store, entry_ids).await
    }

    fn as_any(&self) -> &dyn Any {
        self.0.as_any()
    }

    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        self.0.get_tree(tree).await
    }

    async fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>> {
        self.0.get_store(tree, store).await
    }

    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        self.0.get_tree_from_tips(tree, tips).await
    }

    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.0.store_at(tree, store, snapshot).await
    }

    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        self.0
            .get_sorted_store_parents(tree_id, entry_id, store)
            .await
    }

    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: Option<&ID>,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        self.0
            .get_path_from_to(tree_id, store, from_id, to_ids)
            .await
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.0.get_instance_metadata().await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.0.set_instance_metadata(metadata).await
    }

    async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        self.0.get_instance_secrets().await
    }

    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()> {
        self.0.set_instance_secrets(secrets).await
    }

    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        match self.1.as_ref() {
            Some(_) => self.0.resolve_store_state(request).await,
            None => Err(BackendError::StoreStateStorageUnsupported.into()),
        }
    }

    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken> {
        match self.1.as_ref() {
            Some(_) => self.0.begin_store_state_staging(request).await,
            None => Err(BackendError::StoreStateStorageUnsupported.into()),
        }
    }

    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        match self.1.as_ref() {
            Some(_) => self.0.stage_store_state_records(token, records).await,
            None => Err(BackendError::StoreStateStorageUnsupported.into()),
        }
    }

    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        match self.1.as_ref() {
            Some(_) => self.0.publish_store_state(token).await,
            None => Err(BackendError::StoreStateStorageUnsupported.into()),
        }
    }

    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let Some(stale) = self.1.as_ref() else {
            return Err(BackendError::StoreStateStorageUnsupported.into());
        };
        if stale.phase.load(Ordering::SeqCst) == 0 {
            return self.0.store_state_record_get(view, key).await;
        }
        if stale.get.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(BackendError::InvalidStoreStateView.into());
        }
        if stale.phase.load(Ordering::SeqCst) == 1 {
            return Err(BackendError::StoreStateStorageUnsupported.into());
        }
        self.0.store_state_record_get(view, key).await
    }

    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        let Some(stale) = self.1.as_ref() else {
            return Err(BackendError::StoreStateStorageUnsupported.into());
        };
        if stale.phase.load(Ordering::SeqCst) < 2 {
            return self
                .0
                .store_state_record_scan(view, range, after, limit)
                .await;
        }
        if stale.scan.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(BackendError::InvalidStoreStateView.into());
        }
        if stale.phase.load(Ordering::SeqCst) == 2 {
            return Err(BackendError::StoreStateStorageUnsupported.into());
        }
        self.0
            .store_state_record_scan(view, range, after, limit)
            .await
    }

    // Record-based Store-state methods keep their defaults, which report
    // `StoreStateStorageUnsupported` — exactly what an old custom backend does.
}

/// Writes land and reads come back through real transactions even though the
/// backend has no record substrate: materialization folds history instead of
/// failing on the unsupported capability. A genuine storage failure must still
/// surface — this backend forwards every entry call, so only the record
/// capability is missing, and the reads below would fail if the fallback
/// swallowed real errors.
#[tokio::test]
async fn old_backend_without_record_support_reads_through_history() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(Recordless(InMemory::new(), None)),
        NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    for (key, value) in [("alpha", "1"), ("beta", "2")] {
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("data").await.unwrap();
        store.set(key, value).await.unwrap();
        tx.commit().await.unwrap();
    }

    // Cold read, then a second read over the same history.
    for _ in 0..2 {
        let viewer = database.get_store_viewer::<DocStore>("data").await.unwrap();
        assert_eq!(viewer.get_string("alpha").await.unwrap(), "1");
        assert_eq!(viewer.get_string("beta").await.unwrap(), "2");
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct TableRow {
    value: u32,
}

/// Table keeps its historical behavior when a custom backend does not implement
/// cached records, including ordered paging and transaction-local changes.
#[tokio::test]
async fn table_uses_history_when_record_storage_is_unsupported() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(Recordless(InMemory::new(), None)),
        NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let tx = database.new_transaction().await.unwrap();
    let table = tx.get_store::<Table<TableRow>>("rows").await.unwrap();
    table.set("b", TableRow { value: 2 }).await.unwrap();
    table.set("a", TableRow { value: 1 }).await.unwrap();
    tx.commit().await.unwrap();

    let tx = database.new_transaction().await.unwrap();
    let table = tx.get_store::<Table<TableRow>>("rows").await.unwrap();
    table.set("c", TableRow { value: 3 }).await.unwrap();
    assert!(table.delete("a").await.unwrap());
    assert_eq!(table.get("b").await.unwrap().value, 2);
    assert!(table.get("a").await.is_err());

    let first = table.scan_page(None, 1).await.unwrap();
    assert_eq!(first.rows, [("b".to_string(), TableRow { value: 2 })]);
    let second = table.scan_page(first.next.as_ref(), 1).await.unwrap();
    assert_eq!(second.rows, [("c".to_string(), TableRow { value: 3 })]);
    assert!(second.next.is_none());
}

#[tokio::test]
async fn table_dotted_json_rows_match_fresh_and_historical_projection() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(Recordless(InMemory::new(), None)),
        NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<serde_json::Value>>("rows")
        .await
        .unwrap();
    table.set("a.b", serde_json::json!({})).await.unwrap();
    table.set("a.c", serde_json::json!([])).await.unwrap();
    table
        .set("a.d", serde_json::json!({"nested": {"value": 1}}))
        .await
        .unwrap();
    table.set("top", serde_json::json!({})).await.unwrap();
    table.set("z", serde_json::json!(null)).await.unwrap();
    let expected = vec![
        ("a.b".to_string(), serde_json::json!({})),
        ("a.c".to_string(), serde_json::json!([])),
        (
            "a.d".to_string(),
            serde_json::json!({"nested": {"value": 1}}),
        ),
        ("top".to_string(), serde_json::json!({})),
        ("z".to_string(), serde_json::json!(null)),
    ];
    assert_eq!(table.get("a.b").await.unwrap(), serde_json::json!({}));
    assert_eq!(table.scan_page(None, 5).await.unwrap().rows, expected);
    tx.commit().await.unwrap();

    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<serde_json::Value>>("rows")
        .await
        .unwrap();
    assert_eq!(table.get("a.b").await.unwrap(), serde_json::json!({}));
    assert_eq!(table.scan_page(None, 5).await.unwrap().rows, expected);
    drop(tx);

    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<serde_json::Value>>("rows")
        .await
        .unwrap();
    assert!(table.delete("a.b").await.unwrap());
    tx.commit().await.unwrap();

    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<serde_json::Value>>("rows")
        .await
        .unwrap();
    assert!(table.get("a.b").await.is_err());
    assert_eq!(
        table.scan_page(None, 5).await.unwrap().rows,
        expected
            .iter()
            .filter(|(key, _)| key != "a.b")
            .cloned()
            .collect::<Vec<_>>()
    );
    table.set("a.b", serde_json::json!({})).await.unwrap();
    tx.commit().await.unwrap();

    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<serde_json::Value>>("rows")
        .await
        .unwrap();
    assert_eq!(table.get("a.b").await.unwrap(), serde_json::json!({}));
    assert_eq!(table.scan_page(None, 5).await.unwrap().rows, expected);
}

/// A stale record view may discover that the refreshed backend no longer
/// supports record reads; both get and scan must take the normal history path.
#[tokio::test]
async fn stale_record_view_retries_fallback_to_history() {
    let state = Arc::new(StaleThenUnsupported {
        phase: AtomicU8::new(0),
        get: AtomicU8::new(0),
        scan: AtomicU8::new(0),
    });
    let (instance, _admin) = Instance::create_backend(
        Box::new(Recordless(InMemory::new(), Some(state.clone()))),
        NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let tx = database.new_transaction().await.unwrap();
    let table = tx.get_store::<Table<TableRow>>("rows").await.unwrap();
    table.set("a", TableRow { value: 1 }).await.unwrap();
    table.set("b", TableRow { value: 2 }).await.unwrap();
    tx.commit().await.unwrap();

    let tx = database.new_transaction().await.unwrap();
    let table = tx.get_store::<Table<TableRow>>("rows").await.unwrap();
    state.phase.store(1, Ordering::SeqCst);
    assert_eq!(table.get("a").await.unwrap().value, 1);
    state.phase.store(2, Ordering::SeqCst);
    let page = table.scan_page(None, 2).await.unwrap();
    assert_eq!(
        page.rows,
        [
            ("a".to_string(), TableRow { value: 1 }),
            ("b".to_string(), TableRow { value: 2 }),
        ]
    );
}

async fn encrypted_table_with_overlays(database: &Database) -> Table<TableRow> {
    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<Table<TableRow>>>("encrypted_rows")
        .await
        .unwrap();
    encrypted.open("pass").unwrap();
    let table = encrypted.inner().await.unwrap();
    assert!(table.delete("amber").await.unwrap());
    table.set("cobalt", TableRow { value: 100 }).await.unwrap();
    table.set("new-row", TableRow { value: 200 }).await.unwrap();
    table
}

async fn scan_pages(
    table: &Table<TableRow>,
    limit: usize,
) -> Vec<eidetica::store::TablePage<TableRow>> {
    scan_pages_after(table, None, limit).await
}

async fn scan_pages_after(
    table: &Table<TableRow>,
    mut cursor: Option<eidetica::store::TableCursor>,
    limit: usize,
) -> Vec<eidetica::store::TablePage<TableRow>> {
    let mut pages = Vec::new();
    loop {
        let page = table.scan_page(cursor.as_ref(), limit).await.unwrap();
        cursor = page.next.clone();
        pages.push(page);
        if cursor.is_none() {
            return pages;
        }
    }
}

/// Encrypted cursors always describe physical-key order, including when a
/// record-capable backend becomes unavailable between pages.
#[tokio::test]
async fn encrypted_table_history_fallback_preserves_physical_pagination() {
    let _guard = ENCRYPTED_FALLBACK_TEST.lock().await;
    let state = Arc::new(StaleThenUnsupported {
        phase: AtomicU8::new(0),
        get: AtomicU8::new(0),
        scan: AtomicU8::new(0),
    });
    let (instance, _admin) = Instance::create_backend(
        Box::new(Recordless(InMemory::new(), Some(state.clone()))),
        NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<Table<TableRow>>>("encrypted_rows")
        .await
        .unwrap();
    encrypted.initialize("pass", Doc::new()).await.unwrap();
    let table = encrypted.inner().await.unwrap();
    for (index, key) in [
        "amber", "bronze", "cobalt", "denim", "emerald", "fuchsia", "gold", "hazel", "indigo",
        "jade", "khaki", "lilac",
    ]
    .into_iter()
    .enumerate()
    {
        table
            .set(
                key,
                TableRow {
                    value: index as u32,
                },
            )
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let database = Database::open(&instance, database.root_id())
        .await
        .unwrap()
        .allow_unverified();

    let cached = scan_pages(&encrypted_table_with_overlays(&database).await, 3).await;
    let cached_keys = cached
        .iter()
        .flat_map(|page| page.rows.iter().map(|(key, _)| key.clone()))
        .collect::<Vec<_>>();
    let mut logical_keys = cached_keys.clone();
    logical_keys.sort();
    assert_ne!(
        cached_keys, logical_keys,
        "fixture must distinguish physical and logical order"
    );

    state.phase.store(0, Ordering::SeqCst);
    let history_table = encrypted_table_with_overlays(&database).await;
    state.scan.store(0, Ordering::SeqCst);
    state.phase.store(2, Ordering::SeqCst);
    let historical = scan_pages(&history_table, 3).await;
    assert_eq!(
        historical, cached,
        "cache and history must return identical pages and cursors"
    );

    state.phase.store(0, Ordering::SeqCst);
    let cached_then_history = encrypted_table_with_overlays(&database).await;
    let first = cached_then_history.scan_page(None, 3).await.unwrap();
    assert_eq!(first, cached[0]);
    state.scan.store(0, Ordering::SeqCst);
    state.phase.store(2, Ordering::SeqCst);
    assert_eq!(
        scan_pages_after(&cached_then_history, first.next, 3).await,
        cached[1..],
        "a cached cursor must continue through all remaining history pages"
    );

    state.phase.store(0, Ordering::SeqCst);
    let history_then_cached = encrypted_table_with_overlays(&database).await;
    state.scan.store(0, Ordering::SeqCst);
    state.phase.store(2, Ordering::SeqCst);
    let first = history_then_cached.scan_page(None, 3).await.unwrap();
    assert_eq!(first, cached[0]);
    state.phase.store(3, Ordering::SeqCst);
    assert_eq!(
        scan_pages_after(&history_then_cached, first.next, 3).await,
        cached[1..],
        "a history cursor must continue through all remaining cached pages"
    );
}
