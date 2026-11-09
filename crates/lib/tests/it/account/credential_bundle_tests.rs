use eidetica::{ID, account::CredentialBundle, auth::crypto::generate_keypair};

#[test]
fn test_credential_bundle_explicit_scope_and_client_unlock() {
    let account = ID::from_bytes("personal-account");
    let app = ID::from_bytes("app-account");
    let (personal_key, personal_id) = generate_keypair();
    let (app_key, app_id) = generate_keypair();
    let personal = CredentialBundle::seal(&account, &[personal_key], "personal password").unwrap();
    let scoped =
        CredentialBundle::seal(&app, std::slice::from_ref(&app_key), "app password").unwrap();
    let bytes = serde_json::to_vec(&scoped).unwrap();
    let encoded = std::str::from_utf8(&bytes).unwrap();
    assert!(!encoded.contains(app_key.to_prefixed_string().as_str()));
    assert!(
        !encoded.contains(&app.to_string()),
        "locator is inside the authenticated ciphertext"
    );
    let shipped_to_app: CredentialBundle = serde_json::from_slice(&bytes).unwrap();
    let unlocked = shipped_to_app.unlock_for(&app, "app password").unwrap();
    assert_eq!(unlocked.key_ids(), std::slice::from_ref(&app_id));
    assert!(unlocked.signer(&personal_id).is_err());
    assert!(personal.unlock_for(&app, "personal password").is_err());
    assert!(scoped.unlock_for(&account, "app password").is_err());
    assert!(scoped.unlock("personal password").is_err());
    let message = b"actual app signature";
    let signature = unlocked.signer(&app_id).unwrap().sign(message);
    app_id.verify(message, &signature).unwrap();
    assert!(personal_id.verify(message, &signature).is_err());
}

#[test]
fn test_credential_bundle_rewrap_retains_signers_not_revocation() {
    let account = ID::from_bytes("same-account");
    let (key, key_id) = generate_keypair();
    let original = CredentialBundle::seal(&account, &[key], "old").unwrap();
    let mut unlocked = original.unlock("old").unwrap();
    let retained = unlocked.signer(&key_id).unwrap();
    let (new_key, new_id) = generate_keypair();
    unlocked.add_key(new_key).unwrap();
    let updated = unlocked.seal().unwrap();
    let repeated = unlocked.seal().unwrap();
    assert_ne!(
        serde_json::to_value(&updated).unwrap()["nonce"],
        serde_json::to_value(&repeated).unwrap()["nonce"]
    );
    let rewrapped = unlocked.rewrap("new").unwrap();
    drop(unlocked);
    assert!(rewrapped.unlock("old").is_err());
    let relogin = rewrapped.unlock("new").unwrap();
    assert_eq!(relogin.key_ids(), [key_id.clone(), new_id]);
    assert_eq!(updated.unlock("old").unwrap().key_ids(), relogin.key_ids());
    // Rewrapping does not revoke old copies or already returned handles.
    assert_eq!(
        original.unlock("old").unwrap().key_ids(),
        std::slice::from_ref(&key_id)
    );
    key_id
        .verify(b"retained", &retained.sign(b"retained"))
        .unwrap();
    let other_installation =
        CredentialBundle::seal(&account, &[generate_keypair().0], "second-device").unwrap();
    assert_ne!(
        other_installation
            .unlock("second-device")
            .unwrap()
            .key_ids(),
        relogin.key_ids()
    );
}

#[test]
fn test_credential_bundle_tampering_and_invalid_formats_fail_closed() {
    let account = ID::from_bytes("account");
    let (key, _) = generate_keypair();
    assert!(CredentialBundle::seal(&account, &[], "password").is_err());
    assert!(CredentialBundle::seal(&account, &[key.clone(), key.clone()], "password").is_err());
    let original = CredentialBundle::seal(&account, &[key], "password").unwrap();
    let value = serde_json::to_value(&original).unwrap();
    for field in ["version", "salt", "nonce", "ciphertext"] {
        let mut modified = value.clone();
        if field == "version" {
            modified[field] = 99.into();
        } else if field == "salt" {
            modified[field] = "aDifferentSaltStringXX1".into();
        } else {
            modified[field][0] = (modified[field][0].as_u64().unwrap() ^ 1).into();
        }
        let bundle: CredentialBundle = serde_json::from_value(modified).unwrap();
        assert!(
            bundle.unlock("password").is_err(),
            "accepted altered {field}"
        );
    }
    let mut invalid = value.clone();
    invalid["nonce"] = serde_json::json!([1, 2]);
    assert!(
        serde_json::from_value::<CredentialBundle>(invalid)
            .unwrap()
            .unlock("password")
            .is_err()
    );
    let mut unknown = value;
    unknown["unimplemented_kdf_cost"] = 999999999.into();
    assert!(serde_json::from_value::<CredentialBundle>(unknown).is_err());
}

