//! Password-encrypted store wrapper for transparent encryption of any Store type.
//!
//! This module provides [`PasswordStore`], which encrypts both data and metadata
//! using AES-256-GCM with Argon2id-derived keys. The wrapped store's type and
//! configuration are stored encrypted in the `_index` subtree.
//!
//! # Encryption Architecture
//!
//! Encryption is transparent to the wrapped store. Data flows as:
//!
//! ```text
//! Write: WrappedStore.put() → JSON → encrypt() → base64 → stored in entry
//! Read:  entry data → base64 decode → decrypt() → JSON → WrappedStore CRDT merge
//! ```
//!
//! The underlying CRDT (e.g., Doc) handles merging of decrypted data from multiple
//! entry tips. The encrypted wrapper is purely for storage - it has no CRDT semantics.
//!
//! # Relay Node Support
//!
//! Relay nodes without the decryption key can store and forward encrypted entries.
//! It is unnecessary to decrypt the data before forwarding it to other nodes.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng},
};
use argon2::{Argon2, Params, password_hash::SaltString};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{
    Result, Transaction,
    crdt::Data,
    store::{Registered, Store, StoreError},
};

/// Encrypted data fragment containing ciphertext and nonce.
///
/// This is a simple storage container for encrypted data. It has no CRDT semantics -
/// merging of encrypted data happens at the entry level, where each entry's data is
/// decrypted independently and then merged using the underlying CRDT's merge logic.
///
/// # Fields
///
/// * `ciphertext` - AES-256-GCM encrypted data
/// * `nonce` - 12-byte nonce (must be unique per encryption)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EncryptedFragment {
    /// AES-256-GCM encrypted ciphertext.
    pub ciphertext: Vec<u8>,
    /// 12-byte nonce for AES-GCM (must be unique per encryption).
    pub nonce: Vec<u8>,
}

impl Data for EncryptedFragment {}

/// AES-256-GCM nonce size (96 bits / 12 bytes).
const AES_GCM_NONCE_SIZE: usize = 12;

/// Default Argon2 memory cost in KiB (19 MiB)
pub const DEFAULT_ARGON2_M_COST: u32 = 19 * 1024;
/// Default Argon2 time cost (iterations)
pub const DEFAULT_ARGON2_T_COST: u32 = 2;
/// Default Argon2 parallelism
pub const DEFAULT_ARGON2_P_COST: u32 = 1;

/// Encryption metadata stored in _index config (plaintext)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EncryptionInfo {
    /// Encryption algorithm (always "aes-256-gcm" for v1)
    pub algorithm: String,
    /// Key derivation function (always "argon2id" for v1)
    pub kdf: String,
    /// Base64-encoded salt for Argon2 (16 bytes)
    pub salt: String,
    /// Version for future compatibility
    pub version: String,
    /// Argon2 memory cost in KiB (defaults to 19 MiB if not specified)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argon2_m_cost: Option<u32>,
    /// Argon2 time cost / iterations (defaults to 2 if not specified)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argon2_t_cost: Option<u32>,
    /// Argon2 parallelism (defaults to 1 if not specified)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argon2_p_cost: Option<u32>,
}

/// Configuration stored in _index for PasswordStore.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PasswordStoreConfig {
    /// Encryption parameters (stored in plaintext in _index).
    pub encryption: EncryptionInfo,
    /// Encrypted wrapped store metadata.
    /// Contains the wrapped store's configuration, e.g: {"type": "docstore:v0", "config": "{}"}
    pub wrapped_config: EncryptedFragment,
}

/// Wrapped store metadata (stored encrypted in config)
#[derive(Serialize, Deserialize, Clone, Debug)]
struct WrappedStoreInfo {
    #[serde(rename = "type")]
    type_id: String,
    config: String,
}

/// Internal state of a PasswordStore
#[derive(Debug, Clone, PartialEq, Eq)]
enum PasswordStoreState {
    /// Just created via get_store(), no encryption configured yet
    Uninitialized,
    /// Has encryption config, but not yet decrypted for this session
    Locked,
    /// Decrypted, ready to use the wrapped store
    Unlocked,
}

/// Securely stored password with automatic zeroization
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
struct Password {
    salt: String,
    password: String,
    /// Argon2 memory cost in KiB
    argon2_m_cost: u32,
    /// Argon2 time cost
    argon2_t_cost: u32,
    /// Argon2 parallelism
    argon2_p_cost: u32,
}

