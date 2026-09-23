//! Tests for the transaction module.

use super::*;

use crate::{
    Instance,
    auth::crypto::generate_keypair,
    backend::database::InMemory,
    backend::{CacheScope, ProjectionDescriptor, StoreStateLifecycle, StoreStateRequest},
    crdt::{CRDT, Codec},
    store::{DocStore, Registered},
};

#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct MaxCounter(u64);

impl Codec for MaxCounter {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(self.0.to_le_bytes().to_vec())
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let bytes =
            bytes
                .try_into()
                .map_err(|_| crate::crdt::CRDTError::DeserializationFailed {
                    reason: "expected exactly eight counter bytes".into(),
                })?;
        Ok(Self(u64::from_le_bytes(bytes)))
    }
}

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
        tx.update_subtree("counter", MaxCounter(value).encode().unwrap())
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

// The facade is deliberately non-Serde; only its chosen binary codec knows
// how the nested algebra is encoded.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct StagedRows(crate::crdt::LwwMap<String, String>);
impl Codec for StagedRows {
    fn encode(&self) -> Result<Vec<u8>> {
        serde_ipld_dagcbor::to_vec(&self.0).map_err(|error| {
            crate::crdt::CRDTError::SerializationFailed {
                reason: error.to_string(),
            }
            .into()
        })
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        serde_ipld_dagcbor::from_slice(bytes)
            .map(Self)
            .map_err(|error| {
                crate::crdt::CRDTError::DeserializationFailed {
                    reason: error.to_string(),
                }
                .into()
            })
    }
}
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

struct RowsStore {
    name: String,
    txn: Transaction,
}
impl Registered for RowsStore {
    fn type_id() -> &'static str {
        "test:binary-rows"
    }
}
#[async_trait::async_trait]
impl Store for RowsStore {
    type Data = StagedRows;
    fn state_model() -> crate::store::StoreStateModel<Self::Data> {
        crate::store::StoreStateModel::Records(std::sync::Arc::new(RowsProjection))
    }
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

fn row(key: &str, value: &str) -> StagedRows {
    let mut rows = crate::crdt::LwwMap::new();
    rows.set(key.to_string(), value.to_string());
    StagedRows(rows)
}

#[tokio::test]
async fn projected_binary_codec_rebuilds_and_rejects_malformed_history() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let delta = row("key", "exact bytes");
    let bytes = delta.encode().unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(StagedRows::decode(&trailing).is_err());

    let tx = db.new_transaction().await.unwrap();
    let store = tx.get_store::<RowsStore>("rows").await.unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, delta.clone())
        .await
        .unwrap();
    assert_eq!(store.local_data().unwrap(), Some(delta));
    let entry = tx.commit().await.unwrap();
    assert_eq!(
        db.ops().get(&entry).await.unwrap().data("rows").unwrap(),
        &bytes
    );
    for _ in 0..2 {
        let tx = db.new_transaction().await.unwrap();
        assert_eq!(
            tx.projected_get("rows", &RowsProjection, b"key")
                .await
                .unwrap(),
            Some(b"exact bytes".to_vec())
        );
    }
    db.backend()
        .unwrap()
        .clear_derived_store_state()
        .await
        .unwrap();
    let tx = db.new_transaction().await.unwrap();
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"key")
            .await
            .unwrap(),
        Some(b"exact bytes".to_vec())
    );
    tx.update_subtree("rows", trailing).await.unwrap();
    tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    assert!(matches!(
        tx.projected_get("rows", &RowsProjection, b"key").await,
        Err(crate::Error::CRDT(_))
    ));
}

// A reversible test envelope detects calls made on the wrong side of the
// encryption boundary; production PasswordStore crypto has separate coverage.
struct BinaryEnvelope;
impl Encryptor for BinaryEnvelope {
    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        Ok([
            b"encrypted:".as_slice(),
            &plaintext.iter().map(|byte| byte ^ 0xff).collect::<Vec<_>>(),
        ]
        .concat())
    }
    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let bytes = ciphertext.strip_prefix(b"encrypted:").ok_or_else(|| {
            crate::crdt::CRDTError::DeserializationFailed {
                reason: "expected encrypted test envelope".into(),
            }
        })?;
        Ok(bytes.iter().map(|byte| byte ^ 0xff).collect())
    }
}

