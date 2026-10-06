//! Focused expand-slice tests; legacy service operations remain in place.

use super::*;
use eidetica::crdt::{CRDT, Codec};
use eidetica::instance::backend::{Backend, LocalBackend};
use eidetica::service::protocol::WIRE_REVISION;
use eidetica::store::query::{
    QueryOutcome, QuerySource, StoreQueryContext, StoreQueryHandler, StoreQueryRequest,
};
use eidetica::store::{ExecuteQuery, GetValue};
use eidetica::{Registered, Result, Store, Transaction};

// Adapted from the typed-codec fixture: no Serde and a nonzero identity.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SocketCounter(u64);
impl Default for SocketCounter {
    fn default() -> Self {
        Self(7)
    }
}
impl Codec for SocketCounter {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(self.0.to_le_bytes().to_vec())
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        let bytes =
            bytes
                .try_into()
                .map_err(|_| eidetica::crdt::CRDTError::DeserializationFailed {
                    reason: "expected exactly eight counter bytes".into(),
                })?;
        Ok(Self(u64::from_le_bytes(bytes)))
    }
}
impl CRDT for SocketCounter {
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
        "test:socket-counter-binary:v1"
    }
}
#[async_trait::async_trait]
impl Store for CounterStore {
    type Data = SocketCounter;
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
impl StoreQueryHandler for CounterStore {
    async fn handle_query(context: &StoreQueryContext, query: &[u8]) -> Result<QueryOutcome> {
        // Public threshold query delegates a different, exact binary message.
        if query == b"NO-CAP" {
            return Ok(QueryOutcome::Unavailable);
        }
        if query != b"MAX\0" {
            return Err(eidetica::store::StoreError::InvalidOperation {
                store: context.store().into(),
                operation: "counter query".into(),
                reason: "invalid opcode".into(),
            }
            .into());
        }
        if context.store() == "bad-response" {
            // Only the consuming Store can validate its opaque result codec.
            return Ok(QueryOutcome::Result(vec![0]));
        }
        Ok(QueryOutcome::Result(context.fold::<Self>()?.encode()?))
    }
}
// Rc proves the public query is not Send; it is also borrowed/non-Clone/non-Serde.
struct AtLeast<'q>(&'q std::rc::Rc<u64>);
impl<'q> ExecuteQuery<AtLeast<'q>> for CounterStore {
    type Output = bool;
    async fn execute<'a>(&'a self, query: AtLeast<'q>) -> Result<bool>
    where
        AtLeast<'q>: 'a,
    {
        let reply = self
            .txn
            .query_store(&self.name, Self::type_id(), b"MAX\0".to_vec())
            .await?;
        let QueryOutcome::Result(bytes) = reply.outcome else {
            panic!("installed handler refused");
        };
        let committed = SocketCounter::decode(&bytes)?;
        let combined = committed.merge(&self.local_data()?.unwrap_or_default())?;
        Ok(combined.0 >= **query.0)
    }
}

async fn insert_counter(db: &eidetica::Database, owner: &eidetica::user::User, bytes: Vec<u8>) {
    let store = "counter";
    let ctx = db
        .transaction_context(&[store.into()], ReadScope::Verified)
        .await
        .unwrap();
    let key = owner.get_default_key().unwrap();
    let signing = owner.get_signing_key(&key).unwrap();
    let entry = Entry::builder(db.root_id().clone())
        .set_parents(ctx.main_parents.iter().map(|(id, _)| id.clone()).collect())
        .set_subtree_data(store, bytes)
        .set_subtree_parents(
            store,
            ctx.subtree_parents[store]
                .iter()
                .map(|(id, _)| id.clone())
                .collect(),
        )
        .set_subtree_height(
            store,
            Some(
                ctx.subtree_parents[store]
                    .iter()
                    .map(|(_, h)| *h)
                    .max()
                    .unwrap_or(0)
                    + 1,
            ),
        )
        .set_metadata(
            serde_json::to_vec(
                &serde_json::json!({"settings_tips": ctx.settings_tips, "entropy": null}),
            )
            .unwrap(),
        )
        .set_height(ctx.main_parents.iter().map(|(_, h)| *h).max().unwrap_or(0) + 1)
        .build()
        .unwrap()
        .with_auth(|auth| auth.key = SigKey::from_pubkey(&key));
    let signature = sign_entry(&entry, &signing).unwrap();
    let id = db
        .insert_raw(entry.with_auth(|auth| auth.signature = Some(signature)))
        .await
        .unwrap();
    db.verify().await.unwrap();
    assert!(db.snapshot().await.unwrap().tips().contains(&id));
}