#[tokio::test]
async fn test_native_login_bundle_authenticates_account_locator() {
    use eidetica::{Database, Instance, NewAccount, backend::database::InMemory, crdt::Doc};
    let (instance, session) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::with_password("local", "password"),
    )
    .await
    .unwrap();
    let key = session
        .get_signing_key(&session.get_default_key().unwrap())
        .unwrap();
    let other = Database::create(&instance, key, Doc::new()).await.unwrap();
    let backend = instance.backend().local_engine().unwrap();
    let old = backend
        .local_account_record("login:local")
        .await
        .unwrap()
        .unwrap();
    let mut altered: serde_json::Value = serde_json::from_slice(&old).unwrap();
    altered["info"]["user_database_id"] = serde_json::to_value(other.root_id()).unwrap();
    assert!(
        backend
            .compare_exchange_local_account_record(
                "login:local",
                Some(&old),
                &serde_json::to_vec(&altered).unwrap()
            )
            .await
            .unwrap()
    );
    assert!(
        instance
            .login_account("local", Some("password"))
            .await
            .is_err(),
        "an unauthenticated directory locator rebound an unlocked login"
    );
}

async fn native_bundle_lifecycle(server: &eidetica::Instance, client: &eidetica::Instance) {
    use eidetica::account::KeyStorage;
    let mut first = client
        .login_account("local", Some("password"))
        .await
        .unwrap();
    let mut stale = client
        .login_account("local", Some("password"))
        .await
        .unwrap();
    let root = first.get_default_key().unwrap();
    let a = first.add_private_key(Some("first")).await.unwrap();
    let retained = first
        .create_database(eidetica::crdt::Doc::new(), &a)
        .await
        .unwrap();
    let (imported, b) = generate_keypair();
    assert_eq!(
        stale
            .import_private_key(imported.clone(), Some("second"))
            .await
            .unwrap(),
        b
    );
    assert!(
        stale
            .import_private_key(imported, Some("duplicate"))
            .await
            .is_err()
    );
    assert_eq!(stale.key_display_name(&b), Some("second"));
    let backend = server.backend().local_engine().unwrap();
    let name = format!("account-state:{}", first.login_id());
    let bytes = backend.local_account_record(&name).await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let KeyStorage::Encrypted { bundle } =
        serde_json::from_value(value["credentials"].clone()).unwrap()
    else {
        panic!("protected login wrote plaintext")
    };
    let unlocked = bundle.unlock_for(first.account().id(), "password").unwrap();
    let ids = unlocked.key_ids();
    assert_eq!(
        ids.len(),
        3,
        "stale session overwrote an independent key addition"
    );
    for id in [&root, &a, &b] {
        assert!(ids.contains(id));
        assert!(value["keys"][id.to_string()].get("storage").is_none());
        let secret = unlocked.signer(id).unwrap().to_prefixed_string();
        assert!(
            !std::str::from_utf8(&bytes)
                .unwrap()
                .contains(secret.as_str())
        );
    }
    // Make the next successful login change the observed record deterministically,
    // not only when this test happens to cross a wall-clock second.
    let mut before_returning = value.clone();
    before_returning["last_login"] = 0.into();
    assert!(
        backend
            .compare_exchange_local_account_record(
                &name,
                Some(&bytes),
                &serde_json::to_vec(&before_returning).unwrap()
            )
            .await
            .unwrap()
    );
    first.logout().unwrap();
    stale.logout().unwrap();
    assert!(client.login_account("local", Some("wrong")).await.is_err());
    assert!(client.login_account("local", None).await.is_err());
    let returned = client
        .login_account("local", Some("password"))
        .await
        .unwrap();
    assert_eq!(returned.list_keys().unwrap().len(), 3);
    for id in [&root, &a, &b] {
        let key = returned.get_signing_key(id).unwrap();
        id.verify(b"relogin", &key.sign(b"relogin")).unwrap();
    }
    // This handle owns its signer independently of the login that created it.
    // Reauthentication must not silently replace it with the login root key.
    let tx = retained.new_transaction().await.unwrap();
    tx.get_store::<eidetica::store::DocStore>("data")
        .await
        .unwrap()
        .set("after-relogin", "written")
        .await
        .unwrap();
    let entry = tx.commit().await.unwrap();
    eidetica::auth::crypto::verify_entry_signature(&retained.get_entry(entry).await.unwrap(), &a)
        .unwrap();
    let (login, read) = tokio::join!(
        client.login_account("local", Some("password")),
        retained.snapshot(),
    );
    login.unwrap();
    read.unwrap();
    returned.logout().unwrap();
    server
        .login_account("local", Some("password"))
        .await
        .unwrap()
        .admin()
        .await
        .unwrap()
        .create_account(eidetica::NewAccount::passwordless("other"))
        .await
        .unwrap();
    let other = client.login_account("other", None).await.unwrap();
    assert!(
        other.get_signing_key(&a).is_err(),
        "another login inherited a private key"
    );
    assert!(!other.list_keys().unwrap().contains(&a));
    // Replacing the connection login does not replace independently held keys.
    let tx = retained.new_transaction().await.unwrap();
    tx.get_store::<eidetica::store::DocStore>("data")
        .await
        .unwrap()
        .set("other-login", "still-own-signer")
        .await
        .unwrap();
    let entry = tx.commit().await.unwrap();
    eidetica::auth::crypto::verify_entry_signature(&retained.get_entry(entry).await.unwrap(), &a)
        .unwrap();
    other.logout().unwrap();
    let current_bytes = backend.local_account_record(&name).await.unwrap().unwrap();
    let current: serde_json::Value = serde_json::from_slice(&current_bytes).unwrap();
    assert_ne!(
        current["last_login"], 0,
        "successful login did not update its local timestamp"
    );
    // A valid foreign bundle with the same password is still not this Account.
    let foreign = CredentialBundle::seal(
        &ID::from_bytes("foreign-account"),
        &[generate_keypair().0],
        "password",
    )
    .unwrap();
    let mut swapped = current.clone();
    swapped["last_login"] = 123.into();
    swapped["credentials"] =
        serde_json::to_value(KeyStorage::Encrypted { bundle: foreign }).unwrap();
    assert!(
        backend
            .compare_exchange_local_account_record(
                &name,
                Some(&current_bytes),
                &serde_json::to_vec(&swapped).unwrap()
            )
            .await
            .unwrap()
    );
    assert!(
        client
            .login_account("local", Some("password"))
            .await
            .is_err()
    );
    assert_eq!(
        backend.local_account_record(&name).await.unwrap().unwrap(),
        serde_json::to_vec(&swapped).unwrap(),
        "failed unlock wrote login bookkeeping"
    );
    // Restore only this test instance's exact record and corrupt its payload.
    let swapped_bytes = backend.local_account_record(&name).await.unwrap().unwrap();
    let mut corrupted = current;
    corrupted["last_login"] = 456.into();
    let byte = corrupted["credentials"]["bundle"]["ciphertext"][0]
        .as_u64()
        .unwrap();
    corrupted["credentials"]["bundle"]["ciphertext"][0] = (byte ^ 1).into();
    assert!(
        backend
            .compare_exchange_local_account_record(
                &name,
                Some(&swapped_bytes),
                &serde_json::to_vec(&corrupted).unwrap()
            )
            .await
            .unwrap()
    );
    assert!(
        client
            .login_account("local", Some("password"))
            .await
            .is_err()
    );
    assert_eq!(
        backend.local_account_record(&name).await.unwrap().unwrap(),
        serde_json::to_vec(&corrupted).unwrap()
    );
}

