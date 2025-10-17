//! Settings store for managing database settings and authentication configuration
//!
//! This module provides a high-level interface for managing the `_settings` subtree,
//! including specialized methods for authentication configuration. It wraps DocStore
//! to provide settings-specific functionality while maintaining proper CRDT semantics.

use std::sync::Arc;

use crate::{
    Result, Transaction,
    auth::{
        settings::AuthSettings,
        types::{AuthKey, ResolvedAuth, SigKey},
    },
    backend::BackendDB,
    crdt::Doc,
    store::{DocStore, Store},
};

/// A settings-specific Store that wraps DocStore and provides specialized methods
/// for managing database settings and authentication configuration.
///
/// SettingsStore provides a clean abstraction over the `_settings` subtree, offering
/// type-safe methods for common settings operations while delegating to AuthSettings
/// for authentication-specific functionality.
pub struct SettingsStore {
    /// The underlying DocStore for the _settings subtree
    inner: DocStore,
}

impl SettingsStore {
    /// Create a new SettingsStore from a Transaction
    ///
    /// This creates a SettingsStore that operates on the `_settings` subtree
    /// within the given transaction context.
    ///
    /// # Arguments
    /// * `transaction` - The transaction to operate within
    ///
    /// # Returns
    /// A Result containing the SettingsStore or an error if creation fails
    pub fn new(transaction: &Transaction) -> Result<Self> {
        let inner = <DocStore as Store>::new(transaction, "_settings")?;
        Ok(Self { inner })
    }

    /// Get the database name from settings
    ///
    /// # Returns
    /// The database name as a string, or an error if not found or invalid
    pub fn get_name(&self) -> Result<String> {
        self.inner.get_string("name")
    }

    /// Set the database name in settings
    ///
    /// # Arguments
    /// * `name` - The name to set for the database
    ///
    /// # Returns
    /// Result indicating success or failure
    pub fn set_name(&self, name: &str) -> Result<()> {
        self.inner.set_result("name", name)
    }

    /// Get the current authentication settings as an AuthSettings instance
    ///
    /// This method loads the auth section from the settings and returns it as
    /// an AuthSettings object for convenient manipulation.
    ///
    /// # Returns
    /// An AuthSettings instance representing the current auth configuration
    pub fn get_auth_settings(&self) -> Result<AuthSettings> {
        // Try to get the existing auth document
        match self.inner.get("auth") {
            Ok(auth_value) => {
                // Convert the Value to a Doc and create AuthSettings from it
                match auth_value {
                    crate::crdt::doc::Value::Doc(auth_doc) => Ok(AuthSettings::from_doc(auth_doc)),
                    _ => {
                        // Auth exists but isn't a node - return empty AuthSettings
                        Ok(AuthSettings::new())
                    }
                }
            }
            Err(_) => {
                // No auth section exists yet - return empty AuthSettings
                Ok(AuthSettings::new())
            }
        }
    }

