//! System database initialization for the user system
//!
//! Creates and manages _users and _databases system databases.

use handle_trait::Handle;

use super::{
    User,
    crypto::{derive_encryption_key, encrypt_private_key, hash_password},
    errors::UserError,
    key_manager::UserKeyManager,
    types::{KeyStorage, UserInfo, UserKey, UserStatus},
};
use crate::{
    Database, Instance, Result,
    auth::{
        crypto::{PrivateKey, format_public_key, generate_keypair},
        types::{AuthKey, Permission},
    },
    constants::{DATABASES, INSTANCE, USERS},
    crdt::Doc,
    database::DatabaseKey,
    store::Table,
};

/// Create the _instance system database
///
/// This database stores Instance-level configuration and metadata.
/// Auth is bootstrapped by `Database::create` with the device key as Admin(0).
///
/// # Arguments
/// * `instance` - The Instance handle
/// * `device_signing_key` - The device's Ed25519 signing key
///
/// # Returns
/// The _instance Database
pub async fn create_instance_database(
    instance: &Instance,
    device_signing_key: &PrivateKey,
) -> Result<Database> {
    let mut settings = Doc::new();
    settings.set("name", INSTANCE);
    settings.set("type", "system");
    settings.set("description", "Instance configuration and management");

    Database::create(instance, device_signing_key.clone(), settings).await
}

/// Create the _users system database
///
/// This database stores the user directory mapping user_id -> UserInfo.
/// Auth is bootstrapped by `Database::create` with the device key as Admin(0).
///
/// # Arguments
/// * `instance` - The Instance handle
/// * `device_signing_key` - The device's Ed25519 signing key
///
/// # Returns
/// The created _users Database
pub async fn create_users_database(
    instance: &Instance,
    device_signing_key: &PrivateKey,
) -> Result<Database> {
    let mut settings = Doc::new();
    settings.set("name", USERS);
    settings.set("type", "system");
    settings.set("description", "User directory database");

    Database::create(instance, device_signing_key.clone(), settings).await
}

/// Create the _databases tracking database
///
/// This database stores the database tracking information mapping
/// database_id -> DatabaseTracking.
/// Auth is bootstrapped by `Database::create` with the device key as Admin(0).
///
/// # Arguments
/// * `instance` - The Instance handle
/// * `device_signing_key` - The device's Ed25519 signing key
///
/// # Returns
/// The created _databases Database
pub async fn create_databases_tracking(
    instance: &Instance,
    device_signing_key: &PrivateKey,
) -> Result<Database> {
    let mut settings = Doc::new();
    settings.set("name", DATABASES);
    settings.set("type", "system");
    settings.set("description", "Database tracking and registry");

    Database::create(instance, device_signing_key.clone(), settings).await
}

