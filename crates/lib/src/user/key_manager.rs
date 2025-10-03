//! User key manager for session-based key management
//!
//! Manages decrypted private keys during a user session.
//! Keys are decrypted on login and kept in memory for the session duration.

use std::collections::HashMap;

use ed25519_dalek::SigningKey;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::{
    crypto::{decrypt_private_key, derive_encryption_key, encrypt_private_key},
    errors::UserError,
    types::UserKey,
};
use crate::Result;

/// Internal key manager that holds decrypted keys during user session
///
/// # Security
///
/// This struct holds sensitive cryptographic material in memory:
/// - `decrypted_keys`: Contains plaintext SigningKeys (ed25519_dalek implements Drop with zeroization)
/// - `encryption_key`: Password-derived key (zeroized via manual Zeroize impl)
///
/// All sensitive data is zeroized when the struct is dropped.
pub struct UserKeyManager {
    /// Decrypted keys (key_id → SigningKey)
    decrypted_keys: HashMap<String, SigningKey>,

    /// Key metadata (loaded from user database)
    key_metadata: HashMap<String, UserKey>,

    /// User's password-derived encryption key (for saving new keys)
    encryption_key: Vec<u8>,
}

impl UserKeyManager {
    /// Create from user password and encrypted keys
    ///
    /// Decrypts all provided keys using the password-derived encryption key.
    ///
    /// # Arguments
    /// * `password` - The user's password
    /// * `salt` - The password salt (base64 encoded string)
    /// * `encrypted_keys` - Vec of encrypted UserKey entries from database
    ///
    /// # Returns
    /// A UserKeyManager with all keys decrypted and ready for use
    pub fn new(password: &str, salt: &str, encrypted_keys: Vec<UserKey>) -> Result<Self> {
        // Derive encryption key from password
        let encryption_key = derive_encryption_key(password, salt)?;

        // Create empty manager with pre-allocated capacity
        let capacity = encrypted_keys.len();
        let mut manager = Self {
            decrypted_keys: HashMap::with_capacity(capacity),
            key_metadata: HashMap::with_capacity(capacity),
            encryption_key,
        };

        // Add all keys using add_key
        for user_key in encrypted_keys {
            manager.add_key(user_key)?;
        }

        Ok(manager)
    }

    /// Get a decrypted signing key
    ///
    /// # Arguments
    /// * `key_id` - The key identifier
    ///
    /// # Returns
    /// A reference to the SigningKey if found
    pub fn get_signing_key(&self, key_id: &str) -> Option<&SigningKey> {
        self.decrypted_keys.get(key_id)
    }

    /// Add a key to the manager from encrypted metadata
    ///
    /// Decrypts the key from the provided metadata and stores it in memory.
    /// Use `serialize_keys()` to get updated encrypted keys for storage.
    ///
    /// # Arguments
    /// * `metadata` - The UserKey metadata with encrypted_private_key and nonce
    ///
    /// # Returns
    /// Ok(()) if the key was successfully decrypted and added
    pub fn add_key(&mut self, metadata: UserKey) -> Result<()> {
        let key_id = metadata.key_id.clone();

        // Decrypt the key from metadata
        let signing_key = decrypt_private_key(
            &metadata.encrypted_private_key,
            &metadata.nonce,
            &self.encryption_key,
        )?;

        self.decrypted_keys.insert(key_id.clone(), signing_key);
        self.key_metadata.insert(key_id, metadata);

        Ok(())
    }

    /// Encrypt and serialize all keys for storage
    ///
    /// Re-encrypts all keys with the current encryption key and returns
    /// updated UserKey metadata suitable for storing in the database.
    ///
    /// Keys are returned in sorted order by key_id for deterministic output.
    ///
    /// # Returns
    /// Vec of UserKey with updated encrypted_private_key and nonce, sorted by key_id
    pub fn serialize_keys(&self) -> Result<Vec<UserKey>> {
        let mut serialized = Vec::new();

        for (key_id, signing_key) in &self.decrypted_keys {
            // Get metadata
            let metadata = self
                .key_metadata
                .get(key_id)
                .ok_or_else(|| UserError::KeyNotFound {
                    key_id: key_id.clone(),
                })?;

            // Re-encrypt the key
            let (encrypted_key, nonce) = encrypt_private_key(signing_key, &self.encryption_key)?;

            // Create updated UserKey
            let updated_key = UserKey {
                key_id: key_id.clone(),
                encrypted_private_key: encrypted_key,
                nonce,
                display_name: metadata.display_name.clone(),
                created_at: metadata.created_at,
                last_used: metadata.last_used,
                database_sigkeys: metadata.database_sigkeys.clone(),
            };

            serialized.push(updated_key);
        }

        // Sort by key_id for deterministic output
        serialized.sort_by(|a, b| a.key_id.cmp(&b.key_id));

        Ok(serialized)
    }

