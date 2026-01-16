//! Settings store for managing database settings and authentication configuration
//!
//! This module provides a high-level interface for managing the `_settings` subtree,
//! including specialized methods for authentication configuration. It wraps DocStore
//! to provide settings-specific functionality while maintaining proper CRDT semantics.

use crate::{
    Instance, Result, Transaction,
    auth::{
        settings::AuthSettings,
        types::{AuthKey, ResolvedAuth, SigKey},
    },
    crdt::{Doc, doc},
    height::HeightStrategy,
    store::DocStore,
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
    /// This is crate-private - users should use `Transaction::get_settings()` instead.
    ///
    /// # Arguments
    /// * `transaction` - The transaction to operate within
    ///
    /// # Returns
    /// A Result containing the SettingsStore or an error if creation fails
    pub(crate) fn new(transaction: &Transaction) -> Result<Self> {
        // Note: We create DocStore directly here instead of using Store::new()
        // because SettingsStore is a wrapper that doesn't implement the Store trait itself.
        // This avoids the async requirement for this simple internal construction.
        let inner = DocStore {
            name: "_settings".to_string(),
            atomic_op: transaction.clone(),
        };
        Ok(Self { inner })
    }

    /// Get the database name from settings
    ///
    /// # Returns
    /// The database name as a string, or an error if not found or invalid
    pub async fn get_name(&self) -> Result<String> {
        self.inner.get_string("name").await
    }

    /// Set the database name in settings
    ///
    /// # Arguments
    /// * `name` - The name to set for the database
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn set_name(&self, name: &str) -> Result<()> {
        self.inner.set_result("name", name).await
    }

    /// Get a value from settings by key
    ///
    /// # Arguments
    /// * `key` - The key to retrieve
    ///
    /// # Returns
    /// The value associated with the key, or an error if not found
    pub async fn get(&self, key: impl AsRef<str>) -> Result<doc::Value> {
        self.inner.get(key).await
    }

    /// Get a string value from settings by key
    ///
    /// # Arguments
    /// * `key` - The key to retrieve
    ///
    /// # Returns
    /// The string value associated with the key, or an error if not found or wrong type
    pub async fn get_string(&self, key: impl AsRef<str>) -> Result<String> {
        self.inner.get_string(key).await
    }

    /// Get all settings as a Doc
    ///
    /// Returns a complete snapshot of all settings in the _settings subtree.
    ///
    /// # Returns
    /// A Doc containing all current settings
    pub async fn get_all(&self) -> Result<Doc> {
        self.inner.get_all().await
    }

    /// Get the height strategy for this database.
    ///
    /// Returns [`HeightStrategy::Incremental`] if no strategy is configured,
    /// ensuring backwards compatibility with existing databases.
    ///
    /// # Returns
    /// The configured height strategy, or the default (Incremental)
    pub async fn get_height_strategy(&self) -> Result<HeightStrategy> {
        match self.inner.get("height_strategy").await {
            Ok(value) => {
                // HeightStrategy is stored as JSON in a Text value
                let json = match value {
                    doc::Value::Text(s) => s,
                    _ => return Ok(HeightStrategy::default()),
                };
                serde_json::from_str(&json).map_err(|e| {
                    crate::crdt::CRDTError::DeserializationFailed {
                        reason: e.to_string(),
                    }
                    .into()
                })
            }
            Err(e) if e.is_not_found() => Ok(HeightStrategy::default()),
            Err(e) => Err(e),
        }
    }

    /// Set the height strategy for this database.
    ///
    /// # Arguments
    /// * `strategy` - The height strategy to use
    pub async fn set_height_strategy(&self, strategy: HeightStrategy) -> Result<()> {
        let json = serde_json::to_string(&strategy).map_err(|e| {
            crate::crdt::CRDTError::SerializationFailed {
                reason: e.to_string(),
            }
        })?;
        self.inner
            .set("height_strategy", doc::Value::Text(json))
            .await
    }

    /// Get the current authentication settings as an AuthSettings instance
    ///
    /// This method loads the auth section from the settings and returns it as
    /// an AuthSettings object for convenient manipulation.
    ///
    /// # Returns
    /// An AuthSettings instance representing the current auth configuration
    pub async fn get_auth_settings(&self) -> Result<AuthSettings> {
        // Try to get the existing auth document
        match self.inner.get("auth").await {
            Ok(auth_value) => {
                // Convert the Value to a Doc and create AuthSettings from it
                match auth_value {
                    doc::Value::Doc(auth_doc) => Ok(AuthSettings::from_doc(auth_doc)),
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
    pub async fn update_auth_settings<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut AuthSettings) -> Result<()>,
    {
        // Get current auth settings
        let mut auth_settings = self.get_auth_settings().await?;

        // Apply the update function
        f(&mut auth_settings)?;

        // Save the updated auth settings back to the store
        self.inner
            .set_node("auth", auth_settings.as_doc().clone())
            .await?;

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
    pub async fn set_auth_key(&self, key_name: &str, key: AuthKey) -> Result<()> {
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
        .await
    }

    /// Get an authentication key from the settings
    ///
    /// # Arguments
    /// * `key_name` - The name/identifier of the key to retrieve
    ///
    /// # Returns
    /// AuthKey if found, or error if not present or operation fails
    pub async fn get_auth_key(&self, key_name: &str) -> Result<AuthKey> {
        let auth_settings = self.get_auth_settings().await?;
        auth_settings.get_key(key_name)
    }

    /// Revoke an authentication key in the settings
    ///
    /// # Arguments
    /// * `key_name` - The name/identifier of the key to revoke
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn revoke_auth_key(&self, key_name: &str) -> Result<()> {
        self.update_auth_settings(|auth| auth.revoke_key(key_name))
            .await
    }

    /// Get the auth document for validation purposes
    ///
    /// This returns the raw Doc containing auth configuration, suitable for
    /// use with AuthValidator and other validation components that expect
    /// the raw CRDT state.
    ///
    /// # Returns
    /// A Doc containing the auth configuration
    pub async fn get_auth_doc_for_validation(&self) -> Result<Doc> {
        let auth_settings = self.get_auth_settings().await?;
        Ok(auth_settings.as_doc().clone())
    }

    /// Validate entry authentication using the current settings
    ///
    /// This is a convenience method that delegates to AuthSettings.validate_entry_auth
    ///
    /// # Arguments
    /// * `sig_key` - The signature key to validate
    /// * `instance` - Optional instance for delegation path validation
    ///
    /// # Returns
    /// ResolvedAuth information if validation succeeds
    pub async fn validate_entry_auth(
        &self,
        sig_key: &SigKey,
        instance: Option<&Instance>,
    ) -> Result<ResolvedAuth> {
        let auth_settings = self.get_auth_settings().await?;
        auth_settings.validate_entry_auth(sig_key, instance).await
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
        crdt::Doc,
        store::Store,
    };

    async fn create_test_database() -> (Instance, Database) {
        let backend = Box::new(InMemory::new());
        let instance = Instance::open(backend)
            .await
            .expect("Failed to create test instance");

        // Use User API to create database
        instance.create_user("test", None).await.unwrap();
        let mut user = instance.login_user("test", None).await.unwrap();
        let key_id = user.add_private_key(None).await.unwrap();
        let database = user.create_database(Doc::new(), &key_id).await.unwrap();

        // Set initial database name using transaction
        let transaction = database.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();
        settings_store.set_name("test_db").await.unwrap();
        transaction.commit().await.unwrap();

        (instance, database)
    }

    #[tokio::test]
    async fn test_settings_store_creation() {
        let (_instance, database) = create_test_database().await;
        let transaction = database.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Should be able to create successfully
        assert!(settings_store.as_doc_store().name() == "_settings");
    }

    #[tokio::test]
    async fn test_name_operations() {
        let (_instance, database) = create_test_database().await;
        let transaction = database.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Should be able to get the initial name
        let name = settings_store.get_name().await.unwrap();
        assert_eq!(name, "test_db");

        // Should be able to set a new name
        settings_store.set_name("updated_name").await.unwrap();
        let updated_name = settings_store.get_name().await.unwrap();
        assert_eq!(updated_name, "updated_name");
    }

    #[tokio::test]
    async fn test_auth_settings_integration() {
        let (_instance, database) = create_test_database().await;
        let transaction = database.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Get the initial auth settings (may contain a default key from database creation)
        let initial_auth_settings = settings_store.get_auth_settings().await.unwrap();
        let initial_key_count = initial_auth_settings.get_all_keys().unwrap().len();

        // Should be able to add an auth key
        let auth_key = AuthKey::active(generate_public_key(), Permission::Admin(1)).unwrap();

        settings_store
            .set_auth_key("new_test_key", auth_key.clone())
            .await
            .unwrap();

        // Should be able to retrieve the key
        let retrieved_key = settings_store.get_auth_key("new_test_key").await.unwrap();
        assert_eq!(retrieved_key.pubkey(), auth_key.pubkey());
        assert_eq!(retrieved_key.permissions(), auth_key.permissions());
        assert_eq!(retrieved_key.status(), auth_key.status());

        // Should have one more key than initially
        let final_auth_settings = settings_store.get_auth_settings().await.unwrap();
        let final_key_count = final_auth_settings.get_all_keys().unwrap().len();
        assert_eq!(final_key_count, initial_key_count + 1);
    }

    #[tokio::test]
    async fn test_auth_key_operations() {
        let (_instance, database) = create_test_database().await;
        let transaction = database.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        let auth_key = AuthKey::active(generate_public_key(), Permission::Write(5)).unwrap();

        // Add key
        settings_store
            .set_auth_key("laptop", auth_key.clone())
            .await
            .unwrap();

        // Verify key exists
        let retrieved = settings_store.get_auth_key("laptop").await.unwrap();
        assert_eq!(retrieved.pubkey(), auth_key.pubkey());
        assert_eq!(retrieved.status(), &KeyStatus::Active);

        // Revoke key
        settings_store.revoke_auth_key("laptop").await.unwrap();

        // Verify key is revoked
        let revoked_key = settings_store.get_auth_key("laptop").await.unwrap();
        assert_eq!(revoked_key.status(), &KeyStatus::Revoked);
    }

    #[tokio::test]
    async fn test_update_auth_settings_closure() {
        let (_instance, database) = create_test_database().await;
        let transaction = database.new_transaction().await.unwrap();
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
            .await
            .unwrap();

        // Verify both keys were added (plus any existing keys from database creation)
        let auth_settings = settings_store.get_auth_settings().await.unwrap();
        let all_keys = auth_settings.get_all_keys().unwrap();
        assert!(all_keys.len() >= 2); // At least the two we added
        assert!(all_keys.contains_key("admin"));
        assert!(all_keys.contains_key("writer"));
    }

    #[tokio::test]
    async fn test_auth_doc_for_validation() {
        let (_instance, database) = create_test_database().await;
        let transaction = database.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Add a key
        let valid_pubkey = generate_public_key();
        let auth_key = AuthKey::active(valid_pubkey.clone(), Permission::Read).unwrap();
        settings_store
            .set_auth_key("validator", auth_key)
            .await
            .unwrap();

        // Get auth doc for validation
        let auth_doc = settings_store.get_auth_doc_for_validation().await.unwrap();

        // Should contain the key
        let validator_key: AuthKey = auth_doc.get_json("validator").unwrap();
        assert_eq!(validator_key.pubkey(), &valid_pubkey);
    }

    #[tokio::test]
    async fn test_error_handling() {
        let (_instance, database) = create_test_database().await;
        let transaction = database.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();

        // Getting non-existent auth key should return KeyNotFound error
        let result = settings_store.get_auth_key("nonexistent").await;
        assert!(result.is_err());
        if let Err(crate::Error::Auth(auth_err)) = result {
            assert!(auth_err.is_key_not_found());
        } else {
            panic!("Expected Auth(KeyNotFound) error");
        }

        // Revoking non-existent key should fail
        let revoke_result = settings_store.revoke_auth_key("nonexistent").await;
        assert!(revoke_result.is_err());
    }
}