/// Wrapper for derived key with automatic zeroization
#[derive(ZeroizeOnDrop)]
struct DerivedKey {
    key: Option<Vec<u8>>,
}

impl DerivedKey {
    fn new() -> Self {
        Self { key: None }
    }

    fn set(&mut self, key: Vec<u8>) {
        self.key = Some(key);
    }

    fn get(&self) -> Option<&Vec<u8>> {
        self.key.as_ref()
    }
}

/// Password-based encryptor implementing the Encryptor trait
///
/// Provides AES-256-GCM encryption with Argon2id key derivation.
/// Caches the derived key to avoid expensive re-derivation on every operation.
struct PasswordEncryptor {
    password: Password,
    subtree_name: String,
    /// Cached derived key (zeroized on drop, thread-safe)
    derived_key: Arc<Mutex<DerivedKey>>,
}

impl PasswordEncryptor {
    /// Create a new PasswordEncryptor
    fn new(password: Password, subtree_name: String) -> Self {
        Self {
            password,
            subtree_name,
            derived_key: Arc::new(Mutex::new(DerivedKey::new())),
        }
    }

    /// Execute a function with access to the encryption key (with caching)
    ///
    /// Provides a reference to the key without cloning it. This avoids
    /// leaving unzeroized copies of the key in memory. The lock is held
    /// during key derivation to prevent concurrent derivation races.
    fn with_key<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&[u8]) -> Result<R>,
    {
        let mut guard = self.derived_key.lock().unwrap();

        // Check if key is already cached
        if let Some(key) = guard.get() {
            return f(key);
        }

        // Derive the key (expensive Argon2 operation, but only done once)
        let mut key = vec![0u8; 32];
        let salt = SaltString::from_b64(&self.password.salt).map_err(|e| {
            StoreError::ImplementationError {
                store: self.subtree_name.clone(),
                reason: format!("Invalid salt: {e}"),
            }
        })?;

        // Build Argon2 with configured parameters
        let params = Params::new(
            self.password.argon2_m_cost,
            self.password.argon2_t_cost,
            self.password.argon2_p_cost,
            Some(32), // output length
        )
        .map_err(|e| StoreError::ImplementationError {
            store: self.subtree_name.clone(),
            reason: format!("Invalid Argon2 parameters: {e}"),
        })?;

        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

        argon2
            .hash_password_into(
                self.password.password.as_bytes(),
                salt.as_str().as_bytes(),
                &mut key,
            )
            .map_err(|e| StoreError::ImplementationError {
                store: self.subtree_name.clone(),
                reason: format!("Key derivation failed: {e}"),
            })?;

        // Cache the key for future use
        guard.set(key);

        // Execute function with the derived key (guard still held)
        f(guard.get().unwrap())
    }
}

impl crate::transaction::Encryptor for PasswordEncryptor {
    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        // Wire format: nonce (12 bytes) || ciphertext
        if ciphertext.len() < AES_GCM_NONCE_SIZE {
            return Err(StoreError::DeserializationFailed {
                store: self.subtree_name.clone(),
                reason: format!(
                    "Ciphertext too short: expected at least {} bytes, got {}",
                    AES_GCM_NONCE_SIZE,
                    ciphertext.len()
                ),
            }
            .into());
        }

        let (nonce_bytes, encrypted_data) = ciphertext.split_at(AES_GCM_NONCE_SIZE);

        // Use the encryption key without cloning
        self.with_key(|encryption_key| {
            // Create cipher
            let cipher = Aes256Gcm::new_from_slice(encryption_key).map_err(|e| {
                StoreError::ImplementationError {
                    store: self.subtree_name.clone(),
                    reason: format!("Failed to create cipher: {e}"),
                }
            })?;

            // Decrypt
            let nonce = Nonce::from_slice(nonce_bytes);
            cipher.decrypt(nonce, encrypted_data).map_err(|_| {
                StoreError::ImplementationError {
                    store: self.subtree_name.clone(),
                    reason: "Decryption failed".to_string(),
                }
                .into()
            })
        })
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        // Use the encryption key without cloning
        self.with_key(|encryption_key| {
            // Create cipher
            let cipher = Aes256Gcm::new_from_slice(encryption_key).map_err(|e| {
                StoreError::ImplementationError {
                    store: self.subtree_name.clone(),
                    reason: format!("Failed to create cipher: {e}"),
                }
            })?;

            // Generate random nonce
            let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

            // Encrypt
            let ciphertext =
                cipher
                    .encrypt(&nonce, plaintext)
                    .map_err(|e| StoreError::ImplementationError {
                        store: self.subtree_name.clone(),
                        reason: format!("Encryption failed: {e}"),
                    })?;

            // Wire format: nonce (12 bytes) || ciphertext
            let mut result = nonce.to_vec();
            result.extend(ciphertext);
            Ok(result)
        })
    }
}