#[tokio::test]
async fn test_native_bundle_lifecycle_embedded() {
    let (instance, _) = eidetica::Instance::create_backend(
        Box::new(eidetica::backend::database::InMemory::new()),
        eidetica::NewAccount::with_password("local", "password"),
    )
    .await
    .unwrap();
    native_bundle_lifecycle(&instance, &instance).await;
}

#[tokio::test]
#[cfg(all(unix, feature = "service"))]
async fn test_native_bundle_lifecycle_connected() {
    use eidetica::{Instance, NewAccount, backend::database::InMemory, service::ServiceServer};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("bundle.sock");
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::with_password("local", "password"),
    )
    .await
    .unwrap();
    let service = ServiceServer::bind(instance.clone(), &path).await.unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(());
    let task = tokio::spawn(service.run(rx));
    let client = Instance::connect(format!("unix://{}", path.display()))
        .await
        .unwrap();
    native_bundle_lifecycle(&instance, &client).await;
    drop(client);
    drop(shutdown);
    task.await.unwrap().unwrap();
}

#[tokio::test]
#[cfg(feature = "sqlite")]
async fn test_complete_native_bundle_survives_sqlite_restart() {
    use eidetica::{Instance, NewAccount, backend::database::Sqlite, crdt::Doc};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.db");
    let (account_id, added, target_id) = {
        let (_instance, mut session) = Instance::create_backend(
            Box::new(Sqlite::open(&path).await.unwrap()),
            NewAccount::with_password("local", "password"),
        )
        .await
        .unwrap();
        let added = session.add_private_key(Some("persisted")).await.unwrap();
        let target = session.create_database(Doc::new(), &added).await.unwrap();
        session
            .set_reference("app", "target", target.root_id())
            .await
            .unwrap();
        (
            session.account().id().clone(),
            added,
            target.root_id().clone(),
        )
    };
    let instance = Instance::open_backend(Box::new(Sqlite::open(&path).await.unwrap()))
        .await
        .unwrap();
    let returning = instance
        .login_account("local", Some("password"))
        .await
        .unwrap();
    assert_eq!(returning.account().id(), &account_id);
    assert_eq!(returning.key_display_name(&added), Some("persisted"));
    let target = returning
        .open_reference("app", "target")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.root_id(), &target_id);
    let tx = target.new_transaction().await.unwrap();
    tx.get_store::<eidetica::store::DocStore>("data")
        .await
        .unwrap()
        .set("after-restart", "written")
        .await
        .unwrap();
    let entry_id = tx.commit().await.unwrap();
    eidetica::auth::crypto::verify_entry_signature(
        &target.get_entry(entry_id).await.unwrap(),
        &added,
    )
    .unwrap();
}

