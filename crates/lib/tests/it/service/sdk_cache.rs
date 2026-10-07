//! Ordinary Store queries drive private assistance, observed through real sockets.
use super::*;
use eidetica::backend::{BackendError, RecordMutation};
use eidetica::service::client::PrivateCacheAssistance;
use eidetica::service::protocol::DatabaseOp as Op;
use eidetica::store::{
    ExecuteQuery, GetValue, OPAQUE_STATE_KEY, assistance::PrivateRepresentation,
};
use std::sync::{Arc, Mutex};

struct ReadCounter<'a>(&'a std::rc::Rc<u64>);
impl<'q> ExecuteQuery<ReadCounter<'q>> for CounterStore {
    type Output = u64;
    async fn execute<'a>(&'a self, query: ReadCounter<'q>) -> eidetica::Result<u64>
    where
        ReadCounter<'q>: 'a,
    {
        self.transaction()
            .query_store_or_cached_fold::<SocketCounter, _>(
                self.name(),
                Self::type_id(),
                b"client-owned-counter".to_vec(),
                counter_representation(),
                |bytes| Ok(SocketCounter::decode(bytes)?.0 + **query.0),
                |state| Ok(state.0 + **query.0),
            )
            .await
    }
}
fn counter_representation() -> PrivateRepresentation {
    PrivateRepresentation::opaque(
        CounterStore::state_model().descriptor(),
        CounterStore::type_id(),
    )
}
fn doc_representation() -> PrivateRepresentation {
    PrivateRepresentation::opaque(DocStore::state_model().descriptor(), DocStore::type_id())
}

#[derive(Clone, Copy, Default)]
enum Fault {
    #[default]
    None,
    Corrupt,
    CorruptPayload,
    BadBinding,
    EmptyCache,
    BadShape,
    BadKey,
    BadRawSource,
    SourceUnavailable,
    BadResponse,
    ExpiredView,
    InvalidSource,
    InvalidToken,
    Denied,
    RepeatedExpiry,
    Quota,
    ChangedRefusal,
    MetadataSourceMissing,
    LostChunkAck,
    LostFinishAck,
}
#[derive(Default)]
struct Observation {
    requests: Vec<Op>,
    fault: Fault,
}
impl Observation {
    fn raw_count(&self) -> usize {
        self.requests
            .iter()
            .filter(|op| matches!(op, Op::ReadRawStore { .. }))
            .count()
    }
    fn lookup_count(&self) -> usize {
        self.requests
            .iter()
            .filter(|op| matches!(op, Op::LookupPrivateMaterialization { .. }))
            .count()
    }
    fn begin_count(&self) -> usize {
        self.requests
            .iter()
            .filter(|op| matches!(op, Op::BeginPrivateAssistance { .. }))
            .count()
    }
    fn reset(&mut self, fault: Fault) {
        self.requests.clear();
        self.fault = fault;
    }
}
fn wire_error(error: BackendError) -> ServerFrame {
    let error: eidetica::Error = error.into();
    ServerFrame::Response(Box::new(ServiceResponse::Error((&error).into())))
}

