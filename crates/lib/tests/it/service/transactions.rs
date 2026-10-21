//! Transaction inputs stay pinned to one daemon snapshot; keys stay on the client.
use super::*;
use eidetica::{Database, Error, HeightStrategy, Snapshot, Store, entry::ID, store::StoreError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::{net::UnixListener, task::JoinSet};

#[derive(Clone, Copy, PartialEq)]
enum Peer {
    Current,
    LegacyContext,
    Unsupported,
}

/// A framed relay lets tests observe the production path and exercise v0
/// replies without replacing either the client or the daemon with a mock.
async fn relay(
    daemon: PathBuf,
    path: PathBuf,
    peer: Peer,
    ops: Arc<Mutex<Vec<String>>>,
    tasks: &mut JoinSet<()>,
) {
    let listener = UnixListener::bind(path).unwrap();
    tasks.spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut server = UnixStream::connect(daemon).await.unwrap();
        while let Some(request) = read_frame::<_, Value>(&mut client).await.unwrap() {
            let op = request.get("AuthenticatedDb").and_then(|db| db.get("op"));
            let name = op.and_then(|op| {
                op.as_str()
                    .or_else(|| op.as_object()?.keys().next().map(String::as_str))
            });
            if let Some(name) = name {
                ops.lock().unwrap().push(name.to_string());
            }
            let is_begin = name == Some("BeginTransaction");
            let response = if is_begin && peer == Peer::Unsupported {
                json!({"Response": "Unsupported"})
            } else {
                write_frame(&mut server, &request).await.unwrap();
                let mut response: Value = read_frame(&mut server).await.unwrap().unwrap();
                if is_begin && peer == Peer::LegacyContext {
                    let context = &mut response["Response"]["TransactionContext"];
                    context.as_object_mut().unwrap().remove("index_value");
                    // The old server returned only explicitly requested stores.
                    context["subtree_parents"]
                        .as_object_mut()
                        .unwrap()
                        .retain(|name, _| name == "_index");
                }
                response
            };
            write_frame(&mut client, &response).await.unwrap();
        }
    });
}

#[tokio::test]
async fn test_remote_transaction_context_build_and_v0_fallback() {
    for peer in [Peer::Current, Peer::LegacyContext, Peer::Unsupported] {
        let (socket, _shutdown, server, dir) = start_test_server().await;
        let mut owner = server.login_account("admin", None).await.unwrap();
        let pubkey = owner.get_default_key().unwrap();
        let key = owner.get_signing_key(&pubkey).unwrap();
        let db = owner.create_database(Doc::new(), &pubkey).await.unwrap();
        let tx = db.new_transaction().await.unwrap();
        let data = tx.get_store::<DocStore>("data").await.unwrap();
        data.set("key", "old").await.unwrap();
        data.set_height_strategy(Some(HeightStrategy::Incremental))
            .await
            .unwrap();
        let parent = tx.commit().await.unwrap();
        let parent_entry = server.backend().get(&parent).await.unwrap();

        let proxy = dir.path().join("relay.sock");
        let ops = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = JoinSet::new();
        relay(socket, proxy.clone(), peer, ops.clone(), &mut tasks).await;
        let client = Instance::connect(format!("unix://{}", proxy.display()))
            .await
            .unwrap();
        let _session = client.login_account("admin", None).await.unwrap();
        let remote = Database::open_remote(
            &client,
            remote_conn(&client),
            db.root_id(),
            SigKey::from_pubkey(&pubkey),
        )
        .await
        .unwrap()
        .with_key(key);
        ops.lock().unwrap().clear();
        let tx = remote.new_transaction().await.unwrap();
        let data = tx.get_store::<DocStore>("data").await.unwrap();
        data.set("key", "new").await.unwrap();
        let id = tx.commit().await.unwrap();
        let requests = ops.lock().unwrap().clone();
        assert_eq!(requests.first().unwrap(), "BeginTransaction");
        if peer == Peer::Current {
            assert_eq!(
                requests,
                ["BeginTransaction", "SubmitSignedEntry"],
                "existing-store build/sign must consume the bundled inputs"
            );
        } else {
            assert!(requests.iter().any(|op| op == "GetStoreTipsUpToEntries"));
        }
        let entry = server.backend().get(&id).await.unwrap();
        assert_eq!(entry.parents().unwrap(), vec![parent.clone()]);
        assert_eq!(entry.subtree_parents("data").unwrap(), vec![parent.clone()]);
        assert_eq!(entry.height(), parent_entry.height() + 1);
        assert_eq!(
            entry.subtree_height("data").unwrap(),
            parent_entry.subtree_height("data").unwrap() + 1
        );
        assert_eq!(
            server.backend().get_verification_status(&id).await.unwrap(),
            VerificationStatus::Verified
        );
        assert_eq!(
            db.new_transaction()
                .await
                .unwrap()
                .get_store::<DocStore>("data")
                .await
                .unwrap()
                .get_string("key")
                .await
                .unwrap(),
            "new"
        );
        if peer == Peer::Current {
            // A completed transaction must not become a cross-request auth cache.
            let revoke = db.new_transaction().await.unwrap();
            revoke
                .get_settings()
                .unwrap()
                .revoke_auth_key(&pubkey)
                .await
                .unwrap();
            revoke.commit().await.unwrap();
            ops.lock().unwrap().clear();
            let error = match remote.new_transaction().await {
                Err(error) => error,
                Ok(_) => panic!("revoked key must not reuse a previous context"),
            };
            assert!(
                error.to_string().contains("[auth::PermissionDenied]"),
                "{error}"
            );
            assert_eq!(*ops.lock().unwrap(), ["BeginTransaction"]);
        }
        drop(client);
        tasks.abort_all();
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                assert!(error.is_cancelled(), "relay failed: {error}");
            }
        }
    }
}