#[tokio::test]
async fn projected_binary_codec_decrypts_before_replay_and_encrypts_records() {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.register_encryptor("rows", Box::new(BinaryEnvelope))
        .unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("key", "secret"))
        .await
        .unwrap();
    let id = tx.commit().await.unwrap();
    let entry = db.ops().get(&id).await.unwrap();
    let ciphertext = entry.data("rows").unwrap();
    assert!(ciphertext.starts_with(b"encrypted:"));
    assert!(StagedRows::decode(ciphertext).is_err());
    let tx = db.new_transaction().await.unwrap();
    tx.register_encryptor("rows", Box::new(BinaryEnvelope))
        .unwrap();
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"key")
            .await
            .unwrap(),
        Some(b"secret".to_vec())
    );
    let view = tx.record_view("rows", &RowsProjection).await.unwrap();
    let record = db
        .ops()
        .store_state_record_get(&view, b"key")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(record, b"secret");
    assert_eq!(BinaryEnvelope.decrypt(&record).unwrap(), b"secret");
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn binary_custom_store_authorized_daemon_fallback_rejects_bad_codec_and_identity() {
    use crate::auth::types::SigKey;
    use crate::service::{ServiceServer, client::RemoteConnection};
    use std::os::unix::fs::PermissionsExt;

    let (instance, mut admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let key = admin.get_default_key().unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    for value in [4, 9, 2] {
        let tx = db.new_transaction().await.unwrap();
        tx.get_store::<CounterStore>("counter").await.unwrap();
        tx.update_subtree("counter", MaxCounter(value).encode().unwrap())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(
        db.get_store_state::<CounterStore>("counter").await.unwrap(),
        MaxCounter(9)
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("codec.sock");
    let (shutdown, receiver) = tokio::sync::watch::channel(());
    let server = ServiceServer::bind(instance.clone(), &socket)
        .await
        .unwrap();
    let task = tokio::spawn(server.run(receiver));
    let conn = RemoteConnection::connect(&socket).await.unwrap();
    conn.trusted_login("admin", None).await.unwrap();
    let identity = Database::find_sigkeys(&instance, db.root_id(), &key)
        .await
        .unwrap()
        .remove(0)
        .0;
    assert_eq!(
        conn.get_store_state::<CounterStore>(
            db.root_id().clone(),
            identity.clone(),
            "counter".into()
        )
        .await
        .unwrap(),
        MaxCounter(9)
    );
    let (_, foreign) = generate_keypair();
    let denied = conn
        .get_store_state::<CounterStore>(
            db.root_id().clone(),
            SigKey::from_pubkey(&foreign),
            "counter".into(),
        )
        .await
        .unwrap_err();
    assert!(
        denied.to_string().contains("SigningKeyMismatch"),
        "{denied}"
    );
    let mismatch = conn
        .get_store_state::<DocStore>(db.root_id().clone(), identity.clone(), "counter".into())
        .await
        .unwrap_err();
    assert!(mismatch.to_string().contains("TypeMismatch"), "{mismatch}");

    let tx = db.new_transaction().await.unwrap();
    let mut trailing = MaxCounter(12).encode().unwrap();
    trailing.push(0);
    tx.update_subtree("counter", trailing).await.unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        db.get_store_state::<CounterStore>("counter").await,
        Err(crate::Error::CRDT(_))
    ));
    assert!(matches!(
        conn.get_store_state::<CounterStore>(db.root_id().clone(), identity, "counter".into())
            .await,
        Err(crate::Error::CRDT(_))
    ));
    shutdown.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn binary_service_state_decodes_bytes_and_never_falls_back_on_bad_codec() {
    use crate::service::client::RemoteConnection;
    use crate::service::protocol::{
        DatabaseOp, Handshake, HandshakeAck, PROTOCOL_VERSION, ServerFrame, ServiceRequest,
        ServiceResponse, read_frame, write_frame,
    };
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("codec-peer.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let handshake: Handshake = read_frame(&mut stream).await.unwrap().unwrap();
        assert_eq!(handshake.protocol_version, PROTOCOL_VERSION);
        write_frame(
            &mut stream,
            &HandshakeAck {
                protocol_version: PROTOCOL_VERSION,
                wire_revision: crate::service::protocol::WIRE_REVISION,
            },
        )
        .await
        .unwrap();
        let good = MaxCounter(9).encode().unwrap();
        let mut bad = good.clone();
        bad.push(0);
        for bytes in [good, bad] {
            let request: ServiceRequest = read_frame(&mut stream).await.unwrap().unwrap();
            assert!(
                matches!(request, ServiceRequest::AuthenticatedDb(ref request) if matches!(request.op, DatabaseOp::EnsureStoreStateGeneration { .. }))
            );
            write_frame(
                &mut stream,
                &ServerFrame::Response(Box::new(ServiceResponse::StoreState(bytes))),
            )
            .await
            .unwrap();
        }
        // A decoding error is not a capability refusal: there must be no
        // GetVerifiedTips/GetStoreEntries fallback request on this socket.
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                read_frame::<_, ServiceRequest>(&mut stream)
            )
            .await
            .is_err()
        );
    });
    let conn = RemoteConnection::connect(&socket).await.unwrap();
    assert_eq!(
        conn.get_store_state::<CounterStore>(ID::default(), SigKey::default(), "counter".into())
            .await
            .unwrap(),
        MaxCounter(9)
    );
    assert!(matches!(
        conn.get_store_state::<CounterStore>(ID::default(), SigKey::default(), "counter".into())
            .await,
        Err(crate::Error::CRDT(_))
    ));
    tokio::time::timeout(Duration::from_secs(5), peer)
        .await
        .unwrap()
        .unwrap();
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
    async fn query_store(
        &self,
        tree: &ID,
        request: &crate::store::query::StoreQueryRequest,
    ) -> Result<crate::store::query::StoreQueryReply> {
        self.0.query_store(tree, request).await
    }
    async fn raw_store_page(
        &self,
        _tree: &ID,
        _request: &crate::store::source::RawStoreRequest,
    ) -> Result<crate::store::source::RawStorePage> {
        panic!("this spy must never fetch raw after registered success or a hard error")
    }
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
    let (instance, _daemon) = projection_backend().await;
    let mut admin = instance.login_user("admin", None).await.unwrap();
    let key = match admin.get_default_key() {
        Ok(key) => key,
        Err(_) => admin.add_private_key(Some("projection")).await.unwrap(),
    };
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
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

