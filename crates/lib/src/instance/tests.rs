//! Tests for the instance module.

use super::*;
use crate::{Error, NewUser, backend::database::InMemory, crdt::Doc, user::User};
use std::path::Path;

async fn save_in_memory_backend(instance: &Instance, path: &Path) -> Result<(), Error> {
    let backend = instance.require_local_engine().expect("local backend");
    let in_memory = backend
        .as_any()
        .downcast_ref::<InMemory>()
        .expect("Expected in-memory backend");
    in_memory.save_to_file(path).await
}

async fn load_in_memory_backend(path: &Path) -> Result<InMemory, Error> {
    InMemory::load_from_file(path).await
}

/// List users via an admin User session.
///
/// `Instance::list_users` was removed — listing users reads `_users` and is
/// an admin operation reached through [`User::admin`].
async fn list_users_via(admin: &User) -> Result<Vec<String>, Error> {
    admin.admin().await?.list_users().await
}

/// Convenience: create a fresh instance with `admin` (passwordless) as the
/// initial admin user. Mirrors the test-harness default and gives every test
/// a logged-in admin to play with.
async fn instance_with_admin() -> Result<(Instance, User), Error> {
    Instance::create(Box::new(InMemory::new()), NewUser::passwordless("admin")).await
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
async fn test_create_user() -> Result<(), Error> {
    let (instance, admin) = instance_with_admin().await?;

    // Admin creates a password-protected user via the admin path.
    let alice_uuid = admin
        .admin()
        .await?
        .create_user(NewUser::with_password("alice", "password123"))
        .await?;
    assert!(!alice_uuid.is_empty());

    // Verify both users appear in list.
    let users = list_users_via(&admin).await?;
    assert_eq!(users.len(), 2, "Should have 2 users (admin + alice)");
    assert!(users.contains(&"admin".to_string()), "Admin should exist");
    assert!(users.contains(&"alice".to_string()), "Alice should exist");

    // Confirm alice can log in with her password.
    let alice_reloaded = instance.login_user("alice", Some("password123")).await?;
    assert_eq!(alice_reloaded.username(), "alice");
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
async fn test_login_user() -> Result<(), Error> {
    // Bootstrap alice as the initial admin with a password.
    let (instance, _alice) = Instance::create(
        Box::new(InMemory::new()),
        NewUser::with_password("alice", "password123"),
    )
    .await?;

    // Correct password should work.
    let user = instance.login_user("alice", Some("password123")).await?;
    assert_eq!(user.username(), "alice");

    // Invalid password should fail.
    let result = instance.login_user("alice", Some("wrong_password")).await;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn test_new_database() {
    // Bootstrap "test" as the initial user.
    let (_instance, mut user) =
        Instance::create(Box::new(InMemory::new()), NewUser::passwordless("test"))
            .await
            .expect("Failed to create test instance");

    let key_id = user.add_private_key(None).await.unwrap();

    let mut settings = Doc::new();
    settings.set("name", "test_db");
    let database = user.create_database(settings, &key_id).await.unwrap();
    assert_eq!(database.get_name().await.unwrap(), "test_db");
}

#[tokio::test]
async fn test_create_database_with_default_settings() {
    let (_instance, mut user) =
        Instance::create(Box::new(InMemory::new()), NewUser::passwordless("test"))
            .await
            .expect("Failed to create test instance");

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
    let (_instance, mut user) =
        Instance::create(Box::new(InMemory::new()), NewUser::passwordless("test")).await?;

    // Database creation requires a valid signing key.
    let mut settings = Doc::new();
    settings.set("name", "test_db");

    // This should fail with a nonexistent key_id.
    let (_, fake_key) = crate::auth::generate_keypair();
    let result = user.create_database(settings, &fake_key).await;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn test_instance_load_new_backend() -> Result<(), Error> {
    use crate::clock::FixedClock;

    // Initialise a fresh backend with alice as initial user, with an
    // injectable clock for deterministic timestamps.
    let (_instance, user) = Instance::create_with_clock(
        Box::new(InMemory::new()),
        Arc::new(FixedClock::default()),
        NewUser::passwordless("alice"),
    )
    .await?;

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
    let (instance1, mut user1) = Instance::create_with_clock(
        Box::new(InMemory::new()),
        Arc::new(FixedClock::default()),
        NewUser::passwordless("bob"),
    )
    .await?;

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

    // Load a new backend from the saved file — Instance::open is load-only
    // and must find existing metadata.
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 =
        Instance::open_with_clock(Box::new(backend2), Arc::new(FixedClock::default())).await?;

    // Verify the bob user still exists.
    let bob = instance2.login_user("bob", None).await?;
    let users = list_users_via(&bob).await?;
    assert_eq!(users.len(), 1, "Should have 1 user (bob)");
    assert!(users.contains(&"bob".to_string()), "Bob should exist");

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
    let (instance1, _user) =
        Instance::create(Box::new(InMemory::new()), NewUser::passwordless("admin")).await?;
    let device_id1 = instance1.id().to_string();

    // Save backend
    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Load backend and verify device_id is the same
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 = Instance::open(Box::new(backend2)).await?;
    let device_id2 = instance2.id().to_string();

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

    // Bootstrap a password-protected user as initial admin.
    let (instance1, user1) = Instance::create(
        Box::new(InMemory::new()),
        NewUser::with_password("secure_alice", "secret123"),
    )
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

    // Bootstrap admin and let admin create the other users.
    let (instance1, admin1) = instance_with_admin().await?;
    let admin_view = admin1.admin().await?;
    admin_view
        .create_user(NewUser::passwordless("alice"))
        .await?;
    admin_view
        .create_user(NewUser::with_password("bob", "bobpass"))
        .await?;
    admin_view
        .create_user(NewUser::passwordless("charlie"))
        .await?;
    admin_view
        .create_user(NewUser::with_password("diana", "dianapass"))
        .await?;

    // Verify all users can login
    instance1.login_user("alice", None).await?;
    instance1.login_user("bob", Some("bobpass")).await?;
    instance1.login_user("charlie", None).await?;
    instance1.login_user("diana", Some("dianapass")).await?;

    // Save backend
    save_in_memory_backend(&instance1, &path).await?;
    drop(admin1);
    drop(instance1);

    // Reload and verify all users still exist and can login
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 = Instance::open(Box::new(backend2)).await?;
    let admin2 = instance2.login_user("admin", None).await?;

    let users = list_users_via(&admin2).await?;
    assert_eq!(
        users.len(),
        5,
        "All 5 users (admin + 4 created) should be present after reload"
    );
    assert!(users.contains(&"admin".to_string()), "Admin should exist");
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

    // Bootstrap eve as the initial user.
    let (instance1, mut user1) = Instance::create_with_clock(
        Box::new(InMemory::new()),
        Arc::new(FixedClock::default()),
        NewUser::passwordless("eve"),
    )
    .await?;

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
    let (instance1, _frank) = Instance::create_with_clock(
        Box::new(InMemory::new()),
        Arc::new(FixedClock::default()),
        NewUser::passwordless("frank"),
    )
    .await?;
    let device_id1 = instance1.id().to_string();

    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Load the same backend multiple times and verify consistency
    for i in 0..3 {
        let backend = load_in_memory_backend(&path).await?;
        let instance =
            Instance::open_with_clock(Box::new(backend), Arc::new(FixedClock::default())).await?;

        // Device ID should be the same every time
        let device_id = instance.id().to_string();
        assert_eq!(
            device_id, device_id1,
            "Device ID should be consistent on reload {i}"
        );

        // User list should be the same (just frank).
        let user = instance.login_user("frank", None).await?;
        assert_eq!(user.username(), "frank");
        let users = list_users_via(&user).await?;
        assert_eq!(users.len(), 1, "Should have 1 user (frank)");
        assert!(users.contains(&"frank".to_string()), "Frank should exist");

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

    // Create first instance with grace as initial user.
    let (instance1, _grace) = Instance::create_with_clock(
        Box::new(InMemory::new()),
        Arc::new(FixedClock::default()),
        NewUser::passwordless("grace"),
    )
    .await?;
    let device_id1 = instance1.id().to_string();

    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Load existing backend
    let backend2 = load_in_memory_backend(&path).await?;
    let instance2 =
        Instance::open_with_clock(Box::new(backend2), Arc::new(FixedClock::default())).await?;
    let device_id2 = instance2.id().to_string();

    // Device ID should match (existing backend)
    assert_eq!(device_id1, device_id2);

    // Grace should still exist (existing backend).
    let grace = instance2.login_user("grace", None).await?;
    let users = list_users_via(&grace).await?;
    assert_eq!(users.len(), 1, "Should have 1 user (grace)");
    assert!(users.contains(&"grace".to_string()), "Grace should exist");
    drop(instance2);

    // Create a separate fresh instance (different backend) — distinct device id.
    let (instance3, henry) = Instance::create_with_clock(
        Box::new(InMemory::new()),
        Arc::new(FixedClock::default()),
        NewUser::passwordless("henry"),
    )
    .await?;
    let device_id3 = instance3.id().to_string();

    // Device ID should be different (new backend).
    assert_ne!(device_id1, device_id3);

    // Only the initial user (henry) should exist on the new backend.
    let users = list_users_via(&henry).await?;
    assert_eq!(users.len(), 1, "Should have 1 user (henry)");
    assert!(users.contains(&"henry".to_string()), "Henry should exist");

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
async fn test_instance_create_strict_fails_on_existing() -> Result<(), Error> {
    // Test that Instance::create() fails on already-initialized backend.
    use crate::clock::FixedClock;

    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_create_strict.json");

    // Create first instance with alice as initial user.
    let (instance1, _alice) = Instance::create_with_clock(
        Box::new(InMemory::new()),
        Arc::new(FixedClock::default()),
        NewUser::passwordless("alice"),
    )
    .await?;

    // Save backend
    save_in_memory_backend(&instance1, &path).await?;
    drop(instance1);

    // Try to create() on the existing backend - should fail.
    let backend2 = load_in_memory_backend(&path).await?;
    let result = Instance::create(Box::new(backend2), NewUser::passwordless("bob")).await;
    assert!(result.is_err(), "create() should fail on existing backend");

    // Verify error type
    if let Err(err) = result {
        if let crate::Error::Instance(ref instance_err) = err {
            assert!(
                instance_err.is_already_exists(),
                "Error should be InstanceAlreadyExists"
            );
        } else {
            panic!("Expected Instance error");
        }
    }

    // Verify open() still works on the existing backend.
    let backend3 = load_in_memory_backend(&path).await?;
    let instance3 =
        Instance::open_with_clock(Box::new(backend3), Arc::new(FixedClock::default())).await?;
    let alice = instance3.login_user("alice", None).await?;
    let users = list_users_via(&alice).await?;
    assert_eq!(users.len(), 1, "Should have 1 user (alice)");
    assert!(users.contains(&"alice".to_string()), "Alice should exist");

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses SystemTime for timestamps in create_user
async fn test_instance_create_on_fresh_backend() -> Result<(), Error> {
    // Test that Instance::create() succeeds on fresh backend.
    let (instance, bob) =
        Instance::create(Box::new(InMemory::new()), NewUser::passwordless("bob")).await?;
    assert_eq!(bob.username(), "bob");

    // Bob can immediately log back in.
    let bob_reloaded = instance.login_user("bob", None).await?;
    assert_eq!(bob_reloaded.username(), "bob");

    Ok(())
}

#[tokio::test]
async fn test_instance_open_fails_on_empty_backend() -> Result<(), Error> {
    // `Instance::open` is load-only. An empty backend must error with
    // NotInitialized rather than auto-bootstrapping.
    let backend = InMemory::new();
    let result = Instance::open(Box::new(backend)).await;
    let err = result.expect_err("open() must reject an uninitialised backend");

    if let crate::Error::Instance(boxed) = &err {
        assert!(
            matches!(boxed.as_ref(), InstanceError::NotInitialized),
            "Expected InstanceError::NotInitialized, got {err:?}"
        );
    } else {
        panic!("Expected an Instance error, got {err:?}");
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
async fn test_open_or_create_fresh_and_existing() -> Result<(), Error> {
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("eidetica_test_open_or_create.json");

    // Fresh: should return Some(User) for the just-created initial user.
    let (instance1, maybe_user) =
        Instance::open_or_create(Box::new(InMemory::new()), NewUser::passwordless("alice")).await?;
    let alice = maybe_user.expect("fresh backend should yield the initial user");
    assert_eq!(alice.username(), "alice");

    save_in_memory_backend(&instance1, &path).await?;
    drop(alice);
    drop(instance1);

    // Existing: should return None (caller logs in explicitly).
    let backend2 = load_in_memory_backend(&path).await?;
    let (instance2, maybe_user) = Instance::open_or_create(
        Box::new(backend2),
        // The supplied NewUser is unused on the existing-instance branch;
        // we still have to provide one because the signature requires it.
        NewUser::passwordless("alice"),
    )
    .await?;
    assert!(
        maybe_user.is_none(),
        "existing instance should not produce a NewUser"
    );

    // Login should still work for the persisted alice.
    let alice2 = instance2.login_user("alice", None).await?;
    assert_eq!(alice2.username(), "alice");

    // Clean up
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    Ok(())
}
