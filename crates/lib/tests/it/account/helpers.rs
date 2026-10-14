//! Comprehensive helper functions for AccountSession system testing
//!
//! This module provides utilities for testing AccountSession functionality including
//! user creation, authentication, key management, database operations, and
//! multi-user scenarios.
//!
//! Pattern mirrors `instance/helpers.rs` for consistency.

#![allow(dead_code)]

use eidetica::{
    Database, Instance, account::AccountSession, auth::crypto::PublicKey, crdt::Doc, entry::ID,
    sync::Sync,
};

use crate::helpers::test_instance;

// ===== ADMIN HELPERS =====

/// Create a user via the bootstrapped admin session (works on both local
/// and remote Instances). Thin alias over [`crate::helpers::create_account`].
async fn admin_create_user(instance: &Instance, username: &str, password: Option<&str>) {
    crate::helpers::create_account(instance, username, password)
        .await
        .expect("Failed to create user");
}

// ===== INSTANCE SETUP HELPERS =====

/// Create a new Instance for user testing
///
/// Uses configurable backend via TEST_BACKEND env var.
pub async fn setup_instance() -> Instance {
    test_instance().await
}

/// Create an Instance with a single user already created
///
/// Returns (Instance, username) for easy access.
///
/// Honors `TEST_BACKEND=service`: in service mode this returns a connected
/// remote Instance, so callers exercise the wire path. Single-session use
/// only — tests that log in multiple sessions on the same Instance (multi-
/// device key visibility, sequential-login lifecycle) must use
/// [`setup_local_instance_with_user`] instead, since the wire connection
/// model is one-identity-per-connection and a second `login_account` would
/// clobber the first session's `session_pubkey`.
pub async fn setup_instance_with_user(
    username: &str,
    password: Option<&str>,
) -> (Instance, String) {
    let instance = crate::helpers::test_instance().await;
    admin_create_user(&instance, username, password).await;
    (instance, username.to_string())
}

/// Always-local variant of [`setup_instance_with_user`] for tests that log
/// in multiple sessions on the same Instance — multi-device key visibility,
/// sequential-login lifecycle, logout/relogin, etc. Bypasses
/// `TEST_BACKEND=service` because the wire model is one-identity-per-
/// connection (a second `login_account` against a connected Instance would
/// clobber the first session's `session_pubkey`). Single-session tests
/// should prefer [`setup_instance_with_user`] for wire coverage.
pub async fn setup_local_instance_with_user(
    username: &str,
    password: Option<&str>,
) -> (Instance, String) {
    let instance = crate::helpers::test_local_instance().await;
    admin_create_user(&instance, username, password).await;
    (instance, username.to_string())
}

/// Create an Instance with multiple users
///
/// Returns (Instance, Vec<username>).
///
/// Honors `TEST_BACKEND=service`. Same single-session constraint as
/// [`setup_instance_with_user`]: only the first `login_account` call against
/// the returned Instance is wire-safe. Tests that log in as more than one
/// user (or the same user repeatedly) must use
/// [`setup_local_instance_with_users`].
pub async fn setup_instance_with_users(
    user_configs: &[(&str, Option<&str>)],
) -> (Instance, Vec<String>) {
    let instance = crate::helpers::test_instance().await;
    let mut usernames = Vec::new();

    for (username, password) in user_configs {
        admin_create_user(&instance, username, *password).await;
        usernames.push(username.to_string());
    }

    (instance, usernames)
}

/// Always-local variant of [`setup_instance_with_users`] for tests that
/// log in as more than one of the created users (or the same user
/// multiple times) on the shared Instance. See
/// [`setup_local_instance_with_user`] for the wire-model rationale.
pub async fn setup_local_instance_with_users(
    user_configs: &[(&str, Option<&str>)],
) -> (Instance, Vec<String>) {
    let instance = crate::helpers::test_local_instance().await;
    let mut usernames = Vec::new();

    for (username, password) in user_configs {
        admin_create_user(&instance, username, *password).await;
        usernames.push(username.to_string());
    }

    (instance, usernames)
}

// ===== USER LOGIN HELPERS =====

/// Login a user and return AccountSession
pub async fn login_account(
    instance: &Instance,
    username: &str,
    password: Option<&str>,
) -> AccountSession {
    instance
        .login_account(username, password)
        .await
        .expect("Failed to login user")
}

// ===== KEY MANAGEMENT HELPERS =====

