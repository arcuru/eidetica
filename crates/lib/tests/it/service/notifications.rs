//! End-to-end v0.1 Snapshot watches: distinct registration/view/identity,
//! initial ordering, coalescing and cancellation. Shared socket backlog
//! bounding remains a separate transport concern.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use eidetica::{
    Database, Entry, Instance, Snapshot, WriteCallback, WriteSource,
    auth::{
        crypto::{PrivateKey, sign_entry},
        types::SigKey,
    },
    backend::{VerificationStatus, database::InMemory},
    crdt::Doc,
    service::protocol::{AuthenticatedDbRequest, DatabaseOp, ReadScope, ServiceRequest},
    store::DocStore,
};
use tokio::sync::{Semaphore, mpsc};

use super::{create_user_via_admin, remote_conn, start_test_server};

async fn next(rx: &mut mpsc::UnboundedReceiver<Snapshot>) -> Snapshot {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("watch stalled")
        .expect("watch closed")
}
async fn watch(db: &Database) -> (WriteCallback, mpsc::UnboundedReceiver<Snapshot>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cb = db
        .on_write(move |snapshot, _| {
            let _ = tx.send(snapshot.clone());
            async { Ok(()) }
        })
        .await
        .unwrap();
    (cb, rx)
}
async fn put(db: &Database, value: &str) -> eidetica::ID {
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("key", value)
        .await
        .unwrap();
    tx.commit().await.unwrap()
}
async fn client(socket: &std::path::Path, root: &eidetica::ID) -> (Instance, Database) {
    let instance = Instance::connect(format!("unix://{}", socket.display()))
        .await
        .unwrap();
    let _user = instance.login_account("alice", None).await.unwrap();
    let db = Database::open(&instance, root).await.unwrap();
    (instance, db)
}
async fn server_database(server: &Instance) -> (Database, PrivateKey) {
    create_user_via_admin(server, "alice").await;
    let mut user = server.login_account("alice", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let signing = user.get_signing_key(&key).unwrap();
    (
        user.create_database(Doc::new(), &key).await.unwrap(),
        signing,
    )
}
async fn source_chain(server: &Instance, db: &Database, key: PrivateKey) -> (Entry, Entry) {
    let source = Instance::create_backend(
        Box::new(InMemory::new()),
        eidetica::NewAccount::passwordless("source"),
    )
    .await
    .unwrap()
    .0;
    source
        .put_entry(
            db.root_id(),
            VerificationStatus::Verified,
            server.backend().get(db.root_id()).await.unwrap(),
            WriteSource::Remote,
        )
        .await
        .unwrap();
    let source_db = Database::open(&source, db.root_id())
        .await
        .unwrap()
        .with_key(key);
    let first = put(&source_db, "first").await;
    let second = put(&source_db, "second").await;
    (
        source.backend().get(&first).await.unwrap(),
        source.backend().get(&second).await.unwrap(),
    )
}

#[tokio::test]
async fn snapshot_watch_service_quiet_initial_and_concurrent_registrations() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (server_db, _) = server_database(&server).await;
    let (_instance, db) = client(&socket, server_db.root_id()).await;
    let initial = db.snapshot().await.unwrap();
    let ((one, mut a), (two, mut b)) = tokio::join!(watch(&db), watch(&db));
    assert_eq!(next(&mut a).await, initial);
    assert_eq!(next(&mut b).await, initial);
    let id = put(&server_db, "server write").await;
    let latest = Snapshot::new(vec![id]);
    assert_eq!(next(&mut a).await, latest);
    assert_eq!(next(&mut b).await, latest);
    drop(one);
    let id = put(&server_db, "after one cancellation").await;
    assert_eq!(next(&mut b).await, Snapshot::new(vec![id]));
    drop(two);
    assert!(a.recv().await.is_none());
}

#[tokio::test]
async fn snapshot_watch_service_registration_races_and_cross_client_parity() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (server_db, _) = server_database(&server).await;
    let (_one, db) = client(&socket, server_db.root_id()).await;
    let (_two, other) = client(&socket, server_db.root_id()).await;
    for i in 0..12 {
        let before = db.snapshot().await.unwrap();
        let value = i.to_string();
        let (registered, id) = tokio::join!(watch(&db), put(&server_db, &value));
        let (_cb, mut rx) = registered;
        let after = Snapshot::new(vec![id]);
        let initial = next(&mut rx).await;
        assert!(initial == before || initial == after);
        if initial == before {
            assert_eq!(next(&mut rx).await, after);
        }
        let (_other_cb, mut other_rx) = watch(&other).await;
        assert_eq!(next(&mut other_rx).await, after);
    }
}

