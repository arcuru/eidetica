//! Tests for the instance module.

use super::*;
use crate::{Error, backend::database::InMemory, crdt::Doc};
use std::path::Path;

async fn save_in_memory_backend(instance: &Instance, path: &Path) -> Result<(), Error> {
    let backend = instance.backend().as_arc_backend_impl();
    let in_memory = backend
        .as_any()
        .downcast_ref::<InMemory>()
        .expect("Expected in-memory backend");
    in_memory.save_to_file(path).await
}

async fn load_in_memory_backend(path: &Path) -> Result<InMemory, Error> {
    InMemory::load_from_file(path).await
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
async fn test_create_user() -> Result<(), Error> {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).await?;

    // Create user with password
    let user_uuid = instance
        .create_user("alice", Some("password123"))
        .await
        .unwrap();

    assert!(!user_uuid.is_empty());

    // Verify user appears in list
    let users = instance.list_users().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0], "alice");
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
async fn test_login_user() -> Result<(), Error> {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).await?;

    // Create user
    instance
        .create_user("alice", Some("password123"))
        .await
        .unwrap();

    // Login user
    let user = instance
        .login_user("alice", Some("password123"))
        .await
        .unwrap();
    assert_eq!(user.username(), "alice");

    // Invalid password should fail
    let result = instance.login_user("alice", Some("wrong_password")).await;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn test_new_database() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend))
        .await
        .expect("Failed to create test instance");

    // Create database with User API
    instance.create_user("test", None).await.unwrap();
    let mut user = instance.login_user("test", None).await.unwrap();
    let key_id = user.add_private_key(None).await.unwrap();

    let mut settings = Doc::new();
    settings.set("name", "test_db");
    let database = user.create_database(settings, &key_id).await.unwrap();
    assert_eq!(database.get_name().await.unwrap(), "test_db");
}

#[tokio::test]
async fn test_create_database_with_default_settings() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend))
        .await
        .expect("Failed to create test instance");

    // Create database with User API (default settings via Doc::new())
    instance.create_user("test", None).await.unwrap();
    let mut user = instance.login_user("test", None).await.unwrap();
    let key_id = user.add_private_key(None).await.unwrap();
    let database = user.create_database(Doc::new(), &key_id).await.unwrap();

    // Database should have a valid root_id
    assert!(!database.root_id().is_empty());

    // Database should be loadable via user
    let loaded = user.open_database(database.root_id()).await.unwrap();
    assert_eq!(loaded.root_id(), database.root_id());
}