/// Add a private key to a user and return the key ID
pub async fn add_user_key(user: &mut AccountSession, display_name: Option<&str>) -> PublicKey {
    user.add_private_key(display_name)
        .await
        .expect("Failed to add private key")
}

/// Add multiple keys to a user
pub async fn add_multiple_keys(user: &mut AccountSession, count: usize) -> Vec<PublicKey> {
    let mut keys = Vec::with_capacity(count);
    for i in 0..count {
        keys.push(add_user_key(user, Some(&format!("key_{i}"))).await);
    }
    keys
}

// ===== DATABASE OPERATION HELPERS =====

/// Create a database for a user with default settings
pub async fn create_user_database(user: &mut AccountSession) -> Database {
    let mut settings = Doc::new();
    settings.set("name", "Test Database");

    // Get the default key (earliest created key)
    let default_key = user.get_default_key().expect("Failed to get default key");

    user.create_database(settings, &default_key)
        .await
        .expect("Failed to create database")
}

/// Create a database with custom name
pub async fn create_named_database(user: &mut AccountSession, name: &str) -> Database {
    let mut settings = Doc::new();
    settings.set("name", name);

    // Get the default key (earliest created key)
    let default_key = user.get_default_key().expect("Failed to get default key");

    user.create_database(settings, &default_key)
        .await
        .expect("Failed to create database")
}

/// Create multiple databases for a user
pub async fn create_multiple_databases(user: &mut AccountSession, names: &[&str]) -> Vec<Database> {
    let mut databases = Vec::with_capacity(names.len());
    for name in names {
        databases.push(create_named_database(user, name).await);
    }
    databases
}

/// Create a database and return both the database and its ID
pub async fn create_database_with_id(user: &mut AccountSession, name: &str) -> (Database, ID) {
    let db = create_named_database(user, name).await;
    let id = db.root_id().clone();
    (db, id)
}

// ===== MULTI-USER SETUP HELPERS =====