#[tokio::test]
async fn test_remote_transaction_context_boundary_and_staged_metadata() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (client, root, identity) = setup_db(&server, &socket, "alice").await;
    let owner = server.login_account("alice", None).await.unwrap();
    let key = owner
        .get_signing_key(&owner.get_default_key().unwrap())
        .unwrap();
    let db = Database::open(&server, &root)
        .await
        .unwrap()
        .with_key(key.clone());
    let seed = db.new_transaction().await.unwrap();
    seed.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("before", "yes")
        .await
        .unwrap();
    let parent = seed.commit().await.unwrap();
    let remote = Database::open_remote(&client, remote_conn(&client), &root, identity)
        .await
        .unwrap()
        .with_key(key);
    let tx = remote.new_transaction().await.unwrap();
    let pin = db
        .backend()
        .unwrap()
        .store_snapshot_at(&root, "_settings", &Snapshot::from(vec![parent.clone()]))
        .await
        .unwrap();

    // A different writer advances both the registry and settings after Begin.
    let concurrent = db.new_transaction().await.unwrap();
    concurrent
        .get_settings()
        .unwrap()
        .set_name("later")
        .await
        .unwrap();
    concurrent
        .get_store::<Table<String>>("later")
        .await
        .unwrap()
        .set("row", "other".to_string())
        .await
        .unwrap();
    concurrent
        .get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("after", "no")
        .await
        .unwrap();
    concurrent.commit().await.unwrap();

    let data = tx.get_store::<DocStore>("data").await.unwrap();
    let error = data.get_string("after").await.unwrap_err();
    assert!(matches!(error, Error::Store(error)
        if matches!(*error, StoreError::KeyNotFound { ref store, ref key }
            if store == "data" && key == "after")));
    data.set("own", "yes").await.unwrap();
    let new_store = tx.get_store::<DocStore>("later").await.unwrap();
    new_store.set("branch", "yes").await.unwrap();
    let error = match tx.get_store::<Table<String>>("later").await {
        Err(error) => error,
        Ok(_) => panic!("staged _index type must win"),
    };
    assert!(matches!(error, Error::Store(error)
        if matches!(*error, StoreError::TypeMismatch { ref store, .. }
            if store == "later")));
    let settings = tx.get_settings().unwrap();
    assert_eq!(settings.get_name().await.unwrap(), "alice_db");
    settings.set_name("staged").await.unwrap();
    assert_eq!(settings.get_name().await.unwrap(), "staged");
    data.set_height_strategy(Some(HeightStrategy::Incremental))
        .await
        .unwrap();
    let id = tx.commit().await.unwrap();
    let entry = server.backend().get(&id).await.unwrap();
    assert_eq!(entry.parents().unwrap(), vec![parent.clone()]);
    assert_eq!(entry.subtree_parents("data").unwrap(), vec![parent.clone()]);
    assert!(entry.subtree_parents("later").unwrap().is_empty());
    assert_eq!(entry.subtree_parents("_settings").unwrap(), pin.tips());
    let metadata: Value = serde_json::from_slice(entry.metadata().unwrap()).unwrap();
    assert_eq!(metadata["settings_tips"], json!(pin));
    assert_eq!(
        entry.subtree_height("data").unwrap(),
        server
            .backend()
            .get(&parent)
            .await
            .unwrap()
            .subtree_height("data")
            .unwrap()
            + 1
    );
    assert_eq!(
        server.backend().get_verification_status(&id).await.unwrap(),
        VerificationStatus::Verified
    );
    // Explicit historical transactions must not silently switch to current tips.
    let historical = remote
        .new_transaction_at(&Snapshot::from(vec![parent.clone()]))
        .await
        .unwrap();
    historical
        .get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("historical", "yes")
        .await
        .unwrap();
    let id = historical.commit().await.unwrap();
    assert_eq!(
        server.backend().get(&id).await.unwrap().parents().unwrap(),
        vec![parent]
    );
}