#[tokio::test]
async fn test_empty_password_login_uses_encrypted_complete_bundle() {
    use eidetica::{Instance, NewAccount, account::KeyStorage, backend::database::InMemory};
    for initial in [NewAccount::passwordless("local"), NewAccount::with_password("local", "")] {
        let (instance, mut session) = Instance::create_backend(Box::new(InMemory::new()), initial)
            .await.unwrap();
        let root = session.get_default_key().unwrap();
        let backend = instance.backend().local_engine().unwrap();
        let login = backend.local_account_record("login:local").await.unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&login).unwrap();
        let storage: KeyStorage = serde_json::from_value(value["info"]["credentials"]["bundle"].clone()).unwrap();
        let KeyStorage::Encrypted { bundle } = storage else { panic!("empty-password root stored plaintext") };
        let unlocked = bundle.unlock_for(session.account().id(), "").unwrap();
        assert_eq!(unlocked.key_ids(), [root.clone()]);
        let added = session.add_private_key(Some("generated")).await.unwrap();
        let (imported, imported_id) = generate_keypair();
        session.import_private_key(imported, Some("imported")).await.unwrap();
        let state = backend.local_account_record(&format!("account-state:{}", session.login_id())).await.unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&state).unwrap();
        let storage: KeyStorage = serde_json::from_value(value["credentials"].clone()).unwrap();
        let KeyStorage::Encrypted { bundle } = storage else { panic!("added keys stored plaintext") };
        let unlocked = bundle.unlock_for(session.account().id(), "").unwrap();
        assert_eq!(unlocked.key_ids().len(), 3);
        assert!(bundle.unlock("not empty").is_err());
        for id in [&root, &added, &imported_id] {
            let secret = unlocked.signer(id).unwrap().to_prefixed_string();
            for bytes in [&login, &state] {
                assert!(!std::str::from_utf8(bytes).unwrap().contains(secret.as_str()));
            }
        }
        session.logout().unwrap();
        assert!(instance.login_account("local", Some("wrong")).await.is_err());
        let returning = instance.login_account("local", Some("")).await.unwrap();
        for id in [&root, &added, &imported_id] {
            id.verify(b"empty relogin", &returning.get_signing_key(id).unwrap().sign(b"empty relogin")).unwrap();
        }
    }
}