// Unlike unit fixtures exercised by every runner, this factory selects the real
// backend named by the matrix. Service uses an authenticated socket, not its engine.
async fn projection_backend() -> (
    Instance,
    Option<(tokio::sync::watch::Sender<()>, tempfile::TempDir)>,
) {
    let backend: Box<dyn crate::backend::BackendImpl> =
        match std::env::var("TEST_BACKEND").as_deref() {
            #[cfg(feature = "sqlite")]
            Ok("sqlite") => Box::new(crate::backend::database::Sqlite::in_memory().await.unwrap()),
            #[cfg(feature = "postgres")]
            Ok("postgres") => Box::new(
                crate::backend::database::Postgres::connect_isolated(
                    &std::env::var("TEST_POSTGRES_URL")
                        .unwrap_or_else(|_| "postgres://localhost/eidetica_test".into()),
                )
                .await
                .unwrap(),
            ),
            #[cfg(all(unix, feature = "service"))]
            Ok("service") => {
                let dir = tempfile::tempdir().unwrap();
                let socket = dir.path().join("projection.sock");
                let (server, _) = Instance::create_backend(
                    Box::new(InMemory::new()),
                    crate::NewUser::passwordless("admin"),
                )
                .await
                .unwrap();
                let mut admin = server.login_user("admin", None).await.unwrap();
                admin.add_private_key(Some("projection")).await.unwrap();
                let daemon = crate::service::ServiceServer::bind(server, socket.clone())
                    .await
                    .unwrap();
                let (stop, rx) = tokio::sync::watch::channel(());
                tokio::spawn(daemon.run(rx));
                let client = Instance::connect(format!("unix://{}", socket.display()))
                    .await
                    .unwrap();
                client.login_user("admin", None).await.unwrap();
                return (client, Some((stop, dir)));
            }
            Ok("inmemory") | Err(_) => Box::new(InMemory::new()),
            Ok(other) => panic!("unsupported projection backend: {other}"),
        };
    let (instance, _) = Instance::create_backend(backend, crate::NewUser::passwordless("admin"))
        .await
        .unwrap();
    (instance, None)
}

