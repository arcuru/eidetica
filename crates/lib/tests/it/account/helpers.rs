//! Comprehensive helper functions for AccountSession system testing
//!
//! This module provides utilities for testing AccountSession functionality including
//! user creation, authentication, key management, database operations, and
//! multi-user scenarios.
//!
//! Pattern mirrors `instance/helpers.rs` for consistency.

use eidetica::{
    Database, Instance, account::AccountSession, auth::crypto::PublicKey, crdt::Doc, entry::ID,
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
pub async fn create_database_and_relogin(
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
