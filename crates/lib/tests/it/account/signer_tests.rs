use eidetica::{
    Database, Instance, NewAccount,
    account::{AccountSession, AccountSigner, CredentialBundle},
    auth::{
        crypto::{PublicKey, generate_keypair, verify_entry_signature},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
    },
    backend::database::InMemory,
    crdt::Doc,
    store::DocStore,
};

async fn write_as(database: &Database, label: &str, signer: &PublicKey, identity: &SigKey) {
    let tx = database.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set(label, label)
        .await
        .unwrap();
    let id = tx
        .commit()
        .await
        .unwrap_or_else(|error| panic!("{label} commit failed: {error:?}"));
    let entry = database.get_entry(id).await.unwrap();
    assert_eq!(&entry.auth().key, identity);
    verify_entry_signature(&entry, signer).unwrap();
}

async fn explicit_signers(server: &Instance, url: Option<&str>) {
    let (first, first_id) = generate_keypair();
    let (second, second_id) = generate_keypair();
    let account = Database::create(server, first.clone(), Doc::new())
        .await
        .unwrap();
    let target = Database::create(server, first.clone(), Doc::new())
        .await
        .unwrap();
    let tx = target.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &second_id,
            AuthKey::active(Some("writer"), Permission::Write(1)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let bundle =
        CredentialBundle::seal(account.root_id(), &[first.clone(), second], "local").unwrap();
    let unlocked = bundle.unlock_for(account.root_id(), "local").unwrap();
    let a = AccountSigner::new(
        account.root_id().clone(),
        unlocked.signer(&first_id).unwrap(),
    );
    let b = AccountSigner::new(
        account.root_id().clone(),
        eidetica::database::DatabaseKey::with_name(unlocked.signer(&second_id).unwrap(), "writer"),
    );
    // No daemon-held bundle was installed for these keys. Both come from the
    // same caller-side interface; service connections prove their own signer.
    let instance = match url {
        Some(url) => a.connect(url).await.unwrap(),
        None => server.clone(),
    };
    let a_database = a.open_database(&instance, target.root_id()).await.unwrap();
    let b_database = b.open_database(&instance, target.root_id()).await.unwrap();
    assert_eq!(a.account_id(), account.root_id());
    assert!(
        b.open_account(&instance).await.is_err(),
        "the locator must not grant Account access"
    );
    let mismatched = AccountSigner::new(
        account.root_id().clone(),
        eidetica::database::DatabaseKey::with_identity(
            unlocked.signer(&second_id).unwrap(),
            SigKey::from_pubkey(&first_id),
        ),
    );
    assert!(
        mismatched
            .open_database(&instance, target.root_id())
            .await
            .is_err(),
        "another proven key must not stand in for the actual signer"
    );
    drop(unlocked);
    write_as(&b_database, "new", &second_id, b.identity()).await;
    write_as(&a_database, "retained", &first_id, a.identity()).await;
    assert!(
        verify_entry_signature(
            &a_database
                .get_entry(a_database.snapshot().await.unwrap().tips()[0].clone())
                .await
                .unwrap(),
            &second_id
        )
        .is_err()
    );

    let identity = Database::create(server, first, Doc::new()).await.unwrap();
    let tips = identity.snapshot().await.unwrap().into_tips();
    let tx = target.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(2),
                min: None,
            },
            tree: TreeReference {
                root: identity.root_id().clone(),
                tips: tips.clone(),
            },
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = account.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Admin(0),
                min: None,
            },
            tree: TreeReference {
                root: identity.root_id().clone(),
                tips: tips.clone(),
            },
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: identity.root_id().clone(),
            tips,
        }],
        hint: KeyHint::from_pubkey(&first_id),
    };
    let delegated = a.with_identity(path.clone());
    let direct_session = AccountSession::from_signer(&instance, a.clone())
        .await
        .unwrap();
    let delegated_session = AccountSession::from_signer(&instance, delegated.clone())
        .await
        .unwrap();
    delegated_session
        .set_reference("identity", "root", target.root_id())
        .await
        .unwrap();
    write_as(
        delegated_session.account().database(),
        "delegated-account",
        &first_id,
        &path,
    )
    .await;
    write_as(
        direct_session.account().database(),
        "retained-direct-account",
        &first_id,
        a.identity(),
    )
    .await;
    direct_session.logout().unwrap();
    delegated_session.logout().unwrap();
    let delegated_database = delegated
        .open_database(&instance, target.root_id())
        .await
        .unwrap();
    write_as(&delegated_database, "delegated", &first_id, &path).await;
    write_as(&a_database, "still-direct", &first_id, a.identity()).await;
    assert_ne!(a.identity(), delegated.identity());
    let wrong_path = a.with_identity(SigKey::Delegation {
        path: vec![DelegationStep {
            tree: account.root_id().clone(),
            tips: account.snapshot().await.unwrap().into_tips(),
        }],
        hint: KeyHint::from_pubkey(&first_id),
    });
    assert!(
        AccountSession::from_signer(&instance, wrong_path.clone())
            .await
            .is_err()
    );
    assert!(
        wrong_path
            .open_database(&instance, target.root_id())
            .await
            .is_err(),
        "no fallback to the same key's direct grant"
    );
    let stranger = AccountSigner::new(account.root_id().clone(), generate_keypair().0);
    assert!(
        stranger
            .open_database(&instance, target.root_id())
            .await
            .is_err()
    );
    if let Some(url) = url {
        // New connection with a different signer; retained handles on the first
        // connection remain bound and usable. No global login/selection mutation.
        let second_connection = b.connect(url).await.unwrap();
        let reopened = b
            .open_database(&second_connection, target.root_id())
            .await
            .unwrap();
        write_as(&reopened, "reconnected", &second_id, b.identity()).await;
        write_as(&a_database, "after-reconnect", &first_id, a.identity()).await;
    }
}

