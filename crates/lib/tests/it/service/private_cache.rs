use super::*;
use eidetica::backend::{BackendError, CacheScope, DerivedCachePolicy, StoreStateLifecycle};
use eidetica::{Database, ID, auth::crypto::PublicKey, auth::types::AuthKey};

async fn set_permission(db: &Database, key: &PublicKey, permission: Permission) {
    let txn = db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(key, AuthKey::active(None, permission))
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

async fn new_reader(server: &Instance, name: &str) -> PublicKey {
    create_user_via_admin(server, name).await;
    server
        .login_account(name, "")
        .await
        .unwrap()
        .get_default_key()
        .unwrap()
}

#[tokio::test]
async fn read_only_private_cache_cold_warm_and_refused_reads() {
    let (socket, _tx, server, _dir) = start_test_server().await;
    let mut owner = server.login_account("admin", "").await.unwrap();
    let key = owner.get_default_key().unwrap();
    let local = owner.create_database(Doc::new(), &key).await.unwrap();
    let reader = new_reader(&server, "reader").await;
    set_permission(&local, &reader, Permission::Read).await;
    let root = local.root_id();
    let identity = SigKey::from_pubkey(&reader);
    let txn = local.new_transaction().await.unwrap();
    let row = ServiceTodo {
        title: "from history".into(),
        done: false,
    };
    for store in ["scan", "point", "bypass"] {
        txn.get_store::<Table<ServiceTodo>>(store)
            .await
            .unwrap()
            .set("a", row.clone())
            .await
            .unwrap();
    }
    for store in ["opaque", "bypass_doc"] {
        txn.get_store::<DocStore>(store)
            .await
            .unwrap()
            .set("key", "from history")
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();

    let engine = server.backend().local_engine().unwrap();
    let memory = engine.as_any().downcast_ref::<InMemory>().unwrap();
    assert_eq!(memory.store_state_record_count(root, "scan"), 0);
    assert_eq!(memory.store_state_record_count(root, "point"), 0);
    for warm in [false, true] {
        // No SDK caches or view tokens are shared across these connections.
        let client = login_client(&socket, "reader").await;
        let db = Database::open_remote(&client, remote_conn(&client), root, identity.clone())
            .await
            .unwrap();
        let scan = db
            .get_store_viewer::<Table<ServiceTodo>>("scan")
            .await
            .unwrap();
        let point = db
            .get_store_viewer::<Table<ServiceTodo>>("point")
            .await
            .unwrap();
        let opaque = db.get_store_viewer::<DocStore>("opaque").await.unwrap();
        let reads_before = memory.store_history_read_count();
        assert_eq!(
            scan.search(|_| true).await.unwrap(),
            vec![("a".into(), row.clone())]
        );
        assert_eq!(point.get("a").await.unwrap(), row);
        assert_eq!(opaque.get_string("key").await.unwrap(), "from history");
        let reads_after = memory.store_history_read_count();
        if warm {
            assert_eq!(
                reads_after, reads_before,
                "fresh connection must reuse the materializations"
            );
        } else {
            assert!(
                reads_after > reads_before,
                "the cold reads must fold history"
            );
        }
        assert_eq!(memory.store_state_record_count(root, "scan"), 1);
        assert_eq!(memory.store_state_record_count(root, "point"), 1);
    }

    // A second connection sees the authenticated login's in-flight count.
    let holder = login_client(&socket, "reader").await;
    let holder_conn = remote_conn(&holder);
    let hold = derived_request(root, "hold");
    let mut tokens = Vec::new();
    for _ in 0..8 {
        tokens.push(
            holder_conn
                .begin_store_state_staging(identity.clone(), hold.clone())
                .await
                .unwrap(),
        );
    }
    let client = login_client(&socket, "reader").await;
    let conn = remote_conn(&client);
    assert!(
        conn.begin_store_state_staging(identity.clone(), hold)
            .await
            .unwrap_err()
            .is_store_state_not_retained()
    );
    let db = Database::open_remote(&client, conn, root, identity.clone())
        .await
        .unwrap();
    for disabled in [false, true] {
        if disabled {
            for token in tokens.drain(..) {
                holder_conn
                    .abort_store_state(root.clone(), identity.clone(), token)
                    .await
                    .unwrap();
            }
            memory.testing_set_derived_cache_policy(DerivedCachePolicy {
                max_bytes: 0,
                max_namespaces: 0,
            });
        }
        let before = memory.store_history_read_count();
        assert_eq!(
            db.get_store_viewer::<Table<ServiceTodo>>("bypass")
                .await
                .unwrap()
                .get("a")
                .await
                .unwrap(),
            row
        );
        assert_eq!(
            db.get_store_viewer::<DocStore>("bypass_doc")
                .await
                .unwrap()
                .get_string("key")
                .await
                .unwrap(),
            "from history"
        );
        assert!(memory.store_history_read_count() > before);
        assert_eq!(memory.store_state_record_count(root, "bypass"), 0);
    }

    let txn = local.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&reader)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let denied = db
        .get_store_viewer::<DocStore>("bypass_doc")
        .await
        .err()
        .expect("revocation is not a cache miss");
    assert_denied(denied);
}

fn assert_denied(error: eidetica::Error) {
    assert!(
        error.to_string().contains("[auth::PermissionDenied]"),
        "{error}"
    );
}

fn assert_invalid_token(error: eidetica::Error) {
    assert!(
        matches!(error, eidetica::Error::Backend(error) if matches!(*error, BackendError::InvalidStoreStateStagingToken))
    );
}

#[tokio::test]
async fn private_cache_staging_uses_live_read_and_exact_session_binding() {
    let (socket, _tx, server, _dir) = start_test_server().await;
    let mut owner = server.login_account("admin", "").await.unwrap();
    let key = owner.get_default_key().unwrap();
    let local = owner.create_database(Doc::new(), &key).await.unwrap();
    let other_db = owner.create_database(Doc::new(), &key).await.unwrap();
    let root = local.root_id();
    let request = derived_request(root, "scope-boundary");
    let reader = new_reader(&server, "reader").await;
    let identity = SigKey::from_pubkey(&reader);
    let client = login_client(&socket, "reader").await;
    let conn = remote_conn(&client);
    assert_denied(
        conn.begin_store_state_staging(identity.clone(), request.clone())
            .await
            .unwrap_err(),
    );
    let missing = ID::from_bytes("no-such-database");
    assert_denied(
        conn.begin_store_state_staging(identity.clone(), derived_request(&missing, "absent"))
            .await
            .unwrap_err(),
    );

    set_permission(&local, &reader, Permission::Read).await;
    set_permission(&other_db, &reader, Permission::Read).await;
    for bad in [
        StoreStateRequest {
            lifecycle: StoreStateLifecycle::Authoritative,
            ..request.clone()
        },
        StoreStateRequest {
            scope: CacheScope::Login("another-login".into()),
            ..request.clone()
        },
    ] {
        assert_invalid_token(
            conn.begin_store_state_staging(identity.clone(), bad)
                .await
                .unwrap_err(),
        );
    }
    let engine = server.backend().local_engine().unwrap();
    let shared = engine
        .begin_store_state_staging(request.clone())
        .await
        .unwrap();
    engine
        .stage_store_state_records(&shared, [(vec![0], Some(b"daemon".to_vec()))].into())
        .await
        .unwrap();
    let shared = engine.publish_store_state(shared).await.unwrap();

    let token = conn
        .begin_store_state_staging(identity.clone(), request.clone())
        .await
        .unwrap();
    assert_invalid_token(
        conn.publish_store_state(other_db.root_id().clone(), identity.clone(), token.clone())
            .await
            .unwrap_err(),
    );
    let other_key = new_reader(&server, "other_reader").await;
    set_permission(&local, &other_key, Permission::Read).await;
    let other_client = login_client(&socket, "other_reader").await;
    let other = remote_conn(&other_client);
    let other_identity = SigKey::from_pubkey(&other_key);
    assert_invalid_token(
        other
            .publish_store_state(root.clone(), other_identity.clone(), token.clone())
            .await
            .unwrap_err(),
    );
    let records = BTreeMap::from([(vec![0], Some(b"private".to_vec()))]);
    for _ in 0..2 {
        conn.stage_store_state_records(
            root.clone(),
            identity.clone(),
            token.clone(),
            0,
            records.clone(),
        )
        .await
        .unwrap();
    }
    assert_invalid_token(
        conn.stage_store_state_records(
            root.clone(),
            identity.clone(),
            token.clone(),
            0,
            [(vec![0], Some(b"different".to_vec()))].into(),
        )
        .await
        .unwrap_err(),
    );
    let view = conn
        .publish_store_state(root.clone(), identity.clone(), token)
        .await
        .unwrap();
    assert_eq!(
        conn.store_state_record_get(root.clone(), identity.clone(), view, vec![0])
            .await
            .unwrap(),
        Some(b"private".to_vec())
    );
    let view = other
        .resolve_store_state(other_identity.clone(), request.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        other
            .store_state_record_get(root.clone(), other_identity, view, vec![0])
            .await
            .unwrap(),
        Some(b"daemon".to_vec())
    );
    assert_eq!(
        engine.store_state_record_get(&shared, &[0]).await.unwrap(),
        Some(b"daemon".to_vec())
    );

    let user = client.login_account("reader", "").await.unwrap();
    let signer = user.get_signing_key(&reader).unwrap();
    let entry = signed_write(&conn, root, &identity, &signer).await;
    let id = entry.id();
    conn.submit_signed_entry(root.clone(), identity.clone(), entry)
        .await
        .expect_err("a Read signer is not a Write signer");
    assert!(!local.snapshot().await.unwrap().tips().contains(&id));

    // Same signature/build path with an actual Write grant must succeed.
    set_permission(&local, &reader, Permission::Write(7)).await;
    let entry = signed_write(&conn, root, &identity, &signer).await;
    let id = entry.id();
    conn.submit_signed_entry(root.clone(), identity.clone(), entry)
        .await
        .unwrap();
    assert!(local.snapshot().await.unwrap().tips().contains(&id));
    set_permission(&local, &reader, Permission::Read).await;

    let request = derived_request(root, "revoked-build");
    let token = conn
        .begin_store_state_staging(identity.clone(), request.clone())
        .await
        .unwrap();
    let txn = local.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&reader)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_denied(
        conn.stage_store_state_records(root.clone(), identity.clone(), token.clone(), 0, records)
            .await
            .unwrap_err(),
    );
    assert_denied(
        conn.publish_store_state(root.clone(), identity.clone(), token.clone())
            .await
            .unwrap_err(),
    );
    assert_denied(
        conn.abort_store_state(root.clone(), identity, token)
            .await
            .unwrap_err(),
    );
    let private = StoreStateRequest {
        scope: CacheScope::Login(user.login_id().into()),
        ..request
    };
    assert!(
        engine
            .resolve_store_state(&private)
            .await
            .unwrap()
            .is_none()
    );
}