/// Create a new user account
///
/// This function:
/// 1. Optionally hashes the user's password (if provided)
/// 2. Generates a device keypair for the user
/// 3. Creates a user database for storing keys (encrypted or unencrypted)
/// 4. Creates UserInfo and stores it in _users database with auto-generated UUID
///
/// # Arguments
/// * `users_db` - The _users system database
/// * `instance` - The Instance handle
/// * `username` - Unique username for login
/// * `password` - Optional password. If None, creates passwordless user (instant login, no encryption)
///
/// # Returns
/// A tuple of (user_uuid, UserInfo) where user_uuid is the generated primary key
pub async fn create_user(
    users_db: &Database,
    instance: &Instance,
    username: impl AsRef<str>,
    password: Option<&str>,
) -> Result<(String, UserInfo)> {
    let username = username.as_ref();
    // FIXME: Race condition - multiple concurrent creates with same username
    // can both succeed, creating duplicate users. This requires either:
    // 1. Distributed locking mechanism
    // 2. Backend-level unique constraints
    // 3. Periodic cleanup/reconciliation process
    // For now, duplicate detection happens at login time.

    // Check if username already exists
    let users_table = users_db
        .get_store_viewer::<Table<UserInfo>>("users")
        .await?;
    let existing = users_table.search(|u| u.username == username).await?;
    if !existing.is_empty() {
        return Err(UserError::UsernameAlreadyExists {
            username: username.to_string(),
        }
        .into());
    }

    // 1. Hash password if provided
    let (password_hash, password_salt) = match password {
        Some(pwd) => {
            let (hash, salt) = hash_password(pwd)?;
            (Some(hash), Some(salt))
        }
        None => (None, None),
    };

    // 2. Generate default keypair for this user (kept in memory only)
    let (user_private_key, user_public_key) = generate_keypair();
    let user_public_key_str = format_public_key(&user_public_key);

    // 3. Create user database with the user's key in auth (device key added automatically)
    let mut user_db_settings = Doc::new();
    user_db_settings.set("name", format!("_user_{username}"));
    user_db_settings.set("type", "user");
    user_db_settings.set("description", format!("User database for {username}"));

    // Get device key for database creation (used as the signing key)
    let device_private_key = instance.device_key().clone();

    // Create database using device_key as the signing key.
    // Database::create bootstraps auth with device key as Admin(0).
    let user_database = Database::create(instance, device_private_key, user_db_settings).await?;
    let user_database_id = user_database.root_id().clone();

    // Add user's key as an equal owner
    // FIXME: can we restrict the Device ID's ownership?
    let txn = user_database.new_transaction().await?;
    let settings = txn.get_settings()?;
    settings
        .set_auth_key(
            &user_public_key_str,
            AuthKey::active(Some("user"), Permission::Admin(0)),
        )
        .await?;
    txn.commit().await?;

    // 4. Store user's private key (encrypted or unencrypted based on password)
    let user_key = match (password, &password_salt) {
        (Some(pwd), Some(salt)) => {
            // Password-protected: encrypt the key
            let encryption_key = derive_encryption_key(pwd, salt)?;
            let (ciphertext, nonce) = encrypt_private_key(&user_private_key, &encryption_key)?;

            UserKey {
                key_id: user_public_key.clone(),
                storage: KeyStorage::Encrypted {
                    algorithm: "aes-256-gcm".to_string(),
                    ciphertext,
                    nonce,
                },
                display_name: Some("Default Key".to_string()),
                created_at: instance.clock().now_secs(),
                last_used: None,
                is_default: true, // First key is always default
                database_sigkeys: std::collections::HashMap::new(),
            }
        }
        _ => {
            // Passwordless: store unencrypted
            UserKey {
                key_id: user_public_key.clone(),
                storage: KeyStorage::Unencrypted {
                    key: user_private_key,
                },
                display_name: Some("Default Key".to_string()),
                created_at: instance.clock().now_secs(),
                last_used: None,
                is_default: true, // First key is always default
                database_sigkeys: std::collections::HashMap::new(),
            }
        }
    };

    let tx = user_database.new_transaction().await?;
    let keys_table = tx.get_store::<Table<UserKey>>("keys").await?;
    keys_table.insert(user_key).await?;
    tx.commit().await?;

    // 5. Create UserInfo
    let user_info = UserInfo {
        username: username.to_string(),
        user_database_id,
        password_hash,
        password_salt,
        created_at: instance.clock().now_secs(),
        status: UserStatus::Active,
    };

    // 6. Store UserInfo in _users database with auto-generated UUID
    let tx = users_db.new_transaction().await?;
    let users_table = tx.get_store::<Table<UserInfo>>("users").await?;
    let user_uuid = users_table.insert(user_info.clone()).await?; // Generate UUID primary key
    tx.commit().await?;

    Ok((user_uuid, user_info))
}