#[tokio::test]
async fn snapshot_watch_service_views_promotion_and_failed_restoration() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (server_db, key) = server_database(&server).await;
    let (first, second) = source_chain(&server, &server_db, key).await;
    let (_instance, db) = client(&socket, server_db.root_id()).await;
    let opted = db.clone().allow_unverified();
    let (_v, mut verified) = watch(&db).await;
    let (_u, mut unverified) = watch(&opted).await;
    let checkpoint = next(&mut verified).await;
    assert_eq!(next(&mut unverified).await, checkpoint);
    server
        .put_entry(
            db.root_id(),
            VerificationStatus::Unverified,
            second.clone(),
            WriteSource::Remote,
        )
        .await
        .unwrap();
    assert_eq!(
        next(&mut unverified).await,
        Snapshot::new(vec![db.root_id().clone(), second.id()])
    );
    assert_eq!(
        opted.snapshot().await.unwrap(),
        Snapshot::new(vec![db.root_id().clone(), second.id()])
    );
    assert_eq!(db.snapshot().await.unwrap(), checkpoint);
    server
        .put_entry(
            db.root_id(),
            VerificationStatus::Unverified,
            first.clone(),
            WriteSource::Remote,
        )
        .await
        .unwrap();
    assert_eq!(
        next(&mut unverified).await,
        Snapshot::new(vec![second.id()])
    );
    server_db.verify().await.unwrap();
    let latest = next(&mut verified).await;
    assert_eq!(latest, Snapshot::new(vec![second.id()]));
    assert_eq!(
        db.ids_added(&checkpoint, &latest).await.unwrap(),
        vec![first.id(), second.id()]
    );
    assert!(
        unverified.try_recv().is_err(),
        "promotion is invisible to the opt-in frontier"
    );

    let bad = first.clone().with_auth(|auth| {
        auth.signature = Some(sign_entry(&first, &PrivateKey::generate()).unwrap());
    });
    server
        .put_entry(
            db.root_id(),
            VerificationStatus::Unverified,
            bad.clone(),
            WriteSource::Remote,
        )
        .await
        .unwrap();
    let pending = next(&mut unverified).await;
    assert_eq!(pending, Snapshot::new(vec![second.id(), bad.id()]));
    assert_eq!(server_db.verify().await.unwrap().failed, 1);
    assert_eq!(next(&mut unverified).await, latest);
    assert_eq!(opted.snapshot().await.unwrap(), latest);
    assert!(
        verified.try_recv().is_err(),
        "Unverified/Failed payload must not leak into the default watch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_watch_service_slow_callback_initial_order_and_same_tree_independence() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (server_db, _) = server_database(&server).await;
    let (_instance, db) = client(&socket, server_db.root_id()).await;
    let first = Arc::new(AtomicBool::new(true));
    let release = Arc::new(Semaphore::new(0));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _slow = db
        .on_write({
            let release = release.clone();
            move |snapshot, readonly| {
                let snapshot = snapshot.clone();
                let readonly = readonly.clone();
                let tx = tx.clone();
                let release = release.clone();
                let first = first.swap(false, Ordering::SeqCst);
                async move {
                    // A wire read from a callback must not deadlock the socket reader.
                    readonly.snapshot().await?;
                    tx.send(snapshot).unwrap();
                    if first {
                        release.acquire().await.unwrap().forget();
                    }
                    Ok(())
                }
            }
        })
        .await
        .unwrap();
    let initial = next(&mut rx).await;
    let (_fast, mut fast) = watch(&db).await;
    assert_eq!(next(&mut fast).await, initial);
    // Also exercise a different tree on this same connection.
    let mut user = server.login_account("alice", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let second_server = user.create_database(Doc::new(), &key).await.unwrap();
    let second = Database::open(&_instance, second_server.root_id())
        .await
        .unwrap();
    let (_second_cb, mut second_rx) = watch(&second).await;
    next(&mut second_rx).await;
    let mut final_snapshot = initial.clone();
    for i in 0..24 {
        final_snapshot = Snapshot::new(vec![put(&server_db, &i.to_string()).await]);
    }
    while next(&mut fast).await != final_snapshot {}
    let second_id = put(&second_server, "unblocked").await;
    assert_eq!(next(&mut second_rx).await, Snapshot::new(vec![second_id]));
    // The fast registration's final Snapshot is a wire reader barrier for all
    // already-sent frames. Wait for a fresh selected read too before releasing.
    assert_eq!(db.snapshot().await.unwrap(), final_snapshot);
    release.add_permits(1);
    loop {
        let selected = next(&mut rx).await;
        assert_ne!(
            selected, initial,
            "initial cannot arrive after newer updates"
        );
        if selected == final_snapshot {
            break;
        }
    }
}

#[tokio::test]
async fn snapshot_watch_service_keyless_remote_identity_and_revocation() {
    use eidetica::auth::{AuthKey, Permission};
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (owner, _) = server_database(&server).await;
    let (instance, session_db) = client(&socket, owner.root_id()).await;
    let mut user = instance.login_account("alice", None).await.unwrap();
    let member = user.add_private_key(None).await.unwrap();
    owner
        .with_transaction(|tx| {
            let pubkey = member.clone();
            async move {
                tx.get_settings()?
                    .set_auth_key(&pubkey, AuthKey::active(None, Permission::Read))
                    .await
            }
        })
        .await
        .unwrap();
    let acting = SigKey::from_pubkey(&member);
    user.map_key(&member, owner.root_id(), acting.clone())
        .await
        .unwrap();
    // This supported public path proves the non-login key to the connection.
    user.open_database_with_key(owner.root_id(), &member)
        .await
        .unwrap();
    let conn = remote_conn(&instance);
    let member_db = Database::open_remote(&instance, conn.clone(), owner.root_id(), acting.clone())
        .await
        .unwrap();
    assert!(
        member_db.auth_identity().is_none(),
        "read-only remote identity is in the Backend seam"
    );
    let (_member_cb, mut member_rx) = watch(&member_db).await;
    let (_owner_cb, mut owner_rx) = watch(&session_db).await;
    next(&mut member_rx).await;
    next(&mut owner_rx).await;
    let id = put(&owner, "both identities").await;
    assert_eq!(next(&mut member_rx).await, Snapshot::new(vec![id.clone()]));
    assert_eq!(next(&mut owner_rx).await, Snapshot::new(vec![id]));
    owner
        .with_transaction(|tx| {
            let pubkey = member.clone();
            async move { tx.get_settings()?.revoke_auth_key(&pubkey).await }
        })
        .await
        .unwrap();
    assert!(
        member_db.snapshot().await.is_err(),
        "cursors must not grant a revoked identity content access"
    );
    assert!(
        member_db.on_write(|_, _| async { Ok(()) }).await.is_err(),
        "another identity's subscription must not bypass a fresh Read gate"
    );
    assert!(session_db.snapshot().await.is_ok());
}

#[tokio::test]
async fn snapshot_watch_service_scope_binding_unsubscribe_and_disconnect_cleanup() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (owner, _) = server_database(&server).await;
    let (instance, db) = client(&socket, owner.root_id()).await;
    let (mut reader, mut writer) = super::raw_handshake(&socket).await;
    let user = server.login_account("alice", None).await.unwrap();
    let key = user
        .get_signing_key(&user.get_default_key().unwrap())
        .unwrap();
    let call = ServiceRequest::TrustedLoginAccount {
        username: "alice".into(),
    };
    let response = raw_call(&mut reader, &mut writer, call).await;
    let eidetica::service::protocol::ServiceResponse::TrustedLoginChallenge { challenge, .. } =
        response
    else {
        panic!("no challenge")
    };
    let proof = eidetica::auth::crypto::create_challenge_response(&challenge, &key);
    assert!(matches!(
        raw_call(
            &mut reader,
            &mut writer,
            ServiceRequest::TrustedLoginProve { signature: proof }
        )
        .await,
        eidetica::service::protocol::ServiceResponse::TrustedLoginOk
    ));
    let request = |op| {
        ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
            root_id: owner.root_id().clone(),
            identity: SigKey::from_pubkey(&key.public_key()),
            op,
        }))
    };
    let baseline = server.write_callback_count(owner.root_id());
    for _ in 0..2 {
        assert!(matches!(
            raw_call(
                &mut reader,
                &mut writer,
                request(DatabaseOp::SubscribeWrites {
                    subscription_id: 999,
                    scope: ReadScope::Verified
                })
            )
            .await,
            eidetica::service::protocol::ServiceResponse::Ok
        ));
        assert_eq!(server.write_callback_count(owner.root_id()), baseline + 1);
    }
    assert!(matches!(
        raw_call(
            &mut reader,
            &mut writer,
            request(DatabaseOp::SubscribeWrites {
                subscription_id: 999,
                scope: ReadScope::AllowUnverified
            })
        )
        .await,
        eidetica::service::protocol::ServiceResponse::Error(_)
    ));
    assert!(matches!(
        raw_call(
            &mut reader,
            &mut writer,
            request(DatabaseOp::UnsubscribeWrites {
                subscription_id: 999
            })
        )
        .await,
        eidetica::service::protocol::ServiceResponse::Ok
    ));
    assert_eq!(server.write_callback_count(owner.root_id()), baseline);
    drop(reader);
    drop(writer);
    let (_callback, mut rx) = watch(&db).await;
    next(&mut rx).await;
    assert_eq!(server.write_callback_count(owner.root_id()), baseline + 1);
    drop(user);
    drop(db);
    drop(instance);
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.write_callback_count(owner.root_id()) != baseline {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disconnect retained daemon registrations");
}

