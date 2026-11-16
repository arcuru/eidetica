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

/// Two live sessions must discover and open mappings created after login. The
/// service case uses separate connections, never reauthenticates the observer.
#[tokio::test]
async fn two_live_sessions_observe_native_mapping_changes() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        use eidetica::{
            Instance, NewAccount,
            auth::{
                AuthKey, Permission, SigKey,
                crypto::{generate_keypair, verify_entry_signature},
            },
            store::DocStore,
        };
        let (server, mut setup) = Instance::create_backend(
            crate::helpers::test_backend().await,
            NewAccount::passwordless("mapping-owner"),
        )
        .await
        .unwrap();
        let root_key = setup.get_default_key().unwrap();
        let extra = setup
            .add_private_key(Some("database signer"))
            .await
            .unwrap();
        #[cfg(not(all(unix, feature = "service")))]
        let (creator_instance, observer_instance) = (server.clone(), server.clone());
        #[cfg(all(unix, feature = "service"))]
        let (creator_instance, observer_instance, shutdown, server_task, _directory) = {
            let mut creator_instance = server.clone();
            let mut observer_instance = server.clone();
            use std::os::unix::fs::PermissionsExt;
            let directory = tempfile::tempdir().unwrap();
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let (shutdown, rx) = tokio::sync::watch::channel(());
            let task = if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
                let path = directory.path().join("mappings.sock");
                let service = eidetica::service::ServiceServer::bind(server.clone(), &path)
                    .await
                    .unwrap();
                let task = tokio::spawn(service.run(rx));
                creator_instance = Instance::connect(format!("unix://{}", path.display()))
                    .await
                    .unwrap();
                observer_instance = Instance::connect(format!("unix://{}", path.display()))
                    .await
                    .unwrap();
                Some(task)
            } else {
                None
            };
            (
                creator_instance,
                observer_instance,
                shutdown,
                task,
                directory,
            )
        };
        let mut creator = creator_instance
            .login_account("mapping-owner", "")
            .await
            .unwrap();
        let observer = observer_instance
            .login_account("mapping-owner", "")
            .await
            .unwrap();
        let before = observer.account().database().snapshot().await.unwrap();
        let target = creator.create_database(Doc::new(), &extra).await.unwrap();
        // Mirrors the consumer's catalog-then-open sequence. The native read
        // response is also a notification barrier, not a sleep/polling loop.
        assert!(
            observer
                .databases()
                .await
                .unwrap()
                .iter()
                .any(|row| row.database_id == *target.root_id())
        );
        assert_eq!(
            observer.find_key(target.root_id()).unwrap(),
            Some(extra.clone())
        );
        let retained = observer.open_database(target.root_id()).await.unwrap();
        let tx = retained.new_transaction().await.unwrap();
        tx.get_store::<DocStore>("data")
            .await
            .unwrap()
            .set("message", "retained session")
            .await
            .unwrap();
        let entry = tx.commit().await.unwrap();
        verify_entry_signature(&retained.get_entry(entry).await.unwrap(), &extra).unwrap();
        assert_eq!(
            observer.account().database().snapshot().await.unwrap(),
            before
        );

        // An explicit invalid identity must replace, not fall back to, the
        // formerly working direct-key mapping.
        let wrong = SigKey::from_name("not-an-authorized-identity");
        creator
            .map_key(&extra, target.root_id(), wrong.clone())
            .await
            .unwrap();
        observer.refresh_mapping_metadata().await.unwrap();
        assert_eq!(
            observer.key_mapping(&extra, target.root_id()).unwrap(),
            Some(wrong)
        );
        let invalid = observer.open_database(target.root_id()).await;
        assert!(match invalid {
            Err(_) => true,
            Ok(db) => db.current_permission().await.is_err(),
        });
        creator
            .map_key(&extra, target.root_id(), SigKey::from_pubkey(&extra))
            .await
            .unwrap();
        observer.refresh_mapping_metadata().await.unwrap();
        observer
            .open_database(target.root_id())
            .await
            .unwrap()
            .snapshot()
            .await
            .unwrap();

        // A valid read-only mapping stays read-only; mapping is not authority.
        target
            .with_transaction(|tx| {
                let root_key = root_key.clone();
                async move {
                    tx.get_settings()?
                        .set_auth_key(&root_key, AuthKey::active(None, Permission::Read))
                        .await
                }
            })
            .await
            .unwrap();
        creator
            .map_key(&root_key, target.root_id(), SigKey::from_pubkey(&root_key))
            .await
            .unwrap();
        observer.refresh_mapping_metadata().await.unwrap();
        let read_only = observer
            .open_database_with_key(target.root_id(), &root_key)
            .await
            .unwrap();
        assert_eq!(
            read_only.current_permission().await.unwrap(),
            Permission::Read
        );
        let tx = read_only.new_transaction().await.unwrap();
        let write = async {
            tx.get_store::<DocStore>("data")
                .await?
                .set("forbidden", "write")
                .await?;
            tx.commit().await
        }
        .await;
        assert!(write.is_err());

        let foreign = eidetica::Database::create(&server, generate_keypair().0, Doc::new())
            .await
            .unwrap();
        creator
            .set_reference("app", "foreign", foreign.root_id())
            .await
            .unwrap();
        assert!(
            observer.open_reference("app", "foreign").await.is_err(),
            "a catalog reference supplied a key"
        );
        creator
            .map_key(&extra, foreign.root_id(), SigKey::from_pubkey(&extra))
            .await
            .unwrap();
        observer.refresh_mapping_metadata().await.unwrap();
        let denied = observer
            .open_database_with_key(foreign.root_id(), &extra)
            .await;
        assert!(
            match denied {
                Err(_) => true,
                Ok(db) => db.current_permission().await.is_err(),
            },
            "a mapping bypassed Auth"
        );

        // Public metadata never imports private key material into a live session.
        let later_key = creator
            .add_private_key(Some("not unlocked by observer"))
            .await
            .unwrap();
        let later_db = creator
            .create_database(Doc::new(), &later_key)
            .await
            .unwrap();
        observer.refresh_mapping_metadata().await.unwrap();
        assert!(observer.get_signing_key(&later_key).is_err());
        assert!(observer.open_database(later_db.root_id()).await.is_err());
        // Already opened handles remain immutable; mapping changes are not revocation.
        retained.snapshot().await.unwrap();
        drop(observer);
        drop(creator);
        drop(observer_instance);
        drop(creator_instance);
        #[cfg(all(unix, feature = "service"))]
        {
            drop(shutdown);
            if let Some(task) = server_task {
                task.await.unwrap().unwrap();
            }
        }
    })
    .await
    .expect("native mapping regression stalled");
}
