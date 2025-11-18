//! Comprehensive helper functions for User system testing
//!
//! This module provides utilities for testing User functionality including
//! user creation, authentication, key management, database operations, and
//! multi-user scenarios.
//!
//! Pattern mirrors `instance/helpers.rs` for consistency.

#![allow(dead_code)]

use eidetica::{Database, Instance, backend::database::InMemory, user::User};

// ===== INSTANCE SETUP HELPERS =====

/// Create a new Instance for user testing
///
/// Uses InMemory backend and the new unified API (no implicit user).
pub fn setup_instance() -> Instance {
    let backend = Box::new(InMemory::new());
    Instance::open(backend).expect("Failed to create instance")
}

/// Create an Instance with a single user already created
///
/// Returns (Instance, username) for easy access
pub fn setup_instance_with_user(username: &str, password: Option<&str>) -> (Instance, String) {
    let instance = setup_instance();
    instance
        .create_user(username, password)
        .expect("Failed to create user");
    (instance, username.to_string())
}

/// Create an Instance with multiple users
///
/// Returns (Instance, Vec<username>)
pub fn setup_instance_with_users(user_configs: &[(&str, Option<&str>)]) -> (Instance, Vec<String>) {
    let instance = setup_instance();
    let mut usernames = Vec::new();

    for (username, password) in user_configs {
        instance
            .create_user(username, *password)
            .expect("Failed to create user");
        usernames.push(username.to_string());
    }

    (instance, usernames)
}

// ===== USER LOGIN HELPERS =====

/// Login a user and return User session
pub fn login_user(instance: &Instance, username: &str, password: Option<&str>) -> User {
    instance
        .login_user(username, password)
        .expect("Failed to login user")
}

// ===== KEY MANAGEMENT HELPERS =====

/// Add a private key to a user and return the key ID
pub fn add_user_key(user: &mut User, display_name: Option<&str>) -> String {
    user.add_private_key(display_name)
        .expect("Failed to add private key")
}

/// Add multiple keys to a user
pub fn add_multiple_keys(user: &mut User, count: usize) -> Vec<String> {
    (0..count)
        .map(|i| add_user_key(user, Some(&format!("key_{i}"))))
        .collect()
}

// ===== DATABASE OPERATION HELPERS =====

/// Create a database for a user with default settings
pub fn create_user_database(user: &mut User) -> Database {
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", "Test Database");

    // Get the default key (earliest created key)
    let default_key = user.get_default_key().expect("Failed to get default key");

    user.create_database(settings, &default_key)
        .expect("Failed to create database")
}

/// Create a database with custom name
pub fn create_named_database(user: &mut User, name: &str) -> Database {
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", name);

    // Get the default key (earliest created key)
    let default_key = user.get_default_key().expect("Failed to get default key");

    user.create_database(settings, &default_key)
        .expect("Failed to create database")
}

/// Create multiple databases for a user
pub fn create_multiple_databases(user: &mut User, names: &[&str]) -> Vec<Database> {
    names
        .iter()
        .map(|name| create_named_database(user, name))
        .collect()
}

/// Create a database and return both the database and its ID
pub fn create_database_with_id(user: &mut User, name: &str) -> (Database, eidetica::entry::ID) {
    let db = create_named_database(user, name);
    let id = db.root_id().clone();
    (db, id)
}

// ===== MULTI-USER SETUP HELPERS =====

/// Setup two users on the same instance (passwordless)
///
/// Returns (Instance, User1, User2, username1, username2)
pub fn setup_two_passwordless_users(
    username1: &str,
    username2: &str,
) -> (Instance, User, User, String, String) {
    let instance = setup_instance();
    instance
        .create_user(username1, None)
        .expect("Failed to create user1");
    instance
        .create_user(username2, None)
        .expect("Failed to create user2");

    let user1 = login_user(&instance, username1, None);
    let user2 = login_user(&instance, username2, None);

    (
        instance,
        user1,
        user2,
        username1.to_string(),
        username2.to_string(),
    )
}

/// Setup two users with one database shared via bootstrap
///
/// Returns (Instance, User1 with database, User2, Database, DatabaseID)
pub fn setup_users_with_shared_database(
    owner_name: &str,
    requester_name: &str,
    db_name: &str,
) -> (Instance, User, User, Database, eidetica::entry::ID) {
    let instance = setup_instance();
    instance
        .create_user(owner_name, None)
        .expect("Failed to create owner");
    instance
        .create_user(requester_name, None)
        .expect("Failed to create requester");

    let mut owner = login_user(&instance, owner_name, None);
    let requester = login_user(&instance, requester_name, None);

    let (database, db_id) = create_database_with_id(&mut owner, db_name);

    (instance, owner, requester, database, db_id)
}

// ===== VERIFICATION HELPERS =====

