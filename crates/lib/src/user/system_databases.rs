//! System database initialization for the user system
//!
//! Creates and manages _users and _databases system databases.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    User,
    crypto::{derive_encryption_key, encrypt_private_key, hash_password},
    errors::UserError,
    key_manager::UserKeyManager,
    types::{KeyEncryption, UserInfo, UserKey, UserStatus},
};
use crate::{
    Database, Result,
    auth::{
        crypto::generate_keypair,
        settings::AuthSettings,
        types::{AuthKey, Permission},
    },
    backend::BackendDB,
    constants::{DATABASES, INSTANCE, USERS},
    crdt::Doc,
    store::Table,
};

/// Create the _instance system database
///
/// This database stores Instance-level configuration and metadata.
/// It is authenticated with the Instance's _device_key.
///
/// # Arguments
/// * `backend` - The database backend
/// * `device_pubkey` - The public key for _device_key (used as admin)
///
/// # Returns
/// The _instance Database
pub fn create_instance_database(
    backend: Arc<dyn BackendDB>,
    device_pubkey: &str,
) -> Result<Database> {
    // Create database settings
    let mut settings = Doc::new();
    settings.set_string("name", INSTANCE);
    settings.set_string("type", "system");
    settings.set_string("description", "Instance configuration and management");

    // Set up auth with device key as admin
    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "_device_key",
        AuthKey::active(device_pubkey, Permission::Admin(0))?,
    )?;
    settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create the database
    let database = Database::new(settings, backend, "_device_key")?;

    Ok(database)
}

/// Create the _users system database
///
/// This database stores the user directory mapping user_id → UserInfo.
/// It is authenticated with the Instance's _device_key.
///
/// # Arguments
/// * `backend` - The database backend
/// * `device_pubkey` - The public key for _device_key (used as admin)
///
/// # Returns
/// The created _users Database
pub fn create_users_database(backend: Arc<dyn BackendDB>, device_pubkey: &str) -> Result<Database> {
    // Create settings for _users database
    let mut settings = Doc::new();
    settings.set_string("name", USERS);
    settings.set_string("type", "system");
    settings.set_string("description", "User directory database");

    // Create auth settings with device key as admin
    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "_device_key",
        AuthKey::active(device_pubkey, Permission::Admin(0))?,
    )?;

    settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create the database authenticated as _device_key
    let database = Database::new(settings, backend, "_device_key")?;

    Ok(database)
}

