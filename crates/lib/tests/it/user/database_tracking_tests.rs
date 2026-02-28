//! Tests for User database tracking functionality

use eidetica::{
    Database, Result,
    auth::{
        crypto::generate_keypair,
        types::{AuthKey, Permission},
    },
    crdt::Doc,
    user::types::SyncSettings,
};

use super::helpers::{login_user, setup_instance};
use crate::helpers::add_auth_key;

/// Test tracking a database
#[tokio::test]
async fn test_track_database() -> Result<()> {
    // Create instance with a database that has global permissions
    let instance = setup_instance().await;

    // Create a user
    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let user_key = user.get_default_key()?;

    // Create a database with global Write permission
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "shared_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;

    // Add global Write permission (signing key is already Admin(0))
    add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;
    let db_id = db.root_id().clone();

    // Track the database
    // Add a database to the test user, which uses the global permissions
    user.track_database(db_id.clone(), &user_key, SyncSettings::enabled())
        .await?;

    // Verify it was added
    let tracked_dbs = user.databases().await?;
    assert_eq!(tracked_dbs.len(), 1);
    assert_eq!(tracked_dbs[0].database_id, db_id);
    assert_eq!(tracked_dbs[0].key_id, user_key);
    assert!(tracked_dbs[0].sync_settings.sync_enabled);

    Ok(())
}

/// Test that tracking a database with no available SigKey returns an error
#[tokio::test]
async fn test_track_database_no_sigkey_error() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let user_key = user.get_default_key()?;

    // Create a database without global permissions (user has no access)
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "private_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;
    let db_id = db.root_id().clone();

    // Try to add database - should fail because user has no SigKey
    let result = user
        .track_database(db_id, &user_key, SyncSettings::disabled())
        .await;
    assert!(result.is_err());

    Ok(())
}

/// Test listing tracked databases
#[tokio::test]
async fn test_list_databases() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let user_key = user.get_default_key()?;

    // Initially should be empty
    assert_eq!(user.databases().await?.len(), 0);

    // Create and add multiple databases
    for i in 0..3 {
        let (alice_key, _) = generate_keypair();

        let mut db_settings = Doc::new();
        db_settings.set("name", format!("db_{i}"));

        let db = Database::create(&instance, alice_key, db_settings).await?;

        // Add global Write permission (signing key is already Admin(0))
        add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;

        user.track_database(db.root_id().clone(), &user_key, SyncSettings::disabled())
            .await?;
    }

    // Should now have 3 databases
    let tracked = user.databases().await?;
    assert_eq!(tracked.len(), 3);

    Ok(())
}

/// Test getting a specific tracked database
#[tokio::test]
async fn test_get_tracked_database() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let user_key = user.get_default_key()?;

    // Create database
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;
    let db_id = db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;

    // Add database
    user.track_database(
        db_id.clone(),
        &user_key,
        SyncSettings::on_commit().with_interval(30),
    )
    .await?;

    // Get preferences
    let retrieved = user.database(&db_id).await?;
    assert_eq!(retrieved.database_id, db_id);
    assert_eq!(retrieved.key_id, user_key);
    assert!(retrieved.sync_settings.sync_enabled);
    assert!(retrieved.sync_settings.sync_on_commit);
    assert_eq!(retrieved.sync_settings.interval_seconds, Some(30));

    Ok(())
}

/// Test updating a tracked database (upsert behavior)
#[tokio::test]
async fn test_update_tracked_database() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let user_key = user.get_default_key()?;

    // Create database
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;
    let db_id = db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;

    // Add database with initial settings
    user.track_database(db_id.clone(), &user_key, SyncSettings::disabled())
        .await?;

    // Update by calling track_database again (upsert)
    user.track_database(
        db_id.clone(),
        &user_key,
        SyncSettings::on_commit().with_interval(60),
    )
    .await?;

    // Verify updates succeeded
    let retrieved = user.database(&db_id).await?;
    assert!(retrieved.sync_settings.sync_enabled);
    assert!(retrieved.sync_settings.sync_on_commit);
    assert_eq!(retrieved.sync_settings.interval_seconds, Some(60));

    Ok(())
}

/// Test untracking a database
#[tokio::test]
async fn test_untrack_database() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let user_key = user.get_default_key()?;

    // Create database
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;
    let db_id = db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;

    // Add database
    user.track_database(db_id.clone(), &user_key, SyncSettings::disabled())
        .await?;
    assert_eq!(user.databases().await?.len(), 1);

    // Remove database
    user.untrack_database(&db_id).await?;
    assert_eq!(user.databases().await?.len(), 0);

    // Try to get preferences for removed database - should fail
    let result = user.database(&db_id).await;
    assert!(result.is_err());

    Ok(())
}

