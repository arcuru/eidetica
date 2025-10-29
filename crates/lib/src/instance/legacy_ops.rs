//! Legacy Instance operations trait
//!
//! **WARNING**: These methods bypass proper user context and should only be used
//! for testing or during migration from legacy code. Production code should use
//! the User API instead.
//!
//! This trait provides deprecated Instance-level database and key operations that
//! skip the User API workflow. These operations are preserved for:
//! - Migrating legacy test code
//! - Testing low-level functionality
//! - Backward compatibility during transition period
//!
//! ## Migration Guide
//!
//! Instead of using these methods, use the User API:
//!
//! ```rust,ignore
//! // OLD (deprecated):
//! use eidetica::instance::LegacyInstanceOps;
//! let instance = Instance::new(...);
//! instance.add_private_key("my_key")?;
//! let db = instance.new_database_default("my_key")?;
//!
//! // NEW (recommended):
//! let instance = Instance::new(...);
//! instance.register_user("username", None)?;
//! let user = instance.login_user("username", None)?;
//! let key = user.get_default_key()?;
//! let db = user.create_database(settings, &key)?;
//! ```

use ed25519_dalek::VerifyingKey;
use rand::Rng;

use super::Instance;
use crate::{Database, Result, auth::crypto::format_public_key, crdt::Doc};

/// Trait providing legacy Instance-level database and key operations.
///
/// **WARNING**: These methods bypass proper user context and should only be used
/// for testing or during migration from legacy code. Production code should use
/// the User API instead.
///
/// Enable this trait to access deprecated Instance methods for testing purposes.
/// Import this trait explicitly to signal legacy API usage:
///
/// ```rust,ignore
/// use eidetica::instance::LegacyInstanceOps;
/// ```
pub trait LegacyInstanceOps {
    /// Create a new database in the instance (deprecated).
    ///
    /// **DEPRECATED**: Use `User::create_database()` instead. This method will be removed in a future version.
    ///
    /// # Arguments
    /// * `settings` - The initial settings for the database
    /// * `signing_key_name` - The name of the signing key to use
    ///
    /// # Returns
    /// A `Result` containing the newly created `Database` or an error.
    fn new_database(&self, settings: Doc, signing_key_name: impl AsRef<str>) -> Result<Database>;

    /// Create a new database with default empty settings (deprecated).
    ///
    /// **DEPRECATED**: Use `User::create_database()` instead. This method will be removed in a future version.
    ///
    /// # Arguments
    /// * `signing_key_name` - The name of the signing key to use
    ///
    /// # Returns
    /// A `Result` containing the newly created `Database` or an error.
    fn new_database_default(&self, signing_key_name: impl AsRef<str>) -> Result<Database>;

    /// Generate a new Ed25519 keypair (deprecated).
    ///
    /// **DEPRECATED**: Use `User::add_private_key()` instead. This method will be removed in a future version.
    ///
    /// # Arguments
    /// * `display_name` - Optional display name for the key
    ///
    /// # Returns
    /// A `Result` containing the key ID (public key string) or an error.
    fn add_private_key(&self, display_name: &str) -> Result<VerifyingKey>;

    /// Import an existing Ed25519 keypair (deprecated).
    ///
    /// **DEPRECATED**: Use `User::add_private_key()` with generated keys instead.
    ///
    /// # Arguments
    /// * `key_id` - Key identifier (usually the public key string)
    /// * `signing_key` - The Ed25519 signing key to import
    ///
    /// # Returns
    /// A `Result` containing the key ID or an error.
    fn import_private_key(
        &self,
        key_id: &str,
        signing_key: ed25519_dalek::SigningKey,
    ) -> Result<String>;

    /// Get the public key for a stored private key (deprecated).
    ///
    /// **DEPRECATED**: Use `User::get_signing_key()` instead.
    ///
    /// # Arguments
    /// * `key_id` - The key identifier
    ///
    /// # Returns
    /// A `Result` containing the public key or an error.
    fn get_public_key(&self, key_id: &str) -> Result<VerifyingKey>;

    /// Get the formatted public key string for a stored private key (deprecated).
    ///
    /// **DEPRECATED**: Use `User::get_signing_key()` instead and format manually if needed.
    ///
    /// # Arguments
    /// * `key_name` - The key identifier
    ///
    /// # Returns
    /// A `Result` containing the formatted public key string.
    fn get_formatted_public_key(&self, key_name: impl AsRef<str>) -> Result<String>;
}

impl LegacyInstanceOps for Instance {
    fn new_database(&self, settings: Doc, signing_key_name: impl AsRef<str>) -> Result<Database> {
        use crate::auth::AuthError;
        let signing_key = match self
            .inner
            .backend
            .get_private_key(signing_key_name.as_ref())?
        {
            Some(key) => key,
            None => {
                return Err(AuthError::KeyNotFound {
                    key_name: signing_key_name.as_ref().to_string(),
                }
                .into());
            }
        };
        let database = Database::create(
            settings,
            self,
            signing_key,
            signing_key_name.as_ref().to_string(),
        )?;
        Ok(database)
    }

    fn new_database_default(&self, signing_key_name: impl AsRef<str>) -> Result<Database> {
        let mut settings = Doc::new();

        // Add a unique database identifier to ensure each database gets a unique root ID
        // This prevents content-addressable collision when creating multiple databases
        // with identical settings
        let unique_id = format!(
            "database_{}",
            rand::thread_rng()
                .sample_iter(&rand::distributions::Alphanumeric)
                .take(16)
                .map(char::from)
                .collect::<String>()
        );
        settings.set_string("database_id", unique_id);

        self.new_database(settings, signing_key_name)
    }

    fn add_private_key(&self, display_name: &str) -> Result<VerifyingKey> {
        // Generate keypair using backend-stored keys (legacy path)
        use crate::auth::crypto::generate_keypair;
        let (signing_key, verifying_key) = generate_keypair();

        // Store in backend with display_name as the key name (legacy storage)
        self.inner
            .backend
            .store_private_key(display_name, signing_key)?;

        Ok(verifying_key)
    }

    fn import_private_key(
        &self,
        key_id: &str,
        signing_key: ed25519_dalek::SigningKey,
    ) -> Result<String> {
        // Import key into backend storage (legacy path)
        self.inner.backend.store_private_key(key_id, signing_key)?;

        Ok(key_id.to_string())
    }

    fn get_public_key(&self, key_id: &str) -> Result<VerifyingKey> {
        use crate::instance::InstanceError;
        // Get signing key from backend storage (legacy path)
        let signing_key = self.inner.backend.get_private_key(key_id)?.ok_or_else(|| {
            InstanceError::SigningKeyNotFound {
                key_name: key_id.to_string(),
            }
        })?;

        Ok(signing_key.verifying_key())
    }

    fn get_formatted_public_key(&self, key_name: impl AsRef<str>) -> Result<String> {
        let public_key = self.get_public_key(key_name.as_ref())?;
        Ok(format_public_key(&public_key))
    }
}