/// Password-encrypted store wrapper
///
/// Provides transparent encryption for any Store type using AES-256-GCM
/// with password-derived keys (Argon2id). Both data and metadata are encrypted.
///
/// # State Machine
///
/// PasswordStore has three states (derived from internal fields):
///
/// 1. **Uninitialized** - Created via `get_store()`, no encryption configured
/// 2. **Locked** - Has encryption config, not yet decrypted
/// 3. **Unlocked** - Decrypted and ready to use
///
/// State transitions:
/// - `get_store()` → Uninitialized (new) or Locked (existing)
/// - `initialize()` → Unlocked (from Uninitialized only)
/// - `open()` → Unlocked (from Locked only)
/// - `close()` → Locked (from Unlocked)
///
/// # Security
///
/// - **Encryption**: AES-256-GCM authenticated encryption
/// - **Key Derivation**: Argon2id memory-hard password hashing
/// - **Nonces**: Unique random nonce per encryption operation
/// - **Zeroization**: Passwords cleared from memory on drop
///
/// # Limitations
///
///   - Uses last-write-wins (may lose data in concurrent scenarios)
/// - **Password Loss**: Losing the password means permanent data loss
/// - **Performance**: Encryption/decryption overhead on every operation
///
/// # Examples
///
/// Creating a new encrypted store:
///
/// ```rust,no_run
/// # use eidetica::{Instance, backend::database::InMemory, crdt::Doc, Database};
/// # use eidetica::store::{PasswordStore, DocStore};
/// # use eidetica::auth::generate_keypair;
/// # async fn example() -> eidetica::Result<()> {
/// # let backend = InMemory::new();
/// # let instance = Instance::open(Box::new(backend)).await?;
/// # let (private_key, _) = generate_keypair();
/// # let db = Database::create(Doc::new(), &instance, private_key, "key".to_string()).await?;
/// let tx = db.new_transaction().await?;
/// let mut encrypted = tx.get_store::<PasswordStore>("secrets").await?;
/// encrypted.initialize("my_password", "docstore:v0", "{}").await?;
///
/// let docstore = encrypted.unwrap::<DocStore>().await?;
/// docstore.set("key", "secret value").await?;
/// tx.commit().await?;
/// # Ok(())
/// # }
/// ```
///
/// Opening an existing encrypted store:
///
/// ```rust,no_run
/// # use eidetica::{Instance, backend::database::InMemory, crdt::Doc, Database};
/// # use eidetica::store::{PasswordStore, DocStore};
/// # use eidetica::auth::generate_keypair;
/// # async fn example() -> eidetica::Result<()> {
/// # let backend = InMemory::new();
/// # let instance = Instance::open(Box::new(backend)).await?;
/// # let (private_key, _) = generate_keypair();
/// # let db = Database::create(Doc::new(), &instance, private_key, "key".to_string()).await?;
/// let tx = db.new_transaction().await?;
/// let mut store = tx.get_store::<PasswordStore>("secrets").await?;
/// store.open("my_password")?;
///
/// let docstore = store.unwrap::<DocStore>().await?;
/// let value = docstore.get("key").await?;
/// # Ok(())
/// # }
/// ```
pub struct PasswordStore {
    /// Subtree name
    name: String,
    /// Transaction reference
    transaction: Transaction,
    /// Encryption configuration (None if uninitialized)
    config: Option<PasswordStoreConfig>,
    /// Cached password (zeroized on drop)
    cached_password: Option<Password>,
    /// Decrypted wrapped store info (only available after open())
    wrapped_info: Option<WrappedStoreInfo>,
}