/// Test that user can open a tracked database
#[tokio::test]
async fn test_load_tracked_database() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let user_key = user.get_default_key()?;

    // Create database with global permission
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;
    let db_id = db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;

    // Add to user's tracked databases
    user.track_database(db_id.clone(), &user_key, SyncSettings::disabled())
        .await?;

    // Open the database
    let loaded_db = user.open_database(&db_id).await?;
    assert_eq!(loaded_db.root_id(), &db_id);
    assert_eq!(loaded_db.get_name().await?, "test_db");

    Ok(())
}

/// Test updating tracked database with valid key change (auto-creates mapping)
#[tokio::test]
async fn test_update_tracked_valid_key_change() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let key1 = user.get_default_key()?;

    // Add a second key to the user
    let key2 = user.add_private_key(Some("Second Key")).await?;

    // Create database with global permission
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;
    let db_id = db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;

    // Add database with key1
    user.track_database(db_id.clone(), &key1, SyncSettings::disabled())
        .await?;

    // Update preferences to use key2 - should succeed and auto-create mapping
    user.track_database(
        db_id.clone(),
        &key2,
        SyncSettings::on_commit().with_interval(120),
    )
    .await?;

    // Verify the update
    let retrieved = user.database(&db_id).await?;
    assert_eq!(retrieved.key_id, key2);
    assert!(retrieved.sync_settings.sync_enabled);
    assert!(retrieved.sync_settings.sync_on_commit);
    assert_eq!(retrieved.sync_settings.interval_seconds, Some(120));

    Ok(())
}

/// Test updating tracked database with non-existent key fails
#[tokio::test]
async fn test_update_tracked_nonexistent_key_fails() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let user_key = user.get_default_key()?;

    // Create database with global permission
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;
    let db_id = db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;

    // Add database
    user.track_database(db_id.clone(), &user_key, SyncSettings::disabled())
        .await?;

    // Try to update with non-existent key - should fail with KeyNotFound
    let (_, fake_key_id) = generate_keypair();
    let result = user
        .track_database(db_id.clone(), &fake_key_id, SyncSettings::disabled())
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Key not found"));

    Ok(())
}

/// Test updating tracked database with key that has no database access fails
#[tokio::test]
async fn test_update_tracked_no_access_fails() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let key1 = user.get_default_key()?;

    // Add a second key
    let key2 = user.add_private_key(Some("Second Key")).await?;

    // Create database WITHOUT global permission - only alice has access
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "private_db");

    let db = Database::create(&instance, alice_key.clone(), db_settings).await?;
    let db_id = db.root_id().clone();

    // Give key1 explicit access by adding it to the database
    let key1_pubkey = key1.to_string();
    let tx = db.new_transaction().await?;
    let settings_store = tx.get_settings()?;
    settings_store
        .set_auth_key(
            &key1_pubkey,
            AuthKey::active(Some("user_key1"), Permission::Write(5)),
        )
        .await?;
    tx.commit().await?;

    // Add database using key1
    user.track_database(db_id.clone(), &key1, SyncSettings::disabled())
        .await?;

    // Try to update to key2 which has NO access to database - should fail
    let result = user
        .track_database(db_id.clone(), &key2, SyncSettings::disabled())
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No SigKey found"));

    Ok(())
}

/// Test updating tracked database auto-creates mapping for key with global access
#[tokio::test]
async fn test_update_tracked_auto_creates_mapping() -> Result<()> {
    let instance = setup_instance().await;

    instance.create_user("test_user", None).await?;
    let mut user = login_user(&instance, "test_user", None).await;
    let key1 = user.get_default_key()?;

    // Add a second key
    let key2 = user.add_private_key(Some("Second Key")).await?;

    // Create database with global permission (both keys can access)
    let (alice_key, _) = generate_keypair();

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let db = Database::create(&instance, alice_key, db_settings).await?;
    let db_id = db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_key(&db, "*", AuthKey::active(None, Permission::Write(10))).await;

    // Add database with key1 (creates mapping: key1 -> "*")
    user.track_database(db_id.clone(), &key1, SyncSettings::disabled())
        .await?;

    // Update to key2 - should succeed and auto-create mapping
    // key2 CAN access the database (via "*"), mapping will be auto-created
    // Should succeed - auto-creates the mapping
    user.track_database(
        db_id.clone(),
        &key2,
        SyncSettings::enabled().with_interval(90),
    )
    .await?;

    // Verify the update succeeded
    let retrieved = user.database(&db_id).await?;
    assert_eq!(retrieved.key_id, key2);
    assert!(retrieved.sync_settings.sync_enabled);
    assert_eq!(retrieved.sync_settings.interval_seconds, Some(90));

    // Verify the mapping was auto-created (global permission)
    let mapping = user.key_mapping(&key2, &db_id)?;
    assert!(
        mapping.as_ref().map(|s| s.is_global()).unwrap_or(false),
        "Expected global permission mapping, got: {:?}",
        mapping
    );

    Ok(())
}
