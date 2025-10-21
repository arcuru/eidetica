//! User key manager for session-based key management
//!
//! Manages decrypted private keys during a user session.
//! Keys are decrypted on login and kept in memory for the session duration.

use std::collections::HashMap;

use ed25519_dalek::{SigningKey, VerifyingKey};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::{
    crypto::{decrypt_private_key, derive_encryption_key, encrypt_private_key},
    errors::UserError,
    types::{KeyEncryption, UserKey},
};
use crate::Result;

/// Internal key manager that holds decrypted keys during user session
///
/// # Security
///
/// This struct holds sensitive cryptographic material in memory:
/// - `decrypted_keys`: Contains plaintext SigningKeys (ed25519_dalek implements Drop with zeroization)
/// - `encryption_key`: Password-derived key (zeroized via manual Zeroize impl), None for passwordless users
///
/// All sensitive data is zeroized when the struct is dropped.
pub struct UserKeyManager {
    /// Decrypted keys (key_id → SigningKey)
    decrypted_keys: HashMap<String, SigningKey>,

    /// Key metadata (loaded from user database)
    key_metadata: HashMap<String, UserKey>,

    /// User's password-derived encryption key (for saving new keys)
    /// None for passwordless users
    encryption_key: Option<Vec<u8>>,
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
            encryption_key: Some(encryption_key),
        };

        // Add all keys using add_key
        for user_key in encrypted_keys {
            manager.add_key(user_key)?;
        }

        Ok(manager)
    }

    /// Create from unencrypted keys (for passwordless users)
    ///
    /// Keys are stored and loaded unencrypted for performance.
    ///
    /// # Arguments
    /// * `keys` - Vec of UserKey entries with unencrypted private_key_bytes
    ///
    /// # Returns
    /// A UserKeyManager with all keys ready for use
    pub fn new_passwordless(keys: Vec<UserKey>) -> Result<Self> {
        let capacity = keys.len();
        let mut manager = Self {
            decrypted_keys: HashMap::with_capacity(capacity),
            key_metadata: HashMap::with_capacity(capacity),
            encryption_key: None,
        };

        // Add all keys
        for user_key in keys {
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

    /// Get the public key (VerifyingKey) for a given key ID
    ///
    /// # Arguments
    /// * `key_id` - The key identifier
    ///
    /// # Returns
    /// The VerifyingKey (public key) if the signing key is found
    pub fn get_public_key(&self, key_id: &str) -> Option<VerifyingKey> {
        self.decrypted_keys.get(key_id).map(|sk| sk.verifying_key())
    }

    /// Add a key to the manager from metadata
    ///
    /// Handles both encrypted and unencrypted keys based on metadata.
    /// Use `serialize_keys()` to get updated keys for storage.
    ///
    /// # Arguments
    /// * `metadata` - The UserKey metadata with private_key_bytes and encryption info
    ///
    /// # Returns
    /// Ok(()) if the key was successfully added
    pub fn add_key(&mut self, metadata: UserKey) -> Result<()> {
        let key_id = metadata.key_id.clone();

        // Decrypt or deserialize the key based on encryption status
        let signing_key = match &metadata.encryption {
            KeyEncryption::Encrypted { nonce } => {
                // Encrypted key - needs decryption
                let encryption_key =
                    self.encryption_key
                        .as_ref()
                        .ok_or_else(|| UserError::PasswordRequired {
                            operation: "decrypt encrypted key".to_string(),
                        })?;
                decrypt_private_key(&metadata.private_key_bytes, nonce, encryption_key)?
            }
            KeyEncryption::Unencrypted => {
                // Unencrypted key - direct deserialization
                SigningKey::from_bytes(metadata.private_key_bytes.as_slice().try_into().map_err(
                    |_| UserError::InvalidKeyFormat {
                        reason: "Invalid key length".to_string(),
                    },
                )?)
            }
        };

        self.decrypted_keys.insert(key_id.clone(), signing_key);
        self.key_metadata.insert(key_id, metadata);

        Ok(())
    }

    /// Serialize all keys for storage
    ///
    /// Returns UserKey metadata suitable for storing in the database.
    /// Encrypted keys are re-encrypted with the current encryption key.
    /// Unencrypted keys are serialized directly.
    ///
    /// Keys are returned in sorted order by key_id for deterministic output.
    ///
    /// # Returns
    /// Vec of UserKey with updated private_key_bytes and encryption info, sorted by key_id
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

            // Encrypt or serialize based on encryption status
            let (private_key_bytes, encryption) = match &metadata.encryption {
                KeyEncryption::Encrypted { .. } => {
                    // Re-encrypt the key
                    let encryption_key = self.encryption_key.as_ref().ok_or_else(|| {
                        UserError::PasswordRequired {
                            operation: "encrypt key".to_string(),
                        }
                    })?;
                    let (encrypted_key, nonce) = encrypt_private_key(signing_key, encryption_key)?;
                    (encrypted_key, KeyEncryption::Encrypted { nonce })
                }
                KeyEncryption::Unencrypted => {
                    // Serialize unencrypted
                    (signing_key.to_bytes().to_vec(), KeyEncryption::Unencrypted)
                }
            };

            // Create updated UserKey
            let updated_key = UserKey {
                key_id: key_id.clone(),
                private_key_bytes,
                encryption,
                display_name: metadata.display_name.clone(),
                created_at: metadata.created_at,
                last_used: metadata.last_used,
                is_default: metadata.is_default,
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
    ///
    /// Returns key IDs sorted by creation timestamp (oldest first) for deterministic behavior.
    pub fn list_key_ids(&self) -> Vec<String> {
        let mut keys: Vec<(String, i64)> = self
            .decrypted_keys
            .keys()
            .filter_map(|key_id| {
                self.key_metadata
                    .get(key_id)
                    .map(|meta| (key_id.clone(), meta.created_at))
            })
            .collect();

        // Sort by created_at timestamp (oldest first)
        keys.sort_by_key(|(_, created_at)| *created_at);

        // Return just the key IDs
        keys.into_iter().map(|(key_id, _)| key_id).collect()
    }

    /// Get metadata for a key
    pub fn get_key_metadata(&self, key_id: &str) -> Option<&UserKey> {
        self.key_metadata.get(key_id)
    }

    /// Get the default key ID
    ///
    /// Returns the key marked as is_default=true, or falls back to the
    /// oldest key by creation timestamp if no default is explicitly set.
    ///
    /// # Returns
    /// The key ID of the default key, or None if there are no keys
    pub fn get_default_key_id(&self) -> Option<String> {
        // First try to find a key explicitly marked as default
        for (key_id, metadata) in &self.key_metadata {
            if metadata.is_default {
                return Some(key_id.clone());
            }
        }

        // Fall back to oldest key by creation timestamp
        self.list_key_ids().first().cloned()
    }

    /// Get the encryption key for encrypting new keys
    ///
    /// Returns None for passwordless users.
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
    pub(super) fn encryption_key(&self) -> Option<&[u8]> {
        self.encryption_key.as_deref()
    }
}

impl Zeroize for UserKeyManager {
    fn zeroize(&mut self) {
        // Zeroize the encryption key if present
        if let Some(key) = &mut self.encryption_key {
            key.zeroize();
        }

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
    use crate::user::crypto::{current_timestamp, encrypt_private_key, hash_password};

    fn create_test_user_key(
        key_id: &str,
        signing_key: &SigningKey,
        encryption_key: &[u8],
    ) -> UserKey {
        let (encrypted_key, nonce) = encrypt_private_key(signing_key, encryption_key).unwrap();

        UserKey {
            key_id: key_id.to_string(),
            private_key_bytes: encrypted_key,
            encryption: KeyEncryption::Encrypted { nonce },
            display_name: Some(format!("Test key {key_id}")),
            created_at: current_timestamp().unwrap(),
            last_used: None,
            is_default: false,
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
    fn test_key_manager_get_public_key() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        let (key1, pub_key1) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);

        let manager = UserKeyManager::new(password, &salt, vec![user_key1]).unwrap();

        // Get public key and verify it matches the original
        let retrieved_pub_key = manager.get_public_key("key1").unwrap();
        assert_eq!(retrieved_pub_key, pub_key1);

        // Verify non-existent key returns None
        assert!(manager.get_public_key("nonexistent").is_none());
    }

    #[test]
    fn test_key_manager_get_public_key_multiple_keys() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        // Create multiple keys
        let (key1, pub_key1) = generate_keypair();
        let (key2, pub_key2) = generate_keypair();
        let (key3, pub_key3) = generate_keypair();

        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);
        let user_key2 = create_test_user_key("key2", &key2, &encryption_key);
        let user_key3 = create_test_user_key("key3", &key3, &encryption_key);

        let manager =
            UserKeyManager::new(password, &salt, vec![user_key1, user_key2, user_key3]).unwrap();

        // Verify all public keys match
        assert_eq!(manager.get_public_key("key1").unwrap(), pub_key1);
        assert_eq!(manager.get_public_key("key2").unwrap(), pub_key2);
        assert_eq!(manager.get_public_key("key3").unwrap(), pub_key3);

        // Verify all keys are different
        assert_ne!(pub_key1, pub_key2);
        assert_ne!(pub_key2, pub_key3);
        assert_ne!(pub_key1, pub_key3);
    }

    #[test]
    fn test_key_manager_get_public_key_passwordless() {
        // Create passwordless keys
        let (key1, pub_key1) = generate_keypair();
        let (key2, pub_key2) = generate_keypair();

        let user_key1 = UserKey {
            key_id: "key1".to_string(),
            private_key_bytes: key1.to_bytes().to_vec(),
            encryption: KeyEncryption::Unencrypted,
            display_name: Some("Key 1".to_string()),
            created_at: current_timestamp().unwrap(),
            last_used: None,
            is_default: true,
            database_sigkeys: HashMap::new(),
        };

        let user_key2 = UserKey {
            key_id: "key2".to_string(),
            private_key_bytes: key2.to_bytes().to_vec(),
            encryption: KeyEncryption::Unencrypted,
            display_name: Some("Key 2".to_string()),
            created_at: current_timestamp().unwrap(),
            last_used: None,
            is_default: false,
            database_sigkeys: HashMap::new(),
        };

        let manager = UserKeyManager::new_passwordless(vec![user_key1, user_key2]).unwrap();

        // Verify public keys match for passwordless keys
        assert_eq!(manager.get_public_key("key1").unwrap(), pub_key1);
        assert_eq!(manager.get_public_key("key2").unwrap(), pub_key2);
    }

    #[test]
    fn test_key_manager_get_public_key_consistency_with_signing_key() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        let (key1, pub_key1) = generate_keypair();
        let user_key1 = create_test_user_key("key1", &key1, &encryption_key);

        let manager = UserKeyManager::new(password, &salt, vec![user_key1]).unwrap();

        // Get both signing key and public key
        let signing_key = manager.get_signing_key("key1").unwrap();
        let public_key = manager.get_public_key("key1").unwrap();

        // Verify public key matches the signing key's verifying key
        assert_eq!(signing_key.verifying_key(), public_key);
        assert_eq!(public_key, pub_key1);
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

        // Extract nonce from encryption metadata
        let nonce = match &serialized_key.encryption {
            KeyEncryption::Encrypted { nonce } => nonce,
            KeyEncryption::Unencrypted => panic!("Expected encrypted key"),
        };

        let decrypted =
            decrypt_private_key(&serialized_key.private_key_bytes, nonce, &encryption_key).unwrap();
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
    fn test_key_manager_list_key_ids_sorted_by_timestamp() {
        let password = "test_password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        // Create keys with specific timestamps
        let (key_new, _) = generate_keypair();
        let (key_old, _) = generate_keypair();
        let (key_mid, _) = generate_keypair();

        let (encrypted_new, nonce_new) = encrypt_private_key(&key_new, &encryption_key).unwrap();
        let (encrypted_old, nonce_old) = encrypt_private_key(&key_old, &encryption_key).unwrap();
        let (encrypted_mid, nonce_mid) = encrypt_private_key(&key_mid, &encryption_key).unwrap();

        // Create keys with explicit timestamps (old, middle, new)
        let user_key_old = UserKey {
            key_id: "key_old".to_string(),
            private_key_bytes: encrypted_old,
            encryption: KeyEncryption::Encrypted { nonce: nonce_old },
            display_name: Some("Old Key".to_string()),
            created_at: 1000, // Oldest
            last_used: None,
            is_default: true, // Mark oldest as default
            database_sigkeys: HashMap::new(),
        };

        let user_key_mid = UserKey {
            key_id: "key_mid".to_string(),
            private_key_bytes: encrypted_mid,
            encryption: KeyEncryption::Encrypted { nonce: nonce_mid },
            display_name: Some("Mid Key".to_string()),
            created_at: 2000, // Middle
            last_used: None,
            is_default: false,
            database_sigkeys: HashMap::new(),
        };

        let user_key_new = UserKey {
            key_id: "key_new".to_string(),
            private_key_bytes: encrypted_new,
            encryption: KeyEncryption::Encrypted { nonce: nonce_new },
            display_name: Some("New Key".to_string()),
            created_at: 3000, // Newest
            last_used: None,
            is_default: false,
            database_sigkeys: HashMap::new(),
        };

        // Add keys in non-chronological order
        let manager = UserKeyManager::new(
            password,
            &salt,
            vec![user_key_new, user_key_old, user_key_mid],
        )
        .unwrap();

        // list_key_ids() should return keys sorted by created_at (oldest first)
        let key_ids = manager.list_key_ids();
        assert_eq!(key_ids.len(), 3);
        assert_eq!(key_ids[0], "key_old"); // created_at: 1000
        assert_eq!(key_ids[1], "key_mid"); // created_at: 2000
        assert_eq!(key_ids[2], "key_new"); // created_at: 3000
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
