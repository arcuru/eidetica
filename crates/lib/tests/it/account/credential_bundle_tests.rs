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
    let (instance, session) = Instance::create_backend(Box::new(InMemory::new()), NewAccount::with_password("local", "password")).await.unwrap();
    let key = session.get_signing_key(&session.get_default_key().unwrap()).unwrap();
    let other = Database::create(&instance, key, Doc::new()).await.unwrap();
    let backend = instance.backend().local_engine().unwrap();
    let old = backend.local_account_record("login:local").await.unwrap().unwrap();
    let mut altered: serde_json::Value = serde_json::from_slice(&old).unwrap();
    altered["info"]["user_database_id"] = serde_json::to_value(other.root_id()).unwrap();
    assert!(backend.compare_exchange_local_account_record("login:local", Some(&old), &serde_json::to_vec(&altered).unwrap()).await.unwrap());
    assert!(instance.login_account("local", Some("password")).await.is_err(), "an unauthenticated directory locator rebound an unlocked login");
}
