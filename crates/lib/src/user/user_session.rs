//! User session management
//!
//! Represents an authenticated user session with decrypted keys.

use std::sync::Arc;

use super::{UserKeyManager, types::UserInfo};
use crate::{Database, Result, backend::BackendDB};

/// User session object, returned after successful login
///
/// Represents an authenticated user with decrypted private keys loaded in memory.
/// The User struct provides access to key management, database preferences, and
/// bootstrap approval operations.
pub struct User {
    /// User identifier
    user_id: String,

    /// User's private database (contains encrypted keys and preferences)
    user_database: Database,

    /// Backend reference for database operations
    backend: Arc<dyn BackendDB>,

    /// Decrypted user keys (in memory only during session)
    key_manager: UserKeyManager,

    /// User info (cached from _users database)
    user_info: UserInfo,
}

impl User {
    /// Create a new User session
    ///
    /// This is an internal constructor used after successful login.
    /// Use `Instance::login_user()` to create a User session.
    ///
    /// # Arguments
    /// * `user_info` - User information from _users database
    /// * `user_database` - The user's private database
    /// * `backend` - Backend reference
    /// * `key_manager` - Initialized key manager with decrypted keys
    #[allow(dead_code)]
    pub(crate) fn new(
        user_info: UserInfo,
        user_database: Database,
        backend: Arc<dyn BackendDB>,
        key_manager: UserKeyManager,
    ) -> Self {
        Self {
            user_id: user_info.user_id.clone(),
            user_database,
            backend,
            key_manager,
            user_info,
        }
    }

    // === Basic Session Methods ===

    /// Get the user ID
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Get a reference to the user's database
    pub fn user_database(&self) -> &Database {
        &self.user_database
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Arc<dyn BackendDB> {
        &self.backend
    }

    /// Get a reference to the user info
    pub fn user_info(&self) -> &UserInfo {
        &self.user_info
    }

    /// Logout (consumes self and clears decrypted keys from memory)
    ///
    /// After logout, all decrypted keys are zeroized and the session is ended.
    /// Keys are automatically cleared when the User is dropped.
    pub fn logout(self) -> Result<()> {
        // Consume self, all keys are stored in other Types that zeroize themselves on drop
        Ok(())
    }

    // === Key Manager Access (Internal) ===

    /// Get a reference to the key manager (for internal use)
    #[allow(dead_code)]
    pub(crate) fn key_manager(&self) -> &UserKeyManager {
        &self.key_manager
    }

    /// Get a mutable reference to the key manager (for internal use)
    #[allow(dead_code)]
    pub(crate) fn key_manager_mut(&mut self) -> &mut UserKeyManager {
        &mut self.key_manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::crypto::{format_public_key, generate_keypair},
        backend::database::InMemory,
        user::{
            crypto::{derive_encryption_key, encrypt_private_key, hash_password},
            types::{UserKey, UserStatus},
        },
    };
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_test_user_session() -> User {
        let backend = Arc::new(InMemory::new());

        // Create user database
        let (device_key, device_pubkey) = generate_keypair();
        let device_pubkey_str = format_public_key(&device_pubkey);

        backend
            .store_private_key("_device_key", device_key.clone())
            .unwrap();

        let mut db_settings = crate::crdt::Doc::new();
        db_settings.set_string("name", "test_user_db");

        let mut auth_settings = crate::auth::settings::AuthSettings::new();
        auth_settings
            .add_key(
                "_device_key",
                crate::auth::types::AuthKey::active(
                    &device_pubkey_str,
                    crate::auth::types::Permission::Admin(0),
                )
                .unwrap(),
            )
            .unwrap();
        db_settings.set_doc("auth", auth_settings.as_doc().clone());

        let user_database = Database::new(db_settings, backend.clone(), "_device_key").unwrap();

        // Create user info
        let password = "test_password";
        let (password_hash, password_salt) = hash_password(password).unwrap();

        let user_info = UserInfo {
            user_id: "test_user".to_string(),
            user_database_id: user_database.root_id().clone(),
            password_hash,
            password_salt: password_salt.clone(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            last_login: None,
            status: UserStatus::Active,
        };

        // Create encrypted key for key manager
        let encryption_key = derive_encryption_key(password, &password_salt).unwrap();
        let (encrypted_key, nonce) = encrypt_private_key(&device_key, &encryption_key).unwrap();

        let user_key = UserKey {
            key_id: "_device_key".to_string(),
            encrypted_private_key: encrypted_key,
            nonce,
            display_name: Some("Device Key".to_string()),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            last_used: None,
            database_sigkeys: HashMap::new(),
        };

        // Create key manager
        let key_manager = UserKeyManager::new(password, &password_salt, vec![user_key]).unwrap();

        User::new(user_info, user_database, backend, key_manager)
    }

    #[test]
    fn test_user_creation() {
        let user = create_test_user_session();
        assert_eq!(user.user_id(), "test_user");
    }

    #[test]
    fn test_user_getters() {
        let user = create_test_user_session();

        assert_eq!(user.user_id(), "test_user");
        assert_eq!(user.user_info().user_id, "test_user");
        assert!(!user.user_database().root_id().to_string().is_empty());
    }

    #[test]
    fn test_user_logout() {
        let user = create_test_user_session();
        let user_id = user.user_id().to_string();

        // Logout consumes the user
        user.logout().unwrap();

        // User is dropped, keys should be cleared
        assert_eq!(user_id, "test_user");
    }

    #[test]
    fn test_user_drop() {
        {
            let _user = create_test_user_session();
            // User will be dropped when it goes out of scope
        }
        // Keys should be cleared automatically
    }
}
