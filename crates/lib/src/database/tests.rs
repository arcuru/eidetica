//! Tests for the database module.

use super::*;
use crate::{auth::crypto::generate_keypair, backend::database::InMemory};

#[tokio::test]
async fn test_find_sigkeys_returns_sorted_by_permission() -> Result<()> {
    // Create instance
    let instance = Instance::open(Box::new(InMemory::new())).await?;

    // Generate a test key
    let (signing_key, public_key) = generate_keypair();
    let pubkey_str = format_public_key(&public_key);

    // Create database (Database::create bootstraps signing key as Admin(0))
    let db = Database::create(&instance, signing_key, Doc::new()).await?;

    // Add global Write(10) key via follow-up transaction (bootstrap key stays at Admin(0))
    let txn = db.new_transaction().await?;
    let settings_store = txn.get_settings()?;
    settings_store
        .set_auth_key("*", AuthKey::active(None, Permission::Write(10)))
        .await?;
    txn.commit().await?;

    // Call find_sigkeys
    let results = Database::find_sigkeys(&instance, db.root_id(), &pubkey_str).await?;

    // Verify we got 2 entries (direct key + global)
    assert_eq!(results.len(), 2, "Should find direct key and global option");

    // Verify they're sorted by permission, highest first
    // Admin(0) > Write(10)
    assert_eq!(
        results[0].1,
        Permission::Admin(0),
        "First should be Admin(0) from bootstrap key"
    );
    assert_eq!(
        results[1].1,
        Permission::Write(10),
        "Second should be Write(10) from global"
    );

    // Verify the SigKey types
    assert!(
        results[0].0.has_pubkey_hint(&pubkey_str),
        "First should be direct pubkey hint"
    );
    assert!(results[1].0.is_global(), "Second should be global hint");

    Ok(())
}

#[tokio::test]
async fn test_create_bootstraps_signing_key_as_admin_zero() -> Result<()> {
    let instance = Instance::open(Box::new(InMemory::new())).await?;

    let (signing_key, signing_pubkey) = generate_keypair();
    let signing_pubkey_str = format_public_key(&signing_pubkey);

    // Create database (signing key is bootstrapped as Admin(0))
    let db = Database::create(&instance, signing_key, Doc::new()).await?;

    // Verify the signing key was bootstrapped as Admin(0)
    let results = Database::find_sigkeys(&instance, db.root_id(), &signing_pubkey_str).await?;
    assert_eq!(results.len(), 1, "Signing key should be present in auth");
    assert_eq!(
        results[0].1,
        Permission::Admin(0),
        "Signing key should be Admin(0)"
    );

    Ok(())
}

#[tokio::test]
async fn test_create_rejects_preconfigured_auth() -> Result<()> {
    let instance = Instance::open(Box::new(InMemory::new())).await?;

    let (signing_key, _) = generate_keypair();

    let (_, other_pubkey) = generate_keypair();
    let other_pubkey_str = format_public_key(&other_pubkey);

    // Pre-configure auth in settings — this should be rejected
    let mut settings = Doc::new();
    settings.set("name", "test_reject");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        &other_pubkey_str,
        AuthKey::active(Some("other_user"), Permission::Write(5)),
    )?;
    settings.set("auth", auth_settings.as_doc().clone());

    // Database::create should return an error
    let result = Database::create(&instance, signing_key, settings).await;
    assert!(result.is_err(), "Should reject preconfigured auth");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("must not contain auth configuration"),
        "Error should mention auth configuration, got: {err_msg}"
    );

    Ok(())
}