    /// Update authentication settings using a closure
    ///
    /// This method loads the current auth settings, allows modification via the closure,
    /// and then saves the updated settings back to the store.
    ///
    /// # Arguments
    /// * `f` - Closure that takes a mutable AuthSettings and returns a Result
    ///
    /// # Returns
    /// Result indicating success or failure of the update operation
    pub fn update_auth_settings<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut AuthSettings) -> Result<()>,
    {
        // Get current auth settings
        let mut auth_settings = self.get_auth_settings()?;

        // Apply the update function
        f(&mut auth_settings)?;

        // Save the updated auth settings back to the store
        self.inner
            .set_node("auth", auth_settings.as_doc().clone())?;

        Ok(())
    }

    /// Set an authentication key in the settings
    ///
    /// This method provides upsert behavior for authentication keys:
    /// - If the key doesn't exist: creates it
    /// - If the key exists with the same public key: updates permissions and status
    /// - If the key exists with a different public key: returns KeyNameConflict error
    ///
    /// # Arguments
    /// * `key_name` - The name/identifier for the key
    /// * `key` - The AuthKey to set
    ///
    /// # Returns
    /// Result indicating success or failure
    pub fn set_auth_key(&self, key_name: &str, key: AuthKey) -> Result<()> {
        self.update_auth_settings(|auth| {
            // Check if key already exists
            match auth.get_key(key_name) {
                Ok(existing_key) => {
                    // Key exists - check if same public key
                    if existing_key.pubkey() == key.pubkey() {
                        // Same public key - update with new permissions/status
                        auth.overwrite_key(key_name, key)
                    } else {
                        // Different public key - this is a conflict
                        Err(crate::auth::errors::AuthError::KeyNameConflict {
                            key_name: key_name.to_string(),
                            existing_pubkey: existing_key.pubkey().to_string(),
                            new_pubkey: key.pubkey().to_string(),
                        }
                        .into())
                    }
                }
                Err(crate::Error::Auth(auth_err)) if auth_err.is_key_not_found() => {
                    // Key doesn't exist - create it
                    auth.overwrite_key(key_name, key)
                }
                Err(e) => {
                    // Other error (e.g., format error) - propagate it
                    Err(e)
                }
            }
        })
    }

    /// Get an authentication key from the settings
    ///
    /// # Arguments
    /// * `key_name` - The name/identifier of the key to retrieve
    ///
    /// # Returns
    /// AuthKey if found, or error if not present or operation fails
    pub fn get_auth_key(&self, key_name: &str) -> Result<AuthKey> {
        let auth_settings = self.get_auth_settings()?;
        auth_settings.get_key(key_name)
    }

    /// Revoke an authentication key in the settings
    ///
    /// # Arguments
    /// * `key_name` - The name/identifier of the key to revoke
    ///
    /// # Returns
    /// Result indicating success or failure
    pub fn revoke_auth_key(&self, key_name: &str) -> Result<()> {
        self.update_auth_settings(|auth| auth.revoke_key(key_name))
    }

    /// Get the auth document for validation purposes
    ///
    /// This returns the raw Doc containing auth configuration, suitable for
    /// use with AuthValidator and other validation components that expect
    /// the raw CRDT state.
    ///
    /// # Returns
    /// A Doc containing the auth configuration
    pub fn get_auth_doc_for_validation(&self) -> Result<Doc> {
        let auth_settings = self.get_auth_settings()?;
        Ok(auth_settings.as_doc().clone())
    }

    /// Validate entry authentication using the current settings
    ///
    /// This is a convenience method that delegates to AuthSettings.validate_entry_auth
    ///
    /// # Arguments
    /// * `sig_key` - The signature key to validate
    /// * `backend` - Optional backend for delegation path validation
    ///
    /// # Returns
    /// ResolvedAuth information if validation succeeds
    pub fn validate_entry_auth(
        &self,
        sig_key: &SigKey,
        backend: Option<&Arc<dyn BackendDB>>,
    ) -> Result<ResolvedAuth> {
        let auth_settings = self.get_auth_settings()?;
        auth_settings.validate_entry_auth(sig_key, backend)
    }

    /// Get access to the underlying DocStore for advanced operations
    ///
    /// This provides direct access to the DocStore for cases where the
    /// SettingsStore abstraction is insufficient.
    ///
    /// # Returns
    /// A reference to the underlying DocStore
    pub fn as_doc_store(&self) -> &DocStore {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Database, Instance,
        auth::{
            generate_public_key,
            types::{KeyStatus, Permission},
        },
        backend::database::InMemory,
    };

    fn create_test_database() -> Database {
        let backend = Box::new(InMemory::new());
        let instance = Instance::open(backend).expect("Failed to create test instance");

        let database = instance.new_database_default("_device_key").unwrap();

        // Set initial database name using transaction
        let transaction = database.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();
        settings_store.set_name("test_db").unwrap();
        transaction.commit().unwrap();

        database
    }

    #[test]
    fn test_settings_store_creation() {
        let database = create_test_database();
        let transaction = database.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Should be able to create successfully
        assert!(settings_store.as_doc_store().name() == "_settings");
    }

    #[test]
    fn test_name_operations() {
        let database = create_test_database();
        let transaction = database.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Should be able to get the initial name
        let name = settings_store.get_name().unwrap();
        assert_eq!(name, "test_db");

        // Should be able to set a new name
        settings_store.set_name("updated_name").unwrap();
        let updated_name = settings_store.get_name().unwrap();
        assert_eq!(updated_name, "updated_name");
    }

    #[test]
    fn test_auth_settings_integration() {
        let database = create_test_database();
        let transaction = database.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Get the initial auth settings (may contain a default key from database creation)
        let initial_auth_settings = settings_store.get_auth_settings().unwrap();
        let initial_key_count = initial_auth_settings.get_all_keys().unwrap().len();

        // Should be able to add an auth key
        let auth_key = AuthKey::active(generate_public_key(), Permission::Admin(1)).unwrap();

        settings_store
            .set_auth_key("new_test_key", auth_key.clone())
            .unwrap();

        // Should be able to retrieve the key
        let retrieved_key = settings_store.get_auth_key("new_test_key").unwrap();
        assert_eq!(retrieved_key.pubkey(), auth_key.pubkey());
        assert_eq!(retrieved_key.permissions(), auth_key.permissions());
        assert_eq!(retrieved_key.status(), auth_key.status());

        // Should have one more key than initially
        let final_auth_settings = settings_store.get_auth_settings().unwrap();
        let final_key_count = final_auth_settings.get_all_keys().unwrap().len();
        assert_eq!(final_key_count, initial_key_count + 1);
    }

    #[test]
    fn test_auth_key_operations() {
        let database = create_test_database();
        let transaction = database.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        let auth_key = AuthKey::active(generate_public_key(), Permission::Write(5)).unwrap();

        // Add key
        settings_store
            .set_auth_key("laptop", auth_key.clone())
            .unwrap();

        // Verify key exists
        let retrieved = settings_store.get_auth_key("laptop").unwrap();
        assert_eq!(retrieved.pubkey(), auth_key.pubkey());
        assert_eq!(retrieved.status(), &KeyStatus::Active);

        // Revoke key
        settings_store.revoke_auth_key("laptop").unwrap();

        // Verify key is revoked
        let revoked_key = settings_store.get_auth_key("laptop").unwrap();
        assert_eq!(revoked_key.status(), &KeyStatus::Revoked);
    }

    #[test]
    fn test_update_auth_settings_closure() {
        let database = create_test_database();
        let transaction = database.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Use the closure-based update
        settings_store
            .update_auth_settings(|auth| {
                let key1 = AuthKey::active(generate_public_key(), Permission::Admin(1)).unwrap();
                let key2 = AuthKey::active(generate_public_key(), Permission::Write(5)).unwrap();

                auth.add_key("admin", key1)?;
                auth.add_key("writer", key2)?;
                Ok(())
            })
            .unwrap();

        // Verify both keys were added (plus any existing keys from database creation)
        let auth_settings = settings_store.get_auth_settings().unwrap();
        let all_keys = auth_settings.get_all_keys().unwrap();
        assert!(all_keys.len() >= 2); // At least the two we added
        assert!(all_keys.contains_key("admin"));
        assert!(all_keys.contains_key("writer"));
    }

    #[test]
    fn test_auth_doc_for_validation() {
        let database = create_test_database();
        let transaction = database.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Add a key
        let valid_pubkey = generate_public_key();
        let auth_key = AuthKey::active(valid_pubkey.clone(), Permission::Read).unwrap();
        settings_store.set_auth_key("validator", auth_key).unwrap();

        // Get auth doc for validation
        let auth_doc = settings_store.get_auth_doc_for_validation().unwrap();

        // Should contain the key
        let validator_key: AuthKey = auth_doc.get_json("validator").unwrap();
        assert_eq!(validator_key.pubkey(), &valid_pubkey);
    }

    #[test]
    fn test_error_handling() {
        let database = create_test_database();
        let transaction = database.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Getting non-existent auth key should return KeyNotFound error
        let result = settings_store.get_auth_key("nonexistent");
        assert!(result.is_err());
        if let Err(crate::Error::Auth(auth_err)) = result {
            assert!(auth_err.is_key_not_found());
        } else {
            panic!("Expected Auth(KeyNotFound) error");
        }

        // Revoking non-existent key should fail
        let revoke_result = settings_store.revoke_auth_key("nonexistent");
        assert!(revoke_result.is_err());
    }
}
