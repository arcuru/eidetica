//! Tests for the database module.

use super::*;
use crate::{
    auth::crypto::generate_keypair,
    backend::database::InMemory,
};

#[tokio::test]
async fn test_find_sigkeys_returns_sorted_by_permission() -> Result<()> {
    // Create instance
    let instance = Instance::open(Box::new(InMemory::new())).await?;

    // Generate a test key
    let (signing_key, public_key) = generate_keypair();
    let pubkey_str = format_public_key(&public_key);

    // Create initial settings
    let mut settings = Doc::new();
    settings.set("name", "test_db");

    let mut auth_settings = AuthSettings::new();

    // In the new design, keys are stored by pubkey (one entry per pubkey).
    // To test sorting, we add a direct key and a global permission.
    // The direct key should be returned along with the global option.
    auth_settings.add_key(
        &pubkey_str,
        AuthKey::active(Some("my_device"), Permission::Admin(5)),
    )?;

    // Add global permission with lower priority
    auth_settings.add_key("*", AuthKey::active(None::<String>, Permission::Write(10)))?;

    settings.set("auth", auth_settings.as_doc().clone());

    // Create database
    let db = Database::create(settings, &instance, signing_key, "my_device".to_string()).await?;

    // Call find_sigkeys
    let results = Database::find_sigkeys(&instance, db.root_id(), &pubkey_str).await?;

    // Verify we got 2 entries (direct key + global)
    assert_eq!(results.len(), 2, "Should find direct key and global option");

    // Verify they're sorted by permission, highest first
    // Admin(5) > Write(10)
    assert_eq!(
        results[0].1,
        Permission::Admin(5),
        "First should be Admin(5) from direct key"
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
async fn test_create_merges_signing_key_into_preconfigured_auth() -> Result<()> {
    let instance = Instance::open(Box::new(InMemory::new())).await?;

    // Generate two separate keypairs: one for signing, one pre-configured in auth
    let (signing_key, signing_pubkey) = generate_keypair();
    let signing_pubkey_str = format_public_key(&signing_pubkey);

    let (_, other_pubkey) = generate_keypair();
    let other_pubkey_str = format_public_key(&other_pubkey);

    // Pre-configure auth with ONLY the other key (not the signing key)
    let mut settings = Doc::new();
    settings.set("name", "test_merge");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        &other_pubkey_str,
        AuthKey::active(Some("other_user"), Permission::Write(5)),
    )?;
    settings.set("auth", auth_settings.as_doc().clone());

    // Create database -- signing key is NOT in the pre-configured auth
    let db = Database::create(
        settings,
        &instance,
        signing_key,
        signing_pubkey_str.clone(),
    )
    .await?;

    // Verify the signing key was auto-added as Admin(0)
    let signing_key_results =
        Database::find_sigkeys(&instance, db.root_id(), &signing_pubkey_str).await?;
    assert_eq!(
        signing_key_results.len(),
        1,
        "Signing key should be present in auth"
    );
    assert_eq!(
        signing_key_results[0].1,
        Permission::Admin(0),
        "Signing key should be Admin(0)"
    );

    // Verify the pre-configured key is still present and unchanged
    let other_key_results =
        Database::find_sigkeys(&instance, db.root_id(), &other_pubkey_str).await?;
    assert_eq!(
        other_key_results.len(),
        1,
        "Pre-configured key should still be present"
    );
    assert_eq!(
        other_key_results[0].1,
        Permission::Write(5),
        "Pre-configured key should retain its original permission"
    );

    Ok(())
}
