//! Real-socket acceptance tests for daemon-local advisory leases.

use super::*;
use crate::lease::granted;
use eidetica::auth::types::AuthKey;
use eidetica::service::protocol::{AuthenticatedDbRequest, DatabaseOp, Notification};
use eidetica::{
    Database, ID,
    lease::{LeaseAcquire, LeaseAcquireResponse, MAX_LEASE_NAME_BYTES},
};
use tokio::time::timeout;
use uuid::Uuid;

async fn admin_db(server: &Instance) -> Database {
    let mut admin = server.login_user("admin", None).await.unwrap();
    let key = admin.get_default_key().unwrap();
    admin.create_database(Doc::new(), &key).await.unwrap()
}

struct RawClient {
    reader: ReadHalf<UnixStream>,
    writer: WriteHalf<UnixStream>,
    identity: SigKey,
}

impl RawClient {
    async fn login(path: &PathBuf, server: &Instance, name: &str) -> Self {
        let (mut reader, mut writer) = raw_handshake(path).await;
        let user = server.login_user(name, None).await.unwrap();
        let key = user.get_default_key().unwrap();
        let signing_key = user.get_signing_key(&key).unwrap();
        write_frame(
            &mut writer,
            &ServiceRequest::TrustedLoginUser {
                username: name.into(),
            },
        )
        .await
        .unwrap();
        let ServiceResponse::TrustedLoginChallenge { challenge, .. } =
            read_response(&mut reader).await
        else {
            panic!("missing challenge");
        };
        write_frame(
            &mut writer,
            &ServiceRequest::TrustedLoginProve {
                signature: create_challenge_response(&challenge, &signing_key),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_response(&mut reader).await,
            ServiceResponse::TrustedLoginOk
        ));
        Self {
            reader,
            writer,
            identity: SigKey::from_pubkey(&key),
        }
    }

    async fn send(&mut self, root: &ID, identity: SigKey, op: DatabaseOp) {
        write_frame(
            &mut self.writer,
            &ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
                root_id: root.clone(),
                identity,
                op,
            })),
        )
        .await
        .unwrap();
    }

    async fn op(&mut self, root: &ID, op: DatabaseOp) -> ServiceResponse {
        self.send(root, self.identity.clone(), op).await;
        timeout(Duration::from_secs(3), read_response(&mut self.reader))
            .await
            .unwrap()
    }

    async fn acquire(&mut self, root: &ID) -> Uuid {
        match self
            .op(
                root,
                DatabaseOp::TryAcquireLease {
                    name: "runtime".into(),
                },
            )
            .await
        {
            ServiceResponse::LeaseAcquired(LeaseAcquireResponse::Granted { grant_id }) => grant_id,
            other => panic!("expected raw grant, got {other:?}"),
        }
    }
}