#[tokio::test]
async fn test_new_database_without_key_fails() -> Result<(), Error> {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).await?;

    // Create user but try to use nonexistent key
    instance.create_user("test", None).await?;
    let mut user = instance.login_user("test", None).await?;

    // Create database requires a valid signing key
    let mut settings = Doc::new();
    settings.set("name", "test_db");

    // This should fail with a nonexistent key_id
    let (_, fake_key) = crate::auth::generate_keypair();
    let result = user.create_database(settings, &fake_key).await;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn test_instance_load_new_backend() -> Result<(), Error> {
    use crate::clock::FixedClock;

    // Test that Instance::load() creates new system state for empty backend
    let backend = InMemory::new();
    let instance =
        Instance::open_with_clock(Box::new(backend), Arc::new(FixedClock::default())).await?;

    // Verify we can create and login a user
    instance.create_user("alice", None).await?;
    let user = instance.login_user("alice", None).await?;
    assert_eq!(user.username(), "alice");

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
async fn test_instance_load_existing_backend() -> Result<(), Error> {
    use crate::clock::FixedClock;

    // Use a temporary file path for testing
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_instance_load.json");

    // Create an instance and user, then save the backend
    let backend1 = InMemory::new();
    let instance1 =
        Instance::open_with_clock(Box::new(backend1), Arc::new(FixedClock::default())).await?;
    instance1.create_user("bob", None).await?;
    let mut user1 = instance1.login_user("bob", None).await?;

    // Get the default key (earliest created key)
    let default_key = user1.get_default_key()?;

    // Create a user database to verify it persists
    let mut settings = Doc::new();
    settings.set("name", "bob_database");
    user1.create_database(settings, &default_key).await?;

    // Save the backend to file
    save_in_memory_backend(&instance1, &path).await?;

    // Drop the first instance
    drop(instance1);
    drop(user1);

    // Load a new backend from the saved file
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 =
        Instance::open_with_clock(Box::new(backend2), Arc::new(FixedClock::default())).await?;

    // Verify the user still exists
    let users = instance2.list_users().await?;
    assert_eq!(users.len(), 1);
    assert_eq!(users[0], "bob");

    // Verify we can login the existing user
    let user2 = instance2.login_user("bob", None).await?;
    assert_eq!(user2.username(), "bob");

    // Clean up the temporary file
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
async fn test_instance_load_device_id_persistence() -> Result<(), Error> {
    // Test that device_id remains the same across reloads
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_device_id.json");

    // Create instance and get device_id
    let backend1 = InMemory::new();
    let instance1 = Instance::open(Box::new(backend1)).await?;
    let device_id1 = instance1.device_id_string();

    // Save backend
    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Load backend and verify device_id is the same
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 = Instance::open(Box::new(backend2)).await?;
    let device_id2 = instance2.device_id_string();

    assert_eq!(
        device_id1, device_id2,
        "Device ID should persist across reloads"
    );

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
async fn test_instance_load_with_password_protected_users() -> Result<(), Error> {
    // Test that password-protected users work correctly after reload
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_password_users.json");

    // Create instance with password-protected user
    let backend1 = InMemory::new();
    let instance1 = Instance::open(Box::new(backend1)).await?;
    instance1
        .create_user("secure_alice", Some("secret123"))
        .await?;
    let user1 = instance1
        .login_user("secure_alice", Some("secret123"))
        .await?;
    assert_eq!(user1.username(), "secure_alice");
    drop(user1);

    // Save backend
    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Reload and verify password still works
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 = Instance::open(Box::new(backend2)).await?;

    // Correct password should work
    let user2 = instance2
        .login_user("secure_alice", Some("secret123"))
        .await?;
    assert_eq!(user2.username(), "secure_alice");

    // Wrong password should fail
    let result = instance2
        .login_user("secure_alice", Some("wrong_password"))
        .await;
    assert!(result.is_err(), "Login with wrong password should fail");

    // No password should fail
    let result = instance2.login_user("secure_alice", None).await;
    assert!(
        result.is_err(),
        "Login without password should fail for password-protected user"
    );

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
async fn test_instance_load_multiple_users() -> Result<(), Error> {
    // Test that multiple users persist correctly
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_multiple_users.json");

    // Create instance with multiple users (mix of passwordless and password-protected)
    let backend1 = InMemory::new();
    let instance1 = Instance::open(Box::new(backend1)).await?;

    instance1.create_user("alice", None).await?;
    instance1.create_user("bob", Some("bobpass")).await?;
    instance1.create_user("charlie", None).await?;
    instance1.create_user("diana", Some("dianapass")).await?;

    // Verify all users can login
    instance1.login_user("alice", None).await?;
    instance1.login_user("bob", Some("bobpass")).await?;
    instance1.login_user("charlie", None).await?;
    instance1.login_user("diana", Some("dianapass")).await?;

    // Save backend
    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Reload and verify all users still exist and can login
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 = Instance::open(Box::new(backend2)).await?;

    let users = instance2.list_users().await?;
    assert_eq!(users.len(), 4, "All 4 users should be present after reload");
    assert!(users.contains(&"alice".to_string()));
    assert!(users.contains(&"bob".to_string()));
    assert!(users.contains(&"charlie".to_string()));
    assert!(users.contains(&"diana".to_string()));

    // Verify login still works for all users
    instance2.login_user("alice", None).await?;
    instance2.login_user("bob", Some("bobpass")).await?;
    instance2.login_user("charlie", None).await?;
    instance2.login_user("diana", Some("dianapass")).await?;

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
async fn test_instance_load_user_databases_persist() -> Result<(), Error> {
    use crate::clock::FixedClock;

    // Test that user-created databases persist across reloads
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_user_dbs.json");

    // Create instance, user, and multiple databases
    let backend1 = InMemory::new();
    let instance1 =
        Instance::open_with_clock(Box::new(backend1), Arc::new(FixedClock::default())).await?;
    instance1.create_user("eve", None).await?;
    let mut user1 = instance1.login_user("eve", None).await?;

    // Get the default key (earliest created key)
    let default_key = user1.get_default_key()?;

    // Create multiple databases
    let mut settings1 = Doc::new();
    settings1.set("name", "database_one");
    settings1.set("purpose", "testing");
    let db1 = user1.create_database(settings1, &default_key).await?;
    let db1_root = db1.root_id().clone();

    let mut settings2 = Doc::new();
    settings2.set("name", "database_two");
    settings2.set("purpose", "production");
    let db2 = user1.create_database(settings2, &default_key).await?;
    let db2_root = db2.root_id().clone();

    drop(db1);
    drop(db2);
    drop(user1);

    // Save backend
    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Reload and verify databases still exist
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 =
        Instance::open_with_clock(Box::new(backend2), Arc::new(FixedClock::default())).await?;
    let user2 = instance2.login_user("eve", None).await?;

    // Load databases by root_id and verify their settings
    let loaded_db1 = user2.open_database(&db1_root).await?;
    assert_eq!(loaded_db1.get_name().await?, "database_one");
    let settings1_doc = loaded_db1.get_settings().await?;
    assert_eq!(settings1_doc.get_string("purpose").await?, "testing");

    let loaded_db2 = user2.open_database(&db2_root).await?;
    assert_eq!(loaded_db2.get_name().await?, "database_two");
    let settings2_doc = loaded_db2.get_settings().await?;
    assert_eq!(settings2_doc.get_string("purpose").await?, "production");

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
async fn test_instance_load_idempotency() -> Result<(), Error> {
    use crate::clock::FixedClock;

    // Test that loading the same backend multiple times gives consistent results
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_idempotency.json");

    // Create and save initial state
    let backend1 = InMemory::new();
    let instance1 =
        Instance::open_with_clock(Box::new(backend1), Arc::new(FixedClock::default())).await?;
    instance1.create_user("frank", None).await?;
    let device_id1 = instance1.device_id_string();

    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Load the same backend multiple times and verify consistency
    for i in 0..3 {
        let backend = load_in_memory_backend(&path).await?;
        let instance =
            Instance::open_with_clock(Box::new(backend), Arc::new(FixedClock::default())).await?;

        // Device ID should be the same every time
        let device_id = instance.device_id_string();
        assert_eq!(
            device_id, device_id1,
            "Device ID should be consistent on reload {i}"
        );

        // User list should be the same
        let users = instance.list_users().await?;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0], "frank");

        // Should be able to login
        let user = instance.login_user("frank", None).await?;
        assert_eq!(user.username(), "frank");

        drop(user);
        drop(instance);
    }

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
async fn test_instance_load_new_vs_existing() -> Result<(), Error> {
    use crate::clock::FixedClock;

    // Test the difference between loading new and existing backends
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_new_vs_existing.json");

    // Create first instance (new backend)
    let backend1 = InMemory::new();
    let instance1 =
        Instance::open_with_clock(Box::new(backend1), Arc::new(FixedClock::default())).await?;
    let device_id1 = instance1.device_id_string();
    instance1.create_user("grace", None).await?;

    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Load existing backend
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 =
        Instance::open_with_clock(Box::new(backend2), Arc::new(FixedClock::default())).await?;
    let device_id2 = instance2.device_id_string();

    // Device ID should match (existing backend)
    assert_eq!(device_id1, device_id2);

    // User should exist (existing backend)
    let users = instance2.list_users().await?;
    assert_eq!(users.len(), 1);
    assert_eq!(users[0], "grace");
    drop(instance2);

    // Create completely new instance (different backend)
    let backend3 = InMemory::new();
    let instance3 =
        Instance::open_with_clock(Box::new(backend3), Arc::new(FixedClock::default())).await?;
    let device_id3 = instance3.device_id_string();

    // Device ID should be different (new backend)
    assert_ne!(device_id1, device_id3);

    // No users should exist (new backend)
    let users = instance3.list_users().await?;
    assert_eq!(users.len(), 0);

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
async fn test_instance_create_strict_fails_on_existing() -> Result<(), Error> {
    // Test that Instance::create() fails on already-initialized backend
    use crate::clock::FixedClock;

    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_create_strict.json");

    // Create first instance
    let backend1 = Arc::new(InMemory::new());
    let instance1 = Instance::create_internal(backend1, Arc::new(FixedClock::default())).await?;
    instance1.create_user("alice", None).await?;

    // Save backend
    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Try to create() on the existing backend - should fail
    // (fails immediately before clock is used, so SystemClock is fine)
    let backend2 = load_in_memory_backend(&path).await?;
    let result = Instance::create(Box::new(backend2)).await;
    assert!(result.is_err(), "create() should fail on existing backend");

    // Verify error type
    if let Err(err) = result {
        if let crate::Error::Instance(instance_err) = err {
            assert!(
                instance_err.is_already_exists(),
                "Error should be InstanceAlreadyExists"
            );
        } else {
            panic!("Expected Instance error");
        }
    }

    // Verify open() still works
    let backend3 = load_in_memory_backend(&path).await?;
    let instance3 =
        Instance::open_with_clock(Box::new(backend3), Arc::new(FixedClock::default())).await?;
    let users = instance3.list_users().await?;
    assert_eq!(users.len(), 1);
    assert_eq!(users[0], "alice");

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses SystemTime for timestamps in create_user
async fn test_instance_create_on_fresh_backend() -> Result<(), Error> {
    // Test that Instance::create() succeeds on fresh backend
    let backend = InMemory::new();
    let instance = Instance::create(Box::new(backend)).await?;

    // Verify we can create users
    instance.create_user("bob", None).await?;
    let user = instance.login_user("bob", None).await?;
    assert_eq!(user.username(), "bob");

    Ok(())
}