#[tokio::test]
async fn projected_backend_matrix_physical_pages_and_cold_delete() {
    let (instance, _daemon) = projection_backend().await;
    let mut admin = instance.login_user("admin", None).await.unwrap();
    let key = match admin.get_default_key() {
        Ok(key) => key,
        Err(_) => admin.add_private_key(Some("projection")).await.unwrap(),
    };
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
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
    let mut deleted = crate::crdt::LwwMap::new();
    deleted.delete("000".to_string());
    deleted.delete("139".to_string());
    tx.stage_projected_delta("rows", &RowsProjection, StagedRows(deleted))
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
        tx.projected_get("rows", &RowsProjection, b"128")
            .await
            .unwrap(),
        Some(b"old".to_vec())
    );
    let view = tx.record_view("rows", &RowsProjection).await.unwrap();
    assert_eq!(
        db.ops()
            .store_state_record_get(&view, b"139")
            .await
            .unwrap(),
        None
    );
    tx.stage_projected_delta("rows", &RowsProjection, row("001", "updated"))
        .await
        .unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("zzz", "new"))
        .await
        .unwrap();
    let mut after = None;
    let mut keys = Vec::new();
    loop {
        let (page, next) = tx
            .projected_record_scan_page("rows", &RowsProjection, after.as_ref(), 7)
            .await
            .unwrap();
        keys.extend(page.records);
        after = next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(keys.len(), 139);
    assert_eq!(keys.first(), Some(&(b"001".to_vec(), b"updated".to_vec())));
    assert_eq!(keys.last(), Some(&(b"zzz".to_vec(), b"new".to_vec())));
    assert!(!keys.iter().any(|(k, _)| k == b"139"));
    assert_eq!(
        db.ops()
            .store_state_record_get(&view, b"001")
            .await
            .unwrap(),
        Some(b"old".to_vec())
    );
}

// Deliberately simple test-only transform: authenticates the logical key in the
// record envelope and makes its physical order different from logical order.
struct KeyedRows;
impl Encryptor for KeyedRows {
    fn encrypt(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        Ok(bytes.iter().map(|byte| byte ^ 0x5a).collect())
    }
    fn decrypt(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        self.encrypt(bytes)
    }
    fn physical_record_key(&self, key: &[u8]) -> Result<Vec<u8>> {
        Ok(key.iter().map(|byte| !byte).collect())
    }
    fn encrypt_record(&self, key: &[u8], value: &[u8]) -> Result<Vec<u8>> {
        let mut envelope = vec![key.len() as u8];
        envelope.extend_from_slice(key);
        envelope.extend_from_slice(value);
        self.encrypt(&envelope)
    }
    fn decrypt_record(&self, _: &[u8], bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let envelope = self.decrypt(bytes)?;
        let (len, rest) = envelope.split_first().unwrap();
        let (key, value) = rest.split_at(*len as usize);
        Ok((key.to_vec(), value.to_vec()))
    }
    fn projection_descriptor(&self, descriptor: ProjectionDescriptor) -> ProjectionDescriptor {
        ProjectionDescriptor {
            name: format!("keyed/{}", descriptor.name),
            version: descriptor.version,
        }
    }
}

