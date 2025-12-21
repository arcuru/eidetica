//! Database operations tests: create, load, and manage user databases
//!
//! Tests database operations including:
//! - Creating new databases
//! - Loading existing databases
//! - Finding keys for databases
//! - Database metadata and settings

use super::helpers::*;

// ===== CREATE DATABASE TESTS =====

#[tokio::test]
async fn test_create_database_passwordless_user() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create a database
    let database = create_named_database(&mut user, "Test Database").await;

    // Verify database was created
    assert!(
        !database.root_id().to_string().is_empty(),
        "Database should have a root ID"
    );
}

#[tokio::test]
async fn test_create_database_with_custom_name() {
    let (instance, username) = setup_instance_with_user("bob", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create database with specific name
    let db_name = "My Custom Database";
    let database = create_named_database(&mut user, db_name).await;

    // Verify name is set
    assert_database_name(&database, db_name).await;
}

#[tokio::test]
async fn test_create_multiple_databases() {
    let (instance, username) = setup_instance_with_user("charlie", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create 3 databases
    let db1 = create_named_database(&mut user, "Database 1").await;
    let db2 = create_named_database(&mut user, "Database 2").await;
    let db3 = create_named_database(&mut user, "Database 3").await;

    // Verify all have unique root IDs
    assert_ne!(db1.root_id(), db2.root_id());
    assert_ne!(db2.root_id(), db3.root_id());
    assert_ne!(db1.root_id(), db3.root_id());

    // Verify names
    assert_database_name(&db1, "Database 1").await;
    assert_database_name(&db2, "Database 2").await;
    assert_database_name(&db3, "Database 3").await;
}

#[tokio::test]
async fn test_database_has_unique_id() {
    let (instance, username) = setup_instance_with_user("diana", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create two databases with same name
    let db1 = create_named_database(&mut user, "Same Name").await;
    let db2 = create_named_database(&mut user, "Same Name").await;

    // IDs should be different even with same name (entropy ensures uniqueness)
    assert_ne!(
        db1.root_id(),
        db2.root_id(),
        "Databases should have unique IDs even with same name"
    );
}

// ===== LOAD DATABASE TESTS =====

#[tokio::test]
async fn test_load_database_after_creation() {
    let (instance, username) = setup_instance_with_user("eve", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create and get ID
    let (db1, db_id) = create_database_with_id(&mut user, "Test DB").await;

    // Load the database
    let db2 = user
        .open_database(&db_id)
        .await
        .expect("Failed to load database");

    // Verify it's the same database
    assert_eq!(db1.root_id(), db2.root_id());
}

#[tokio::test]
async fn test_load_database_preserves_name() {
    let (instance, username) = setup_instance_with_user("frank", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let db_name = "Persistent Name";
    let (_db, db_id) = create_database_with_id(&mut user, db_name).await;

    // Load and verify name
    let loaded_db = user
        .open_database(&db_id)
        .await
        .expect("Failed to load database");
    assert_database_name(&loaded_db, db_name).await;
}

// ===== FIND KEY FOR DATABASE TESTS =====

#[tokio::test]
async fn test_find_key_for_created_database() {
    let (instance, username) = setup_instance_with_user("grace", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create database
    let database = create_named_database(&mut user, "Test DB").await;
    let db_id = database.root_id();

    // Should find a key
    let key_opt = user.find_key(db_id).expect("Should not error");

    assert!(key_opt.is_some(), "Should find key for created database");
}

#[tokio::test]
async fn test_find_key_returns_valid_key_id() {
    let (instance, username) = setup_instance_with_user("henry", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create database
    let database = create_named_database(&mut user, "Test DB").await;
    let db_id = database.root_id();

    // Get the key
    let key_id = user
        .find_key(db_id)
        .expect("Should not error")
        .expect("Should find key");

    // Verify we can get the signing key with this ID
    let _signing_key = user
        .get_signing_key(&key_id)
        .expect("Key ID should be valid");
}

#[tokio::test]
async fn test_find_key_for_nonexistent_database() {
    let (instance, username) = setup_instance_with_user("iris", None).await;
    let user = login_user(&instance, &username, None).await;

    // Fake database ID
    use eidetica::entry::ID;
    let fake_id = ID::from("nonexistent_database");

    // Should return None
    let result = user.find_key(&fake_id).expect("Should not error");

    assert!(
        result.is_none(),
        "Should not find key for nonexistent database"
    );
}

// ===== DATABASE WITH MULTIPLE KEYS TESTS =====

#[tokio::test]
async fn test_create_database_with_second_key() {
    let (instance, username) = setup_instance_with_user("jack", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Add a second key
    let _key2 = add_user_key(&mut user, Some("Second Key")).await;

    // Create database (should use first key by default)
    let database = create_named_database(&mut user, "Test DB").await;

    // Should be able to find a key for it
    let key_opt = user.find_key(database.root_id()).expect("Should not error");

    assert!(key_opt.is_some(), "Should find key for database");
}

// ===== DATABASE SETTINGS TESTS =====

#[tokio::test]
async fn test_database_has_settings() {
    let (instance, username) = setup_instance_with_user("kate", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create database
    let database = create_named_database(&mut user, "Settings Test").await;

    // Verify we can read settings
    assert_database_name(&database, "Settings Test").await;
}

#[tokio::test]
async fn test_database_settings_include_name() {
    let (instance, username) = setup_instance_with_user("leo", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let expected_name = "Named Database";
    let database = create_named_database(&mut user, expected_name).await;

    // Read and verify name from settings
    assert_database_name(&database, expected_name).await;
}

// ===== DATABASE OPERATIONS WITH DATA TESTS =====

#[tokio::test]
async fn test_database_supports_transactions() {
    let (instance, username) = setup_instance_with_user("mia", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let database = create_named_database(&mut user, "Transaction Test").await;

    // Create a transaction
    let tx = database
        .new_transaction()
        .await
        .expect("Should create transaction");

    // Commit it
    tx.commit().await.expect("Should commit transaction");
}

#[tokio::test]
async fn test_database_supports_stores() {
    let (instance, username) = setup_instance_with_user("noah", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let database = create_named_database(&mut user, "Store Test").await;

    // Access a store
    use eidetica::store::DocStore;
    let tx = database
        .new_transaction()
        .await
        .expect("Should create transaction");
    let _store = tx
        .get_store::<DocStore>("test_store")
        .await
        .expect("Should access store");

    tx.commit().await.expect("Should commit");
}

// ===== CONCURRENT DATABASE OPERATIONS =====

#[tokio::test]
async fn test_multiple_users_create_databases() {
    let (instance, _) =
        setup_instance_with_users(&[("alice", None), ("bob", None), ("charlie", None)]).await;

    // Each user creates a database
    let mut alice = login_user(&instance, "alice", None).await;
    let mut bob = login_user(&instance, "bob", None).await;
    let mut charlie = login_user(&instance, "charlie", None).await;

    let alice_db = create_named_database(&mut alice, "Alice DB").await;
    let bob_db = create_named_database(&mut bob, "Bob DB").await;
    let charlie_db = create_named_database(&mut charlie, "Charlie DB").await;

    // All should have unique IDs
    assert_ne!(alice_db.root_id(), bob_db.root_id());
    assert_ne!(bob_db.root_id(), charlie_db.root_id());
    assert_ne!(alice_db.root_id(), charlie_db.root_id());

    // Verify names
    assert_database_name(&alice_db, "Alice DB").await;
    assert_database_name(&bob_db, "Bob DB").await;
    assert_database_name(&charlie_db, "Charlie DB").await;
}

// ===== DATABASE ROOT ID TESTS =====

#[tokio::test]
async fn test_database_root_id_format() {
    let (instance, username) = setup_instance_with_user("olivia", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let database = create_named_database(&mut user, "Test").await;

    // Root ID should be a valid SHA256 hash format
    let id_str = database.root_id().to_string();
    assert!(
        id_str.starts_with("sha256:"),
        "Root ID should start with 'sha256:'"
    );
    assert!(id_str.len() > 7, "Root ID should have hash after prefix");
}

#[tokio::test]
async fn test_database_root_id_is_stable() {
    let (instance, username) = setup_instance_with_user("paul", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let database = create_named_database(&mut user, "Stable Test").await;

    // Get root ID multiple times
    let id1 = database.root_id().clone();
    let id2 = database.root_id().clone();
    let id3 = database.root_id().clone();

    // Should all be the same
    assert_eq!(id1, id2);
    assert_eq!(id2, id3);
}

// ===== FIND DATABASE BY NAME TESTS =====

#[tokio::test]
async fn test_find_database_by_name() {
    let (instance, username) = setup_instance_with_user("quinn", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create database with specific name
    let db_name = "Findable Database";
    let _database = create_named_database(&mut user, db_name).await;

    // Find the database by name
    let found = user
        .find_database(db_name)
        .await
        .expect("Should find database");

    assert_eq!(found.len(), 1, "Should find exactly one database");
    assert_database_name(&found[0], db_name).await;
}

#[tokio::test]
async fn test_find_database_returns_all_matches() {
    let (instance, username) = setup_instance_with_user("rachel", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create multiple databases with same name
    // With entropy, these will have unique IDs
    let _db1 = create_named_database(&mut user, "Searchable").await;
    let _db2 = create_named_database(&mut user, "Other").await;
    let _db3 = create_named_database(&mut user, "Searchable").await;

    // Find databases with the name "Searchable"
    // Should find both databases with that name
    let found = user
        .find_database("Searchable")
        .await
        .expect("Should find databases");

    assert_eq!(
        found.len(),
        2,
        "Should find both databases with name 'Searchable'"
    );
    for db in &found {
        assert_database_name(db, "Searchable").await;
    }
}

#[tokio::test]
async fn test_find_database_among_multiple() {
    let (instance, username) = setup_instance_with_user("sam", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create multiple databases with different names
    let _db1 = create_named_database(&mut user, "Database A").await;
    let _db2 = create_named_database(&mut user, "Database B").await;
    let _db3 = create_named_database(&mut user, "Target Database").await;
    let _db4 = create_named_database(&mut user, "Database C").await;

    // Find the specific one
    let found = user
        .find_database("Target Database")
        .await
        .expect("Should find database");

    assert_eq!(found.len(), 1, "Should find exactly one database");
    assert_database_name(&found[0], "Target Database").await;
}

#[tokio::test]
async fn test_find_database_nonexistent_returns_error() {
    let (instance, username) = setup_instance_with_user("tina", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create a database
    let _db = create_named_database(&mut user, "Existing Database").await;

    // Try to find non-existent database
    let result = user.find_database("Nonexistent Database").await;

    assert!(
        result.is_err(),
        "Should return error for nonexistent database"
    );
}

#[tokio::test]
async fn test_find_database_empty_instance() {
    let (instance, username) = setup_instance_with_user("uma", None).await;
    let user = login_user(&instance, &username, None).await;

    // Don't create any databases
    let result = user.find_database("Any Database").await;

    assert!(
        result.is_err(),
        "Should return error when no databases exist"
    );
}

// ===== ERROR CASE TESTS =====

#[tokio::test]
async fn test_load_database_with_invalid_id() {
    let (instance, username) = setup_instance_with_user("victor", None).await;
    let user = login_user(&instance, &username, None).await;

    // Try to load a database that doesn't exist
    use eidetica::entry::ID;
    let fake_id = ID::from("sha256:nonexistent_database_id_12345678");

    let result = user.open_database(&fake_id).await;

    assert!(
        result.is_err(),
        "Should return error for nonexistent database ID"
    );
}

#[tokio::test]
async fn test_create_database_with_nonexistent_key() {
    let (instance, username) = setup_instance_with_user("wendy", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Try to create database with a key that doesn't exist
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "Test DB");

    let result = user.create_database(settings, "nonexistent_key_id").await;

    assert!(result.is_err(), "Should return error for nonexistent key");
}

#[tokio::test]
async fn test_get_signing_key_for_nonexistent_key() {
    let (instance, username) = setup_instance_with_user("xavier", None).await;
    let user = login_user(&instance, &username, None).await;

    // Try to get a signing key that doesn't exist
    let result = user.get_signing_key("nonexistent_key");

    assert!(result.is_err(), "Should return error for nonexistent key");
}

// ===== MULTI-DEVICE / MULTI-KEY SCENARIOS =====

#[tokio::test]
async fn test_create_databases_with_different_keys() {
    let (instance, username) = setup_instance_with_user("yara", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Add a second key
    let key1 = user.list_keys().expect("Should list keys")[0].clone();
    let key2 = add_user_key(&mut user, Some("Second Device")).await;

    // Create database with first key
    let mut settings1 = eidetica::crdt::Doc::new();
    settings1.set("name", "DB from Key 1");
    let db1 = user
        .create_database(settings1, &key1)
        .await
        .expect("Should create with key1");

    // Create database with second key
    let mut settings2 = eidetica::crdt::Doc::new();
    settings2.set("name", "DB from Key 2");
    let db2 = user
        .create_database(settings2, &key2)
        .await
        .expect("Should create with key2");

    // Both databases should exist and be different
    assert_ne!(db1.root_id(), db2.root_id());
    assert_database_name(&db1, "DB from Key 1").await;
    assert_database_name(&db2, "DB from Key 2").await;

    // Each database should be findable via its key
    assert!(
        user.find_key(db1.root_id())
            .expect("Should not error")
            .is_some(),
        "Should find key for db1"
    );
    assert!(
        user.find_key(db2.root_id())
            .expect("Should not error")
            .is_some(),
        "Should find key for db2"
    );
}

// ===== DATABASE LISTING / DISCOVERY =====

#[tokio::test]
async fn test_user_can_discover_own_databases() {
    let (instance, username) = setup_instance_with_user("zara", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Create several databases
    let _db1 = create_named_database(&mut user, "Project A").await;
    let _db2 = create_named_database(&mut user, "Project B").await;
    let _db3 = create_named_database(&mut user, "Project C").await;

    // Find each one by name
    let found_a = user.find_database("Project A").await;
    let found_b = user.find_database("Project B").await;
    let found_c = user.find_database("Project C").await;

    assert!(found_a.is_ok(), "Should find Project A");
    assert!(found_b.is_ok(), "Should find Project B");
    assert!(found_c.is_ok(), "Should find Project C");
}
