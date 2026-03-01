//! Tests for the user_session module.

use super::*;
use crate::backend::database::InMemory;

async fn create_test_user_session() -> (Instance, User) {
    let instance = Instance::create(Box::new(InMemory::new())).await.unwrap();
    instance
        .create_user("test_user", Some("test_password"))
        .await
        .unwrap();
    let user = instance
        .login_user("test_user", Some("test_password"))
        .await
        .unwrap();
    (instance, user)
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_creation() {
    let (_instance, user) = create_test_user_session().await;
    assert_eq!(user.username(), "test_user");
    assert!(!user.user_uuid().is_empty());
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_getters() {
    let (_instance, user) = create_test_user_session().await;

    assert_eq!(user.username(), "test_user");
    assert!(!user.user_uuid().is_empty());
    assert_eq!(user.user_info().username, "test_user");
    assert!(!user.user_database().root_id().to_string().is_empty());
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_logout() {
    let (_instance, user) = create_test_user_session().await;
    let username = user.username().to_string();

    // Logout consumes the user
    user.logout().unwrap();

    // User is dropped, keys should be cleared
    assert_eq!(username, "test_user");
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_drop() {
    {
        let (_instance, _user) = create_test_user_session().await;
        // User will be dropped when it goes out of scope
    }
    // Keys should be cleared automatically
}