#[tokio::test]
async fn projected_encrypted_physical_identity_and_recordless_fallback() {
    let (instance, _daemon) = projection_backend().await;
    let mut admin = instance.login_user("admin", None).await.unwrap();
    let key = match admin.get_default_key() {
        Ok(key) => key,
        Err(_) => admin.add_private_key(Some("projection")).await.unwrap(),
    };
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.register_encryptor("rows", Box::new(KeyedRows)).unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("a", "one"))
        .await
        .unwrap();
    tx.stage_projected_delta("rows", &RowsProjection, row("b", "two"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let tx = db.new_transaction().await.unwrap();
    tx.register_encryptor("rows", Box::new(KeyedRows)).unwrap();
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"a")
            .await
            .unwrap(),
        Some(b"one".to_vec())
    );
    let view = tx.record_view("rows", &RowsProjection).await.unwrap();
    let physical_a = KeyedRows.physical_record_key(b"a").unwrap();
    let physical_b = KeyedRows.physical_record_key(b"b").unwrap();
    let ciphertext = db
        .ops()
        .store_state_record_get(&view, &physical_a)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(ciphertext, b"one");
    assert!(
        matches!(tx.decode_projected_record("rows", &physical_b, &ciphertext), Err(crate::Error::Store(error)) if matches!(*error, StoreError::DataCorruption { .. }))
    );
    let (page, end) = tx
        .projected_record_scan_page("rows", &RowsProjection, None, 1)
        .await
        .unwrap();
    assert_eq!(page.records, vec![(b"b".to_vec(), b"two".to_vec())]);
    let (page, end) = tx
        .projected_record_scan_page("rows", &RowsProjection, end.as_ref(), 1)
        .await
        .unwrap();
    assert_eq!(page.records, vec![(b"a".to_vec(), b"one".to_vec())]);
    assert!(end.is_none());

    let recordless = db
        .clone()
        .with_test_ops(std::sync::Arc::new(RecordlessOps(db.backend().unwrap())));
    let tx = recordless.new_transaction().await.unwrap();
    tx.register_encryptor("rows", Box::new(KeyedRows)).unwrap();
    assert_eq!(
        tx.projected_get("rows", &RowsProjection, b"a")
            .await
            .unwrap(),
        Some(b"one".to_vec())
    );
    let (page, next) = tx
        .projected_record_scan_page("rows", &RowsProjection, None, 1)
        .await
        .unwrap();
    assert_eq!(page.records, vec![(b"b".to_vec(), b"two".to_vec())]);
    assert!(next.is_some());
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
    let (instance, _daemon) = projection_backend().await;
    let mut admin = instance.login_user("admin", None).await.unwrap();
    let key = match admin.get_default_key() {
        Ok(key) => key,
        Err(_) => admin.add_private_key(Some("projection")).await.unwrap(),
    };
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
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

#[derive(Clone, Default)]
struct RacingRows {
    rows: StagedRows,
    barrier: Option<std::sync::Arc<std::sync::Barrier>>,
}
impl Codec for RacingRows {
    fn encode(&self) -> Result<Vec<u8>> {
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        self.rows.encode()
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            rows: StagedRows::decode(bytes)?,
            barrier: None,
        })
    }
}
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

#[derive(Clone, Default)]
struct FallibleRows {
    rows: StagedRows,
    fail: bool,
}
impl Codec for FallibleRows {
    fn encode(&self) -> Result<Vec<u8>> {
        if self.fail {
            return Err(crate::crdt::CRDTError::SerializationFailed {
                reason: "injected encoding failure".into(),
            }
            .into());
        }
        self.rows.encode()
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            rows: StagedRows::decode(bytes)?,
            fail: false,
        })
    }
}
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
async fn test_docstore_set_preserves_malformed_staged_data() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();

    // Malformed staged bytes require the transaction's internal injection seam.
    let malformed = b"not JSON".to_vec();
    tx.update_subtree("data", malformed.clone()).await.unwrap();
    let set_error = store.set("name", "Alice").await.unwrap_err();
    assert!(matches!(
        set_error,
        crate::Error::Transaction(err)
            if matches!(err.as_ref(), TransactionError::StoreDeserializationFailed { store, .. } if store == "data")
    ));
    assert_eq!(
        tx.entry_builder
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .data("data")
            .unwrap(),
        &malformed
    );
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
    let store = tx.get_store::<CounterStore>("counter").await.unwrap();
    assert_eq!(store.local_data().unwrap(), None);
    tx.update_subtree("counter", MaxCounter(7).encode().unwrap())
        .await
        .unwrap();
    assert_eq!(store.local_data().unwrap(), Some(MaxCounter(7)));
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