#[tokio::test]
async fn test_account_signers_work_without_bundle_login_embedded() {
    let (instance, _admin) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    explicit_signers(&instance, None).await;
}

#[tokio::test]
#[cfg(all(unix, feature = "service"))]
async fn test_account_signers_work_without_bundle_login_connected() {
    use eidetica::service::ServiceServer;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("service.sock");
    let (instance, _admin) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(());
    let service = ServiceServer::bind(instance.clone(), &path).await.unwrap();
    let task = tokio::spawn(service.run(rx));
    let before = instance
        .backend()
        .local_engine()
        .unwrap()
        .local_account_names()
        .await
        .unwrap();
    explicit_signers(&instance, Some(&format!("unix://{}", path.display()))).await;
    // Signer authentication must not register any personal or app secrets.
    assert_eq!(
        instance
            .login_account("admin", None)
            .await
            .unwrap()
            .admin()
            .await
            .unwrap()
            .list_accounts()
            .await
            .unwrap(),
        ["admin"]
    );
    assert_eq!(
        instance
            .backend()
            .local_engine()
            .unwrap()
            .local_account_names()
            .await
            .unwrap(),
        before,
        "explicit signers created daemon credential records"
    );
    drop(shutdown);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_account_session_selection_returns_new_retained_signer_handles() {
    let (instance, mut session) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let first = session.get_default_key().unwrap();
    let second = session.add_private_key(Some("second")).await.unwrap();
    let target = session.create_database(Doc::new(), &first).await.unwrap();
    let tx = target.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(&second, AuthKey::active(None, Permission::Write(1)))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let retained = session.acting_as(&first, None).unwrap();
    let selected = session.acting_as(&second, None).unwrap();
    assert!(session.acting_as(&generate_keypair().1, None).is_err());
    session.logout().unwrap();
    write_as(
        &selected
            .open_database(&instance, target.root_id())
            .await
            .unwrap(),
        "new-selection",
        &second,
        selected.identity(),
    )
    .await;
    write_as(
        &retained
            .open_database(&instance, target.root_id())
            .await
            .unwrap(),
        "retained-selection",
        &first,
        retained.identity(),
    )
    .await;
}

async fn local_records(instance: &Instance) -> std::collections::BTreeMap<String, Vec<u8>> {
    let backend = instance.backend().local_engine().unwrap();
    let mut records = std::collections::BTreeMap::new();
    for name in backend.local_account_names().await.unwrap() {
        records.insert(
            name.clone(),
            backend.local_account_record(&name).await.unwrap().unwrap(),
        );
    }
    records
}

async fn caller_held_sessions(server: &Instance, url: Option<&str>) {
    use eidetica::account::{AccountSession, SyncSettings};
    let (personal_key, personal_id) = generate_keypair();
    let personal = Database::create(server, personal_key.clone(), Doc::new())
        .await
        .unwrap();
    let (app_key, app_id) = generate_keypair();
    let (other_key, other_id) = generate_keypair();
    let app = Database::create(server, app_key.clone(), Doc::new())
        .await
        .unwrap();
    let tx = app.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(&other_id, AuthKey::active(None, Permission::Admin(0)))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let records_before = local_records(server).await;
    // This is the complete app input. The personal bundle never crosses this
    // boundary; even a personal Account reference cannot supply its signer.
    let bundle = CredentialBundle::seal(app.root_id(), &[app_key, other_key], "app-only").unwrap();
    assert!(bundle.unlock_for(personal.root_id(), "app-only").is_err());
    let credentials = bundle.unlock("app-only").unwrap();
    assert!(credentials.signer(&personal_id).is_err());
    let first = AccountSigner::new(app.root_id().clone(), credentials.signer(&app_id).unwrap());
    let instance = match url {
        Some(url) => first.connect(url).await.unwrap(),
        None => server.clone(),
    };
    let mut session =
        AccountSession::from_credentials(&instance, bundle.unlock("app-only").unwrap(), &app_id)
            .await
            .unwrap();
    assert!(session.account_info().is_none());
    assert!(session.login_id().is_empty());
    assert!(session.username().is_empty());
    assert_eq!(session.account().id(), app.root_id());
    assert!(session.admin().await.is_err());
    assert!(
        AccountSession::from_credentials(
            &instance,
            bundle.unlock("app-only").unwrap(),
            &personal_id
        )
        .await
        .is_err()
    );
    assert!(
        AccountSession::from_signer(
            &instance,
            AccountSigner::new(
                personal.root_id().clone(),
                credentials.signer(&app_id).unwrap()
            )
        )
        .await
        .is_err()
    );
    session
        .set_reference("app", "personal", personal.root_id())
        .await
        .unwrap();
    assert_eq!(
        session.reference("app", "personal").await.unwrap(),
        Some(personal.root_id().clone())
    );
    assert!(session.open_reference("app", "personal").await.is_err());
    assert!(session.acting_as(&personal_id, None).is_err());
    assert_eq!(session.list_keys().unwrap().len(), 2);
    let target = session.create_database(Doc::new(), &app_id).await.unwrap();
    let tx = target.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(&other_id, AuthKey::active(None, Permission::Write(1)))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    session
        .set_reference("app", "target", target.root_id())
        .await
        .unwrap();
    session
        .track_database(target.root_id(), &other_id, SyncSettings::disabled())
        .await
        .unwrap();
    let retained = session.acting_as(&app_id, None).unwrap();
    let selected = session.acting_as(&other_id, None).unwrap();
    assert!(
        session
            .open_reference("app", "target")
            .await
            .unwrap()
            .is_some()
    );
    let selected_database = session
        .open_database_with_key(target.root_id(), &other_id)
        .await
        .unwrap();
    write_as(
        &selected_database,
        "caller-selected",
        &other_id,
        selected.identity(),
    )
    .await;
    assert_eq!(session.databases().await.unwrap().len(), 1);
    session.untrack_database(target.root_id()).await.unwrap();
    assert!(session.databases().await.unwrap().is_empty());
    // Adding a local key does not add it to the input bundle or any daemon
    // record. External storage ownership stays explicit.
    let additional = session.add_private_key(None).await.unwrap();
    assert!(credentials.signer(&additional).is_err());
    session.logout().unwrap();
    drop(credentials);
    write_as(
        &selected_database,
        "after-caller-logout",
        &other_id,
        selected.identity(),
    )
    .await;
    write_as(
        &retained
            .open_database(&instance, target.root_id())
            .await
            .unwrap(),
        "caller-retained",
        &app_id,
        retained.identity(),
    )
    .await;
    let reopened =
        AccountSession::from_credentials(&instance, bundle.unlock("app-only").unwrap(), &app_id)
            .await
            .unwrap();
    assert!(reopened.databases().await.unwrap().is_empty());
    assert!(reopened.get_signing_key(&additional).is_err());
    assert_eq!(
        reopened.reference("app", "target").await.unwrap(),
        Some(target.root_id().clone())
    );
    assert!(
        reopened.open_reference("app", "target").await.is_err(),
        "references must not activate local mappings"
    );
    assert_eq!(
        local_records(server).await,
        records_before,
        "caller-owned sessions wrote daemon credentials or activation state"
    );
    drop(personal_key);
}

#[tokio::test]
async fn test_caller_held_full_sessions_embedded() {
    let (instance, _) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    caller_held_sessions(&instance, None).await;
}

#[tokio::test]
#[cfg(all(unix, feature = "service"))]
async fn test_caller_held_full_sessions_connected() {
    use eidetica::service::ServiceServer;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("service.sock");
    let (instance, _) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let service = ServiceServer::bind(instance.clone(), &path).await.unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(());
    let task = tokio::spawn(service.run(rx));
    caller_held_sessions(&instance, Some(&format!("unix://{}", path.display()))).await;
    drop(shutdown);
    task.await.unwrap().unwrap();
}