/// Login a user
///
/// This function:
/// 1. Searches for user by username in _users database
/// 2. Verifies password (if provided and required)
/// 3. Opens user's private database
/// 4. Loads and decrypts user keys (or loads unencrypted for passwordless users)
/// 5. Creates UserKeyManager with keys
/// 6. Returns User session object
///
/// # Arguments
/// * `users_db` - The _users system database
/// * `instance` - The Instance handle
/// * `username` - Username for login
/// * `password` - Optional password. None for passwordless users.
///
/// # Returns
/// A User session object with keys loaded
pub async fn login_user(
    users_db: &Database,
    instance: &Instance,
    username: impl AsRef<str>,
    password: Option<&str>,
) -> Result<super::User> {
    let username = username.as_ref();

    // 1. Search for user by username
    let users_table = users_db
        .get_store_viewer::<Table<UserInfo>>("users")
        .await?;
    let results = users_table.search(|u| u.username == username).await?;

    // Check for duplicate users (race condition detection)
    let (user_uuid, user_info) = match results.len() {
        0 => {
            return Err(UserError::UserNotFound {
                username: username.to_string(),
            }
            .into());
        }
        1 => results.into_iter().next().unwrap(),
        count => {
            // FIXME: Multiple users with same username detected!
            // This indicates the race condition occurred during user creation.
            // Resolution requires manual intervention or automated cleanup.
            return Err(UserError::DuplicateUsersDetected {
                username: username.to_string(),
                count,
            }
            .into());
        }
    };

    // Check if user is disabled
    if user_info.status != UserStatus::Active {
        return Err(UserError::UserDisabled {
            username: username.to_string(),
        }
        .into());
    }

    // 2. Verify password compatibility
    let is_passwordless = user_info.password_hash.is_none();
    match (password, is_passwordless) {
        (Some(pwd), false) => {
            // Password provided for password-protected user: verify it
            let password_hash = user_info.password_hash.as_ref().unwrap();
            super::crypto::verify_password(pwd, password_hash)?;
        }
        (None, true) => {
            // No password for passwordless user: OK
        }
        (Some(_), true) => {
            // Password provided for passwordless user: reject
            return Err(UserError::InvalidPassword.into());
        }
        (None, false) => {
            // No password for password-protected user: reject
            return Err(UserError::PasswordRequired {
                operation: "login for password-protected user".to_string(),
            }
            .into());
        }
    }

    // 3. Temporarily open user's private database to read keys (unauthenticated read)
    let temp_user_database =
        Database::open_unauthenticated(user_info.user_database_id.clone(), instance)?;

    // 4. Load keys from user database
    let keys_table = temp_user_database
        .get_store_viewer::<Table<UserKey>>("keys")
        .await?;
    let keys: Vec<UserKey> = keys_table
        .search(|_| true)
        .await? // Get all keys
        .into_iter()
        .map(|(_, key)| key)
        .collect();

    // 5. Create UserKeyManager
    let key_manager = if let Some(pwd) = password {
        // Password-protected: decrypt keys
        let password_salt =
            user_info
                .password_salt
                .as_ref()
                .ok_or_else(|| UserError::PasswordRequired {
                    operation: "decrypt keys for password-protected user".to_string(),
                })?;
        UserKeyManager::new(pwd, password_salt, keys)?
    } else {
        // Passwordless: load unencrypted keys
        UserKeyManager::new_passwordless(keys)?
    };

    // 6. Re-open user database with the user's default key using open()
    // This configures the database to use DatabaseKey with the user's key
    // so all operations work without needing keys in the backend
    let default_key_id = key_manager
        .get_default_key_id()
        .ok_or(UserError::NoKeysAvailable)?;
    let default_signing_key = key_manager
        .get_signing_key(&default_key_id)
        .ok_or_else(|| UserError::KeyNotFound {
            key_id: default_key_id.to_string(),
        })?
        .clone();

    let user_database = Database::open(
        instance.handle(),
        &user_info.user_database_id,
        DatabaseKey::new(default_signing_key),
    )
    .await?;

    // 7. Update last_login in separate table
    // TODO: this is a log, so it will grow unbounded over time and should probably be moved to a log table
    let tx = users_db.new_transaction().await?;
    let last_login_table = tx.get_store::<Table<i64>>("last_login").await?;
    last_login_table
        .set(&user_uuid, instance.clock().now_secs())
        .await?;
    tx.commit().await?;

    // 8. Create User session
    Ok(User::new(
        user_uuid,
        user_info,
        user_database,
        instance.handle(),
        key_manager,
    ))
}