#[tokio::test]
async fn test_remote_transaction_context_read_scope() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (client, root, identity) = setup_db(&server, &socket, "alice").await;
    let owner = server.login_account("alice", None).await.unwrap();
    let key = owner
        .get_signing_key(&owner.get_default_key().unwrap())
        .unwrap();
    let db = Database::open(&server, &root)
        .await
        .unwrap()
        .with_key(key.clone());
    let delegated = Database::create(&server, PrivateKey::generate(), Doc::new())
        .await
        .unwrap();
    let delegated_root = delegated.root_id().clone();
    let missing = ID::from_bytes("missing delegated descendant");
    let grant = db.new_transaction().await.unwrap();
    grant
        .get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(0),
                min: None,
            },
            tree: TreeReference {
                root: delegated_root.clone(),
                tips: vec![delegated_root.clone()],
            },
        })
        .await
        .unwrap();
    let parent_id = grant.commit().await.unwrap();
    let parent = server.backend().get(&parent_id).await.unwrap();
    // Incomplete pinned auth leaves this signed tip Unverified even after
    // opportunistic verification. A failed sibling must never be a parent.
    let build = |label: &str| {
        let entry = Entry::builder(root.clone())
            .set_parents(vec![parent_id.clone()])
            .set_height(parent.height() + 1)
            .set_subtree_data(
                "data",
                serde_json::to_vec(&json!({label: "pending"})).unwrap(),
            )
            .set_metadata(
                serde_json::to_vec(&json!({
                    "settings_tips": [parent_id.clone()],
                    "entropy": null,
                }))
                .unwrap(),
            )
            .build()
            .unwrap()
            .with_auth(|auth| {
                auth.key = SigKey::Delegation {
                    path: vec![DelegationStep {
                        tree: delegated_root.clone(),
                        tips: vec![missing.clone()],
                    }],
                    hint: KeyHint::from_pubkey(&key.public_key()),
                }
            });
        let signature = sign_entry(&entry, &key).unwrap();
        entry.with_auth(|auth| auth.signature = Some(signature))
    };
    let pending = build("unverified");
    let pending_id = pending.id();
    server.backend().put(pending).await.unwrap();
    let failed = build("failed");
    let failed_id = failed.id();
    server.backend().put(failed).await.unwrap();
    server
        .backend()
        .local_engine()
        .unwrap()
        .update_verification_status(&failed_id, VerificationStatus::Failed)
        .await
        .unwrap();
    let remote = Database::open_remote(&client, remote_conn(&client), &root, identity)
        .await
        .unwrap()
        .with_key(key);
    // Open both before submitting either, so each sees the same raw DAG.
    let verified = remote.new_transaction().await.unwrap();
    let unverified = remote.allow_unverified().new_transaction().await.unwrap();
    verified
        .get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("verified", "yes")
        .await
        .unwrap();
    unverified
        .get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("unverified", "yes")
        .await
        .unwrap();
    let verified_id = verified.commit().await.unwrap();
    let unverified_id = unverified.commit().await.unwrap();
    let verified_entry = server.backend().get(&verified_id).await.unwrap();
    let unverified_entry = server.backend().get(&unverified_id).await.unwrap();
    assert_eq!(verified_entry.parents().unwrap(), vec![parent_id]);
    assert_eq!(unverified_entry.parents().unwrap(), vec![pending_id]);
    assert!(!unverified_entry.parents().unwrap().contains(&failed_id));
}