// The proxy forwards to a real authenticated daemon. Mutations target exactly
// one derived response or error class; source/history requests remain observable.
async fn observed_proxy(
    socket: &std::path::Path,
    dir: &std::path::Path,
) -> (
    PathBuf,
    Arc<Mutex<Observation>>,
    tokio::task::JoinHandle<()>,
) {
    let proxy = dir.join("sdk-observer.sock");
    let listener = tokio::net::UnixListener::bind(&proxy).unwrap();
    let socket = socket.to_owned();
    let observed = Arc::new(Mutex::new(Observation::default()));
    let shared = observed.clone();
    let task = tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let daemon = UnixStream::connect(socket).await.unwrap();
        let (mut cr, mut cw) = tokio::io::split(client);
        let (mut dr, mut dw) = tokio::io::split(daemon);
        let handshake: Handshake = read_frame(&mut cr).await.unwrap().unwrap();
        write_frame(&mut dw, &handshake).await.unwrap();
        let ack: HandshakeAck = read_frame(&mut dr).await.unwrap().unwrap();
        write_frame(&mut cw, &ack).await.unwrap();
        while let Some(req) = read_frame::<_, ServiceRequest>(&mut cr).await.unwrap() {
            let (op, fault, raws) = {
                let mut state = shared.lock().unwrap();
                let op = if let ServiceRequest::AuthenticatedDb(e) = &req {
                    state.requests.push(e.op.clone());
                    Some(e.op.clone())
                } else {
                    None
                };
                (op, state.fault, state.raw_count())
            };
            write_frame(&mut dw, &req).await.unwrap();
            let mut response: ServerFrame = read_frame(&mut dr).await.unwrap().unwrap();
            if matches!(op, Some(Op::LookupPrivateMaterialization { .. })) {
                match fault {
                    Fault::Corrupt
                    | Fault::RepeatedExpiry
                    | Fault::BadRawSource
                    | Fault::SourceUnavailable => {
                        if let ServerFrame::Response(ref mut r) = response
                            && let ServiceResponse::PrivateMaterialization(Some(page)) = r.as_mut()
                        {
                            for (_, value) in &mut page.records {
                                *value = vec![0xff];
                            }
                        }
                    }
                    Fault::CorruptPayload | Fault::BadBinding => {
                        if let ServerFrame::Response(ref mut r) = response
                            && let ServiceResponse::PrivateMaterialization(Some(page)) = r.as_mut()
                        {
                            for (_, value) in &mut page.records {
                                let envelope_len = b"eidetica/private-opaque-state/v0\0".len();
                                assert!(value.len() > envelope_len + 32);
                                if matches!(fault, Fault::BadBinding) {
                                    value[envelope_len] ^= 1;
                                } else {
                                    // Keep the valid envelope and binding: only
                                    // the whole-state Codec payload is unusable.
                                    value.truncate(envelope_len + 32);
                                    value.push(0xff);
                                }
                            }
                        }
                    }
                    Fault::BadKey => {
                        if let ServerFrame::Response(ref mut r) = response
                            && let ServiceResponse::PrivateMaterialization(Some(page)) = r.as_mut()
                        {
                            page.records[0].0.push(1);
                        }
                    }
                    Fault::EmptyCache => {
                        response = ServerFrame::Response(Box::new(
                            ServiceResponse::PrivateMaterialization(Some(Default::default())),
                        ))
                    }
                    Fault::BadShape => {
                        if let ServerFrame::Response(ref mut r) = response
                            && let ServiceResponse::PrivateMaterialization(Some(page)) = r.as_mut()
                        {
                            page.next = Some(b"not-a-continuation".to_vec());
                        }
                    }
                    Fault::BadResponse => {
                        response = ServerFrame::Response(Box::new(ServiceResponse::Ok))
                    }
                    Fault::ExpiredView => {
                        response = wire_error(BackendError::InvalidStoreStateView)
                    }
                    Fault::InvalidSource => response = wire_error(BackendError::InvalidRawSource),
                    Fault::InvalidToken => {
                        response = wire_error(BackendError::InvalidStoreStateStagingToken)
                    }
                    Fault::Denied => {
                        response = ServerFrame::Response(Box::new(ServiceResponse::Error(
                            eidetica::service::error::ServiceError {
                                module: "auth".into(),
                                kind: "PermissionDenied".into(),
                                message: "current Read revoked".into(),
                            },
                        )))
                    }
                    _ => {}
                }
            }
            if matches!(op, Some(Op::ReadRawStore { .. })) {
                match fault {
                    Fault::BadRawSource => {
                        if let ServerFrame::Response(ref mut r) = response
                            && let ServiceResponse::RawStore(page) = r.as_mut()
                        {
                            page.source.type_id = "wrong:v0".into();
                        }
                    }
                    Fault::SourceUnavailable => {
                        response = wire_error(BackendError::InvalidRawSource)
                    }
                    _ => {}
                }
            }
            if matches!(op, Some(Op::ReadRawStore { .. })) && matches!(fault, Fault::RepeatedExpiry)
            {
                // A broken unbounded replay is stopped by the fixture after a
                // third request, so its count assertion fails, not a hang.
                response = wire_error(if raws <= 2 {
                    BackendError::InvalidRawCursor
                } else {
                    BackendError::InvalidRawSource
                });
            }
            if matches!(op, Some(Op::BeginPrivateAssistance { .. }))
                && matches!(fault, Fault::Quota)
            {
                response = wire_error(BackendError::PrivateCacheQuotaExceeded);
            }
            if matches!(fault, Fault::MetadataSourceMissing)
                && matches!(&op, Some(Op::QueryStore { request }) if request.store == "_index" || request.store == "_settings")
            {
                response = wire_error(BackendError::EntryNotFound {
                    id: eidetica::entry::ID::from_bytes("damaged-source"),
                });
            }
            if matches!(op, Some(Op::QueryStore { .. }))
                && matches!(fault, Fault::ChangedRefusal)
                && let ServerFrame::Response(ref mut r) = response
                && let ServiceResponse::StoreQuery(reply) = r.as_mut()
                && let Some(source) = &mut reply.raw_source
            {
                source.type_id = "wrong:v0".into();
            }
            if (matches!(op, Some(Op::PrivateAssistanceChunk { .. }))
                && matches!(fault, Fault::LostChunkAck))
                || (matches!(op, Some(Op::FinishPrivateAssistance { .. }))
                    && matches!(fault, Fault::LostFinishAck))
            {
                assert!(
                    matches!(response, ServerFrame::Response(r) if matches!(*r, ServiceResponse::Ok))
                );
                return;
            }
            write_frame(&mut cw, &response).await.unwrap();
        }
    });
    (proxy, observed, task)
}
async fn stop_proxy(task: tokio::task::JoinHandle<()>) {
    // Tests own this bounded fixture, not production detached work.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn sdk_cache_ordinary_unknown_doc_and_encrypted_cold_warm_staged_and_pinned() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_client, root, _) = setup_db(&server, &socket, "owner").await;
    create_user_via_admin(&server, "reader").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let reader = server.login_user("reader", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<CounterStore>("binary").await.unwrap();
    tx.get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("key", "registered")
        .await
        .unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    encrypted.initialize("correct", Doc::new()).await.unwrap();
    encrypted
        .inner()
        .await
        .unwrap()
        .set("key", "only-private")
        .await
        .unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &reader.get_default_key().unwrap(),
            eidetica::auth::types::AuthKey::active(None, Permission::Read),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    insert_signed_store_payload(&db, &owner, "binary", SocketCounter(23).encode().unwrap()).await;
    let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
    let client = login_client(&proxy, "reader").await;
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    let pinned = remote.new_transaction().await.unwrap();
    let pinned_source = pinned.query_source().unwrap();
    let counter = pinned.get_store::<CounterStore>("binary").await.unwrap();
    let offset = std::rc::Rc::new(2);
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(counter.query(ReadCounter(&offset)).await.unwrap(), 25);
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    assert_eq!(seen.lock().unwrap().begin_count(), 1);
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(counter.query(ReadCounter(&offset)).await.unwrap(), 25);
    assert_eq!(seen.lock().unwrap().lookup_count(), 1);
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    assert_eq!(seen.lock().unwrap().begin_count(), 0);
    let docs = pinned.get_store::<DocStore>("docs").await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(
        docs.query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("registered")
    );
    assert_eq!(seen.lock().unwrap().lookup_count(), 0);
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    let mut encrypted = pinned
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    assert!(encrypted.inner().await.is_err());
    assert!(encrypted.open("wrong").is_err());
    encrypted.open("correct").unwrap();
    let inner = encrypted.inner().await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(
        inner
            .query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("only-private")
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    {
        let observed = seen.lock().unwrap();
        for op in &observed.requests {
            if let Op::PrivateAssistanceChunk { mutations, .. } = op {
                for mutation in mutations {
                    if let RecordMutation::Put { key, value } = mutation {
                        assert_ne!(key, OPAQUE_STATE_KEY);
                        assert!(
                            !value
                                .windows(b"only-private".len())
                                .any(|w| w == b"only-private")
                        );
                        assert!(Doc::decode(value).is_err());
                    }
                }
            }
        }
    }
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(
        inner
            .query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("only-private")
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    // Repair stays on this transaction's source, not concurrent new tips.
    insert_signed_store_payload(&db, &owner, "binary", SocketCounter(99).encode().unwrap()).await;
    seen.lock().unwrap().reset(Fault::Corrupt);
    assert_eq!(counter.query(ReadCounter(&offset)).await.unwrap(), 25);
    {
        let observed = seen.lock().unwrap();
        assert_eq!(observed.raw_count(), 1);
        for op in &observed.requests {
            if let Op::ReadRawStore { request } = op {
                assert_eq!(request.source.source, pinned_source);
            }
        }
    }
    seen.lock().unwrap().reset(Fault::Corrupt);
    assert_eq!(
        inner
            .query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("only-private")
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    // A staged point is local. An unrelated staged value permits committed
    // reconstruction but the conservative publication helper excludes staging.
    inner.set("staged", "never-cache").await.unwrap();
    seen.lock().unwrap().reset(Fault::Corrupt);
    assert_eq!(
        inner
            .query(GetValue("staged"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("never-cache")
    );
    assert_eq!(
        inner
            .query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("only-private")
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    assert_eq!(seen.lock().unwrap().begin_count(), 0);
    inner.delete("key").await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(inner.query(GetValue("key")).await.unwrap(), None);
    let fresh = pinned.get_store::<DocStore>("fresh").await.unwrap();
    assert_eq!(fresh.query(GetValue("missing")).await.unwrap(), None);
    fresh.set("key", "speculative").await.unwrap();
    assert_eq!(
        fresh
            .query(GetValue("key"))
            .await
            .unwrap()
            .unwrap()
            .as_text(),
        Some("speculative")
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    assert_eq!(seen.lock().unwrap().begin_count(), 0);
    stop_proxy(task).await;
    drop(shutdown);
}

#[tokio::test]
async fn sdk_cache_repair_bound_and_hard_response_errors_never_fallback() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_client, root, _) = setup_db(&server, &socket, "owner").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<CounterStore>("binary").await.unwrap();
    tx.commit().await.unwrap();
    insert_signed_store_payload(&db, &owner, "binary", SocketCounter(23).encode().unwrap()).await;
    let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
    let client = login_client(&proxy, "owner").await;
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    let tx = remote.new_transaction().await.unwrap();
    let counter = tx.get_store::<CounterStore>("binary").await.unwrap();
    let offset = std::rc::Rc::new(0);
    assert_eq!(counter.query(ReadCounter(&offset)).await.unwrap(), 23);
    for fault in [
        Fault::Corrupt,
        Fault::CorruptPayload,
        Fault::EmptyCache,
        Fault::ExpiredView,
    ] {
        seen.lock().unwrap().reset(fault);
        assert_eq!(counter.query(ReadCounter(&offset)).await.unwrap(), 23);
        assert_eq!(seen.lock().unwrap().lookup_count(), 1);
        assert_eq!(seen.lock().unwrap().raw_count(), 1);
    }
    for fault in [
        Fault::BadShape,
        Fault::BadKey,
        Fault::BadBinding,
        Fault::BadResponse,
        Fault::InvalidSource,
        Fault::InvalidToken,
        Fault::Denied,
        Fault::ChangedRefusal,
    ] {
        seen.lock().unwrap().reset(fault);
        assert!(counter.query(ReadCounter(&offset)).await.is_err());
        assert_eq!(seen.lock().unwrap().raw_count(), 0);
        assert_eq!(seen.lock().unwrap().begin_count(), 0);
    }
    for fault in [Fault::BadRawSource, Fault::SourceUnavailable] {
        seen.lock().unwrap().reset(fault);
        let result = counter.query(ReadCounter(&offset)).await;
        match fault {
            Fault::BadRawSource => assert!(matches!(result,
                Err(eidetica::Error::Backend(e)) if matches!(*e, BackendError::InvalidRawPage))),
            Fault::SourceUnavailable => assert!(matches!(result,
                Err(eidetica::Error::Backend(e)) if matches!(*e, BackendError::InvalidRawSource))),
            _ => unreachable!(),
        }
        assert_eq!(seen.lock().unwrap().lookup_count(), 1);
        assert_eq!(seen.lock().unwrap().raw_count(), 1);
        assert_eq!(seen.lock().unwrap().begin_count(), 0);
    }
    seen.lock().unwrap().reset(Fault::RepeatedExpiry);
    let result = counter.query(ReadCounter(&offset)).await;
    assert_eq!(seen.lock().unwrap().lookup_count(), 1);
    assert_eq!(
        seen.lock().unwrap().raw_count(),
        2,
        "only one same-source raw replay for the whole logical read"
    );
    assert!(matches!(result,
        Err(eidetica::Error::Backend(e)) if matches!(*e, BackendError::InvalidRawCursor)));
    assert_eq!(seen.lock().unwrap().begin_count(), 0);
    // A Store's result mapper is outside cache decode: no repair on a public
    // value/RowCodec-like error even though the opaque cache is valid.
    seen.lock().unwrap().reset(Fault::None);
    assert!(
        tx.query_store_or_cached_fold::<SocketCounter, ()>(
            "binary",
            CounterStore::type_id(),
            vec![],
            counter_representation(),
            |_| panic!("unknown handler"),
            |_| Err(eidetica::crdt::CRDTError::DeserializationFailed {
                reason: "application decoding".into()
            }
            .into())
        )
        .await
        .is_err()
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    stop_proxy(task).await;
    drop(shutdown);
}

#[tokio::test]
async fn sdk_cache_corrupt_persisted_payload_source_errors_empty_store_and_optional_refusal() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_client, root, identity) = setup_db(&server, &socket, "owner").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    for name in ["binary", "empty", "bad-source"] {
        tx.get_store::<CounterStore>(name).await.unwrap();
    }
    tx.commit().await.unwrap();
    insert_signed_store_payload(&db, &owner, "binary", SocketCounter(23).encode().unwrap()).await;
    insert_signed_store_payload(&db, &owner, "bad-source", vec![0xff]).await;
    let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
    let client = login_client(&proxy, "owner").await;
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    let tx = remote.new_transaction().await.unwrap();
    let raw = tx
        .raw_store_source("binary", CounterStore::type_id())
        .await
        .unwrap();
    // Seed genuinely persisted malformed private bytes, not only a proxy fault.
    let mut seed = PrivateCacheAssistance::default();
    seed.publish_best_effort(
        &remote_conn(&client),
        identity,
        (raw.clone(), counter_representation()),
        vec![RecordMutation::Put {
            key: OPAQUE_STATE_KEY.to_vec(),
            value: vec![0xff],
        }],
        (),
    )
    .await;
    let counter = tx.get_store::<CounterStore>("binary").await.unwrap();
    let offset = std::rc::Rc::new(0);
    for _ in 0..2 {
        seen.lock().unwrap().reset(Fault::None);
        assert_eq!(counter.query(ReadCounter(&offset)).await.unwrap(), 23);
        assert_eq!(seen.lock().unwrap().lookup_count(), 1);
        assert_eq!(seen.lock().unwrap().raw_count(), 1);
    }
    seen.lock().unwrap().reset(Fault::Quota);
    let empty = tx.get_store::<CounterStore>("empty").await.unwrap();
    assert_eq!(
        empty.query(ReadCounter(&offset)).await.unwrap(),
        7,
        "miss is not empty/default without reading the source"
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    assert_eq!(seen.lock().unwrap().begin_count(), 1);
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(empty.query(ReadCounter(&offset)).await.unwrap(), 7);
    assert_eq!(
        seen.lock().unwrap().raw_count(),
        1,
        "rejected publication left no readable partial cache"
    );
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(empty.query(ReadCounter(&offset)).await.unwrap(), 7);
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    let bad = tx.get_store::<CounterStore>("bad-source").await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert!(bad.query(ReadCounter(&offset)).await.is_err());
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    assert_eq!(seen.lock().unwrap().begin_count(), 0);
    let mut mismatch = raw.clone();
    mismatch.type_id = "mismatch:v0".into();
    seen.lock().unwrap().reset(Fault::None);
    assert!(
        tx.cached_fold_raw_source::<SocketCounter>(&mismatch, counter_representation())
            .await
            .is_err()
    );
    assert_eq!(seen.lock().unwrap().lookup_count(), 0);
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    let mut different = counter_representation();
    different.configuration.push(1);
    assert_eq!(
        tx.cached_fold_raw_source::<SocketCounter>(&raw, different)
            .await
            .unwrap(),
        SocketCounter(23)
    );
    assert_eq!(
        seen.lock().unwrap().raw_count(),
        1,
        "unlike representation must not reuse poison bytes"
    );
    // Real current Read revocation is checked even on a warm private cache.
    let revoke = db.new_transaction().await.unwrap();
    revoke
        .get_settings()
        .unwrap()
        .revoke_auth_key(&owner.get_default_key().unwrap())
        .await
        .unwrap();
    let (_, replacement) = generate_keypair();
    revoke
        .get_settings()
        .unwrap()
        .set_auth_key(
            &replacement,
            eidetica::auth::types::AuthKey::active(None, Permission::Admin(0)),
        )
        .await
        .unwrap();
    revoke.commit().await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert!(empty.query(ReadCounter(&offset)).await.is_err());
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    stop_proxy(task).await;
    drop(shutdown);
}

#[tokio::test]
async fn sdk_cache_encrypted_locked_and_source_corruption_are_hard() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_client, root, _) = setup_db(&server, &socket, "owner").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    encrypted.initialize("correct", Doc::new()).await.unwrap();
    encrypted
        .inner()
        .await
        .unwrap()
        .set("key", "kept")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
    let client = login_client(&proxy, "owner").await;
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    let tx = remote.new_transaction().await.unwrap();
    let source = tx
        .raw_store_source("secret", PasswordStore::<DocStore>::type_id())
        .await
        .unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert!(
        tx.cached_fold_raw_source::<Doc>(&source, doc_representation())
            .await
            .is_err()
    );
    assert_eq!(seen.lock().unwrap().lookup_count(), 0);
    let mut unlocked = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    assert!(unlocked.open("wrong").is_err());
    unlocked.open("correct").unwrap();
    let inner = unlocked.inner().await.unwrap();
    assert!(inner.query(GetValue("key")).await.unwrap().is_some());
    insert_signed_store_payload(&db, &owner, "secret", vec![0xff; 16]).await;
    let tx = remote.new_transaction().await.unwrap();
    let mut unlocked = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    unlocked.open("correct").unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert!(
        unlocked
            .inner()
            .await
            .unwrap()
            .query(GetValue("key"))
            .await
            .is_err()
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    assert_eq!(seen.lock().unwrap().begin_count(), 0);
    stop_proxy(task).await;
    drop(shutdown);
}

#[tokio::test]
async fn sdk_cache_transaction_owner_retains_exact_encrypted_upload_for_explicit_reauthentication()
{
    for fault in [Fault::LostChunkAck, Fault::LostFinishAck] {
        let (socket, shutdown, server, dir) = start_test_server().await;
        let (_client, root, _) = setup_db(&server, &socket, "owner").await;
        let owner = server.login_user("owner", None).await.unwrap();
        let db = owner.open_database(&root).await.unwrap();
        let write = db.new_transaction().await.unwrap();
        let mut encrypted = write
            .get_store::<PasswordStore<DocStore>>("secret")
            .await
            .unwrap();
        encrypted.initialize("correct", Doc::new()).await.unwrap();
        encrypted
            .inner()
            .await
            .unwrap()
            .set("key", "retained-once")
            .await
            .unwrap();
        write.commit().await.unwrap();
        let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
        let client = login_client(&proxy, "owner").await;
        let remote = eidetica::Database::open(&client, &root).await.unwrap();
        let tx = remote.new_transaction().await.unwrap();
        let mut encrypted = tx
            .get_store::<PasswordStore<DocStore>>("secret")
            .await
            .unwrap();
        encrypted.open("correct").unwrap();
        let inner = encrypted.inner().await.unwrap();
        seen.lock().unwrap().reset(fault);
        assert_eq!(
            inner
                .query(GetValue("key"))
                .await
                .unwrap()
                .unwrap()
                .as_text(),
            Some("retained-once")
        );
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        let source = seen
            .lock()
            .unwrap()
            .requests
            .iter()
            .find_map(|op| {
                if let Op::LookupPrivateMaterialization { source, .. } = op {
                    Some(source.clone())
                } else {
                    None
                }
            })
            .unwrap();
        let original_chunk = seen
            .lock()
            .unwrap()
            .requests
            .iter()
            .find_map(|op| {
                if let Op::PrivateAssistanceChunk { .. } = op {
                    Some(serde_json::to_vec(op).unwrap())
                } else {
                    None
                }
            })
            .unwrap();
        // The canonical source changes, but recovery may only inspect/replay
        // the original durable token, not allocate an unrelated upload.
        let later = db.new_transaction().await.unwrap();
        let mut protected = later
            .get_store::<PasswordStore<DocStore>>("secret")
            .await
            .unwrap();
        protected.open("correct").unwrap();
        protected
            .inner()
            .await
            .unwrap()
            .set("key", "later")
            .await
            .unwrap();
        later.commit().await.unwrap();
        let reconnect_dir = tempfile::tempdir().unwrap();
        let (fresh_proxy, resumed, fresh_task) =
            observed_proxy(&socket, reconnect_dir.path()).await;
        let fresh = login_client(&fresh_proxy, "owner").await;
        let reconnect = remote_conn(&fresh);
        resumed.lock().unwrap().reset(Fault::None);
        tx.clone()
            .resume_private_cache_upload(&reconnect)
            .await
            .unwrap();
        {
            let observed = resumed.lock().unwrap();
            assert_eq!(observed.begin_count(), 0);
            assert_eq!(observed.raw_count(), 0);
            let replay = observed.requests.iter().find_map(|op| {
                if let Op::PrivateAssistanceChunk { .. } = op {
                    Some(serde_json::to_vec(op).unwrap())
                } else {
                    None
                }
            });
            if matches!(fault, Fault::LostChunkAck) {
                assert_eq!(replay.unwrap(), original_chunk);
            } else {
                assert!(
                    replay.is_none(),
                    "lost finish recovers the terminal outcome without new chunks"
                );
            }
        }
        let response = reconnect
            .private_assistance(
                root.clone(),
                reconnect.session_identity().unwrap(),
                Op::LookupPrivateMaterialization {
                    source: source.clone(),
                    representation: doc_representation(),
                    range: Default::default(),
                    after: None,
                },
            )
            .await
            .unwrap();
        let ServiceResponse::PrivateMaterialization(Some(page)) = response else {
            panic!("original upload was not recovered")
        };
        assert_eq!(page.records.len(), 1);
        let (logical_key, _) = tx
            .decode_private_record(&source, &page.records[0].0, &page.records[0].1)
            .unwrap();
        assert_eq!(logical_key, OPAQUE_STATE_KEY);
        // Consume the source-bound envelope through the ordinary SDK path on a
        // reauthenticated handle. A new seal for the same source must still reuse
        // the exact recovered encrypted bytes, not refetch canonical payloads.
        let reopened = eidetica::Database::open(&fresh, &root).await.unwrap();
        let pinned = reopened
            .new_transaction_at(&source.source.main)
            .await
            .unwrap();
        let mut protected = pinned
            .get_store::<PasswordStore<DocStore>>("secret")
            .await
            .unwrap();
        protected.open("correct").unwrap();
        let inner = protected.inner().await.unwrap();
        resumed.lock().unwrap().reset(Fault::None);
        assert_eq!(
            inner
                .query(GetValue("key"))
                .await
                .unwrap()
                .unwrap()
                .as_text(),
            Some("retained-once")
        );
        assert_eq!(resumed.lock().unwrap().lookup_count(), 1);
        assert_eq!(resumed.lock().unwrap().raw_count(), 0);
        assert_eq!(resumed.lock().unwrap().begin_count(), 0);
        assert!(
            tx.resume_private_cache_upload(&reconnect).await.is_err(),
            "terminal recovery clears this owner's pending upload"
        );
        stop_proxy(fresh_task).await;
        drop(shutdown);
    }
}

fn assert_normal_doc_wire(seen: &Observation) {
    assert!(
        seen.requests
            .iter()
            .any(|op| matches!(op, Op::QueryStore { .. }))
    );
    assert!(
        !seen.requests.iter().any(|op| matches!(
            op,
            Op::EnsureStoreStateGeneration { .. }
                | Op::ResolveStoreState { .. }
                | Op::GetStoreEntries { .. }
                | Op::BeginStoreStateStaging { .. }
        )),
        "ordinary Doc/metadata reads must not orchestrate generations or collection"
    );
}

#[tokio::test]
async fn doc_convenience_socket_metadata_encrypted_quota_and_fallible_reads() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_client, root, _) = setup_db(&server, &socket, "owner").await;
    create_user_via_admin(&server, "reader").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let reader = server.login_user("reader", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let write = db.new_transaction().await.unwrap();
    let mut encrypted = write
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    encrypted.initialize("correct", Doc::new()).await.unwrap();
    let docs = encrypted.inner().await.unwrap();
    docs.set("user.name", "canonical").await.unwrap();
    docs.set("user.age", 30).await.unwrap();
    write
        .get_store::<DocStore>("plain")
        .await
        .unwrap()
        .set("key", "plain")
        .await
        .unwrap();
    write
        .get_settings()
        .unwrap()
        .set_auth_key(
            &reader.get_default_key().unwrap(),
            eidetica::auth::types::AuthKey::active(None, Permission::Read),
        )
        .await
        .unwrap();
    write.commit().await.unwrap();
    let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
    let client = login_client(&proxy, "reader").await;
    assert!(
        client.remote_connection().is_some(),
        "must exercise an actual socket"
    );
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    let tx = remote.new_transaction().await.unwrap();
    let plain = tx.get_store::<DocStore>("plain").await.unwrap();
    assert_eq!(plain.get_string("key").await.unwrap(), "plain");
    assert_eq!(plain.get_all().await.unwrap().len(), 1);
    tx.get_index()
        .await
        .unwrap()
        .get_entry("secret")
        .await
        .unwrap();
    tx.get_settings().unwrap().auth_snapshot().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    assert!(encrypted.open("wrong").is_err());
    encrypted.open("correct").unwrap();
    let docs = encrypted.inner().await.unwrap();
    assert_normal_doc_wire(&seen.lock().unwrap());
    seen.lock().unwrap().reset(Fault::MetadataSourceMissing);
    assert!(
        tx.get_index()
            .await
            .unwrap()
            .get_subtree_settings("secret")
            .await
            .is_err()
    );
    assert!(tx.get_index().await.unwrap().list().await.is_err());
    assert!(
        tx.get_settings()
            .unwrap()
            .get_height_strategy()
            .await
            .is_err()
    );
    assert!(
        tx.get_store::<DocStore>("must-not-register-on-source-failure")
            .await
            .is_err()
    );
    seen.lock().unwrap().reset(Fault::Quota);
    assert_eq!(docs.get_node("user").await.unwrap().len(), 2);
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    assert_eq!(seen.lock().unwrap().begin_count(), 1);
    assert_normal_doc_wire(&seen.lock().unwrap());
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(
        docs.get_path_as::<String>(eidetica::crdt::doc::Path::new("user.name"))
            .await
            .unwrap(),
        "canonical"
    );
    assert_eq!(
        docs.get_all().await.unwrap().get("user.age"),
        Some(&eidetica::crdt::doc::Value::Int(30))
    );
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(docs.get_string("user.name").await.unwrap(), "canonical");
    assert_eq!(
        seen.lock().unwrap().raw_count(),
        0,
        "warm convenience read must reuse opaque private bytes"
    );
    for fault in [
        Fault::Denied,
        Fault::InvalidSource,
        Fault::ChangedRefusal,
        Fault::BadKey,
        Fault::BadRawSource,
    ] {
        seen.lock().unwrap().reset(fault);
        assert!(docs.get("user.name").await.is_err());
        assert!(docs.get_all().await.is_err());
        assert!(docs.get_or_insert("new-key", 10).await.is_err());
        assert!(
            docs.get_or_insert_path(eidetica::crdt::doc::Path::new("new.path"), 10)
                .await
                .is_err()
        );
        assert!(docs.insert("new-key", "never").await.is_err());
    }
    // Preserve explicitly lossy public wrappers, not internal fallible decisions.
    seen.lock().unwrap().reset(Fault::Denied);
    assert!(docs.get_option("user.name").await.is_none());
    assert!(!docs.contains_path_str("user.name").await);
    seen.lock().unwrap().reset(Fault::None);
    assert!(docs.get("new-key").await.is_err());
    assert_normal_doc_wire(&seen.lock().unwrap());
    stop_proxy(task).await;
    drop(shutdown);
}

#[tokio::test]
async fn doc_convenience_socket_optional_cache_fault_preserves_signed_commit() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_client, root, _) = setup_db(&server, &socket, "owner").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let write = db.new_transaction().await.unwrap();
    let mut protected = write
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    protected.initialize("correct", Doc::new()).await.unwrap();
    protected
        .inner()
        .await
        .unwrap()
        .set("key", "before")
        .await
        .unwrap();
    write.commit().await.unwrap();
    let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
    let client = login_client(&proxy, "owner").await;
    let authenticated = client.login_user("owner", None).await.unwrap();
    let remote = authenticated.open_database(&root).await.unwrap();
    let tx = remote.new_transaction().await.unwrap();
    let mut protected = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    protected.open("correct").unwrap();
    let docs = protected.inner().await.unwrap();
    seen.lock().unwrap().reset(Fault::Quota);
    assert_eq!(docs.get_string("key").await.unwrap(), "before");
    docs.modify::<String, _>("key", |v| *v = "signed-after".into())
        .await
        .unwrap();
    let id = tx.commit().await.unwrap();
    assert_eq!(
        server.backend().get_verification_status(&id).await.unwrap(),
        VerificationStatus::Verified
    );
    seen.lock().unwrap().reset(Fault::None);
    let tx = remote.new_transaction().await.unwrap();
    let mut protected = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    protected.open("correct").unwrap();
    assert_eq!(
        protected
            .inner()
            .await
            .unwrap()
            .get_string("key")
            .await
            .unwrap(),
        "signed-after"
    );
    assert_normal_doc_wire(&seen.lock().unwrap());
    stop_proxy(task).await;
    drop(shutdown);
}
