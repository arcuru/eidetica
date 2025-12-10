//! Tests for User database tracking functionality

use eidetica::{
    Database,
    auth::{
        crypto::{format_public_key, generate_keypair},
        settings::AuthSettings,
        types::{AuthKey, Permission},
    },
    crdt::Doc,
    user::types::{SyncSettings, TrackedDatabase},
};

use super::helpers::{login_user, setup_instance};

/// Test tracking a database
#[test]
fn test_track_database() -> eidetica::Result<()> {
    // Create instance with a database that has global permissions
    let instance = setup_instance();

    // Create a user
    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let user_key = user.get_default_key()?;

    // Create a database with global Write permission
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);

    let mut db_settings = Doc::new();
    db_settings.set("name", "shared_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    // Track the database
    let prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: user_key.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: Some(60),
            properties: Default::default(),
        },
    };

    // Add a database to the test user, which uses the global permissions
    user.track_database(prefs)?;

    // Verify it was added
    let tracked_dbs = user.databases()?;
    assert_eq!(tracked_dbs.len(), 1);
    assert_eq!(tracked_dbs[0].database_id, db_id);
    assert_eq!(tracked_dbs[0].key_id, user_key);
    assert!(tracked_dbs[0].sync_settings.sync_enabled);

    Ok(())
}

/// Test that tracking a database with no available SigKey returns an error
#[test]
fn test_track_database_no_sigkey_error() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let user_key = user.get_default_key()?;

    // Create a database without global permissions (user has no access)
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);
    instance
        .backend()
        .store_private_key("alice_key", alice_key.clone())?;

    let mut db_settings = Doc::new();
    db_settings.set("name", "private_db");

    let mut auth_settings = AuthSettings::new();
    // Only Alice has access, no global permission
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    let prefs = TrackedDatabase {
        database_id: db_id,
        key_id: user_key,
        sync_settings: Default::default(),
    };

    // Try to add database - should fail because user has no SigKey
    let result = user.track_database(prefs);
    assert!(result.is_err());

    Ok(())
}

/// Test listing tracked databases
#[test]
fn test_list_databases() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let user_key = user.get_default_key()?;

    // Initially should be empty
    assert_eq!(user.databases()?.len(), 0);

    // Create and add multiple databases
    for i in 0..3 {
        let (alice_key, alice_pubkey) = generate_keypair();
        let alice_pubkey_str = format_public_key(&alice_pubkey);

        let mut db_settings = Doc::new();
        db_settings.set("name", format!("db_{i}"));

        let mut auth_settings = AuthSettings::new();
        auth_settings.add_key(
            format!("alice_{i}"),
            AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
        )?;
        auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
        db_settings.set("auth", auth_settings.as_doc().clone());

        let db = Database::create(db_settings, &instance, alice_key, format!("alice_{i}"))?;

        let prefs = TrackedDatabase {
            database_id: db.root_id().clone(),
            key_id: user_key.clone(),
            sync_settings: Default::default(),
        };

        user.track_database(prefs)?;
    }

    // Should now have 3 databases
    let tracked = user.databases()?;
    assert_eq!(tracked.len(), 3);

    Ok(())
}

/// Test getting a specific tracked database
#[test]
fn test_get_tracked_database() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let user_key = user.get_default_key()?;

    // Create database
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    // Add database
    let prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: user_key.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: true,
            interval_seconds: Some(30),
            properties: Default::default(),
        },
    };

    user.track_database(prefs)?;

    // Get preferences
    let retrieved = user.database(&db_id)?;
    assert_eq!(retrieved.database_id, db_id);
    assert_eq!(retrieved.key_id, user_key);
    assert!(retrieved.sync_settings.sync_enabled);
    assert!(retrieved.sync_settings.sync_on_commit);
    assert_eq!(retrieved.sync_settings.interval_seconds, Some(30));

    Ok(())
}

/// Test updating a tracked database (upsert behavior)
#[test]
fn test_update_tracked_database() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let user_key = user.get_default_key()?;

    // Create database
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    // Add database with initial settings
    let initial_prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: user_key.clone(),
        sync_settings: SyncSettings {
            sync_enabled: false,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    };

    user.track_database(initial_prefs)?;

    // Update by calling track_database again (upsert)
    let updated_prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: user_key.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: true,
            interval_seconds: Some(60),
            properties: Default::default(),
        },
    };

    user.track_database(updated_prefs)?;

    // Verify updates succeeded
    let retrieved = user.database(&db_id)?;
    assert!(retrieved.sync_settings.sync_enabled);
    assert!(retrieved.sync_settings.sync_on_commit);
    assert_eq!(retrieved.sync_settings.interval_seconds, Some(60));

    Ok(())
}

/// Test untracking a database
#[test]
fn test_untrack_database() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let user_key = user.get_default_key()?;

    // Create database
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    // Add database
    let prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: user_key,
        sync_settings: Default::default(),
    };

    user.track_database(prefs)?;
    assert_eq!(user.databases()?.len(), 1);

    // Remove database
    user.untrack_database(&db_id)?;
    assert_eq!(user.databases()?.len(), 0);

    // Try to get preferences for removed database - should fail
    let result = user.database(&db_id);
    assert!(result.is_err());

    Ok(())
}

/// Test that user can open a tracked database
#[test]
fn test_load_tracked_database() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let user_key = user.get_default_key()?;

    // Create database with global permission
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    // Add to user's tracked databases
    let prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: user_key,
        sync_settings: Default::default(),
    };

    user.track_database(prefs)?;

    // Open the database
    let loaded_db = user.open_database(&db_id)?;
    assert_eq!(loaded_db.root_id(), &db_id);
    assert_eq!(loaded_db.get_name()?, "test_db");

    Ok(())
}

