//! User lifecycle tests: create, login, logout, and re-login scenarios
//!
//! Tests the fundamental user workflows including:
//! - Creating passwordless and password-protected users
//! - Login with correct/incorrect credentials
//! - Logout and session cleanup
//! - Multiple sequential logins

use super::helpers::*;

// ===== USER CREATION TESTS =====

#[tokio::test]
async fn test_create_passwordless_user() {
    let instance = setup_instance().await;

    // Create a passwordless user
    let username = "alice";
    instance
        .create_user(username, None)
        .await
        .expect("Failed to create user");

    // Verify we can login
    let user = login_user(&instance, username, None).await;

    // Verify user has at least one key
    assert_user_key_count(&user, 1);
}

#[tokio::test]
async fn test_create_password_protected_user() {
    let instance = setup_instance().await;

    // Create a password-protected user
    let username = "bob";
    let password = "secure_password_123";
    instance
        .create_user(username, Some(password))
        .await
        .expect("Failed to create user");

    // Verify we can login with correct password
    let user = login_user(&instance, username, Some(password)).await;

    // Verify user has at least one key
    assert_user_key_count(&user, 1);
}

#[tokio::test]
async fn test_create_multiple_users_same_instance() {
    let user_configs = &[
        ("alice", None),
        ("bob", Some("password123")),
        ("charlie", None),
        ("diana", Some("another_password")),
    ];

    let (instance, usernames) = setup_instance_with_users(user_configs).await;

    assert_eq!(usernames.len(), 4, "Should have created 4 users");

    // Verify each user can login
    let _alice = login_user(&instance, "alice", None).await;
    let _bob = login_user(&instance, "bob", Some("password123")).await;
    let _charlie = login_user(&instance, "charlie", None).await;
    let _diana = login_user(&instance, "diana", Some("another_password")).await;
}

#[tokio::test]
async fn test_create_duplicate_username_fails() {
    let instance = setup_instance().await;
    let username = "alice";

    // Create first user
    instance
        .create_user(username, None)
        .await
        .expect("First user should succeed");

    // Try to create duplicate user
    let result = instance.create_user(username, None).await;
    assert!(result.is_err(), "Creating duplicate username should fail");
}

// ===== LOGIN TESTS =====

#[tokio::test]
async fn test_login_passwordless_user() {
    let (instance, username) = setup_instance_with_user("alice", None).await;

    // Login should succeed
    let user = login_user(&instance, &username, None).await;

    // Verify user has keys
    assert_user_key_count(&user, 1);
}

#[tokio::test]
async fn test_login_with_correct_password() {
    let username = "bob";
    let password = "correct_password";
    let (instance, _) = setup_instance_with_user(username, Some(password)).await;

    // Login with correct password should succeed
    let user = login_user(&instance, username, Some(password)).await;

    // Verify user has keys
    assert_user_key_count(&user, 1);
}

#[tokio::test]
async fn test_login_with_wrong_password() {
    let username = "charlie";
    let correct_password = "correct_password";
    let wrong_password = "wrong_password";

    let (instance, _) = setup_instance_with_user(username, Some(correct_password)).await;

    // Login with wrong password should fail
    assert!(
        instance
            .login_user(username, Some(wrong_password))
            .await
            .is_err(),
        "Login should fail with wrong password"
    );
}

#[tokio::test]
async fn test_login_passwordless_user_with_password_fails() {
    let username = "dave";
    let (instance, _) = setup_instance_with_user(username, None).await;

    // Try to login with a password when user is passwordless
    let result = instance.login_user(username, Some("any_password")).await;
    assert!(
        result.is_err(),
        "Login passwordless user with password should fail"
    );
}

#[tokio::test]
async fn test_login_password_user_without_password_fails() {
    let username = "eve";
    let password = "secure_password";
    let (instance, _) = setup_instance_with_user(username, Some(password)).await;

    // Try to login without password when user requires one
    let result = instance.login_user(username, None).await;
    assert!(
        result.is_err(),
        "Login password-protected user without password should fail"
    );
}

#[tokio::test]
async fn test_login_nonexistent_user() {
    let instance = setup_instance().await;

    // Try to login user that doesn't exist
    assert!(
        instance.login_user("nonexistent_user", None).await.is_err(),
        "Login should fail for non-existent user"
    );
}