async fn signed_write(
    conn: &eidetica::service::client::RemoteConnection,
    root: &ID,
    identity: &SigKey,
    signer: &PrivateKey,
) -> Entry {
    let ctx = conn
        .begin_transaction(
            root.clone(),
            identity.clone(),
            vec!["write".into()],
            ReadScope::Verified,
        )
        .await
        .unwrap();
    let entry = Entry::builder(root.clone())
        .set_parents(
            ctx.main_parents
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>(),
        )
        .set_subtree_data("write", b"{}")
        .set_height(ctx.main_parents.iter().map(|(_, h)| *h).max().unwrap_or(0) + 1)
        .set_metadata(
            serde_json::to_vec(
                &serde_json::json!({ "settings_tips": ctx.settings_tips, "entropy": null }),
            )
            .unwrap(),
        )
        .build()
        .unwrap()
        .with_auth(|auth| auth.key = identity.clone());
    let signature = sign_entry(&entry, signer).unwrap();
    entry.with_auth(|auth| auth.signature = Some(signature))
}

#[tokio::test]
async fn private_staging_reclaims_an_idle_connections_admission() {
    let (socket, _tx, server, _dir) =
        start_test_server_with_token_ttls(Duration::from_secs(1), Duration::from_secs(300)).await;
    let (holder, root, identity) = setup_db(&server, &socket, "alice").await;
    let other = login_client(&socket, "alice").await;
    let conn = remote_conn(&holder);
    let fresh = remote_conn(&other);
    let request = derived_request(&root, "expiry");
    for _ in 0..7 {
        conn.begin_store_state_staging(identity.clone(), request.clone())
            .await
            .unwrap();
    }
    let (a, b) = tokio::join!(
        conn.begin_store_state_staging(identity.clone(), request.clone()),
        fresh.begin_store_state_staging(identity.clone(), request.clone()),
    );
    let error = match (a, b) {
        (Ok(_), Err(error)) | (Err(error), Ok(_)) => error,
        other => panic!("only one connection can take the last slot: {other:?}"),
    };
    assert!(error.is_store_state_not_retained(), "{error}");
    assert!(
        fresh
            .begin_store_state_staging(identity.clone(), request.clone())
            .await
            .unwrap_err()
            .is_store_state_not_retained()
    );
    // Do not send a request on (or close) the idle connection to free its builds.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match fresh
                .begin_store_state_staging(identity.clone(), request.clone())
                .await
            {
                Ok(_) => break,
                Err(error) if error.is_store_state_not_retained() => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .expect("idle connection must release storage admission");
}