/// List all users in the system
///
/// # Arguments
/// * `users_db` - The _users system database
///
/// # Returns
/// Vector of usernames
pub async fn list_users(users_db: &Database) -> Result<Vec<String>> {
    let users_table = users_db
        .get_store_viewer::<Table<UserInfo>>("users")
        .await?;
    let users: Vec<UserInfo> = users_table
        .search(|_| true)
        .await? // Get all users
        .into_iter()
        .map(|(_, user)| user)
        .collect();
    Ok(users.into_iter().map(|u| u.username).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Instance;
    use crate::backend::database::InMemory;
    use crate::store::DocStore;
    use crate::store::SettingsStore;

    use std::sync::Arc;

    /// Test helper: Create Instance with device key initialized
    ///
    /// Uses FixedClock for controllable timestamps.
    async fn setup_instance() -> (Instance, PrivateKey) {
        use crate::clock::FixedClock;

        let backend = Arc::new(InMemory::new());

        // Create Instance with FixedClock for controllable timestamps
        let instance = Instance::create_internal(backend, Arc::new(FixedClock::default()))
            .await
            .unwrap();

        // Get the device key from the instance
        let device_key = instance.device_key().clone();

        (instance, device_key)
    }

    #[tokio::test]
    async fn test_create_instance_database() {
        let (instance, device_key) = setup_instance().await;

        let instance_db = create_instance_database(&instance, &device_key)
            .await
            .unwrap();

        // Verify database was created
        assert!(!instance_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = instance_db.new_transaction().await.unwrap();
        let doc_store = transaction
            .get_store::<DocStore>("_settings")
            .await
            .unwrap();
        let name = doc_store.get_string("name").await.unwrap();
        assert_eq!(name, INSTANCE);

        // Verify auth settings - key is stored by pubkey
        let pubkey_str = instance.device_id_string();
        let settings_store = SettingsStore::new(&transaction).unwrap();
        let auth_settings = settings_store.auth_snapshot().await.unwrap();
        let device_key = auth_settings.get_key_by_pubkey(&pubkey_str).unwrap();
        assert_eq!(device_key.permissions(), &Permission::Admin(0));
        assert_eq!(device_key.name(), None);
    }

    #[tokio::test]
    async fn test_create_users_database() {
        let (instance, device_key) = setup_instance().await;

        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Verify database was created
        assert!(!users_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = users_db.new_transaction().await.unwrap();
        let doc_store = transaction
            .get_store::<DocStore>("_settings")
            .await
            .unwrap();
        let name = doc_store.get_string("name").await.unwrap();
        assert_eq!(name, USERS);
    }

    #[tokio::test]
    async fn test_create_databases_tracking() {
        let (instance, device_key) = setup_instance().await;

        let databases_db = create_databases_tracking(&instance, &device_key)
            .await
            .unwrap();

        // Verify database was created
        assert!(!databases_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = databases_db.new_transaction().await.unwrap();
        let doc_store = transaction
            .get_store::<DocStore>("_settings")
            .await
            .unwrap();
        let name = doc_store.get_string("name").await.unwrap();
        assert_eq!(name, DATABASES);
    }

    #[tokio::test]
    async fn test_system_databases_haveadmin_auth() {
        let (instance, device_key) = setup_instance().await;

        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Verify device key has admin access - key is stored by pubkey
        let pubkey_str = instance.device_id_string();
        let transaction = users_db.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();
        let auth_settings = settings_store.auth_snapshot().await.unwrap();
        let device_key = auth_settings.get_key_by_pubkey(&pubkey_str).unwrap();

        assert_eq!(device_key.permissions(), &Permission::Admin(0));
        assert_eq!(device_key.name(), None);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_create_user() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Create a user with password
        let (user_uuid, user_info) =
            create_user(&users_db, &instance, "alice", Some("password123"))
                .await
                .unwrap();

        // Verify user info
        assert_eq!(user_info.username, "alice");
        assert_eq!(user_info.status, UserStatus::Active);
        assert!(user_info.password_hash.is_some());
        assert!(user_info.password_salt.is_some());
        assert!(!user_uuid.is_empty());

        // Verify user was stored in _users database
        let users_table = users_db
            .get_store_viewer::<Table<UserInfo>>("users")
            .await
            .unwrap();
        let stored_user = users_table.get(&user_uuid).await.unwrap();
        assert_eq!(stored_user.username, "alice");
    }

    #[tokio::test]
    async fn test_create_user_passwordless() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Create a passwordless user
        let (user_uuid, user_info) = create_user(&users_db, &instance, "bob", None)
            .await
            .unwrap();

        // Verify user info
        assert_eq!(user_info.username, "bob");
        assert_eq!(user_info.status, UserStatus::Active);
        assert!(user_info.password_hash.is_none());
        assert!(user_info.password_salt.is_none());
        assert!(!user_uuid.is_empty());

        // Verify user was stored in _users database
        let users_table = users_db
            .get_store_viewer::<Table<UserInfo>>("users")
            .await
            .unwrap();
        let stored_user = users_table.get(&user_uuid).await.unwrap();
        assert_eq!(stored_user.username, "bob");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_create_duplicate_user() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Create first user
        create_user(&users_db, &instance, "alice", Some("password123"))
            .await
            .unwrap();

        // Try to create duplicate
        let result = create_user(&users_db, &instance, "alice", Some("password456")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_login_user() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Create a user with password
        create_user(&users_db, &instance, "bob", Some("bobpassword"))
            .await
            .unwrap();

        // Login user
        let user = login_user(&users_db, &instance, "bob", Some("bobpassword"))
            .await
            .unwrap();

        // Verify user session
        assert_eq!(user.username(), "bob");

        // Verify last_login was recorded in separate table
        let last_login_table = users_db
            .get_store_viewer::<Table<i64>>("last_login")
            .await
            .unwrap();
        let last_login = last_login_table.get(user.user_uuid()).await.unwrap();
        assert!(last_login > 0);
    }

    #[tokio::test]
    async fn test_login_user_passwordless() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Create a passwordless user
        create_user(&users_db, &instance, "charlie", None)
            .await
            .unwrap();

        // Login user without password
        let user = login_user(&users_db, &instance, "charlie", None)
            .await
            .unwrap();

        // Verify user session
        assert_eq!(user.username(), "charlie");

        // Verify last_login was recorded
        let last_login_table = users_db
            .get_store_viewer::<Table<i64>>("last_login")
            .await
            .unwrap();
        let last_login = last_login_table.get(user.user_uuid()).await.unwrap();
        assert!(last_login > 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_login_wrong_password() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Create a user
        create_user(&users_db, &instance, "dave", Some("correct_password"))
            .await
            .unwrap();

        // Try to login with wrong password
        let result = login_user(&users_db, &instance, "dave", Some("wrong_password")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_login_password_mismatch() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Create a passwordless user
        create_user(&users_db, &instance, "eve", None)
            .await
            .unwrap();

        // Try to login with password (should fail)
        let result = login_user(&users_db, &instance, "eve", Some("password")).await;
        assert!(result.is_err());

        // Create a password-protected user
        create_user(&users_db, &instance, "frank", Some("password"))
            .await
            .unwrap();

        // Try to login without password (should fail)
        let result = login_user(&users_db, &instance, "frank", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_login_nonexistent_user() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Try to login user that doesn't exist
        let result = login_user(&users_db, &instance, "nonexistent", Some("password")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_list_users() {
        let (instance, device_key) = setup_instance().await;
        let users_db = create_users_database(&instance, &device_key).await.unwrap();

        // Initially no users
        let users = list_users(&users_db).await.unwrap();
        assert_eq!(users.len(), 0);

        // Create some users (mix of password-protected and passwordless)
        create_user(&users_db, &instance, "alice", Some("pass1"))
            .await
            .unwrap();
        create_user(&users_db, &instance, "bob", None)
            .await
            .unwrap();
        create_user(&users_db, &instance, "charlie", Some("pass3"))
            .await
            .unwrap();

        // List users
        let users = list_users(&users_db).await.unwrap();
        assert_eq!(users.len(), 3);
        assert!(users.contains(&"alice".into()));
        assert!(users.contains(&"bob".into()));
        assert!(users.contains(&"charlie".into()));
    }
}