/// Create the _databases tracking database
///
/// This database stores the database tracking information mapping
/// database_id → DatabaseTracking.
/// It is authenticated with the Instance's _device_key.
///
/// # Arguments
/// * `backend` - The database backend
/// * `device_pubkey` - The public key for _device_key (used as admin)
///
/// # Returns
/// The created _databases Database
pub fn create_databases_tracking(
    backend: Arc<dyn BackendDB>,
    device_pubkey: &str,
) -> Result<Database> {
    // Create settings for _databases database
    let mut settings = Doc::new();
    settings.set_string("name", DATABASES);
    settings.set_string("type", "system");
    settings.set_string("description", "Database tracking and registry");

    // Create auth settings with device key as admin
    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "_device_key",
        AuthKey::active(device_pubkey, Permission::Admin(0))?,
    )?;

    settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create the database authenticated as _device_key
    let database = Database::new(settings, backend, "_device_key")?;

    Ok(database)
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
/// * `backend` - The database backend
/// * `username` - Unique username for login
/// * `password` - Optional password. If None, creates passwordless user (instant login, no encryption)
///
/// # Returns
/// A tuple of (user_uuid, UserInfo) where user_uuid is the generated primary key
pub fn create_user(
    users_db: &Database,
    backend: Arc<dyn BackendDB>,
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
    let users_table = users_db.get_store_viewer::<Table<UserInfo>>("users")?;
    let existing = users_table.search(|u| u.username == username)?;
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
    let user_public_key_str = crate::auth::crypto::format_public_key(&user_public_key);

    // 3. Create user database with authentication for both _device_key and user's key
    let mut user_db_settings = Doc::new();
    user_db_settings.set_string("name", format!("_user_{}", username));
    user_db_settings.set_string("type", "user");
    user_db_settings.set_string("description", format!("User database for {}", username));

    // Get device key public key for auth settings
    let device_private_key =
        backend
            .get_private_key("_device_key")?
            .ok_or_else(|| UserError::KeyNotFound {
                key_id: "_device_key".to_string(),
            })?;
    let device_pubkey = device_private_key.verifying_key();
    let device_pubkey_str = crate::auth::crypto::format_public_key(&device_pubkey);

    // Set up authentication with both keys
    let mut auth_settings = AuthSettings::new();
    // TODO: Is it possible for the device key to only have Read permission?
    // Then the device can read it to let the user login but that's it
    // (Though at the moment it wouldn't need explicit read access, every local DB is readable)
    auth_settings.add_key(
        "_device_key",
        AuthKey::active(&device_pubkey_str, Permission::Admin(0))?,
    )?;
    auth_settings.add_key(
        &user_public_key_str,
        AuthKey::active(&user_public_key_str, Permission::Admin(0))?,
    )?;
    user_db_settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create database using _device_key to sign initial entries
    let user_database = Database::new(user_db_settings, backend.clone(), "_device_key")?;
    let user_database_id = user_database.root_id().clone();

    // 4. Store user's private key (encrypted or unencrypted based on password)
    let user_key = match (password, &password_salt) {
        (Some(pwd), Some(salt)) => {
            // Password-protected: encrypt the key
            let encryption_key = derive_encryption_key(pwd, salt)?;
            let (encrypted_key, nonce) = encrypt_private_key(&user_private_key, &encryption_key)?;

            UserKey {
                key_id: user_public_key_str.clone(),
                private_key_bytes: encrypted_key,
                encryption: KeyEncryption::Encrypted { nonce },
                display_name: Some("Default Key".to_string()),
                created_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                last_used: None,
                is_default: true, // First key is always default
                database_sigkeys: std::collections::HashMap::new(),
            }
        }
        _ => {
            // Passwordless: store unencrypted
            UserKey {
                key_id: user_public_key_str.clone(),
                private_key_bytes: user_private_key.to_bytes().to_vec(),
                encryption: KeyEncryption::Unencrypted,
                display_name: Some("Default Key".to_string()),
                created_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                last_used: None,
                is_default: true, // First key is always default
                database_sigkeys: std::collections::HashMap::new(),
            }
        }
    };

    let tx = user_database.new_transaction()?;
    let keys_table = tx.get_store::<Table<UserKey>>("keys")?;
    keys_table.insert(user_key)?;
    tx.commit()?;

    // 5. Create UserInfo
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let user_info = UserInfo {
        username: username.to_string(),
        user_database_id,
        password_hash,
        password_salt,
        created_at: now,
        status: UserStatus::Active,
    };

    // 6. Store UserInfo in _users database with auto-generated UUID
    let tx = users_db.new_transaction()?;
    let users_table = tx.get_store::<Table<UserInfo>>("users")?;
    let user_uuid = users_table.insert(user_info.clone())?; // Generate UUID primary key
    tx.commit()?;

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
/// * `backend` - The database backend
/// * `username` - Username for login
/// * `password` - Optional password. None for passwordless users.
///
/// # Returns
/// A User session object with keys loaded
pub fn login_user(
    users_db: &Database,
    backend: Arc<dyn BackendDB>,
    username: impl AsRef<str>,
    password: Option<&str>,
) -> Result<super::User> {
    let username = username.as_ref();

    // 1. Search for user by username
    let users_table = users_db.get_store_viewer::<Table<UserInfo>>("users")?;
    let results = users_table.search(|u| u.username == username)?;

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
        Database::new_from_id(user_info.user_database_id.clone(), backend.clone())?;

    // 4. Load keys from user database
    let keys_table = temp_user_database.get_store_viewer::<Table<UserKey>>("keys")?;
    let keys: Vec<UserKey> = keys_table
        .search(|_| true)? // Get all keys
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

    // 6. Re-open user database with the user's default key using load_with_key()
    // This configures the database to use KeySource::Provided with the user's key
    // so all operations work without needing keys in the backend
    let default_key_id = key_manager
        .get_default_key_id()
        .ok_or(UserError::NoKeysAvailable)?;
    let default_signing_key = key_manager
        .get_signing_key(&default_key_id)
        .ok_or_else(|| UserError::KeyNotFound {
            key_id: default_key_id.clone(),
        })?
        .clone();

    let user_database = Database::load_with_key(
        backend.clone(),
        &user_info.user_database_id,
        default_signing_key,
        default_key_id,
    )?;

    // 7. Update last_login in separate table
    // TODO: this is a log, so it will grow unbounded over time and should probably be moved to a log table
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Use _device_key for updating system database
    let tx = users_db.new_authenticated_operation("_device_key")?;
    let last_login_table = tx.get_store::<Table<u64>>("last_login")?;
    last_login_table.set(&user_uuid, now)?;
    tx.commit()?;

    // 8. Create User session
    Ok(User::new(
        user_uuid,
        user_info,
        user_database,
        backend,
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
pub fn list_users(users_db: &Database) -> Result<Vec<String>> {
    let users_table = users_db.get_store_viewer::<Table<UserInfo>>("users")?;
    let users: Vec<UserInfo> = users_table
        .search(|_| true)? // Get all users
        .into_iter()
        .map(|(_, user)| user)
        .collect();
    Ok(users.into_iter().map(|u| u.username).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::crypto::{format_public_key, generate_keypair};
    use crate::backend::database::InMemory;
    use crate::store::DocStore;
    use crate::store::SettingsStore;

    /// Test helper: Create backend with device key initialized
    fn setup_backend() -> (Arc<InMemory>, String) {
        let backend = Arc::new(InMemory::new());
        let (device_key, device_pubkey) = generate_keypair();
        let pubkey_str = format_public_key(&device_pubkey);
        backend
            .store_private_key("_device_key", device_key)
            .unwrap();
        (backend, pubkey_str)
    }

    #[test]
    fn test_create_instance_database() {
        let (backend, pubkey_str) = setup_backend();

        let instance_db = create_instance_database(backend.clone(), &pubkey_str).unwrap();

        // Verify database was created
        assert!(!instance_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = instance_db.new_transaction().unwrap();
        let doc_store = transaction.get_store::<DocStore>("_settings").unwrap();
        let name = doc_store.get_string("name").unwrap();
        assert_eq!(name, INSTANCE);

        // Verify auth settings
        let settings_store = SettingsStore::new(&transaction).unwrap();
        let auth_settings = settings_store.get_auth_settings().unwrap();
        let device_key = auth_settings.get_key("_device_key").unwrap();
        assert_eq!(device_key.permissions(), &Permission::Admin(0));
        assert_eq!(device_key.pubkey(), &pubkey_str);
    }

    #[test]
    fn test_create_users_database() {
        let (backend, pubkey_str) = setup_backend();

        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Verify database was created
        assert!(!users_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = users_db.new_transaction().unwrap();
        let doc_store = transaction.get_store::<DocStore>("_settings").unwrap();
        let name = doc_store.get_string("name").unwrap();
        assert_eq!(name, USERS);
    }

    #[test]
    fn test_create_databases_tracking() {
        let (backend, pubkey_str) = setup_backend();

        let databases_db = create_databases_tracking(backend.clone(), &pubkey_str).unwrap();

        // Verify database was created
        assert!(!databases_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = databases_db.new_transaction().unwrap();
        let doc_store = transaction.get_store::<DocStore>("_settings").unwrap();
        let name = doc_store.get_string("name").unwrap();
        assert_eq!(name, DATABASES);
    }

    #[test]
    fn test_system_databases_have_device_key_auth() {
        let (backend, pubkey_str) = setup_backend();

        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Verify _device_key has admin access
        let transaction = users_db.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();
        let auth_settings = settings_store.get_auth_settings().unwrap();
        let device_key = auth_settings.get_key("_device_key").unwrap();

        assert_eq!(device_key.permissions(), &Permission::Admin(0));
        assert_eq!(device_key.pubkey(), &pubkey_str);
    }

    #[test]
    fn test_create_user() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Create a user with password
        let (user_uuid, user_info) =
            create_user(&users_db, backend.clone(), "alice", Some("password123")).unwrap();

        // Verify user info
        assert_eq!(user_info.username, "alice");
        assert_eq!(user_info.status, UserStatus::Active);
        assert!(user_info.password_hash.is_some());
        assert!(user_info.password_salt.is_some());
        assert!(!user_uuid.is_empty());

        // Verify user was stored in _users database
        let users_table = users_db
            .get_store_viewer::<Table<UserInfo>>("users")
            .unwrap();
        let stored_user = users_table.get(&user_uuid).unwrap();
        assert_eq!(stored_user.username, "alice");
    }

    #[test]
    fn test_create_user_passwordless() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Create a passwordless user
        let (user_uuid, user_info) = create_user(&users_db, backend.clone(), "bob", None).unwrap();

        // Verify user info
        assert_eq!(user_info.username, "bob");
        assert_eq!(user_info.status, UserStatus::Active);
        assert!(user_info.password_hash.is_none());
        assert!(user_info.password_salt.is_none());
        assert!(!user_uuid.is_empty());

        // Verify user was stored in _users database
        let users_table = users_db
            .get_store_viewer::<Table<UserInfo>>("users")
            .unwrap();
        let stored_user = users_table.get(&user_uuid).unwrap();
        assert_eq!(stored_user.username, "bob");
    }

    #[test]
    fn test_create_duplicate_user() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Create first user
        create_user(&users_db, backend.clone(), "alice", Some("password123")).unwrap();

        // Try to create duplicate
        let result = create_user(&users_db, backend.clone(), "alice", Some("password456"));
        assert!(result.is_err());
    }

    #[test]
    fn test_login_user() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Create a user with password
        create_user(&users_db, backend.clone(), "bob", Some("bobpassword")).unwrap();

        // Login user
        let user = login_user(&users_db, backend.clone(), "bob", Some("bobpassword")).unwrap();

        // Verify user session
        assert_eq!(user.username(), "bob");

        // Verify last_login was recorded in separate table
        let last_login_table = users_db
            .get_store_viewer::<Table<u64>>("last_login")
            .unwrap();
        let last_login = last_login_table.get(user.user_uuid()).unwrap();
        assert!(last_login > 0);
    }

    #[test]
    fn test_login_user_passwordless() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Create a passwordless user
        create_user(&users_db, backend.clone(), "charlie", None).unwrap();

        // Login user without password
        let user = login_user(&users_db, backend.clone(), "charlie", None).unwrap();

        // Verify user session
        assert_eq!(user.username(), "charlie");

        // Verify last_login was recorded
        let last_login_table = users_db
            .get_store_viewer::<Table<u64>>("last_login")
            .unwrap();
        let last_login = last_login_table.get(user.user_uuid()).unwrap();
        assert!(last_login > 0);
    }

    #[test]
    fn test_login_wrong_password() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Create a user
        create_user(&users_db, backend.clone(), "dave", Some("correct_password")).unwrap();

        // Try to login with wrong password
        let result = login_user(&users_db, backend.clone(), "dave", Some("wrong_password"));
        assert!(result.is_err());
    }

    #[test]
    fn test_login_password_mismatch() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Create a passwordless user
        create_user(&users_db, backend.clone(), "eve", None).unwrap();

        // Try to login with password (should fail)
        let result = login_user(&users_db, backend.clone(), "eve", Some("password"));
        assert!(result.is_err());

        // Create a password-protected user
        create_user(&users_db, backend.clone(), "frank", Some("password")).unwrap();

        // Try to login without password (should fail)
        let result = login_user(&users_db, backend.clone(), "frank", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_login_nonexistent_user() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Try to login user that doesn't exist
        let result = login_user(&users_db, backend.clone(), "nonexistent", Some("password"));
        assert!(result.is_err());
    }

    #[test]
    fn test_list_users() {
        let (backend, pubkey_str) = setup_backend();
        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Initially no users
        let users = list_users(&users_db).unwrap();
        assert_eq!(users.len(), 0);

        // Create some users (mix of password-protected and passwordless)
        create_user(&users_db, backend.clone(), "alice", Some("pass1")).unwrap();
        create_user(&users_db, backend.clone(), "bob", None).unwrap();
        create_user(&users_db, backend.clone(), "charlie", Some("pass3")).unwrap();

        // List users
        let users = list_users(&users_db).unwrap();
        assert_eq!(users.len(), 3);
        assert!(users.contains(&"alice".into()));
        assert!(users.contains(&"bob".into()));
        assert!(users.contains(&"charlie".into()));
    }
}