/// Test updating tracked database with valid key change (auto-creates mapping)
#[test]
fn test_update_tracked_valid_key_change() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let key1 = user.get_default_key()?;

    // Add a second key to the user
    let key2 = user.add_private_key(Some("Second Key"))?;

    // Create database with global permission
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);
    instance
        .backend()
        .store_private_key("alice_key", alice_key.clone())?;

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    // Add database with key1
    let prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: key1.clone(),
        sync_settings: Default::default(),
    };
    user.track_database(prefs)?;

    // Update preferences to use key2 - should succeed and auto-create mapping
    let updated_prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: key2.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: true,
            interval_seconds: Some(120),
            properties: Default::default(),
        },
    };
    user.track_database(updated_prefs)?;

    // Verify the update
    let retrieved = user.database(&db_id)?;
    assert_eq!(retrieved.key_id, key2);
    assert!(retrieved.sync_settings.sync_enabled);
    assert!(retrieved.sync_settings.sync_on_commit);
    assert_eq!(retrieved.sync_settings.interval_seconds, Some(120));

    Ok(())
}

/// Test updating tracked database with non-existent key fails
#[test]
fn test_update_tracked_nonexistent_key_fails() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let user_key = user.get_default_key()?;

    // Create database with global permission
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);
    instance
        .backend()
        .store_private_key("alice_key", alice_key.clone())?;

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    // Add database
    let prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: user_key,
        sync_settings: Default::default(),
    };
    user.track_database(prefs)?;

    // Try to update with non-existent key - should fail with KeyNotFound
    let fake_key_id = "Ed25519:fake_nonexistent_key_12345".to_string();
    let invalid_prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: fake_key_id,
        sync_settings: Default::default(),
    };

    let result = user.track_database(invalid_prefs);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Key not found"));

    Ok(())
}

/// Test updating tracked database with key that has no database access fails
#[test]
fn test_update_tracked_no_access_fails() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let key1 = user.get_default_key()?;

    // Add a second key
    let key2 = user.add_private_key(Some("Second Key"))?;

    // Create database WITHOUT global permission - only alice has access
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);
    instance
        .backend()
        .store_private_key("alice_key", alice_key.clone())?;

    let mut db_settings = Doc::new();
    db_settings.set("name", "private_db");

    let mut auth_settings = AuthSettings::new();
    // Only alice has access - no global permission
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(
        db_settings,
        &instance,
        alice_key.clone(),
        "alice".to_string(),
    )?;
    let db_id = db.root_id().clone();

    // Give key1 explicit access by adding it to the database
    let key1_pubkey = user.get_public_key(&key1)?;
    let tx = db.new_transaction()?;
    let settings_store = tx.get_settings()?;
    settings_store.update_auth_settings(|auth| {
        auth.add_key(
            "user_key1",
            AuthKey::active(&key1_pubkey, Permission::Write(5))?,
        )
    })?;
    tx.commit()?;

    // Add database using key1
    let prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: key1,
        sync_settings: Default::default(),
    };
    user.track_database(prefs)?;

    // Try to update to key2 which has NO access to database - should fail
    let invalid_prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: key2,
        sync_settings: Default::default(),
    };

    let result = user.track_database(invalid_prefs);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No SigKey found"));

    Ok(())
}

/// Test updating tracked database auto-creates mapping for key with global access
#[test]
fn test_update_tracked_auto_creates_mapping() -> eidetica::Result<()> {
    let instance = setup_instance();

    instance.create_user("test_user", None)?;
    let mut user = login_user(&instance, "test_user", None);
    let key1 = user.get_default_key()?;

    // Add a second key
    let key2 = user.add_private_key(Some("Second Key"))?;

    // Create database with global permission (both keys can access)
    let (alice_key, alice_pubkey) = generate_keypair();
    let alice_pubkey_str = format_public_key(&alice_pubkey);

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "alice",
        AuthKey::active(&alice_pubkey_str, Permission::Admin(1))?,
    )?;
    auth_settings.add_key("*", AuthKey::active("*", Permission::Write(10))?)?;
    db_settings.set("auth", auth_settings.as_doc().clone());

    let db = Database::create(db_settings, &instance, alice_key, "alice".to_string())?;
    let db_id = db.root_id().clone();

    // Add database with key1 (creates mapping: key1 -> "*")
    let prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: key1.clone(),
        sync_settings: Default::default(),
    };
    user.track_database(prefs)?;

    // Update to key2 - should succeed and auto-create mapping
    // key2 CAN access the database (via "*"), mapping will be auto-created
    let updated_prefs = TrackedDatabase {
        database_id: db_id.clone(),
        key_id: key2.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: Some(90),
            properties: Default::default(),
        },
    };

    // Should succeed - auto-creates the mapping
    user.track_database(updated_prefs)?;

    // Verify the update succeeded
    let retrieved = user.database(&db_id)?;
    assert_eq!(retrieved.key_id, key2);
    assert!(retrieved.sync_settings.sync_enabled);
    assert_eq!(retrieved.sync_settings.interval_seconds, Some(90));

    // Verify the mapping was auto-created
    let mapping = user.key_mapping(&key2, &db_id)?;
    assert_eq!(mapping, Some("*".to_string()));

    Ok(())
}