fn request(
    source: QuerySource,
    store: &str,
    expected_type: &str,
    query: &[u8],
) -> StoreQueryRequest {
    StoreQueryRequest {
        source,
        store: store.into(),
        expected_type: expected_type.into(),
        query: query.into(),
    }
}

#[tokio::test]
async fn store_query_local_and_socket_use_unlike_store_vocabularies_and_pinned_sources() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("query.sock");
    let (server, mut owner) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner"))
            .await
            .unwrap();
    let key = owner.get_default_key().unwrap();
    let db = owner.create_database(Doc::new(), &key).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("key", "old")
        .await
        .unwrap();
    tx.get_store::<CounterStore>("counter").await.unwrap();
    tx.get_store::<CounterStore>("bad-response").await.unwrap();
    tx.commit().await.unwrap();
    insert_counter(&db, &owner, SocketCounter(21).encode().unwrap()).await;
    let mut daemon = ServiceServer::bind(server.clone(), &socket).await.unwrap();
    daemon.register_store_query::<CounterStore>().unwrap();
    assert!(daemon.register_store_query::<CounterStore>().is_err());
    let (shutdown, rx) = watch::channel(());
    let task = tokio::spawn(daemon.run(rx));
    let client = Instance::connect(format!("unix://{}", socket.display()))
        .await
        .unwrap();
    let user = client.login_user("owner", None).await.unwrap();
    let remote = user.open_database(db.root_id()).await.unwrap();
    let transaction = remote.new_transaction().await.unwrap();
    let source = transaction.query_source().unwrap();
    let fresh = transaction.get_store::<DocStore>("fresh").await.unwrap();
    assert_eq!(fresh.query(GetValue("missing")).await.unwrap(), None);
    fresh.set("key", "staged").await.unwrap();
    assert_eq!(
        fresh.query(GetValue("key")).await.unwrap(),
        Some(fresh.get("key").await.unwrap())
    );
    assert_eq!(fresh.query(GetValue("missing")).await.unwrap(), None);
    fresh.delete("key").await.unwrap();
    assert_eq!(fresh.query(GetValue("key")).await.unwrap(), None);
    let docs = transaction.get_store::<DocStore>("docs").await.unwrap();
    let counter = transaction
        .get_store::<CounterStore>("counter")
        .await
        .unwrap();
    let refused = transaction
        .query_store("counter", CounterStore::type_id(), b"NO-CAP".to_vec())
        .await
        .unwrap();
    assert_eq!(refused.source, source);
    assert_eq!(refused.outcome, QueryOutcome::Unavailable);
    let invalid_result = transaction
        .get_store::<CounterStore>("bad-response")
        .await
        .unwrap()
        .query(AtLeast(&std::rc::Rc::new(20)))
        .await
        .unwrap_err();
    assert!(
        invalid_result
            .to_string()
            .contains("expected exactly eight counter bytes")
    );
    assert!(counter.query(AtLeast(&std::rc::Rc::new(20))).await.unwrap());
    assert!(!counter.query(AtLeast(&std::rc::Rc::new(22))).await.unwrap());
    docs.set("unrelated", "staged").await.unwrap();
    assert_eq!(
        docs.query(GetValue("unrelated"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("staged")
    );
    assert_eq!(
        docs.query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("old")
    );
    // Advance both Stores after the client selected its source.
    let write = db.new_transaction().await.unwrap();
    write
        .get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("key", "new")
        .await
        .unwrap();
    write.commit().await.unwrap();
    insert_counter(&db, &owner, SocketCounter(99).encode().unwrap()).await;
    assert_eq!(
        docs.query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("old")
    );
    assert!(!counter.query(AtLeast(&std::rc::Rc::new(22))).await.unwrap());
    // The same dispatch executes locally on installed code without service API types.
    let mut local = LocalBackend::new(server.backend().local_engine().unwrap());
    local.register_store_query::<CounterStore>().unwrap();
    let request = request(source.clone(), "counter", CounterStore::type_id(), b"MAX\0");
    let reply = local.query_store(db.root_id(), &request).await.unwrap();
    assert_eq!(reply.source, source);
    assert_eq!(
        reply.outcome,
        QueryOutcome::Result(21u64.to_le_bytes().to_vec())
    );
    let malformed = StoreQueryRequest {
        query: b"MAX\0trailing".to_vec(),
        ..request
    };
    assert!(local.query_store(db.root_id(), &malformed).await.is_err());
    assert!(
        transaction
            .query_store("counter", CounterStore::type_id(), malformed.query)
            .await
            .is_err()
    );
    docs.delete("key").await.unwrap();
    assert_eq!(docs.query(GetValue("key")).await.unwrap(), None);
    assert_eq!(
        remote
            .get_store_viewer::<DocStore>("docs")
            .await
            .unwrap()
            .query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("new")
    );
    // A bad authoritative payload is not capability refusal or empty state.
    insert_counter(&db, &owner, b"malformed counter payload".to_vec()).await;
    let latest = remote.new_transaction().await.unwrap();
    assert!(
        latest
            .get_store::<CounterStore>("counter")
            .await
            .unwrap()
            .query(AtLeast(&std::rc::Rc::new(22)))
            .await
            .is_err()
    );
    // The old boundary remains readable after the bad later operation.
    assert!(!counter.query(AtLeast(&std::rc::Rc::new(22))).await.unwrap());
    drop(client);
    drop(shutdown);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn store_query_refusal_type_encryption_source_and_auth_are_distinct() {
    let (socket, shutdown, server, _dir) = start_test_server().await;
    let (client, root, identity) = setup_db(&server, &socket, "query-user").await;
    let mut owner = server.login_user("query-user", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let txn = db.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("key", "old")
        .await
        .unwrap();
    txn.get_store::<CounterStore>("counter").await.unwrap();
    let mut protected = txn
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    protected
        .initialize("test-password", Doc::new())
        .await
        .unwrap();
    protected
        .inner()
        .await
        .unwrap()
        .set("private", "encrypted-source")
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let old = QuerySource {
        main: db.snapshot().await.unwrap(),
        scope: ReadScope::Verified,
    };
    let remote_db = eidetica::Database::open(&client, &root).await.unwrap();
    let remote_txn = remote_db.new_transaction_at(&old.main).await.unwrap();
    let mut unlocked = remote_txn
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    unlocked.open("test-password").unwrap();
    // Unlocking validates the hidden inner format locally, but does not make
    // its plaintext identity a daemon-visible outer registration.
    assert!(
        unlocked
            .inner()
            .await
            .unwrap()
            .query(GetValue("private"))
            .await
            .is_err()
    );
    let conn = client.remote_connection().unwrap();
    let unknown = request(old.clone(), "counter", CounterStore::type_id(), b"MAX\0");
    let reply = conn
        .query_store(root.clone(), identity.clone(), unknown.clone())
        .await
        .unwrap();
    assert_eq!(reply.source, old);
    assert_eq!(reply.outcome, QueryOutcome::Unavailable);
    let write = db.new_transaction().await.unwrap();
    write
        .get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("key", "new")
        .await
        .unwrap();
    write.commit().await.unwrap();
    // Refusal/fallback retains the exact source rather than selecting fresh tips.
    let after = conn
        .query_store(root.clone(), identity.clone(), unknown)
        .await
        .unwrap();
    assert_eq!(after, reply);
    let pinned = db.new_transaction_at(&reply.source.main).await.unwrap();
    assert_eq!(
        pinned
            .get_store::<DocStore>("docs")
            .await
            .unwrap()
            .query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("old")
    );
    let encrypted = conn
        .query_store(
            root.clone(),
            identity.clone(),
            request(
                old.clone(),
                "secret",
                PasswordStore::<DocStore>::type_id(),
                b"garbage",
            ),
        )
        .await
        .unwrap();
    assert_eq!(encrypted.source, old);
    assert_eq!(encrypted.outcome, QueryOutcome::Unavailable);
    for (store, claimed) in [
        ("counter", DocStore::type_id()),
        ("secret", DocStore::type_id()),
    ] {
        let error = conn
            .query_store(
                root.clone(),
                identity.clone(),
                request(old.clone(), store, claimed, b"garbage"),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("TypeMismatch"), "{error}");
    }
    // Bad common source must fail even for an unavailable handler.
    let foreign = owner
        .create_database(Doc::new(), &owner.get_default_key().unwrap())
        .await
        .unwrap();
    for main in [
        foreign.snapshot().await.unwrap(),
        eidetica::Snapshot::from(vec![eidetica::ID::from_bytes(b"missing")]),
        eidetica::Snapshot::EMPTY,
    ] {
        let bad = QuerySource {
            main,
            ..old.clone()
        };
        assert!(
            conn.query_store(
                root.clone(),
                identity.clone(),
                request(bad, "counter", CounterStore::type_id(), b"MAX\0")
            )
            .await
            .is_err()
        );
    }
    // Connection authentication is required even with a valid source/type.
    let unauth = eidetica::service::client::RemoteConnection::connect(&socket)
        .await
        .unwrap();
    assert!(
        unauth
            .query_store(
                root.clone(),
                identity.clone(),
                request(old.clone(), "counter", CounterStore::type_id(), b"MAX\0")
            )
            .await
            .is_err()
    );
    // Current permissions, not those at the historical source, govern every use.
    let transaction = db.new_transaction().await.unwrap();
    let auth = transaction.get_settings().unwrap();
    auth.revoke_auth_key(&owner.get_default_key().unwrap())
        .await
        .unwrap();
    // Keep another admin so revocation is a valid signed settings update.
    let (_, replacement) = generate_keypair();
    auth.set_auth_key(
        &replacement,
        eidetica::auth::types::AuthKey::active(Some("replacement"), Permission::Admin(0)),
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    assert!(
        conn.query_store(
            root.clone(),
            identity,
            request(old, "counter", CounterStore::type_id(), b"MAX\0")
        )
        .await
        .is_err()
    );
    drop(shutdown);
}

#[tokio::test]
async fn store_query_rejects_incomplete_and_disallowed_source_posture_before_decoding() {
    let (socket, shutdown, server, _dir) = start_test_server().await;
    let (client, root, identity) = setup_db(&server, &socket, "posture").await;
    let owner = server.login_user("posture", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<CounterStore>("counter").await.unwrap();
    tx.commit().await.unwrap();
    let source = QuerySource {
        main: db.snapshot().await.unwrap(),
        scope: ReadScope::Verified,
    };
    let mut local = LocalBackend::new(server.backend().local_engine().unwrap());
    local.register_store_query::<CounterStore>().unwrap();
    // Unverified main ancestry is rejected before the opcode is decoded.
    let entry = Entry::builder(root.clone())
        .set_parents(source.main.tips().to_vec())
        .set_height(100)
        .build()
        .unwrap();
    let id = entry.id();
    server
        .backend()
        .local_engine()
        .unwrap()
        .put(entry)
        .await
        .unwrap();
    let unverified = QuerySource {
        main: vec![id.clone()].into(),
        scope: ReadScope::Verified,
    };
    let error = local
        .query_store(
            &root,
            &request(
                unverified.clone(),
                "counter",
                CounterStore::type_id(),
                b"malformed",
            ),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not permit Verified"));
    let raw = QuerySource {
        scope: ReadScope::AllowUnverified,
        ..unverified
    };
    assert_eq!(
        local
            .query_store(
                &root,
                &request(raw.clone(), "counter", CounterStore::type_id(), b"MAX\0")
            )
            .await
            .unwrap()
            .outcome,
        QueryOutcome::Result(7u64.to_le_bytes().to_vec())
    );
    let engine = server.backend().local_engine().unwrap();
    engine
        .update_verification_status(&id, VerificationStatus::Failed)
        .await
        .unwrap();
    assert!(
        local
            .query_store(
                &root,
                &request(raw, "counter", CounterStore::type_id(), b"MAX\0")
            )
            .await
            .is_err()
    );
    // A missing ancestor is not silently shortened to available history.
    let broken = Entry::builder(root.clone())
        .set_parents(vec![eidetica::ID::from_bytes(b"lost-parent")])
        .set_height(101)
        .build()
        .unwrap();
    let broken_id = broken.id();
    engine.put(broken).await.unwrap();
    let incomplete = QuerySource {
        main: vec![broken_id].into(),
        scope: ReadScope::AllowUnverified,
    };
    let error = client
        .remote_connection()
        .unwrap()
        .query_store(
            root,
            identity,
            request(incomplete, "counter", CounterStore::type_id(), b"malformed"),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("not found") || error.to_string().contains("EntryNotFound"),
        "{error}"
    );
    drop(shutdown);
}

#[derive(Serialize, Deserialize)]
struct LegacyHandshake {
    protocol_version: u32,
}

#[tokio::test]
async fn service_wire_revision_rejects_missing_legacy_and_mismatched_peers_before_auth() {
    assert_eq!(PROTOCOL_VERSION, 0);
    let (socket, shutdown, _server, _dir) = start_test_server().await;
    for body in [
        serde_json::json!({"protocol_version":0}),
        serde_json::json!({"protocol_version":0,"wire_revision":WIRE_REVISION+1}),
        serde_json::json!({"protocol_version":1,"wire_revision":WIRE_REVISION}),
    ] {
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        write_frame(&mut stream, &body).await.unwrap();
        // A missing revision fails deserialization, so no acknowledgment exists.
        if body.get("wire_revision").is_some() {
            let ack: HandshakeAck = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(ack.protocol_version, 0);
            assert_eq!(ack.wire_revision, WIRE_REVISION);
        }
        let eof = tokio::time::timeout(
            Duration::from_secs(5),
            read_frame::<_, ServerFrame>(&mut stream),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            eof.is_none(),
            "incompatible client entered request dispatch"
        );
    }
    for body in [
        serde_json::json!({"protocol_version":0}),
        serde_json::json!({"protocol_version":0,"wire_revision":WIRE_REVISION+1}),
        serde_json::json!({"protocol_version":1,"wire_revision":WIRE_REVISION}),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("legacy.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Actual permissive old reader ignores the new request field.
            let old: LegacyHandshake = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(old.protocol_version, 0);
            write_frame(&mut stream, &body).await.unwrap();
            let next: Option<ServiceRequest> = read_frame(&mut stream).await.unwrap();
            assert!(
                next.is_none(),
                "new client sent an operation to a legacy/incompatible daemon"
            );
        });
        assert!(
            eidetica::service::client::RemoteConnection::connect(&socket)
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(5), peer)
            .await
            .unwrap()
            .unwrap();
    }
    // Matching envelopes do not alter format checking; query tests above still
    // reject wrong outer types. Negative fixture proves why defaulting is unsafe.
    #[derive(Deserialize)]
    struct PermissiveAck {
        protocol_version: u32,
        #[serde(default = "current_revision")]
        wire_revision: u32,
    }
    fn current_revision() -> u32 {
        WIRE_REVISION
    }
    let permissive: PermissiveAck = serde_json::from_str("{\"protocol_version\":0}").unwrap();
    assert_eq!(
        (permissive.protocol_version, permissive.wire_revision),
        (PROTOCOL_VERSION, WIRE_REVISION)
    );
    assert!(serde_json::from_str::<HandshakeAck>("{\"protocol_version\":0}").is_err());
    assert!(serde_json::from_str::<Handshake>("{\"protocol_version\":0}").is_err());
    drop(shutdown);
}

#[tokio::test]
async fn store_query_read_only_client_uses_common_rpc_but_cannot_claim_another_identity() {
    let (socket, shutdown, server, _dir) = start_test_server().await;
    let (_, root, _) = setup_db(&server, &socket, "query-owner").await;
    create_user_via_admin(&server, "query-reader").await;
    let owner = server.login_user("query-owner", None).await.unwrap();
    let reader = server.login_user("query-reader", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let write = db.new_transaction().await.unwrap();
    write
        .get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("key", "readable")
        .await
        .unwrap();
    write
        .get_settings()
        .unwrap()
        .set_auth_key(
            &reader.get_default_key().unwrap(),
            eidetica::auth::types::AuthKey::active(Some("reader"), Permission::Read),
        )
        .await
        .unwrap();
    write.commit().await.unwrap();
    let client = Instance::connect(format!("unix://{}", socket.display()))
        .await
        .unwrap();
    let _user = client.login_user("query-reader", None).await.unwrap();
    // User::open_database selects a signing key; this reader only needs an
    // authenticated read handle and must not acquire write authority.
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    let txn = remote.new_transaction().await.unwrap();
    let docs = txn.get_store::<DocStore>("docs").await.unwrap();
    assert_eq!(
        docs.query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("readable")
    );
    let req = request(
        txn.query_source().unwrap(),
        "docs",
        DocStore::type_id(),
        br#"{"Get":{"key":"key"}}"#,
    );
    let result = client
        .remote_connection()
        .unwrap()
        .query_store(
            root,
            SigKey::from_pubkey(&owner.get_default_key().unwrap()),
            req,
        )
        .await;
    assert!(
        result.is_err(),
        "a reader cannot assert the owner's session key"
    );
    drop(shutdown);
}

#[tokio::test]
async fn store_query_client_rejects_a_result_from_a_different_source_or_posture() {
    let original = QuerySource {
        main: vec![eidetica::ID::from_bytes(b"original-source")].into(),
        scope: ReadScope::Verified,
    };
    for source in [
        QuerySource {
            main: vec![eidetica::ID::from_bytes(b"other-source")].into(),
            ..original.clone()
        },
        QuerySource {
            scope: ReadScope::AllowUnverified,
            ..original.clone()
        },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("wrong-source.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let requested = original.clone();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _: Handshake = read_frame(&mut stream).await.unwrap().unwrap();
            write_frame(
                &mut stream,
                &HandshakeAck {
                    protocol_version: PROTOCOL_VERSION,
                    wire_revision: WIRE_REVISION,
                },
            )
            .await
            .unwrap();
            let request: ServiceRequest = read_frame(&mut stream).await.unwrap().unwrap();
            let ServiceRequest::AuthenticatedDb(envelope) = request else {
                panic!("missing auth envelope");
            };
            let eidetica::service::protocol::DatabaseOp::QueryStore { request } = envelope.op
            else {
                panic!("not the common query RPC");
            };
            assert_eq!(request.source, requested);
            write_frame(
                &mut stream,
                &ServerFrame::Response(Box::new(ServiceResponse::StoreQuery(
                    eidetica::store::query::StoreQueryReply {
                        source,
                        outcome: QueryOutcome::Result(b"irrelevant bytes".to_vec()),
                    },
                ))),
            )
            .await
            .unwrap();
        });
        let connection = eidetica::service::client::RemoteConnection::connect(&socket)
            .await
            .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            connection.query_store(
                eidetica::ID::from_bytes(b"tree"),
                SigKey::default(),
                request(
                    original.clone(),
                    "counter",
                    CounterStore::type_id(),
                    b"MAX\0",
                ),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains("source-bound StoreQuery"));
        tokio::time::timeout(Duration::from_secs(5), peer)
            .await
            .unwrap()
            .unwrap();
    }
}