#[tokio::test]
async fn test_multiple_sequential_logins_same_user() {
    let username = "frank";
    let (instance, _) = setup_instance_with_user(username, None).await;

    // Login multiple times sequentially
    let _user1 = login_user(&instance, username, None).await;
    let _user2 = login_user(&instance, username, None).await;
    let _user3 = login_user(&instance, username, None).await;

    // All logins should succeed (different sessions)
}

// ===== LOGOUT TESTS =====

#[tokio::test]
async fn test_logout_clears_session() {
    let username = "grace";
    let (instance, _) = setup_instance_with_user(username, None).await;

    let user = login_user(&instance, username, None).await;

    // Logout should succeed
    user.logout().expect("Logout should succeed");

    // Can still login again
    let _new_user = login_user(&instance, username, None).await;
}

#[tokio::test]
async fn test_logout_and_relogin() {
    let username = "henry";
    let password = "password123";
    let (instance, _) = setup_instance_with_user(username, Some(password)).await;

    // First session
    let user1 = login_user(&instance, username, Some(password)).await;
    user1.logout().expect("First logout should succeed");

    // Second session
    let user2 = login_user(&instance, username, Some(password)).await;
    user2.logout().expect("Second logout should succeed");

    // Third session
    let _user3 = login_user(&instance, username, Some(password)).await;
}

#[tokio::test]
async fn test_logout_multiple_sessions() {
    let username = "iris";
    let (instance, _) = setup_instance_with_user(username, None).await;

    // Create multiple sessions and logout each one
    let user1 = login_user(&instance, username, None).await;
    user1.logout().expect("First session logout should succeed");

    let user2 = login_user(&instance, username, None).await;
    user2
        .logout()
        .expect("Second session logout should succeed");

    let user3 = login_user(&instance, username, None).await;
    user3.logout().expect("Third session logout should succeed");
}

// ===== COMPLETE LIFECYCLE TESTS =====

#[tokio::test]
async fn test_complete_lifecycle_passwordless() {
    let username = "jack";
    let db_name = "test_database";

    let (_instance, user, database) = test_complete_user_lifecycle(username, None, db_name).await;

    // Verify user can access the database
    assert_user_can_access_database(&user, database.root_id());

    // Verify database has correct name
    assert_database_name(&database, db_name).await;
}

#[tokio::test]
async fn test_complete_lifecycle_password_protected() {
    let username = "kate";
    let password = "secure_password";
    let db_name = "secure_database";

    let (_instance, user, database) =
        test_complete_user_lifecycle(username, Some(password), db_name).await;

    // Verify user can access the database
    assert_user_can_access_database(&user, database.root_id());

    // Verify database has correct name
    assert_database_name(&database, db_name).await;
}

#[tokio::test]
async fn test_user_persistence_across_sessions() {
    let username = "leo";
    let instance = setup_instance().await;

    // Create user and add keys in first session
    instance
        .create_user(username, None)
        .await
        .expect("Failed to create user");
    let mut user1 = login_user(&instance, username, None).await;
    let key1 = add_user_key(&mut user1, Some("key1")).await;
    let key2 = add_user_key(&mut user1, Some("key2")).await;
    user1.logout().expect("Logout should succeed");

    // Login again and verify keys persisted
    let user2 = login_user(&instance, username, None).await;
    assert_user_key_count(&user2, 3); // Initial key + 2 added keys
    assert_user_has_key(&user2, &key1);
    assert_user_has_key(&user2, &key2);
}

#[tokio::test]
async fn test_database_persistence_across_sessions() {
    let username = "mia";
    let instance = setup_instance().await;

    // Create user and database in first session
    instance
        .create_user(username, None)
        .await
        .expect("Failed to create user");
    let mut user1 = login_user(&instance, username, None).await;
    let (db1, db_id) = create_database_with_id(&mut user1, "persistent_db").await;
    user1.logout().expect("Logout should succeed");

    // Login again and verify database can be loaded
    let user2 = login_user(&instance, username, None).await;
    let db2 = user2
        .open_database(&db_id)
        .await
        .expect("Failed to load database");

    // Verify database properties match
    assert_eq!(db1.root_id(), db2.root_id());
    assert_database_name(&db2, "persistent_db").await;
}