fn denied(response: ServiceResponse) -> (String, String) {
    match response {
        ServiceResponse::Error(error) => (error.module, error.kind),
        other => panic!("expected error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_lease_connected_race_and_independent_keys() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let local_db = admin_db(&server).await;
    let mut clients = Vec::new();
    let mut databases = Vec::new();
    for _ in 0..12 {
        let client = login_client(&path, "admin").await;
        databases.push(Database::open(&client, local_db.root_id()).await.unwrap());
        clients.push(client);
    }
    let leases: Vec<_> = databases.iter().map(|db| db.lease("runtime")).collect();
    let results =
        futures_util::future::join_all(leases.iter().map(|lease| lease.try_acquire())).await;
    let mut winner = None;
    for (i, result) in results.into_iter().enumerate() {
        match result.unwrap() {
            LeaseAcquire::Granted(guard) => {
                assert!(winner.is_none());
                winner = Some((i, guard));
            }
            LeaseAcquire::Held(status) => assert!(status.holder.is_some()),
            LeaseAcquire::AlreadyHeld(_) => {
                panic!("different connections are different owners even for the same user")
            }
        }
    }
    let (index, guard) = winner.expect("one winner");
    assert!(matches!(
        leases[index].try_acquire().await.unwrap(),
        LeaseAcquire::AlreadyHeld(_)
    ));
    let loser = (index + 1) % leases.len();
    let _other_name = granted(databases[loser].lease("other").try_acquire().await.unwrap());
    let other_local = admin_db(&server).await;
    let other_db = Database::open(&clients[loser], other_local.root_id())
        .await
        .unwrap();
    let _other_db = granted(other_db.lease("runtime").try_acquire().await.unwrap());
    let before = local_db.snapshot().await.unwrap();
    // Holding a lease does not gate normal writes from another connection.
    let user = clients[loser].login_user("admin", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let db = databases[loser]
        .clone()
        .with_key(user.get_signing_key(&key).unwrap());
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("ordinary")
        .await
        .unwrap()
        .set("value", "still writable")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_ne!(before, local_db.snapshot().await.unwrap());
    guard.release().await.unwrap();
    let _next = timeout(Duration::from_secs(3), leases[loser].wait_acquire())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn test_lease_stale_and_foreign_grant_cannot_release_successor() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let db = admin_db(&server).await;
    let mut owner = RawClient::login(&path, &server, "admin").await;
    let mut foreign = RawClient::login(&path, &server, "admin").await;
    let old = owner.acquire(db.root_id()).await;
    denied(
        foreign
            .op(
                db.root_id(),
                DatabaseOp::ReleaseLease {
                    name: "runtime".into(),
                    grant_id: old,
                },
            )
            .await,
    );
    assert!(matches!(
        owner
            .op(
                db.root_id(),
                DatabaseOp::ReleaseLease {
                    name: "runtime".into(),
                    grant_id: old
                }
            )
            .await,
        ServiceResponse::Ok
    ));
    let successor = owner.acquire(db.root_id()).await;
    assert_ne!(old, successor);
    denied(
        owner
            .op(
                db.root_id(),
                DatabaseOp::ReleaseLease {
                    name: "runtime".into(),
                    grant_id: old,
                },
            )
            .await,
    );
    denied(
        foreign
            .op(
                db.root_id(),
                DatabaseOp::ReleaseLease {
                    name: "runtime".into(),
                    grant_id: successor,
                },
            )
            .await,
    );
    assert!(matches!(
        foreign
            .op(
                db.root_id(),
                DatabaseOp::TryAcquireLease {
                    name: "runtime".into()
                }
            )
            .await,
        ServiceResponse::LeaseAcquired(LeaseAcquireResponse::Held(_))
    ));
    assert!(matches!(
        owner
            .op(
                db.root_id(),
                DatabaseOp::ReleaseLease {
                    name: "runtime".into(),
                    grant_id: successor
                }
            )
            .await,
        ServiceResponse::Ok
    ));
    foreign.acquire(db.root_id()).await;
    denied(
        owner
            .op(
                db.root_id(),
                DatabaseOp::ReleaseLease {
                    name: "runtime".into(),
                    grant_id: successor,
                },
            )
            .await,
    );
}

#[tokio::test]
async fn test_lease_watch_before_response_and_unwatch() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let db = admin_db(&server).await;
    let mut watcher = RawClient::login(&path, &server, "admin").await;
    let mut owner = RawClient::login(&path, &server, "admin").await;
    let id = Uuid::new_v4();
    watcher
        .send(
            db.root_id(),
            watcher.identity.clone(),
            DatabaseOp::WatchLease {
                name: "runtime".into(),
                watch_id: id,
            },
        )
        .await;
    let frame: ServerFrame = read_frame(&mut watcher.reader).await.unwrap().unwrap();
    assert!(
        matches!(frame, ServerFrame::Notification(Notification::LeaseChanged { watch_id, status, .. }) if watch_id == id && status.holder.is_none())
    );
    assert!(matches!(
        read_response(&mut watcher.reader).await,
        ServiceResponse::LeaseStatus(_)
    ));
    owner.acquire(db.root_id()).await;
    let frame: ServerFrame = read_frame(&mut watcher.reader).await.unwrap().unwrap();
    assert!(
        matches!(frame, ServerFrame::Notification(Notification::LeaseChanged { status, .. }) if status.generation == 1 && status.holder.is_some())
    );
    assert!(matches!(
        watcher
            .op(
                db.root_id(),
                DatabaseOp::UnwatchLease {
                    name: "runtime".into(),
                    watch_id: id
                }
            )
            .await,
        ServiceResponse::Ok
    ));
    drop(owner);
    // Status after disconnect may race cleanup; either response must be a
    // response, never a notification to the now-unregistered raw watch.
    assert!(matches!(
        watcher
            .op(
                db.root_id(),
                DatabaseOp::LeaseStatus {
                    name: "runtime".into()
                }
            )
            .await,
        ServiceResponse::LeaseStatus(_)
    ));
}

#[tokio::test]
async fn test_lease_disconnect_and_guard_drop_release() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let local = admin_db(&server).await;
    let first = login_client(&path, "admin").await;
    let second = login_client(&path, "admin").await;
    let first_db = Database::open(&first, local.root_id()).await.unwrap();
    let second_db = Database::open(&second, local.root_id()).await.unwrap();
    let guard = granted(first_db.lease("runtime").try_acquire().await.unwrap());
    let lease = second_db.lease("runtime");
    let mut observer = lease.watch().await.unwrap();
    assert!(observer.status().holder.is_some());
    // SDK guards/watches do not keep the connection alive by themselves.
    drop(first_db);
    drop(first);
    timeout(Duration::from_secs(3), observer.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(guard.is_lost());
    let next = timeout(Duration::from_secs(3), lease.wait_acquire())
        .await
        .unwrap()
        .unwrap();
    drop(next); // best-effort SDK drop release, witnessed by the next client
    let mut raw = RawClient::login(&path, &server, "admin").await;
    let client = login_client(&path, "admin").await;
    let db = Database::open(&client, local.root_id()).await.unwrap();
    let next = timeout(Duration::from_secs(3), db.lease("runtime").wait_acquire())
        .await
        .unwrap()
        .unwrap();
    next.release().await.unwrap();
    raw.acquire(local.root_id()).await;
    // Abrupt raw socket disappearance releases through ConnectionGuard too.
    drop(raw);
    let _after_abrupt = timeout(Duration::from_secs(3), lease.wait_acquire())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn test_lease_wait_acquire_release_race_does_not_block_dispatch() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let local = admin_db(&server).await;
    let first = login_client(&path, "admin").await;
    let second = login_client(&path, "admin").await;
    let db1 = Database::open(&first, local.root_id()).await.unwrap();
    let db2 = Database::open(&second, local.root_id()).await.unwrap();
    for _ in 0..16 {
        let holder = granted(db1.lease("runtime").try_acquire().await.unwrap());
        let lease = db2.lease("runtime");
        let waiting = tokio::spawn(async move { lease.wait_acquire().await });
        // Unrelated RPC on the same waiting connection must still complete.
        timeout(Duration::from_secs(3), db2.snapshot())
            .await
            .unwrap()
            .unwrap();
        holder.release().await.unwrap();
        let winner = timeout(Duration::from_secs(3), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        winner.release().await.unwrap();
    }
}

#[tokio::test]
async fn test_lease_permissions_unknown_roots_and_name_limit() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let db = admin_db(&server).await;
    create_user_via_admin(&server, "reader").await;
    create_user_via_admin(&server, "outsider").await;
    let reader = server.login_user("reader", None).await.unwrap();
    let reader_key = reader.get_default_key().unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(&reader_key, AuthKey::active(None, Permission::Read))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let client = login_client(&path, "reader").await;
    let reader_db = Database::open(&client, db.root_id()).await.unwrap();
    reader_db.lease("runtime").status().await.unwrap();
    let _watch = reader_db.lease("runtime").watch().await.unwrap();
    assert!(reader_db.lease("runtime").try_acquire().await.is_err());
    let (r, w) = raw_handshake(&path).await;
    let mut unauth = RawClient {
        reader: r,
        writer: w,
        identity: SigKey::default(),
    };
    denied(
        unauth
            .op(
                db.root_id(),
                DatabaseOp::TryAcquireLease {
                    name: "runtime".into(),
                },
            )
            .await,
    );
    let mut outsider = RawClient::login(&path, &server, "outsider").await;
    let missing = ID::from_bytes("missing-root");
    for root in [db.root_id(), &missing] {
        for op in [
            DatabaseOp::LeaseStatus {
                name: "runtime".into(),
            },
            DatabaseOp::WatchLease {
                name: "runtime".into(),
                watch_id: Uuid::new_v4(),
            },
            DatabaseOp::TryAcquireLease {
                name: "runtime".into(),
            },
        ] {
            assert_eq!(
                denied(outsider.op(root, op).await),
                ("auth".into(), "PermissionDenied".into())
            );
        }
    }
    let mut admin = RawClient::login(&path, &server, "admin").await;
    denied(
        admin
            .op(
                &missing,
                DatabaseOp::TryAcquireLease {
                    name: "runtime".into(),
                },
            )
            .await,
    );
    denied(
        admin
            .op(
                &missing,
                DatabaseOp::WatchLease {
                    name: "runtime".into(),
                    watch_id: Uuid::new_v4(),
                },
            )
            .await,
    );
    denied(
        admin
            .op(
                &missing,
                DatabaseOp::LeaseStatus {
                    name: "runtime".into(),
                },
            )
            .await,
    );
    // A database-authorized identity not proven on this connection is refused.
    admin
        .send(
            db.root_id(),
            SigKey::from_pubkey(&reader_key),
            DatabaseOp::LeaseStatus {
                name: "runtime".into(),
            },
        )
        .await;
    denied(read_response(&mut admin.reader).await);
    denied(
        admin
            .op(
                db.root_id(),
                DatabaseOp::TryAcquireLease {
                    name: "x".repeat(MAX_LEASE_NAME_BYTES + 1),
                },
            )
            .await,
    );
    assert!(matches!(
        admin
            .op(
                db.root_id(),
                DatabaseOp::TryAcquireLease {
                    name: "x".repeat(MAX_LEASE_NAME_BYTES)
                }
            )
            .await,
        ServiceResponse::LeaseAcquired(LeaseAcquireResponse::Granted { .. })
    ));
}

#[tokio::test]
async fn test_lease_release_and_unwatch_after_permission_revocation() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let db = admin_db(&server).await;
    create_user_via_admin(&server, "writer").await;
    let writer = server.login_user("writer", None).await.unwrap();
    let key = writer.get_default_key().unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(&key, AuthKey::active(None, Permission::Write(10)))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let client = login_client(&path, "writer").await;
    let writer_db = Database::open(&client, db.root_id()).await.unwrap();
    let guard = granted(writer_db.lease("runtime").try_acquire().await.unwrap());
    let watcher = writer_db.lease("runtime").watch().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .revoke_auth_key(&key)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(writer_db.lease("runtime").status().await.is_err());
    assert!(!guard.is_lost()); // permission changes do not revoke advisory work
    drop(watcher);
    guard.release().await.unwrap();
    assert_eq!(db.lease("runtime").status().await.unwrap().holder, None);
}

#[tokio::test]
async fn test_lease_owner_can_release_after_connection_relogin() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let local = admin_db(&server).await;
    create_user_via_admin(&server, "other").await;
    let client = login_client(&path, "admin").await;
    let db = Database::open(&client, local.root_id()).await.unwrap();
    let guard = granted(db.lease("runtime").try_acquire().await.unwrap());
    client.login_user("other", None).await.unwrap();
    assert!(!guard.is_lost()); // ownership belongs to the connection, not the login
    guard.release().await.unwrap();
    assert_eq!(local.lease("runtime").status().await.unwrap().holder, None);
}

