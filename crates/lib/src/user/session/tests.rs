//! Tests for the user_session module.

use super::*;
use crate::{
    Clock, SystemClock,
    auth::{
        settings::AuthSettings,
        types::{AuthKey, Permission},
    },
    backend::database::InMemory,
    crdt::Doc,
    user::{
        crypto::{derive_encryption_key, encrypt_private_key, hash_password},
        types::{KeyEncryption, UserKey, UserStatus},
    },
};
use std::{collections::HashMap, sync::Arc};

async fn create_test_user_session() -> User {
    let backend = Arc::new(InMemory::new());

    // Create Instance for test
    let instance = Instance::create_internal(backend.handle(), Arc::new(SystemClock))
        .await
        .unwrap();

    // Get device key from instance
    let device_key = instance.device_key().clone();
    let device_pubkey_str = instance.device_id_string();

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_user_db");

    let mut auth_settings = AuthSettings::new();
    auth_settings
        .add_key(
            &device_pubkey_str,
            AuthKey::active(Some("admin"), Permission::Admin(0)),
        )
        .unwrap();
    db_settings.set("auth", auth_settings.as_doc().clone());

    let user_database = Database::create(
        db_settings,
        &instance,
        device_key.clone(),
        device_pubkey_str.clone(),
    )
    .await
    .unwrap();

    // Create user info
    let password = "test_password";
    let (password_hash, password_salt) = hash_password(password).unwrap();

    let user_info = UserInfo {
        username: "test_user".to_string(),
        user_database_id: user_database.root_id().clone(),
        password_hash: Some(password_hash),
        password_salt: Some(password_salt.clone()),
        created_at: SystemClock.now_secs(),
        status: UserStatus::Active,
    };

    // Create encrypted key for key manager
    let encryption_key = derive_encryption_key(password, &password_salt).unwrap();
    let (encrypted_key, nonce) = encrypt_private_key(&device_key, &encryption_key).unwrap();

    let user_key = UserKey {
        key_id: "admin".to_string(),
        private_key_bytes: encrypted_key,
        encryption: KeyEncryption::Encrypted { nonce },
        display_name: Some("Device Key".to_string()),
        created_at: SystemClock.now_secs(),
        last_used: None,
        is_default: true,
        database_sigkeys: HashMap::new(),
    };

    // Create key manager
    let key_manager = UserKeyManager::new(password, &password_salt, vec![user_key]).unwrap();

    // Create user with UUID (using a test UUID)
    User::new(
        "test-uuid-1234".to_string(),
        user_info,
        user_database,
        instance,
        key_manager,
    )
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_creation() {
    let user = create_test_user_session().await;
    assert_eq!(user.username(), "test_user");
    assert_eq!(user.user_uuid(), "test-uuid-1234");
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_getters() {
    let user = create_test_user_session().await;

    assert_eq!(user.username(), "test_user");
    assert_eq!(user.user_uuid(), "test-uuid-1234");
    assert_eq!(user.user_info().username, "test_user");
    assert!(!user.user_database().root_id().to_string().is_empty());
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_logout() {
    let user = create_test_user_session().await;
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
        let _user = create_test_user_session().await;
        // User will be dropped when it goes out of scope
    }
    // Keys should be cleared automatically
}