impl PasswordStore {
    /// Derive the current state from internal fields
    fn state(&self) -> PasswordStoreState {
        match (&self.config, &self.cached_password) {
            (None, _) => PasswordStoreState::Uninitialized,
            (Some(_), None) => PasswordStoreState::Locked,
            (Some(_), Some(_)) => PasswordStoreState::Unlocked,
        }
    }
}

impl Registered for PasswordStore {
    fn type_id() -> &'static str {
        // Explicitly use v0 to indicate instability
        "encrypted:password:v0"
    }
}

#[async_trait]
impl Store for PasswordStore {
    async fn new(op: &Transaction, subtree_name: String) -> Result<Self> {
        // Try to load config from _index to determine state
        let index_store = op.get_index().await?;
        let info = index_store.get_entry(&subtree_name).await?;

        // Type validation
        if !Self::supports_type_id(&info.type_id) {
            return Err(StoreError::TypeMismatch {
                store: subtree_name,
                expected: Self::type_id().to_string(),
                actual: info.type_id,
            }
            .into());
        }

        // Determine state based on config content
        // Empty config "{}" means uninitialized, valid JSON means locked
        if info.config == "{}" || info.config.is_empty() {
            Ok(Self {
                name: subtree_name,
                transaction: op.clone(),
                config: None,
                cached_password: None,
                wrapped_info: None,
            })
        } else {
            // Parse the config
            let config: PasswordStoreConfig = serde_json::from_str(&info.config).map_err(|e| {
                StoreError::DeserializationFailed {
                    store: subtree_name.clone(),
                    reason: format!("Failed to parse PasswordStoreConfig: {e}"),
                }
            })?;

            // Validate encryption parameters
            if config.encryption.algorithm != "aes-256-gcm" {
                return Err(StoreError::InvalidConfiguration {
                    store: subtree_name,
                    reason: format!(
                        "Unsupported encryption algorithm: {}",
                        config.encryption.algorithm
                    ),
                }
                .into());
            }

            if config.encryption.kdf != "argon2id" {
                return Err(StoreError::InvalidConfiguration {
                    store: subtree_name,
                    reason: format!("Unsupported KDF: {}", config.encryption.kdf),
                }
                .into());
            }

            Ok(Self {
                name: subtree_name,
                transaction: op.clone(),
                config: Some(config),
                cached_password: None,
                wrapped_info: None,
            })
        }
    }