#[tokio::test]
async fn test_lease_per_database_identity_and_shutdown_loss() {
    let (path, shutdown, _server, _dir) = start_test_server().await;
    let client = login_client(&path, "admin").await;
    let (key, _) = generate_keypair();
    let pubkey = key.public_key();
    let db = Database::create(&client, key, Doc::new()).await.unwrap();
    // Login identity is not a member of this database. The bound backend's
    // registered per-database identity must drive all lease operations.
    let keyless = Database::open_remote(
        &client,
        remote_conn(&client),
        db.root_id(),
        SigKey::from_pubkey(&pubkey),
    )
    .await
    .unwrap();
    assert!(keyless.auth_identity().is_none());
    let mut guard = granted(keyless.lease("runtime").try_acquire().await.unwrap());
    let mut watch = keyless.lease("runtime").watch().await.unwrap();
    for _ in 0..8 {
        assert_eq!(
            keyless.lease("runtime").status().await.unwrap().generation,
            1
        );
        assert!(!guard.is_lost()); // no TTL or automatic eviction
    }
    drop(shutdown);
    timeout(Duration::from_secs(3), guard.lost()).await.unwrap();
    assert!(guard.is_lost());
    assert!(
        timeout(Duration::from_secs(3), watch.changed())
            .await
            .unwrap()
            .is_err()
    );
    assert!(keyless.lease("runtime").try_acquire().await.is_err());
    // Reconnect is explicit and cannot silently revive this guard.
}

#[tokio::test]
async fn test_lease_shutdown_reconnect_does_not_revive_old_guard() {
    let (path, shutdown, server, dir) = start_test_server().await;
    let local = admin_db(&server).await;
    let client = login_client(&path, "admin").await;
    let db = Database::open(&client, local.root_id()).await.unwrap();
    let lease = db.lease("runtime");
    let mut guard = granted(lease.try_acquire().await.unwrap());
    drop(shutdown);
    timeout(Duration::from_secs(3), guard.lost()).await.unwrap();
    assert!(guard.is_lost());
    assert_eq!(local.lease("runtime").status().await.unwrap().holder, None);
    let new_path = dir.path().join("reconnected.sock");
    let (stop, rx) = watch::channel(());
    let restarted = ServiceServer::bind(server, &new_path).await.unwrap();
    let running = tokio::spawn(restarted.run(rx));
    let reconnected = login_client(&new_path, "admin").await;
    let new_db = Database::open(&reconnected, local.root_id()).await.unwrap();
    let _new_guard = granted(new_db.lease("runtime").try_acquire().await.unwrap());
    assert!(guard.is_lost());
    assert!(lease.try_acquire().await.is_err());
    drop(stop);
    running.await.unwrap().unwrap();
}
