//! Bounded assistance reaches real socket/auth/source and staging paths.
use super::*;
use eidetica::backend::{RecordMutation as M, RecordRange, StagingStatus};
use eidetica::service::client::{PrivateCacheAssistance, RemoteConnection};
use eidetica::service::protocol::DatabaseOp as Op;
use eidetica::store::assistance::PrivateRepresentation;
use eidetica::store::source::{RawStoreRequest, StoreSource};

fn representation() -> PrivateRepresentation {
    PrivateRepresentation {
        format: ProjectionDescriptor {
            name: "test/binary-private:v0".into(),
            version: 0,
        },
        configuration: b"opaque-whole-state".to_vec(),
    }
}
async fn source(
    conn: &RemoteConnection,
    db: &eidetica::Database,
    identity: &SigKey,
    store: &str,
    ty: &str,
) -> StoreSource {
    conn.store_source(
        db.root_id().clone(),
        identity.clone(),
        store.into(),
        ty.into(),
        eidetica::store::query::QuerySource {
            main: db.snapshot().await.unwrap(),
            scope: ReadScope::Verified,
        },
    )
    .await
    .unwrap()
}
async fn begin(
    conn: &RemoteConnection,
    identity: &SigKey,
    source: &StoreSource,
    rep: PrivateRepresentation,
) -> String {
    match conn
        .private_assistance(
            source.database.clone(),
            identity.clone(),
            Op::BeginPrivateAssistance {
                source: source.clone(),
                representation: rep,
            },
        )
        .await
        .unwrap()
    {
        ServiceResponse::Token(token) => token,
        other => panic!("{other:?}"),
    }
}
fn chunk(root: &eidetica::ID, identity: &SigKey, token: &str, value: Vec<u8>) -> Vec<u8> {
    RemoteConnection::encode_staging_chunk(
        root.clone(),
        identity.clone(),
        Op::PrivateAssistanceChunk {
            token: token.into(),
            chunk_id: 0,
            mutations: vec![M::Put {
                key: b"state".to_vec(),
                value,
            }],
        },
    )
    .unwrap()
}
async fn status(
    conn: &RemoteConnection,
    root: &eidetica::ID,
    identity: &SigKey,
    token: &str,
) -> StagingStatus {
    match conn
        .private_assistance(
            root.clone(),
            identity.clone(),
            Op::PrivateAssistanceStatus {
                token: token.into(),
            },
        )
        .await
        .unwrap()
    {
        ServiceResponse::StagingStatus(Some(s)) => s,
        other => panic!("{other:?}"),
    }
}
async fn lookup(
    conn: &RemoteConnection,
    identity: &SigKey,
    source: &StoreSource,
    rep: PrivateRepresentation,
) -> Option<eidetica::backend::RecordPage> {
    match conn
        .private_assistance(
            source.database.clone(),
            identity.clone(),
            Op::LookupPrivateMaterialization {
                source: source.clone(),
                representation: rep,
                range: RecordRange::default(),
                after: None,
            },
        )
        .await
        .unwrap()
    {
        ServiceResponse::PrivateMaterialization(page) => page,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn private_assistance_read_only_raw_reuse_isolation_retarget_revocation_and_optional_quota() {
    let (socket, shutdown, server, _dir) = start_test_server().await;
    let (owner_client, root, _) = setup_db(&server, &socket, "owner").await;
    create_user_via_admin(&server, "reader").await;
    create_user_via_admin(&server, "other").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let reader = server.login_user("reader", None).await.unwrap();
    let other = server.login_user("other", None).await.unwrap();
    let identity = SigKey::from_pubkey(&reader.get_default_key().unwrap());
    let other_identity = SigKey::from_pubkey(&other.get_default_key().unwrap());
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<CounterStore>("binary").await.unwrap();
    tx.get_store::<CounterStore>("empty").await.unwrap();
    for user in [&reader, &other] {
        tx.get_settings()
            .unwrap()
            .set_auth_key(
                &user.get_default_key().unwrap(),
                eidetica::auth::types::AuthKey::active(None, Permission::Read),
            )
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    insert_signed_store_payload(&db, &owner, "binary", SocketCounter(23).encode().unwrap()).await;
    let client = login_client(&socket, "reader").await;
    let conn = remote_conn(&client);
    let raw = source(&conn, &db, &identity, "binary", CounterStore::type_id()).await;
    let page = conn
        .raw_store_page(
            root.clone(),
            identity.clone(),
            RawStoreRequest {
                source: raw.clone(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    let mut built = SocketCounter::default();
    for entry in page.entries {
        if let Ok(bytes) = entry.data("binary")
            && !bytes.is_empty()
        {
            built = built.merge(&SocketCounter::decode(bytes).unwrap()).unwrap();
        }
    }
    assert_eq!(built, SocketCounter(23));
    assert!(
        lookup(&conn, &identity, &raw, representation())
            .await
            .is_none()
    );
    let token = begin(&conn, &identity, &raw, representation()).await;
    let (persisted, _) = server
        .backend()
        .local_engine()
        .unwrap()
        .store_state_staging_token(&token)
        .await
        .unwrap()
        .unwrap();
    let serialized = serde_json::to_value(persisted).unwrap();
    let mut target: StoreStateRequest =
        serde_json::from_value(serialized["target"].clone()).unwrap();
    assert_eq!(
        target.lifecycle,
        eidetica::backend::StoreStateLifecycle::Derived
    );
    assert!(matches!(
        target.scope,
        eidetica::backend::CacheScope::User(_)
    ));
    let owner_identity = SigKey::from_pubkey(&owner.get_default_key().unwrap());
    target.scope = eidetica::backend::CacheScope::Shared;
    assert!(
        remote_conn(&owner_client)
            .begin_store_state_staging(owner_identity.clone(), target.clone())
            .await
            .is_err(),
        "legacy Write cannot manufacture reserved assistance authority"
    );
    target.lifecycle = eidetica::backend::StoreStateLifecycle::Authoritative;
    assert!(
        remote_conn(&owner_client)
            .begin_store_state_staging(owner_identity, target)
            .await
            .is_err()
    );
    let exact = chunk(&root, &identity, &token, built.encode().unwrap());
    conn.send_staging_chunk(&exact).await.unwrap();
    conn.send_staging_chunk(&exact).await.unwrap();
    assert!(
        conn.send_staging_chunk(&chunk(
            &root,
            &identity,
            &token,
            SocketCounter(24).encode().unwrap()
        ))
        .await
        .is_err()
    );
    assert!(
        lookup(&conn, &identity, &raw, representation())
            .await
            .is_none(),
        "partial upload invisible"
    );
    assert!(
        conn.private_assistance(
            root.clone(),
            identity.clone(),
            Op::PrivateMaterializationGet {
                token: token.clone(),
                key: b"state".to_vec()
            }
        )
        .await
        .is_err()
    );
    conn.private_assistance(
        root.clone(),
        identity.clone(),
        Op::FinishPrivateAssistance {
            token: token.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        lookup(&conn, &identity, &raw, representation())
            .await
            .unwrap()
            .records,
        vec![(b"state".to_vec(), built.encode().unwrap())]
    );
    assert!(matches!(
        status(&conn, &root, &identity, &token).await,
        StagingStatus::Published(_)
    ));
    // Main/index source changes must separate identities even when these
    // Store tips and the representation are exactly unchanged.
    let unrelated = db.new_transaction().await.unwrap();
    unrelated
        .get_store::<DocStore>("side-docs")
        .await
        .unwrap()
        .set("key", "unrelated")
        .await
        .unwrap();
    unrelated.commit().await.unwrap();
    let advanced = source(&conn, &db, &identity, "binary", CounterStore::type_id()).await;
    assert_eq!(advanced.snapshot, raw.snapshot);
    assert_ne!(advanced.source.main, raw.source.main);
    assert!(
        lookup(&conn, &identity, &advanced, representation())
            .await
            .is_none()
    );
    let mut different = representation();
    different.configuration.push(1);
    assert!(lookup(&conn, &identity, &raw, different).await.is_none());
    let empty_source = source(&conn, &db, &identity, "empty", CounterStore::type_id()).await;
    let empty = begin(&conn, &identity, &empty_source, representation()).await;
    conn.private_assistance(
        root.clone(),
        identity.clone(),
        Op::FinishPrivateAssistance { token: empty },
    )
    .await
    .unwrap();
    assert!(
        lookup(&conn, &identity, &empty_source, representation())
            .await
            .unwrap()
            .records
            .is_empty()
    );
    for field in 0..6 {
        let mut forged = raw.clone();
        match field {
            0 => forged.database = eidetica::ID::from_bytes(b"foreign"),
            1 => forged.store = "empty".into(),
            2 => forged.type_id = DocStore::type_id().into(),
            3 => forged.snapshot = eidetica::Snapshot::EMPTY,
            4 => forged.source.scope = ReadScope::AllowUnverified,
            _ => forged.registration.clear(),
        }
        assert!(
            conn.private_assistance(
                root.clone(),
                identity.clone(),
                Op::BeginPrivateAssistance {
                    source: forged,
                    representation: representation()
                }
            )
            .await
            .is_err()
        );
    }
    let other_client = login_client(&socket, "other").await;
    let other_conn = remote_conn(&other_client);
    assert!(
        other_conn
            .private_assistance(
                root.clone(),
                other_identity.clone(),
                Op::PrivateAssistanceStatus {
                    token: token.clone()
                }
            )
            .await
            .is_err()
    );
    let other_source = source(
        &other_conn,
        &db,
        &other_identity,
        "binary",
        CounterStore::type_id(),
    )
    .await;
    assert!(
        lookup(
            &other_conn,
            &other_identity,
            &other_source,
            representation()
        )
        .await
        .is_none()
    );
    assert!(
        conn.private_assistance(
            eidetica::ID::from_bytes(b"foreign"),
            identity.clone(),
            Op::PrivateAssistanceStatus {
                token: token.clone()
            }
        )
        .await
        .is_err()
    );
    // Reserved private targets cannot be created via the old Write path, nor
    // may old operations launder an assistance token into another lifecycle.
    assert!(
        conn.store_state_staging_status(root.clone(), identity.clone(), token.clone())
            .await
            .is_err()
    );
    assert!(
        conn.begin_store_state_staging(identity.clone(), derived_request(&root, "arbitrary"))
            .await
            .is_err()
    );
    let remote = eidetica::Database::open(&client, &root)
        .await
        .unwrap()
        .with_key(
            reader
                .get_signing_key(&reader.get_default_key().unwrap())
                .unwrap(),
        );
    let write = remote.new_transaction().await.unwrap();
    write
        .get_store::<DocStore>("illicit")
        .await
        .unwrap()
        .set("bad", true)
        .await
        .unwrap();
    assert!(
        write.commit().await.is_err(),
        "Read does not authorize signed authoritative writes"
    );
    let busy1 = begin(&conn, &identity, &raw, representation()).await;
    let busy2 = begin(&conn, &identity, &raw, representation()).await;
    let mut sdk = PrivateCacheAssistance::default();
    assert_eq!(
        sdk.publish_best_effort(
            &conn,
            identity.clone(),
            (raw.clone(), representation()),
            vec![],
            built.clone()
        )
        .await,
        built
    );
    assert!(
        !sdk.has_pending_upload(),
        "quota refusal never replaces successful reconstruction"
    );
    for t in [&busy1, &busy2] {
        conn.private_assistance(
            root.clone(),
            identity.clone(),
            Op::CancelPrivateAssistance { token: t.clone() },
        )
        .await
        .unwrap();
    }
    // Revocation and current source posture are separate checks. A failed
    // original Store tip must refuse cache/status reads, even with current Read.
    let engine = server.backend().local_engine().unwrap();
    let original = raw.snapshot.tips().last().unwrap();
    engine
        .update_verification_status(original, VerificationStatus::Failed)
        .await
        .unwrap();
    for op in [
        Op::PrivateAssistanceStatus {
            token: token.clone(),
        },
        Op::PrivateMaterializationGet {
            token: token.clone(),
            key: b"state".to_vec(),
        },
    ] {
        let error = conn
            .private_assistance(root.clone(), identity.clone(), op)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("does not permit Verified"),
            "{error}"
        );
    }
    engine
        .update_verification_status(original, VerificationStatus::Verified)
        .await
        .unwrap();
    let mut settings_like = representation();
    settings_like.configuration = b"settings-like".to_vec();
    let active = begin(&conn, &identity, &raw, settings_like).await;
    // Canonical auth is independent of client-supplied settings-like records.
    let fake_bytes = br#"{"auth":{"global":{"permission":{"Admin":0}}}}"#.to_vec();
    let fake = chunk(&root, &identity, &active, fake_bytes.clone());
    conn.send_staging_chunk(&fake).await.unwrap();
    conn.private_assistance(
        root.clone(),
        identity.clone(),
        Op::FinishPrivateAssistance {
            token: active.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(conn.private_assistance(root.clone(),identity.clone(),Op::PrivateMaterializationGet{token:active.clone(),key:b"state".to_vec()}).await.unwrap(),ServiceResponse::Record(Some(bytes)) if bytes==fake_bytes)
    );
    let write = remote.new_transaction().await.unwrap();
    write
        .get_store::<DocStore>("still-illicit")
        .await
        .unwrap()
        .set("bad", true)
        .await
        .unwrap();
    assert!(
        write.commit().await.is_err(),
        "even a live settings-like private materialization cannot grant Write"
    );
    let revoke = db.new_transaction().await.unwrap();
    revoke
        .get_settings()
        .unwrap()
        .revoke_auth_key(&reader.get_default_key().unwrap())
        .await
        .unwrap();
    revoke.commit().await.unwrap();
    for op in [
        Op::PrivateAssistanceStatus {
            token: token.clone(),
        },
        Op::PrivateAssistanceChunk {
            token: active.clone(),
            chunk_id: 1,
            mutations: vec![],
        },
        Op::FinishPrivateAssistance {
            token: active.clone(),
        },
        Op::CancelPrivateAssistance {
            token: active.clone(),
        },
        Op::PrivateMaterializationGet {
            token: token.clone(),
            key: b"state".to_vec(),
        },
        Op::PrivateMaterializationPage {
            token: token.clone(),
            range: RecordRange::default(),
            after: None,
        },
        Op::LookupPrivateMaterialization {
            source: raw.clone(),
            representation: representation(),
            range: RecordRange::default(),
            after: None,
        },
    ] {
        assert!(
            conn.private_assistance(root.clone(), identity.clone(), op)
                .await
                .is_err(),
            "revocation checked for every operation"
        );
    }
    assert!(
        conn.raw_store_page(
            root,
            identity,
            RawStoreRequest {
                source: raw,
                cursor: None
            }
        )
        .await
        .is_err()
    );
    drop(shutdown);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn private_assistance_sqlite_restart_reconnect_keeps_original_binding_and_exact_outcomes() {
    use eidetica::backend::database::Sqlite;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("private.db");
    let socket = dir.path().join("private.sock");
    let (server, _) = Instance::create_backend(
        Box::new(Sqlite::open(&file).await.unwrap()),
        NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (stop, rx) = watch::channel(());
    let task = tokio::spawn(
        ServiceServer::bind(server.clone(), &socket)
            .await
            .unwrap()
            .run(rx),
    );
    let (client, root, identity) = setup_db(&server, &socket, "alice").await;
    let owner = server.login_user("alice", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<CounterStore>("binary").await.unwrap();
    tx.commit().await.unwrap();
    insert_signed_store_payload(&db, &owner, "binary", SocketCounter(17).encode().unwrap()).await;
    let conn = remote_conn(&client);
    let raw = source(&conn, &db, &identity, "binary", CounterStore::type_id()).await;
    let token = begin(&conn, &identity, &raw, representation()).await;
    let exact = chunk(
        &root,
        &identity,
        &token,
        SocketCounter(17).encode().unwrap(),
    );
    // Discard the acknowledgement while preserving the exact bytes.
    conn.send_staging_chunk(&exact).await.unwrap();
    let done = begin(&conn, &identity, &raw, representation()).await;
    conn.send_staging_chunk(&chunk(
        &root,
        &identity,
        &done,
        SocketCounter(17).encode().unwrap(),
    ))
    .await
    .unwrap();
    conn.private_assistance(
        root.clone(),
        identity.clone(),
        Op::FinishPrivateAssistance {
            token: done.clone(),
        },
    )
    .await
    .unwrap();
    insert_signed_store_payload(&db, &owner, "binary", SocketCounter(99).encode().unwrap()).await;
    drop(conn);
    drop(client);
    drop(db);
    drop(owner);
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(server);
    let reopened = Sqlite::open(&file).await.unwrap();
    let server = Instance::open_backend(Box::new(reopened)).await.unwrap();
    let (stop, rx) = watch::channel(());
    let task = tokio::spawn(
        ServiceServer::bind(server.clone(), &socket)
            .await
            .unwrap()
            .run(rx),
    );
    let client = login_client(&socket, "alice").await;
    let conn = remote_conn(&client);
    assert_eq!(
        status(&conn, &root, &identity, &token).await,
        StagingStatus::Active
    );
    assert!(matches!(
        status(&conn, &root, &identity, &done).await,
        StagingStatus::Published(_)
    ));
    conn.send_staging_chunk(&exact).await.unwrap();
    assert!(
        conn.send_staging_chunk(&chunk(
            &root,
            &identity,
            &token,
            SocketCounter(99).encode().unwrap()
        ))
        .await
        .is_err()
    );
    conn.private_assistance(
        root.clone(),
        identity.clone(),
        Op::FinishPrivateAssistance {
            token: token.clone(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        status(&conn, &root, &identity, &token).await,
        StagingStatus::Adopted(_)
    ));
    let got = conn
        .private_assistance(
            root.clone(),
            identity.clone(),
            Op::PrivateMaterializationGet {
                token: token.clone(),
                key: b"state".to_vec(),
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(got,ServiceResponse::Record(Some(bytes)) if bytes==SocketCounter(17).encode().unwrap())
    );
    // The expired per-run seal cannot authorize fresh admission or latest tips.
    assert!(
        conn.private_assistance(
            root.clone(),
            identity.clone(),
            Op::BeginPrivateAssistance {
                source: raw.clone(),
                representation: representation()
            }
        )
        .await
        .is_err()
    );
    assert!(
        conn.raw_store_page(
            root.clone(),
            identity.clone(),
            RawStoreRequest {
                source: raw,
                cursor: None
            }
        )
        .await
        .is_err()
    );
    let owner = server.login_user("alice", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let fresh = source(&conn, &db, &identity, "binary", CounterStore::type_id()).await;
    assert!(
        lookup(&conn, &identity, &fresh, representation())
            .await
            .is_none(),
        "advanced tips cannot reuse old data"
    );
    let expired = begin(&conn, &identity, &fresh, representation()).await;
    server
        .testing_age_store_state_staging(&expired, 301)
        .await
        .unwrap();
    assert!(
        conn.private_assistance(
            root.clone(),
            identity.clone(),
            Op::FinishPrivateAssistance {
                token: expired.clone()
            }
        )
        .await
        .is_err()
    );
    server
        .testing_age_store_state_staging(&expired, 301)
        .await
        .unwrap();
    server.testing_reclaim_expired_store_state().await.unwrap();
    assert_eq!(
        status(&conn, &root, &identity, &expired).await,
        StagingStatus::Expired
    );
    conn.private_assistance(
        root.clone(),
        identity.clone(),
        Op::FinishPrivateAssistance { token: done },
    )
    .await
    .unwrap();
    drop(conn);
    drop(client);
    drop(db);
    drop(owner);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

// Forward actual authenticated operations to the real daemon, consume one
// chosen successful acknowledgement, then close. No fake staging responses.
async fn lossy_proxy(
    socket: &std::path::Path,
    dir: &std::path::Path,
    finish: bool,
) -> (PathBuf, tokio::task::JoinHandle<()>) {
    let proxy = dir.join(if finish { "finish.sock" } else { "chunk.sock" });
    let listener = tokio::net::UnixListener::bind(&proxy).unwrap();
    let socket = socket.to_owned();
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
            let discard = matches!(&req,ServiceRequest::AuthenticatedDb(e) if if finish { matches!(e.op,Op::FinishPrivateAssistance{..}) } else { matches!(e.op,Op::PrivateAssistanceChunk{..}) });
            write_frame(&mut dw, &req).await.unwrap();
            let response: ServerFrame = read_frame(&mut dr).await.unwrap().unwrap();
            if discard {
                assert!(
                    matches!(response,ServerFrame::Response(r) if matches!(*r,ServiceResponse::Ok))
                );
                return;
            }
            write_frame(&mut cw, &response).await.unwrap();
        }
    });
    (proxy, task)
}
#[tokio::test]
async fn private_assistance_sdk_keeps_exact_chunks_and_valid_result_on_lost_chunk_and_finish_ack() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (client, root, identity) = setup_db(&server, &socket, "alice").await;
    let owner = server.login_user("alice", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<CounterStore>("binary").await.unwrap();
    tx.commit().await.unwrap();
    insert_signed_store_payload(&db, &owner, "binary", SocketCounter(23).encode().unwrap()).await;
    for finish in [false, true] {
        let (proxy, task) = lossy_proxy(&socket, dir.path(), finish).await;
        let proxy_client = login_client(&proxy, "alice").await;
        let proxy_conn = remote_conn(&proxy_client);
        let raw = source(
            &proxy_conn,
            &db,
            &identity,
            "binary",
            CounterStore::type_id(),
        )
        .await;
        let mut rep = representation();
        rep.configuration.push(u8::from(finish));
        let mut sdk = PrivateCacheAssistance::default();
        let bytes = SocketCounter(23).encode().unwrap();
        let result = sdk
            .publish_best_effort(
                &proxy_conn,
                identity.clone(),
                (raw.clone(), rep.clone()),
                vec![M::Put {
                    key: b"state".to_vec(),
                    value: bytes.clone(),
                }],
                23,
            )
            .await;
        assert_eq!(result, 23);
        assert!(sdk.has_pending_upload());
        task.await.unwrap();
        insert_signed_store_payload(&db, &owner, "binary", SocketCounter(99).encode().unwrap())
            .await;
        let reauthenticated = login_client(&socket, "alice").await;
        let reconnect = remote_conn(&reauthenticated);
        sdk.resume(&reconnect).await.unwrap();
        assert!(!sdk.has_pending_upload());
        assert_eq!(
            lookup(&reconnect, &identity, &raw, rep)
                .await
                .unwrap()
                .records,
            vec![(b"state".to_vec(), bytes)]
        );
    }
    drop(client);
    drop(shutdown);
}

#[tokio::test]
async fn private_assistance_encrypted_sdk_transport_storage_exact_retry_requires_client_key() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_, root, _) = setup_db(&server, &socket, "owner").await;
    create_user_via_admin(&server, "reader").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let reader = server.login_user("reader", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut protected = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    protected.initialize("correct", Doc::new()).await.unwrap();
    protected
        .inner()
        .await
        .unwrap()
        .set("secret-value", "kept-private")
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
    let identity = SigKey::from_pubkey(&reader.get_default_key().unwrap());
    let (proxy, task) = lossy_proxy(&socket, dir.path(), false).await;
    let client = login_client(&proxy, "reader").await;
    let conn = remote_conn(&client);
    let raw = source(
        &conn,
        &db,
        &identity,
        "secret",
        PasswordStore::<DocStore>::type_id(),
    )
    .await;
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    let tx = remote.new_transaction().await.unwrap();
    assert!(tx.fold_raw_source::<Doc>(&raw).await.is_err());
    let mut unlocked = tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    assert!(unlocked.open("wrong").is_err());
    unlocked.open("correct").unwrap();
    let rebuilt = tx.fold_raw_source::<Doc>(&raw).await.unwrap();
    let plaintext = rebuilt.encode().unwrap();
    let logical = b"state".to_vec();
    let mut sdk = PrivateCacheAssistance::default();
    let valid = tx
        .cache_private_records_best_effort(
            &mut sdk,
            (raw.clone(), representation()),
            vec![(logical.clone(), plaintext.clone())],
            rebuilt.clone(),
        )
        .await;
    assert_eq!(valid, rebuilt);
    assert!(sdk.has_pending_upload());
    task.await.unwrap();
    let fresh = login_client(&socket, "reader").await;
    let reconnect = remote_conn(&fresh);
    sdk.resume(&reconnect).await.unwrap();
    let cached = lookup(&reconnect, &identity, &raw, representation())
        .await
        .unwrap();
    assert_eq!(cached.records.len(), 1);
    let (key, ciphertext) = &cached.records[0];
    assert_ne!(*ciphertext, plaintext);
    assert_ne!(*key, logical);
    assert!(
        !ciphertext
            .windows(b"kept-private".len())
            .any(|w| w == b"kept-private")
    );
    let remote = eidetica::Database::open(&fresh, &root).await.unwrap();
    let locked_tx = remote.new_transaction().await.unwrap();
    assert!(
        locked_tx
            .decode_private_record(&raw, key, ciphertext)
            .is_err()
    );
    let mut unlocked = locked_tx
        .get_store::<PasswordStore<DocStore>>("secret")
        .await
        .unwrap();
    unlocked.open("correct").unwrap();
    let (decoded_key, decoded) = locked_tx
        .decode_private_record(&raw, key, ciphertext)
        .unwrap();
    assert_eq!(decoded_key, logical);
    assert_eq!(decoded, plaintext);
    let mut modified = ciphertext.clone();
    modified[0] ^= 1;
    assert!(
        locked_tx
            .decode_private_record(&raw, key, &modified)
            .is_err()
    );
    // A result with staged mutations stays valid but is not published under a
    // committed-only cache identity.
    unlocked
        .inner()
        .await
        .unwrap()
        .set("staged", "uncommitted")
        .await
        .unwrap();
    let mut rep = representation();
    rep.configuration.push(2);
    assert_eq!(
        locked_tx
            .cache_private_records_best_effort(
                &mut sdk,
                (raw.clone(), rep.clone()),
                vec![(logical, plaintext)],
                "valid staged result"
            )
            .await,
        "valid staged result"
    );
    assert!(lookup(&reconnect, &identity, &raw, rep).await.is_none());
    drop(shutdown);
}

#[tokio::test]
async fn private_assistance_record_shaped_pages_and_oversized_sdk_failure_are_bounded() {
    let (socket, shutdown, server, _dir) = start_test_server().await;
    let (client, root, identity) = setup_db(&server, &socket, "alice").await;
    let owner = server.login_user("alice", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<CounterStore>("binary").await.unwrap();
    tx.commit().await.unwrap();
    let conn = remote_conn(&client);
    let raw = source(&conn, &db, &identity, "binary", CounterStore::type_id()).await;
    let expected = (0u16..300)
        .map(|i| (i.to_be_bytes().to_vec(), vec![i as u8; 256]))
        .collect::<Vec<_>>();
    let mut sdk = PrivateCacheAssistance::default();
    let mut rep = representation();
    rep.configuration = b"physical-records".to_vec();
    assert_eq!(
        sdk.publish_best_effort(
            &conn,
            identity.clone(),
            (raw.clone(), rep.clone()),
            expected
                .iter()
                .map(|(key, value)| M::Put {
                    key: key.clone(),
                    value: value.clone()
                })
                .collect(),
            7
        )
        .await,
        7
    );
    assert!(!sdk.has_pending_upload());
    let mut after = None;
    let mut actual = Vec::new();
    let mut pages = 0;
    loop {
        let response = conn
            .private_assistance(
                root.clone(),
                identity.clone(),
                Op::LookupPrivateMaterialization {
                    source: raw.clone(),
                    representation: rep.clone(),
                    range: RecordRange::default(),
                    after: after.clone(),
                },
            )
            .await
            .unwrap();
        assert!(
            serde_json::to_vec(&ServerFrame::Response(Box::new(response.clone())))
                .unwrap()
                .len()
                <= 4 * 1024 * 1024
        );
        let ServiceResponse::PrivateMaterialization(Some(page)) = response else {
            panic!("{response:?}")
        };
        assert!(page.records.len() <= 128);
        actual.extend(page.records);
        pages += 1;
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(actual, expected);
    let token = begin(&conn, &identity, &raw, rep.clone()).await;
    let error = conn
        .private_assistance(
            root.clone(),
            identity.clone(),
            Op::PrivateAssistanceChunk {
                token: token.clone(),
                chunk_id: 0,
                mutations: vec![M::Put {
                    key: vec![0],
                    value: vec![255; 400_000],
                }],
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error,eidetica::Error::Backend(ref e) if matches!(**e,eidetica::backend::BackendError::RecordTooLarge{..}))
    );
    let error = conn
        .private_assistance(
            root.clone(),
            identity.clone(),
            Op::PrivateAssistanceStatus {
                token: "x".repeat(4 * 1024 * 1024),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error,eidetica::Error::Backend(ref e) if matches!(**e,eidetica::backend::BackendError::SourceTooLarge))
    );
    assert_eq!(
        status(&conn, &root, &identity, &token).await,
        StagingStatus::Active
    );
    conn.private_assistance(
        root.clone(),
        identity.clone(),
        Op::CancelPrivateAssistance { token },
    )
    .await
    .unwrap();
    rep.configuration = b"too-large".to_vec();
    assert_eq!(
        sdk.publish_best_effort(
            &conn,
            identity.clone(),
            (raw.clone(), rep.clone()),
            vec![M::Put {
                key: vec![0],
                value: vec![255; 1024 * 1024]
            }],
            "valid read"
        )
        .await,
        "valid read"
    );
    assert!(!sdk.has_pending_upload());
    assert!(lookup(&conn, &identity, &raw, rep).await.is_none());
    drop(shutdown);
}