    async fn init(op: &Transaction, subtree_name: String) -> Result<Self> {
        // Register in _index with empty config (marks as uninitialized)
        let index_store = op.get_index().await?;
        index_store
            .set_entry(&subtree_name, Self::type_id(), Self::default_config())
            .await?;

        Ok(Self {
            name: subtree_name,
            transaction: op.clone(),
            config: None,
            cached_password: None,
            wrapped_info: None,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn transaction(&self) -> &Transaction {
        &self.transaction
    }

    fn default_config() -> String {
        "{}".to_string()
    }
}

impl PasswordStore {
    /// Initialize encryption on an uninitialized store
    ///
    /// This configures encryption for a PasswordStore that was obtained via
    /// `get_store()`. The wrapped store's type and config are encrypted and
    /// stored in the PasswordStore's configuration in `_index`.
    ///
    /// After calling this method, the store transitions to the Unlocked state
    /// and is ready to use.
    ///
    /// # Arguments
    /// * `password` - Password for encryption (will be zeroized after use)
    /// * `wrapped_type_id` - Type ID of wrapped store (e.g., "docstore:v0")
    /// * `wrapped_config` - Configuration for wrapped store
    ///
    /// # Returns
    /// Ok(()) on success, the store is now unlocked
    ///
    /// # Errors
    /// - Returns error if store is not in Uninitialized state
    /// - Returns error if encryption fails
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::{Instance, backend::database::InMemory, crdt::Doc, Database};
    /// # use eidetica::store::{PasswordStore, DocStore};
    /// # use eidetica::auth::generate_keypair;
    /// # async fn example() -> eidetica::Result<()> {
    /// # let backend = InMemory::new();
    /// # let instance = Instance::open(Box::new(backend)).await?;
    /// # let (private_key, _) = generate_keypair();
    /// # let db = Database::create(Doc::new(), &instance, private_key, "key".to_string()).await?;
    /// let tx = db.new_transaction().await?;
    /// let mut encrypted = tx.get_store::<PasswordStore>("secrets").await?;
    /// encrypted.initialize("my_password", "docstore:v0", "{}").await?;
    ///
    /// let docstore = encrypted.unwrap::<DocStore>().await?;
    /// docstore.set("key", "secret value").await?;
    /// tx.commit().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn initialize(
        &mut self,
        password: impl Into<String>,
        wrapped_type_id: impl Into<String>,
        wrapped_config: impl Into<String>,
    ) -> Result<()> {
        // Check state is Uninitialized
        if self.state() != PasswordStoreState::Uninitialized {
            return Err(StoreError::InvalidOperation {
                store: self.name.clone(),
                operation: "initialize".to_string(),
                reason: "Store is already initialized - use open() instead".to_string(),
            }
            .into());
        }

        let password = password.into();
        let wrapped_type_id = wrapped_type_id.into();
        let wrapped_config = wrapped_config.into();

        // Use default Argon2 parameters
        let argon2_m_cost = DEFAULT_ARGON2_M_COST;
        let argon2_t_cost = DEFAULT_ARGON2_T_COST;
        let argon2_p_cost = DEFAULT_ARGON2_P_COST;

        // Generate encryption parameters
        let salt = SaltString::generate(&mut OsRng);
        let salt_str = salt.as_str().to_string();

        // Build Argon2 with configured parameters
        let params =
            Params::new(argon2_m_cost, argon2_t_cost, argon2_p_cost, Some(32)).map_err(|e| {
                StoreError::ImplementationError {
                    store: self.name.clone(),
                    reason: format!("Invalid Argon2 parameters: {e}"),
                }
            })?;
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

        // Derive encryption key from password
        let mut encryption_key = vec![0u8; 32];
        argon2
            .hash_password_into(
                password.as_bytes(),
                salt.as_str().as_bytes(),
                &mut encryption_key,
            )
            .map_err(|e| StoreError::ImplementationError {
                store: self.name.clone(),
                reason: format!("Failed to derive encryption key: {e}"),
            })?;

        // Create cipher
        let cipher = Aes256Gcm::new_from_slice(&encryption_key).map_err(|e| {
            StoreError::ImplementationError {
                store: self.name.clone(),
                reason: format!("Failed to create cipher: {e}"),
            }
        })?;

        // Encrypt wrapped store metadata
        let wrapped_info = WrappedStoreInfo {
            type_id: wrapped_type_id,
            config: wrapped_config,
        };
        let wrapped_json = serde_json::to_string(&wrapped_info)?;
        let config_nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let wrapped_config_ciphertext = cipher
            .encrypt(&config_nonce, wrapped_json.as_bytes())
            .map_err(|e| StoreError::ImplementationError {
                store: self.name.clone(),
                reason: format!("Failed to encrypt wrapped config: {e}"),
            })?;

        // Zeroize the encryption key
        encryption_key.zeroize();

        // Create configuration
        let config = PasswordStoreConfig {
            encryption: EncryptionInfo {
                algorithm: "aes-256-gcm".to_string(),
                kdf: "argon2id".to_string(),
                salt: salt_str.clone(),
                version: "v0".to_string(),
                argon2_m_cost: Some(argon2_m_cost),
                argon2_t_cost: Some(argon2_t_cost),
                argon2_p_cost: Some(argon2_p_cost),
            },
            wrapped_config: EncryptedFragment {
                ciphertext: wrapped_config_ciphertext,
                nonce: config_nonce.to_vec(),
            },
        };

        // Update _index with the encryption config
        let config_json = serde_json::to_string(&config)?;
        self.set_config(config_json).await?;

        // Cache password and create encryptor
        let password_cache = Password {
            salt: salt_str,
            password,
            argon2_m_cost,
            argon2_t_cost,
            argon2_p_cost,
        };

        // Register encryptor with transaction (store is now unlocked)
        let encryptor = Box::new(PasswordEncryptor::new(
            password_cache.clone(),
            self.name.clone(),
        ));
        self.transaction.register_encryptor(&self.name, encryptor)?;

        // Update internal state
        self.config = Some(config);
        self.cached_password = Some(password_cache);
        self.wrapped_info = Some(wrapped_info);

        Ok(())
    }

    /// Open (unlock) the encrypted store with a password
    ///
    /// This decrypts the wrapped store configuration and caches the password
    /// for subsequent encrypt/decrypt operations. The password remains in memory
    /// until `close()` is called or the PasswordStore is dropped.
    ///
    /// # Arguments
    /// * `password` - Password to decrypt the store
    ///
    /// # Returns
    /// Ok(()) if password is correct, Err otherwise
    ///
    /// # Errors
    /// - Returns error if store is Uninitialized (use `initialize()` first)
    /// - Returns error if store is already Unlocked
    /// - Returns error if password is incorrect
    ///
    /// # Security
    /// The password is cached in memory (with zeroization on drop) for
    /// convenience. Call `close()` to explicitly clear it from memory.
    pub fn open(&mut self, password: impl Into<String>) -> Result<()> {
        // Check state
        match self.state() {
            PasswordStoreState::Uninitialized => {
                return Err(StoreError::InvalidOperation {
                    store: self.name.clone(),
                    operation: "open".to_string(),
                    reason: "Store is not initialized - call initialize() first".to_string(),
                }
                .into());
            }
            PasswordStoreState::Unlocked => {
                return Err(StoreError::InvalidOperation {
                    store: self.name.clone(),
                    operation: "open".to_string(),
                    reason: "Store is already open".to_string(),
                }
                .into());
            }
            PasswordStoreState::Locked => {}
        }

        let config = self.config.as_ref().expect("Locked state requires config");
        let password = password.into();

        // Get Argon2 parameters from config (with defaults)
        let argon2_m_cost = config
            .encryption
            .argon2_m_cost
            .unwrap_or(DEFAULT_ARGON2_M_COST);
        let argon2_t_cost = config
            .encryption
            .argon2_t_cost
            .unwrap_or(DEFAULT_ARGON2_T_COST);
        let argon2_p_cost = config
            .encryption
            .argon2_p_cost
            .unwrap_or(DEFAULT_ARGON2_P_COST);

        // Derive encryption key
        let mut encryption_key = vec![0u8; 32];
        let salt = SaltString::from_b64(&config.encryption.salt).map_err(|e| {
            StoreError::ImplementationError {
                store: self.name.clone(),
                reason: format!("Invalid salt in config: {e}"),
            }
        })?;

        // Build Argon2 with configured parameters
        let params =
            Params::new(argon2_m_cost, argon2_t_cost, argon2_p_cost, Some(32)).map_err(|e| {
                StoreError::ImplementationError {
                    store: self.name.clone(),
                    reason: format!("Invalid Argon2 parameters: {e}"),
                }
            })?;
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

        argon2
            .hash_password_into(
                password.as_bytes(),
                salt.as_str().as_bytes(),
                &mut encryption_key,
            )
            .map_err(|e| StoreError::ImplementationError {
                store: self.name.clone(),
                reason: format!("Failed to derive encryption key: {e}"),
            })?;

        // Decrypt wrapped config
        let cipher = Aes256Gcm::new_from_slice(&encryption_key).map_err(|e| {
            StoreError::ImplementationError {
                store: self.name.clone(),
                reason: format!("Failed to create cipher: {e}"),
            }
        })?;

        // Validate nonce length (must be 12 bytes for AES-GCM)
        if config.wrapped_config.nonce.len() != 12 {
            return Err(StoreError::InvalidConfiguration {
                store: self.name.clone(),
                reason: format!(
                    "Invalid nonce length: expected 12 bytes, got {}",
                    config.wrapped_config.nonce.len()
                ),
            }
            .into());
        }
        let config_nonce = Nonce::from_slice(&config.wrapped_config.nonce);

        let decrypted_config = cipher
            .decrypt(config_nonce, config.wrapped_config.ciphertext.as_slice())
            .map_err(|_| StoreError::ImplementationError {
                store: self.name.clone(),
                reason: "Failed to decrypt wrapped config - incorrect password?".to_string(),
            })?;

        // Zeroize encryption key
        encryption_key.zeroize();

        // Parse wrapped store info
        let wrapped_info: WrappedStoreInfo =
            serde_json::from_slice(&decrypted_config).map_err(|e| {
                StoreError::DeserializationFailed {
                    store: self.name.clone(),
                    reason: format!("Failed to parse wrapped store info: {e}"),
                }
            })?;

        // Cache password and wrapped info (state is derived from these fields)
        let password_cache = Password {
            salt: config.encryption.salt.clone(),
            password,
            argon2_m_cost,
            argon2_t_cost,
            argon2_p_cost,
        };
        self.cached_password = Some(password_cache.clone());
        self.wrapped_info = Some(wrapped_info);

        // Register encryptor with the transaction for transparent encryption
        let encryptor = Box::new(PasswordEncryptor::new(password_cache, self.name.clone()));
        self.transaction.register_encryptor(&self.name, encryptor)?;

        Ok(())
    }

    /// Check if the store is currently unlocked (password cached)
    pub fn is_open(&self) -> bool {
        self.state() == PasswordStoreState::Unlocked
    }

    /// Check if the store is initialized (has encryption configuration)
    pub fn is_initialized(&self) -> bool {
        self.state() != PasswordStoreState::Uninitialized
    }

    /// Get the wrapped store's type ID (requires open)
    ///
    /// # Errors
    /// Returns error if store is not opened
    pub fn wrapped_type_id(&self) -> Result<&str> {
        self.wrapped_info
            .as_ref()
            .map(|info| info.type_id.as_str())
            .ok_or_else(|| {
                StoreError::InvalidOperation {
                    store: self.name.clone(),
                    operation: "wrapped_type_id".to_string(),
                    reason: "Store not opened - call open() first".to_string(),
                }
                .into()
            })
    }

    /// Unwrap to get the actual wrapped store with transparent encryption
    ///
    /// Returns a Store instance (DocStore, Table, etc.) that transparently
    /// encrypts data on write and decrypts on read. The wrapped store is
    /// unaware of encryption - all crypto operations are handled by an
    /// encryptor registered with the transaction during `open()` or `initialize()`.
    ///
    /// # Type Parameters
    /// * `T` - The Store type to unwrap to (must match the wrapped type)
    ///
    /// # Errors
    /// - Returns error if store is not opened (call `open()` first)
    /// - Returns error if `T::type_id()` doesn't match the wrapped store's type
    ///
    /// # Implementation Note
    /// The wrapped store uses the same transaction as the PasswordStore. When
    /// `open()` or `initialize()` is called, a `PasswordEncryptor` is registered
    /// with the transaction for this subtree. The transaction then transparently
    /// decrypts data in `get_full_state()` and encrypts data during `commit()`.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::{Instance, backend::database::InMemory, crdt::Doc, Database};
    /// # use eidetica::store::{PasswordStore, DocStore};
    /// # use eidetica::auth::generate_keypair;
    /// # async fn example() -> eidetica::Result<()> {
    /// # let backend = InMemory::new();
    /// # let instance = Instance::open(Box::new(backend)).await?;
    /// # let (private_key, _) = generate_keypair();
    /// # let db = Database::create(Doc::new(), &instance, private_key, "key".to_string()).await?;
    /// # let tx = db.new_transaction().await?;
    /// # let mut encrypted = tx.get_store::<PasswordStore>("test").await?;
    /// # encrypted.initialize("pass", "docstore:v0", "{}").await?;
    /// # tx.commit().await?;
    /// # let tx2 = db.new_transaction().await?;
    /// let mut encrypted = tx2.get_store::<PasswordStore>("test").await?;
    /// encrypted.open("pass")?;
    ///
    /// // Unwrap to DocStore - type must match
    /// let docstore = encrypted.unwrap::<DocStore>().await?;
    /// docstore.set("key", "value").await?; // Automatically encrypted
    /// # Ok(())
    /// # }
    /// ```
    pub async fn unwrap<T: Store>(&self) -> Result<T> {
        // Check if opened
        if !self.is_open() {
            return Err(StoreError::InvalidOperation {
                store: self.name.clone(),
                operation: "unwrap".to_string(),
                reason: "Store not opened - call open() first".to_string(),
            }
            .into());
        }

        let wrapped_info = self.wrapped_info.as_ref().unwrap();

        // Verify type matches
        if T::type_id() != wrapped_info.type_id {
            return Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: T::type_id().to_string(),
                actual: wrapped_info.type_id.clone(),
            }
            .into());
        }

        // Simply create the wrapped store!
        // The transaction has an encryptor registered, so it will transparently:
        // - Decrypt data when get_full_state() is called
        // - Encrypt data when commit() is called
        // The wrapped store is completely unaware of encryption
        // Note: We call T::new() directly, bypassing Transaction::get_store() type checking,
        // since we've already verified the wrapped type matches.
        T::new(&self.transaction, self.name.clone()).await
    }
}