    /// Clear all decrypted keys from memory
    ///
    /// Explicitly zeroizes all sensitive key material.
    /// Called automatically on Drop via ZeroizeOnDrop, but can be called manually to end session early.
    pub fn clear(&mut self) {
        self.zeroize();
    }

    /// List all key IDs managed by this manager
    pub fn list_key_ids(&self) -> Vec<String> {
        self.decrypted_keys.keys().cloned().collect()
    }

    /// Get metadata for a key
    pub fn get_key_metadata(&self, key_id: &str) -> Option<&UserKey> {
        self.key_metadata.get(key_id)
    }

    /// Get the encryption key for encrypting new keys
    ///
    /// # Security Considerations
    ///
    /// This method exposes the raw password-derived encryption key. It should only be used
    /// internally within the user module for operations that require encrypting new key material.
    ///
    /// **WARNING**: Exposing this key outside the user module could compromise the security of
    /// all encrypted keys. The key is derived from the user's password and is capable of
    /// decrypting all stored private keys.
    ///
    /// # Usage
    ///
    /// This is currently used internally for:
    /// - Creating properly encrypted UserKey entries when adding new keys
    /// - Re-encrypting keys when changing passwords
    ///
    /// The `pub(super)` visibility ensures this remains internal to the user module only.
    #[allow(dead_code)]
    pub(super) fn encryption_key(&self) -> &[u8] {
        &self.encryption_key
    }
}

impl Zeroize for UserKeyManager {
    fn zeroize(&mut self) {
        // Zeroize the encryption key
        self.encryption_key.zeroize();

        // Clear the HashMap - this drops all SigningKeys (which zeroizes via ed25519_dalek's Drop)
        self.decrypted_keys.clear();

        // Clear metadata (contains no plaintext sensitive data)
        self.key_metadata.clear();
    }
}

impl ZeroizeOnDrop for UserKeyManager {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::crypto::generate_keypair;
    use crate::user::crypto::{encrypt_private_key, hash_password};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_test_user_key(
        key_id: &str,
        signing_key: &SigningKey,
        encryption_key: &[u8],
    ) -> UserKey {
        let (encrypted_key, nonce) = encrypt_private_key(signing_key, encryption_key).unwrap();

        UserKey {
            key_id: key_id.to_string(),
            encrypted_private_key: encrypted_key,
            nonce,
            display_name: Some(format!("Test key {}", key_id)),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            last_used: None,
            database_sigkeys: HashMap::new(),
        }
    }