#[tokio::test]
async fn snapshot_watch_service_cancelled_id_sweeps_without_cancelling_replacement() {
    eidetica::service::client::set_idle_grace_window_for_test(Duration::from_millis(20));
    eidetica::service::client::set_sweep_interval_for_test(Duration::from_millis(5));
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (owner, _) = server_database(&server).await;
    let (_instance, db) = client(&socket, owner.root_id()).await;
    let baseline = server.write_callback_count(owner.root_id());
    let (old, mut old_rx) = watch(&db).await;
    next(&mut old_rx).await;
    drop(old);
    let (new, mut rx) = watch(&db).await;
    next(&mut rx).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.write_callback_count(owner.root_id()) != baseline + 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("old ID was not released");
    let id = put(&owner, "replacement remains live").await;
    assert_eq!(next(&mut rx).await, Snapshot::new(vec![id]));
    assert!(old_rx.recv().await.is_none());
    drop(new);
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.write_callback_count(owner.root_id()) != baseline {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled registration retained daemon state");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshot_watch_service_concurrent_writers_never_regress_selected_state() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (owner, _) = server_database(&server).await;
    let (one, read_one) = client(&socket, owner.root_id()).await;
    let (two, read_two) = client(&socket, owner.root_id()).await;
    let user_one = one.login_account("alice", None).await.unwrap();
    let user_two = two.login_account("alice", None).await.unwrap();
    let write_one = user_one.open_database(owner.root_id()).await.unwrap();
    let write_two = user_two.open_database(owner.root_id()).await.unwrap();
    let (_a, mut a) = watch(&read_one).await;
    let (_b, mut b) = watch(&read_two).await;
    let mut previous_a = next(&mut a).await;
    let mut previous_b = next(&mut b).await;
    let (ids_a, ids_b) = tokio::join!(
        async {
            let mut ids = Vec::new();
            for i in 0..8 {
                ids.push(put(&write_one, &format!("one-{i}")).await);
            }
            ids
        },
        async {
            let mut ids = Vec::new();
            for i in 0..8 {
                ids.push(put(&write_two, &format!("two-{i}")).await);
            }
            ids
        }
    );
    let final_snapshot = owner.snapshot().await.unwrap();
    for (rx, previous, db) in [
        (&mut a, &mut previous_a, &read_one),
        (&mut b, &mut previous_b, &read_two),
    ] {
        loop {
            let latest = next(rx).await;
            let closure = db.ids_added(&Snapshot::EMPTY, &latest).await.unwrap();
            assert!(
                previous.tips().iter().all(|tip| closure.contains(tip)),
                "newer frontier preceded an older notification"
            );
            *previous = latest.clone();
            if latest == final_snapshot {
                assert!(ids_a.iter().chain(&ids_b).all(|id| closure.contains(id)));
                break;
            }
        }
    }
}

async fn raw_call(
    reader: &mut tokio::io::ReadHalf<tokio::net::UnixStream>,
    writer: &mut tokio::io::WriteHalf<tokio::net::UnixStream>,
    request: ServiceRequest,
) -> eidetica::service::protocol::ServiceResponse {
    use eidetica::service::protocol::{ServerFrame, read_frame, write_frame};
    write_frame(writer, &request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerFrame::Response(response) = read_frame(reader).await.unwrap().unwrap() {
                return *response;
            }
        }
    })
    .await
    .unwrap()
}