/// Setup two users on the same instance (passwordless)
///
/// Returns (Instance, User1, User2, username1, username2)
///
/// Uses an always-local instance: the wire connection model is
/// one-identity-per-connection, so two `AccountSession` sessions sharing a single
/// connected `Instance` would both wedge against the same `session_pubkey`
/// (whichever user logged in last). These tests exercise process-local
/// multi-user semantics that map naturally onto a local instance.
pub async fn setup_two_passwordless_users(
    username1: &str,
    username2: &str,
) -> (Instance, AccountSession, AccountSession, String, String) {
    let instance = crate::helpers::test_local_instance().await;
    admin_create_user(&instance, username1, None).await;
    admin_create_user(&instance, username2, None).await;

    let user1 = login_account(&instance, username1, None).await;
    let user2 = login_account(&instance, username2, None).await;

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
///
/// Local-only — see [`setup_two_passwordless_users`].
pub async fn setup_users_with_shared_database(
    owner_name: &str,
    requester_name: &str,
    db_name: &str,
) -> (Instance, AccountSession, AccountSession, Database, ID) {
    let instance = crate::helpers::test_local_instance().await;
    admin_create_user(&instance, owner_name, None).await;
    admin_create_user(&instance, requester_name, None).await;

    let mut owner = login_account(&instance, owner_name, None).await;
    let requester = login_account(&instance, requester_name, None).await;

    let (database, db_id) = create_database_with_id(&mut owner, db_name).await;

    (instance, owner, requester, database, db_id)
}

// ===== VERIFICATION HELPERS =====

/// Assert that a user has the expected number of keys
pub fn assert_user_key_count(user: &AccountSession, expected_count: usize) {
    let keys = user.list_keys().expect("Failed to list keys");
    assert_eq!(
        keys.len(),
        expected_count,
        "Expected {} keys, found {}",
        expected_count,
        keys.len()
    );
}

/// Assert that a user has a specific key
pub fn assert_user_has_key(user: &AccountSession, key_id: &PublicKey) {
    let keys = user.list_keys().expect("Failed to list keys");
    assert!(
        keys.contains(key_id),
        "AccountSession should have key {}, but it was not found in key list",
        key_id,
    );
}

/// Assert that a user does NOT have a specific key
pub fn assert_user_lacks_key(user: &AccountSession, key_id: &PublicKey) {
    let keys = user.list_keys().expect("Failed to list keys");
    assert!(
        !keys.contains(key_id),
        "AccountSession should NOT have key {key_id}, but found it"
    );
}

/// Assert that a database has a specific name
pub async fn assert_database_name(database: &Database, expected_name: &str) {
    let actual_name = database
        .get_name()
        .await
        .expect("Failed to get database name");
    assert_eq!(
        actual_name, expected_name,
        "Expected database name '{expected_name}', found '{actual_name}'"
    );
}

/// Assert that a user has a sigkey mapping for a database
pub fn assert_user_has_database_access(
    user: &AccountSession,
    key_id: &PublicKey,
    database_id: &ID,
) {
    let sigkey = user
        .key_mapping(key_id, database_id)
        .expect("Failed to get database sigkey");
    assert!(
        sigkey.is_some(),
        "AccountSession should have sigkey mapping for database"
    );
}

/// Assert that a user can find a key for a database
pub fn assert_user_can_access_database(user: &AccountSession, database_id: &ID) {
    let key = user
        .find_key(database_id)
        .expect("Failed to find key for database");
    assert!(
        key.is_some(),
        "AccountSession should have a key for database {database_id}"
    );
}

// ===== INTEGRATION HELPERS =====

/// Complete user workflow: create → login → create DB → logout → login → load DB
///
/// Returns the final AccountSession and Database for verification
pub async fn test_complete_user_lifecycle(
    username: &str,
    password: Option<&str>,
    db_name: &str,
) -> (Instance, AccountSession, Database) {
    // Setup and create user
    let instance = setup_instance().await;
    admin_create_user(&instance, username, password).await;

    // First login
    let mut user = instance
        .login_account(username, password)
        .await
        .expect("Failed to login");

    // Create database
    let (_database, db_id) = create_database_with_id(&mut user, db_name).await;

    // Logout
    user.logout().expect("Failed to logout");

    // Re-login
    let user = instance
        .login_account(username, password)
        .await
        .expect("Failed to re-login");

    // Load database
    let loaded_db = user
        .open_database(&db_id)
        .await
        .expect("Failed to load database");

    (instance, user, loaded_db)
}

/// Multi-user collaboration workflow: AccountSession A creates DB → AccountSession B requests access → AccountSession A approves
///
/// Returns (Instance, UserA, UserB, SharedDatabase, Sync) for verification
///
/// Local-only — `Sync::new` needs a process-local device key, and the
/// multi-user pattern (two `AccountSession` sessions on one `Instance`) maps onto a
/// local instance (see [`setup_two_passwordless_users`]).
pub async fn test_multi_user_bootstrap_workflow(
    owner_name: &str,
    requester_name: &str,
    db_name: &str,
) -> (Instance, AccountSession, AccountSession, Database, Sync, ID) {
    let instance = crate::helpers::test_local_instance().await;

    // Create both users
    admin_create_user(&instance, owner_name, None).await;
    admin_create_user(&instance, requester_name, None).await;

    // Owner creates database
    let mut owner = login_account(&instance, owner_name, None).await;
    let (database, db_id) = create_database_with_id(&mut owner, db_name).await;

    // Requester logs in
    let requester = login_account(&instance, requester_name, None).await;

    // Create sync for bootstrap workflow
    let sync = Sync::new(instance.clone())
        .await
        .expect("Failed to create sync");

    (instance, owner, requester, database, sync, db_id)
}

/// Concurrent database operations: Multiple users creating databases simultaneously
///
/// Returns (Instance, Vec<AccountSession>, Vec<Database>) for verification
///
/// Local-only — see [`setup_two_passwordless_users`] for the
/// one-identity-per-connection rationale.
pub async fn test_concurrent_database_creation(
    user_count: usize,
    databases_per_user: usize,
) -> (Instance, Vec<AccountSession>, Vec<Vec<Database>>) {
    let instance = crate::helpers::test_local_instance().await;
    let mut users = Vec::new();
    let mut all_databases = Vec::new();

    for i in 0..user_count {
        let username = format!("user_{i}");
        admin_create_user(&instance, &username, None).await;
        let mut user = login_account(&instance, &username, None).await;

        let mut user_databases = Vec::new();
        for j in 0..databases_per_user {
            let db_name = format!("user{i}_db{j}");
            let db = create_named_database(&mut user, &db_name).await;
            user_databases.push(db);
        }

        all_databases.push(user_databases);
        users.push(user);
    }

    (instance, users, all_databases)
}
