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
    /// Stable internal user UUID (Table primary key)
    user_uuid: String,

    /// Username (login identifier)
    username: String,

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
    /// * `user_uuid` - Internal UUID (Table primary key)
    /// * `user_info` - User information from _users database
    /// * `user_database` - The user's private database
    /// * `backend` - Backend reference
    /// * `key_manager` - Initialized key manager with decrypted keys
    #[allow(dead_code)]
    pub(crate) fn new(
        user_uuid: String,
        user_info: UserInfo,
        user_database: Database,
        backend: Arc<dyn BackendDB>,
        key_manager: UserKeyManager,
    ) -> Self {
        Self {
            user_uuid,
            username: user_info.username.clone(),
            user_database,
            backend,
            key_manager,
            user_info,
        }
    }

    // === Basic Session Methods ===

    /// Get the internal user UUID (stable identifier)
    pub fn user_uuid(&self) -> &str {
        &self.user_uuid
    }

    /// Get the username (login identifier)
    pub fn username(&self) -> &str {
        &self.username
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

    // === Database Loading ===

    /// Load a database using this user's keys
    ///
    /// This is the primary method for users to access databases. It automatically:
    /// 1. Finds an appropriate key that has access to the database
    /// 2. Retrieves the decrypted SigningKey from the UserKeyManager
    /// 3. Gets the SigKey mapping for this database
    /// 4. Creates a Database instance configured with the user's key
    ///
    /// The returned Database can be used normally - all transactions will
    /// automatically use the user's provided key instead of looking up keys
    /// from backend storage.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database to load
    ///
    /// # Returns
    /// A Database instance configured to use this user's keys
    ///
    /// # Errors
    /// - Returns an error if no key is found for the database
    /// - Returns an error if no SigKey mapping exists
    /// - Returns an error if the key is not in the UserKeyManager
    #[allow(dead_code)]
    pub fn load_database(&self, database_id: &crate::entry::ID) -> Result<Database> {
        // Find an appropriate key for this database
        let key_id = self.find_key_for_database(database_id)?.ok_or_else(|| {
            super::errors::UserError::NoKeyForDatabase {
                database_id: database_id.clone(),
            }
        })?;

        // Get the SigningKey from UserKeyManager
        let signing_key = self.key_manager.get_signing_key(&key_id).ok_or_else(|| {
            super::errors::UserError::KeyNotFound {
                key_id: key_id.clone(),
            }
        })?;

        // Get the SigKey mapping for this database
        let sigkey = self
            .get_database_sigkey(&key_id, database_id)?
            .ok_or_else(|| super::errors::UserError::NoSigKeyMapping {
                key_id: key_id.clone(),
                database_id: database_id.clone(),
            })?;

        // Create Database with user-provided key
        Database::new_with_key(
            self.backend.clone(),
            database_id,
            signing_key.clone(),
            sigkey,
        )
    }

    /// Find the best key for accessing a database
    ///
    /// Searches the user's keys to find one that can access the specified database.
    /// Considers the SigKey mappings stored in user key metadata.
    ///
    /// Returns the key_id of a suitable key, preferring keys with mappings for this database.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    ///
    /// # Returns
    /// Some(key_id) if a suitable key is found, None if no keys can access this database
    #[allow(dead_code)]
    pub fn find_key_for_database(&self, database_id: &crate::entry::ID) -> Result<Option<String>> {
        // Iterate through all keys and find ones with SigKey mappings for this database
        for key_id in self.key_manager.list_key_ids() {
            if let Some(metadata) = self.key_manager.get_key_metadata(&key_id)
                && metadata.database_sigkeys.contains_key(database_id)
            {
                return Ok(Some(key_id));
            }
        }

        // No key found with mapping for this database
        Ok(None)
    }

    /// Get the SigKey mapping for a key in a specific database
    ///
    /// Users map their private keys to SigKey identifiers on a per-database basis.
    /// This method retrieves the SigKey identifier that a specific key uses in
    /// a specific database's authentication settings.
    ///
    /// # Arguments
    /// * `key_id` - The user's key identifier
    /// * `database_id` - The database ID
    ///
    /// # Returns
    /// Some(sigkey) if a mapping exists, None if no mapping is configured
    ///
    /// # Errors
    /// Returns an error if the key_id doesn't exist in the UserKeyManager
    #[allow(dead_code)]
    pub fn get_database_sigkey(
        &self,
        key_id: &str,
        database_id: &crate::entry::ID,
    ) -> Result<Option<String>> {
        let metadata = self.key_manager.get_key_metadata(key_id).ok_or_else(|| {
            super::errors::UserError::KeyNotFound {
                key_id: key_id.to_string(),
            }
        })?;

        Ok(metadata.database_sigkeys.get(database_id).cloned())
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
            username: "test_user".to_string(),
            user_database_id: user_database.root_id().clone(),
            password_hash,
            password_salt: password_salt.clone(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
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

        // Create user with UUID (using a test UUID)
        User::new(
            "test-uuid-1234".to_string(),
            user_info,
            user_database,
            backend,
            key_manager,
        )
    }

    #[test]
    fn test_user_creation() {
        let user = create_test_user_session();
        assert_eq!(user.username(), "test_user");
        assert_eq!(user.user_uuid(), "test-uuid-1234");
    }

    #[test]
    fn test_user_getters() {
        let user = create_test_user_session();

        assert_eq!(user.username(), "test_user");
        assert_eq!(user.user_uuid(), "test-uuid-1234");
        assert_eq!(user.user_info().username, "test_user");
        assert!(!user.user_database().root_id().to_string().is_empty());
    }

    #[test]
    fn test_user_logout() {
        let user = create_test_user_session();
        let username = user.username().to_string();

        // Logout consumes the user
        user.logout().unwrap();

        // User is dropped, keys should be cleared
        assert_eq!(username, "test_user");
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