/// Assert that a user has the expected number of keys
pub fn assert_user_key_count(user: &User, expected_count: usize) {
    let keys = user.list_keys().expect("Failed to list keys");
    assert_eq!(
        keys.len(),
        expected_count,
        "Expected {} keys, found {}",
        expected_count,
        keys.len()
    );
}

/// Assert that a user can get a specific key
pub fn assert_user_has_key(user: &User, key_id: &str) {
    let result = user.get_signing_key(key_id);
    assert!(
        result.is_ok(),
        "User should have key {}, but got error: {:?}",
        key_id,
        result.err()
    );
}

/// Assert that a user does NOT have a specific key
pub fn assert_user_lacks_key(user: &User, key_id: &str) {
    let result = user.get_signing_key(key_id);
    assert!(
        result.is_err(),
        "User should NOT have key {key_id}, but found it"
    );
}

/// Assert that a database has a specific name
pub fn assert_database_name(database: &Database, expected_name: &str) {
    let actual_name = database.get_name().expect("Failed to get database name");
    assert_eq!(
        actual_name, expected_name,
        "Expected database name '{expected_name}', found '{actual_name}'"
    );
}

/// Assert that a user has a sigkey mapping for a database
pub fn assert_user_has_database_access(
    user: &User,
    key_id: &str,
    database_id: &eidetica::entry::ID,
) {
    let sigkey = user
        .key_mapping(key_id, database_id)
        .expect("Failed to get database sigkey");
    assert!(
        sigkey.is_some(),
        "User should have sigkey mapping for database"
    );
}

/// Assert that a user can find a key for a database
pub fn assert_user_can_access_database(user: &User, database_id: &eidetica::entry::ID) {
    let key = user
        .find_key(database_id)
        .expect("Failed to find key for database");
    assert!(
        key.is_some(),
        "User should have a key for database {database_id}"
    );
}

// ===== INTEGRATION HELPERS =====

/// Complete user workflow: create → login → create DB → logout → login → load DB
///
/// Returns the final User session and Database for verification
pub fn test_complete_user_lifecycle(
    username: &str,
    password: Option<&str>,
    db_name: &str,
) -> (Instance, User, Database) {
    // Setup and create user
    let instance = setup_instance();
    instance
        .create_user(username, password)
        .expect("Failed to create user");

    // First login
    let mut user = instance
        .login_user(username, password)
        .expect("Failed to login");

    // Create database
    let (_database, db_id) = create_database_with_id(&mut user, db_name);

    // Logout
    user.logout().expect("Failed to logout");

    // Re-login
    let user = instance
        .login_user(username, password)
        .expect("Failed to re-login");

    // Load database
    let loaded_db = user.open_database(&db_id).expect("Failed to load database");

    (instance, user, loaded_db)
}

/// Multi-user collaboration workflow: User A creates DB → User B requests access → User A approves
///
/// Returns (Instance, UserA, UserB, SharedDatabase, Sync) for verification
pub fn test_multi_user_bootstrap_workflow(
    owner_name: &str,
    requester_name: &str,
    db_name: &str,
) -> (
    Instance,
    User,
    User,
    Database,
    eidetica::sync::Sync,
    eidetica::entry::ID,
) {
    use eidetica::sync::Sync;

    let instance = setup_instance();

    // Create both users
    instance
        .create_user(owner_name, None)
        .expect("Failed to create owner");
    instance
        .create_user(requester_name, None)
        .expect("Failed to create requester");

    // Owner creates database
    let mut owner = login_user(&instance, owner_name, None);
    let (database, db_id) = create_database_with_id(&mut owner, db_name);

    // Requester logs in
    let requester = login_user(&instance, requester_name, None);

    // Create sync for bootstrap workflow
    let sync = Sync::new(instance.clone()).expect("Failed to create sync");

    (instance, owner, requester, database, sync, db_id)
}

/// Concurrent database operations: Multiple users creating databases simultaneously
///
/// Returns (Instance, Vec<User>, Vec<Database>) for verification
pub fn test_concurrent_database_creation(
    user_count: usize,
    databases_per_user: usize,
) -> (Instance, Vec<User>, Vec<Vec<Database>>) {
    let instance = setup_instance();
    let mut users = Vec::new();
    let mut all_databases = Vec::new();

    for i in 0..user_count {
        let username = format!("user_{i}");
        instance
            .create_user(&username, None)
            .expect("Failed to create user");
        let mut user = login_user(&instance, &username, None);

        let mut user_databases = Vec::new();
        for j in 0..databases_per_user {
            let db_name = format!("user{i}_db{j}");
            let db = create_named_database(&mut user, &db_name);
            user_databases.push(db);
        }

        all_databases.push(user_databases);
        users.push(user);
    }

    (instance, users, all_databases)
}
