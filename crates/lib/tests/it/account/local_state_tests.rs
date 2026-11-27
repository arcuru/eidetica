use eidetica::{Database, crdt::Doc, store::Table};

use super::helpers::{login_account, setup_instance_with_user};

#[tokio::test]
async fn account_local_keys_and_activation_never_write_account_entries() {
    let (instance, name) = setup_instance_with_user("local-state", "password").await;
    let mut user = login_account(&instance, &name, "password").await;
    let root_key = user.get_default_key().unwrap();
    let account = Database::open(&instance, user.account().id())
        .await
        .unwrap()
        .with_key(user.get_signing_key(&root_key).unwrap());
    let initial = account.snapshot().await.unwrap();
    let key = user
        .add_private_key(Some("installation-only"))
        .await
        .unwrap();
    let database = user.create_database(Doc::new(), &key).await.unwrap();
    database.share().await.unwrap();
    assert!(database.is_shared().await.unwrap());
    user.set_reference("app", "root", database.root_id())
        .await
        .unwrap();
    let reference_snapshot = account.snapshot().await.unwrap();
    assert_ne!(
        initial, reference_snapshot,
        "portable references still replicate"
    );
    database.stop_sharing().await.unwrap();
    assert!(!database.is_shared().await.unwrap());
    assert_eq!(
        reference_snapshot,
        account.snapshot().await.unwrap(),
        "local choices must not author Account entries"
    );
    // Inspect full history, not only current fields: no keys/unlock or local
    // preference store was ever authored, including during first login.
    assert!(
        user.backend()
            .store_at(
                account.root_id(),
                "keys",
                &account.snapshot().await.unwrap()
            )
            .await
            .unwrap()
            .is_empty(),
        "private-key records replicated in Account history"
    );
    assert!(
        account
            .get_store_viewer::<Table<eidetica::account::TrackedDatabase>>("databases")
            .await
            .unwrap()
            .search(|_| true)
            .await
            .unwrap()
            .is_empty()
    );
    let id = database.root_id().clone();
    let actual = user.get_signing_key(&key).unwrap().public_key();
    user.logout().unwrap();
    let user = login_account(&instance, &name, "password").await;
    assert_eq!(user.get_signing_key(&key).unwrap().public_key(), actual);
    assert!(!user.database(&id).await.unwrap().sync_settings.sync_enabled);
    assert_eq!(user.reference("app", "root").await.unwrap(), Some(id));
}

#[tokio::test]
async fn account_login_directory_has_no_replicated_unlock_history() {
    use eidetica::{Instance, NewAccount, backend::database::InMemory};
    let (instance, admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::with_password("admin", "personal-password"),
    )
    .await
    .unwrap();
    admin
        .admin()
        .await
        .unwrap()
        .create_account(NewAccount::with_password("app", "app-password"))
        .await
        .unwrap();
    let app = instance.login_account("app", "app-password").await.unwrap();
    assert_ne!(
        app.get_default_key().unwrap(),
        admin.get_default_key().unwrap()
    );
    assert!(
        instance
            .login_account("app", "personal-password")
            .await
            .is_err()
    );
    let backend = instance.backend().local_engine().unwrap();
    for entry in backend
        .get_tree(
            &backend
                .get_instance_metadata()
                .await
                .unwrap()
                .unwrap()
                .users_db,
        )
        .await
        .unwrap()
    {
        assert!(
            !entry.in_subtree("users"),
            "login credentials were authored into immutable history"
        );
        assert!(
            !entry.in_subtree("last_login"),
            "local access log was replicated"
        );
    }
    assert!(
        backend
            .local_account_names()
            .await
            .unwrap()
            .iter()
            .any(|name| name == "login:app")
    );
}

// The observer corrupts only fixture-local bookkeeping, not Auth or genesis.
// Repair uses the returned ID; the observer's independently found ID is only an assertion.
async fn creation_failure_retains_committed_root(
    owner: &eidetica::Instance,
    client: &eidetica::Instance,
    user: &mut eidetica::AccountSession,
) {
    use eidetica::{Error, auth::crypto::verify_entry_signature};
    use std::error::Error as _;

    let key = user
        .add_private_key(Some("partial-creation"))
        .await
        .unwrap();
    let engine = owner.backend().local_engine().unwrap();
    let record = format!("account-state:{}", user.login_id());
    let original = engine.local_account_record(&record).await.unwrap().unwrap();
    let before = engine.all_roots().await.unwrap();
    let error = user
        .create_database_with_init(Doc::new(), &key, async |_| {
            assert!(
                engine
                    .compare_exchange_local_account_record(&record, Some(&original), b"{")
                    .await?
            );
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(
        engine
            .compare_exchange_local_account_record(&record, Some(b"{"), &original)
            .await
            .unwrap()
    );

    let id = error
        .created_database_id()
        .expect("committed repair ID")
        .clone();
    let created: Vec<_> = engine
        .all_roots()
        .await
        .unwrap()
        .into_iter()
        .filter(|root| !before.contains(root))
        .collect();
    assert_eq!(created, vec![id.clone()]);
    let Error::DatabaseCreation { source, .. } = &error else {
        panic!("missing creation context: {error:?}");
    };
    assert!(
        source
            .to_string()
            .contains("Invalid installation-local account state")
    );
    assert_eq!(error.source().unwrap().to_string(), source.to_string());
    assert!(error.to_string().contains(&id.to_string()));
    assert!(user.tracked_database(&id).await.unwrap().is_none());
    let genesis = owner.backend().get(&id).await.unwrap();
    verify_entry_signature(&genesis, &key).unwrap();
    assert!(verify_entry_signature(&genesis, &user.get_default_key().unwrap()).is_err());

    // The reported ID grants neither tracking nor a key mapping. Repair is explicit,
    // using the already-held key through the ordinary signer/permission path.
    assert!(user.open_database(&id).await.is_err());
    let reopened = user
        .acting_as(&key, None)
        .unwrap()
        .open_database(client, &id)
        .await
        .unwrap();
    assert_eq!(reopened.root_id(), &id);
    reopened
        .new_transaction()
        .await
        .unwrap()
        .commit()
        .await
        .unwrap();
    assert_eq!(engine.all_roots().await.unwrap().len(), before.len() + 1);
}

#[tokio::test]
async fn embedded_creation_failure_retains_committed_root() {
    let (owner, name) =
        super::helpers::setup_local_instance_with_user("partial-creation", "").await;
    let mut user = login_account(&owner, &name, "").await;
    creation_failure_retains_committed_root(&owner, &owner, &mut user).await;
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn service_creation_failure_retains_committed_root() {
    use eidetica::{Instance, NewAccount, backend::database::InMemory, service::ServiceServer};
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("creation.sock");
    let (owner, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::passwordless("partial-creation"),
    )
    .await
    .unwrap();
    let server = ServiceServer::bind(owner.clone(), &socket).await.unwrap();
    let (shutdown, receiver) = tokio::sync::watch::channel(());
    let task = tokio::spawn(server.run(receiver));
    let client = Instance::connect(format!("unix://{}", socket.display()))
        .await
        .unwrap();
    let mut user = client.login_account("partial-creation", "").await.unwrap();
    creation_failure_retains_committed_root(&owner, &client, &mut user).await;
    shutdown.send(()).unwrap();
    task.await.unwrap().unwrap();
}
