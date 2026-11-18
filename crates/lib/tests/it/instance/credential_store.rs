use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    path::Path,
};

use eidetica::{
    Instance, NewAccount,
    auth::crypto::PrivateKey,
    backend::{BackendImpl, database::InMemory},
    instance::InstanceError,
};

async fn open(snapshot: &Path, directory: &Path) -> eidetica::Result<Instance> {
    let backend = InMemory::try_load_from_file(snapshot).await?.unwrap();
    Instance::open_backend_with_credential_store(Box::new(backend), directory).await
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_credential_store_embedded_snapshot_reopen() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("credentials");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let file = directory.join("instance-secrets.json");
    let snapshot = root.path().join("history.json");
    let message = b"test signing after reload";
    let (id, signature, saved) = {
        let (instance, _) = Instance::create_backend_with_credential_store(
            Box::new(InMemory::new()),
            NewAccount::passwordless("trial"),
            &directory,
        )
        .await
        .unwrap();
        let saved = fs::read(&file).unwrap();
        instance
            .backend()
            .clear_derived_store_state()
            .await
            .unwrap();
        instance.snapshot_to_path(&snapshot).unwrap();
        (
            instance.id(),
            instance.signing_key().unwrap().sign(message),
            saved,
        )
    };
    assert!(
        fs::read(&file).unwrap() == saved,
        "cache clearing touched the device key"
    );
    let backend = InMemory::try_load_from_file(&snapshot)
        .await
        .unwrap()
        .unwrap();
    assert!(backend.get_instance_secrets().await.unwrap().is_none());
    let reopened = open(&snapshot, &directory).await.unwrap();
    assert_eq!(reopened.id(), id);
    assert_eq!(reopened.signing_key().unwrap().sign(message), signature);
    reopened.login_account("trial", "").await.unwrap();

    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        open(&snapshot, &directory).await,
        Err(eidetica::Error::Instance(error)) if matches!(*error, InstanceError::CredentialStore { .. })
    ));
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();

    let real_file = directory.join("renamed-secrets");
    fs::rename(&file, &real_file).unwrap();
    symlink(&real_file, &file).unwrap();
    assert!(matches!(
        open(&snapshot, &directory).await,
        Err(eidetica::Error::Instance(error)) if matches!(*error, InstanceError::CredentialStore { .. })
    ));
    assert!(
        fs::read(&real_file).unwrap() == saved,
        "refusal changed the original"
    );
    fs::remove_file(&file).unwrap();
    fs::rename(&real_file, &file).unwrap();

    let wrong = PrivateKey::generate();
    assert_ne!(wrong.public_key(), id);
    fs::write(&file, serde_json::json!({"signing_key": wrong}).to_string()).unwrap();
    assert!(matches!(
        open(&snapshot, &directory).await,
        Err(eidetica::Error::Instance(error)) if matches!(*error, InstanceError::DeviceKeyMismatch)
    ));

    // The underlying key parser includes an invalid algorithm prefix in its
    // error. The credential-store error must not include the file contents.
    let canary = "private-input-canary";
    fs::write(&file, format!(r#"{{"signing_key":"{canary}:bad"}}"#)).unwrap();
    let error = open(&snapshot, &directory)
        .await
        .expect_err("malformed key accepted");
    assert!(error.to_string().contains("invalid credential file"));
    assert!(!format!("{error:?} {error}").contains(canary));
}