    #[test]
    fn test_key_manager_new() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        // Create some test keys
        let (key1, _) = generate_keypair();
        let (key2, _) = generate_keypair();

        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);
        let user_key2 = create_test_user_key("key2", &key2, &encryption_key);

        // Create key manager
        let manager = UserKeyManager::new(password, &salt, vec![user_key1, user_key2]).unwrap();

        // Verify keys were decrypted
        assert!(manager.get_signing_key("key1").is_some());
        assert!(manager.get_signing_key("key2").is_some());
        assert!(manager.get_signing_key("key3").is_none());
    }

    #[test]
    fn test_key_manager_get_signing_key() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        let (key1, _) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);

        let manager = UserKeyManager::new(password, &salt, vec![user_key1]).unwrap();

        // Get key and verify it's the same
        let retrieved_key = manager.get_signing_key("key1").unwrap();
        assert_eq!(retrieved_key.to_bytes(), key1.to_bytes());
    }

    #[test]
    fn test_key_manager_add_key() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        let (key1, _) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);

        let mut manager = UserKeyManager::new(password, &salt, vec![user_key1]).unwrap();

        // Add a new key - only pass metadata, key is decrypted internally
        let (key2, _) = generate_keypair();
        let user_key2 = create_test_user_key("key2", &key2, &encryption_key);
        manager.add_key(user_key2).unwrap();

        // Verify it was added and decrypted correctly
        assert!(manager.get_signing_key("key2").is_some());
        let retrieved_key = manager.get_signing_key("key2").unwrap();
        assert_eq!(retrieved_key.to_bytes(), key2.to_bytes());
    }

    #[test]
    fn test_key_manager_serialize_keys() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        let (key1, _) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);

        let manager = UserKeyManager::new(password, &salt, vec![user_key1]).unwrap();

        // Serialize keys
        let serialized = manager.serialize_keys().unwrap();
        assert_eq!(serialized.len(), 1);

        // Verify the serialized key can be decrypted
        let serialized_key = &serialized[0];
        assert_eq!(serialized_key.key_id, "key1");

        let decrypted = decrypt_private_key(
            &serialized_key.encrypted_private_key,
            &serialized_key.nonce,
            &encryption_key,
        )
        .unwrap();
        assert_eq!(decrypted.to_bytes(), key1.to_bytes());
    }

    #[test]
    fn test_key_manager_serialize_keys_sorted() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        // Create keys with intentionally non-alphabetical order
        let (key_z, _) = generate_keypair();
        let (key_a, _) = generate_keypair();
        let (key_m, _) = generate_keypair();

        let user_key_z = create_test_user_key("key_z", &key_z, &encryption_key);
        let user_key_a = create_test_user_key("key_a", &key_a, &encryption_key);
        let user_key_m = create_test_user_key("key_m", &key_m, &encryption_key);

        // Add in non-sorted order
        let manager =
            UserKeyManager::new(password, &salt, vec![user_key_z, user_key_a, user_key_m]).unwrap();

        // Serialize should return sorted keys
        let serialized = manager.serialize_keys().unwrap();
        assert_eq!(serialized.len(), 3);

        // Verify keys are sorted alphabetically
        assert_eq!(serialized[0].key_id, "key_a");
        assert_eq!(serialized[1].key_id, "key_m");
        assert_eq!(serialized[2].key_id, "key_z");
    }

    #[test]
    fn test_key_manager_clear() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        let (key1, _) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);

        let mut manager = UserKeyManager::new(password, &salt, vec![user_key1]).unwrap();

        // Verify key exists
        assert!(manager.get_signing_key("key1").is_some());

        // Clear
        manager.clear();

        // Verify keys are gone
        assert!(manager.get_signing_key("key1").is_none());
        assert_eq!(manager.list_key_ids().len(), 0);
    }

    #[test]
    fn test_key_manager_list_key_ids() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        let (key1, _) = generate_keypair();
        let (key2, _) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);
        let user_key2 = create_test_user_key("key2", &key2, &encryption_key);

        let manager = UserKeyManager::new(password, &salt, vec![user_key1, user_key2]).unwrap();

        let key_ids = manager.list_key_ids();
        assert_eq!(key_ids.len(), 2);
        assert!(key_ids.contains(&"key1".to_string()));
        assert!(key_ids.contains(&"key2".to_string()));
    }

    #[test]
    fn test_key_manager_get_metadata() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        let (key1, _) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);

        let manager = UserKeyManager::new(password, &salt, vec![user_key1]).unwrap();

        let metadata = manager.get_key_metadata("key1").unwrap();
        assert_eq!(metadata.key_id, "key1");
        assert_eq!(metadata.display_name, Some("Test key key1".to_string()));
    }

    #[test]
    fn test_key_manager_wrong_password() {
        let correct_password = "correct_password";
        let wrong_password = "wrong_password";
        let (_, salt) = hash_password(correct_password).unwrap();
        let encryption_key = derive_encryption_key(correct_password, &salt).unwrap();

        // Create keys with correct password
        let (key1, _) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);

        // Attempt to create manager with wrong password - should fail
        let result = UserKeyManager::new(wrong_password, &salt, vec![user_key1]);
        assert!(result.is_err());

        // The error should be a decryption failure
        if let Err(err) = result {
            assert!(matches!(
                err,
                crate::Error::User(UserError::DecryptionFailed { .. })
            ));
        }
    }
}