#[tokio::test]
async fn fixed_parent_subtree_read_ignores_concurrent_live_write() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let reader = database.new_transaction().await.unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    *reader.snapshot_pause.lock().unwrap() = Some((entered_tx, resume_rx));
    let read_task =
        tokio::spawn(async move { reader.get_full_state::<MaxCounter>("counter").await });

    entered_rx.await.unwrap();
    let writer = database.new_transaction().await.unwrap();
    writer
        .update_subtree("counter", MaxCounter(7).encode().unwrap())
        .await
        .unwrap();
    writer.commit().await.unwrap();
    resume_tx.send(()).unwrap();

    assert_eq!(read_task.await.unwrap().unwrap(), MaxCounter(0));
}

#[tokio::test]
async fn fixed_parent_auth_settings_read_ignores_concurrent_grant() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let baseline = database
        .new_transaction()
        .await
        .unwrap()
        .get_full_state::<Doc>(SETTINGS)
        .await
        .unwrap();
    let reader = database.new_transaction().await.unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    *reader.snapshot_pause.lock().unwrap() = Some((entered_tx, resume_rx));
    let read_task = tokio::spawn(async move { reader.get_full_state::<Doc>(SETTINGS).await });

    entered_rx.await.unwrap();
    let (_, new_key) = generate_keypair();
    let writer = database.new_transaction().await.unwrap();
    writer
        .get_settings()
        .unwrap()
        .set_auth_key(
            &new_key,
            crate::auth::types::AuthKey::active(
                Some("new-key"),
                crate::auth::types::Permission::Write(1),
            ),
        )
        .await
        .unwrap();
    writer.commit().await.unwrap();
    resume_tx.send(()).unwrap();

    assert_eq!(read_task.await.unwrap().unwrap(), baseline);
}

#[tokio::test]
async fn raw_sdk_registered_success_and_hard_errors_never_fetch_raw() {
    let (_instance, mut owner) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("sdk-owner"),
    )
    .await
    .unwrap();
    let db = owner
        .create_database(Doc::new(), &owner.get_default_key().unwrap())
        .await
        .unwrap();
    let write = db.new_transaction().await.unwrap();
    write
        .get_store::<crate::store::DocStore>("docs")
        .await
        .unwrap()
        .set("key", "value")
        .await
        .unwrap();
    write.commit().await.unwrap();
    let db = db
        .clone()
        .with_test_ops(std::sync::Arc::new(RecordlessOps(db.backend().unwrap())));
    let tx = db.new_transaction().await.unwrap();
    let query = br#"{"Get":{"key":"key"}}"#.to_vec();
    let answer = tx
        .query_store_or_fold::<Doc, _>(
            "docs",
            crate::store::DocStore::type_id(),
            query.clone(),
            |bytes| {
                Ok(serde_json::from_slice::<Option<crate::crdt::doc::Value>>(
                    bytes,
                )?)
            },
            |_| panic!("success cannot fall back"),
        )
        .await
        .unwrap();
    assert_eq!(answer.unwrap().as_text(), Some("value"));
    // Result codec failure, malformed query, wrong type, missing source Store,
    // query encoding limit, and unsupported backend capability are hard errors.
    assert!(
        tx.query_store_or_fold::<Doc, Doc>(
            "docs",
            crate::store::DocStore::type_id(),
            query,
            Doc::decode,
            |_| panic!("decode error cannot fall back")
        )
        .await
        .is_err()
    );
    for (store, type_id, query) in [
        (
            "docs",
            crate::store::DocStore::type_id(),
            b"malformed".to_vec(),
        ),
        ("docs", "wrong:v0", Vec::new()),
        ("missing", crate::store::DocStore::type_id(), Vec::new()),
        (
            "docs",
            crate::store::DocStore::type_id(),
            vec![255; 1_100_000],
        ),
    ] {
        assert!(
            tx.query_store_or_fold::<Doc, Doc>(
                store,
                type_id,
                query,
                |_| panic!("hard error cannot decode"),
                |_| panic!("hard error cannot fall back")
            )
            .await
            .is_err()
        );
    }
}
