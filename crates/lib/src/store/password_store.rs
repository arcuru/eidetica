//! Password-encrypted store wrapper for transparent encryption of any Store type.
//!
//! This module provides [`PasswordStore<S>`], a generic decorator that wraps any
//! [`Store`] type `S` with AES-256-GCM encryption protected by Argon2id-derived keys.
//! The wrapped store's type and configuration are stored encrypted in the `_index`
//! subtree.
//!
//! # Encryption Architecture
//!
//! Encryption is transparent to the wrapped store. Data flows as:
//!
//! ```text
//! Write: WrappedStore.put() → serialized bytes → encrypt() → ciphertext → stored in entry
//! Read:  entry data → decrypt() → serialized bytes → WrappedStore CRDT merge
//! ```
//!
//! Entry payloads are opaque bytes (`RawData = Vec<u8>`), so ciphertext is stored
//! verbatim without an additional text encoding (e.g. base64).
//!
//! The underlying CRDT (e.g., Doc) handles merging of decrypted data from multiple
//! entry tips. `PasswordStore<S>` delegates `Store::Data` to `S::Data` — encryption
//! is a transport-level concern, invisible at the type level.
//!
//! # Relay Node Support
//!
//! Relay nodes without the decryption key can store and forward encrypted entries.
//! It is unnecessary to decrypt the data before forwarding it to other nodes.

use std::marker::PhantomData;
use std::sync::Arc;

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng, Payload},
};
use argon2::{Argon2, Params, password_hash::SaltString};
use async_trait::async_trait;
use rand::RngCore;
use serde::{Deserialize, Deserializer, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use base64ct::{Base64, Encoding};

use crate::{
    Result, Transaction,
    crdt::{CRDTError, Doc, doc::Value},
    store::{ProjectionDescriptor, Registered, Store, StoreError, StoreStateModel},
    transaction::Encryptor,
};

/// Encrypted data fragment containing ciphertext and nonce.
///
/// Used for storing encrypted metadata (e.g., the wrapped store's configuration
/// in [`PasswordStoreConfig::wrapped_config`]). This is a storage container
/// with no CRDT semantics.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedFragment {
    /// AES-256-GCM encrypted ciphertext.
    pub ciphertext: Vec<u8>,
    /// 12-byte nonce for AES-GCM (must be unique per encryption).
    pub nonce: Vec<u8>,
}

/// AES-256-GCM nonce size (96 bits / 12 bytes).
const AES_GCM_NONCE_SIZE: usize = 12;

/// Default Argon2 memory cost in KiB (19 MiB)
pub const DEFAULT_ARGON2_M_COST: u32 = 19 * 1024;
/// Default Argon2 time cost (iterations)
pub const DEFAULT_ARGON2_T_COST: u32 = 2;
/// Default Argon2 parallelism
pub const DEFAULT_ARGON2_P_COST: u32 = 1;

/// Maximum accepted Argon2 memory cost in KiB (256 MiB), checked before derivation.
pub const MAX_ARGON2_M_COST: u32 = 256 * 1024;
/// Maximum accepted Argon2 iteration count, checked before derivation.
pub const MAX_ARGON2_T_COST: u32 = 10;
/// Maximum accepted Argon2 parallelism, checked before derivation.
pub const MAX_ARGON2_P_COST: u32 = 16;

/// Encryption metadata stored in _index config (plaintext)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EncryptionInfo {
    /// Encryption algorithm ("aes-256-gcm").
    pub algorithm: String,
    /// Key derivation function ("argon2id", Argon2 version 0x13).
    pub kdf: String,
    /// Base64-encoded salt for Argon2 (16 bytes)
    pub salt: String,
    /// Envelope version: "v0" derives the data key directly; "v1" wraps it.
    pub version: String,
    /// Argon2 salt input: absent or "base64-text" for v0; "raw" for v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt_encoding: Option<String>,
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
    /// v1 data key encrypted under the password-derived key; absent in v0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_key: Option<EncryptedFragment>,
}

impl EncryptionInfo {
    fn current(m: u32, t: u32, p: u32) -> Self {
        Self {
            algorithm: "aes-256-gcm".to_string(),
            kdf: "argon2id".to_string(),
            salt: SaltString::generate(&mut OsRng).as_str().to_string(),
            version: "v1".to_string(),
            salt_encoding: Some("raw".to_string()),
            argon2_m_cost: Some(m),
            argon2_t_cost: Some(t),
            argon2_p_cost: Some(p),
        }
    }

    fn costs(&self) -> (u32, u32, u32) {
        // These are wire-format defaults, not upgrade policy: keep them fixed
        // when raising DEFAULT_ARGON2_* so old omitted costs stay readable.
        (
            self.argon2_m_cost.unwrap_or(19 * 1024),
            self.argon2_t_cost.unwrap_or(2),
            self.argon2_p_cost.unwrap_or(1),
        )
    }

    fn derive_key(&self, password: &str, store: &str) -> Result<SecretBytes> {
        let invalid = |reason: String| StoreError::InvalidConfiguration {
            store: store.to_string(),
            reason,
        };
        if self.algorithm != "aes-256-gcm" || self.kdf != "argon2id" {
            return Err(invalid("Unsupported encryption algorithm or KDF".to_string()).into());
        }
        let raw_salt = match (self.version.as_str(), self.salt_encoding.as_deref()) {
            ("v0", None | Some("base64-text")) => false,
            ("v1", Some("raw"))
                if self.argon2_m_cost.is_some()
                    && self.argon2_t_cost.is_some()
                    && self.argon2_p_cost.is_some() =>
            {
                true
            }
            _ => {
                return Err(invalid(
                    "Unsupported KDF envelope version or salt encoding".to_string(),
                )
                .into());
            }
        };
        let (m, t, p) = self.costs();
        if m > MAX_ARGON2_M_COST || t > MAX_ARGON2_T_COST || p > MAX_ARGON2_P_COST {
            return Err(invalid(
                "Argon2 parameters exceed the decrypt-side work ceiling".to_string(),
            )
            .into());
        }
        let params = Params::new(m, t, p, Some(32))
            .map_err(|error| invalid(format!("Invalid Argon2 parameters: {error}")))?;
        let salt = SaltString::from_b64(&self.salt)
            .map_err(|error| invalid(format!("Invalid salt: {error}")))?;
        let mut decoded = [0u8; 64];
        let salt_bytes = if raw_salt {
            salt.decode_b64(&mut decoded)
                .map_err(|error| invalid(format!("Invalid salt: {error}")))?
        } else {
            salt.as_str().as_bytes()
        };
        let mut key = SecretBytes::zeroed(32);
        Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
            .hash_password_into(password.as_bytes(), salt_bytes, key.as_mut_bytes())
            .map_err(|error| invalid(format!("Key derivation failed: {error}")))?;
        Ok(key)
    }
}

impl EncryptedFragment {
    fn encrypt(key: &[u8], plaintext: &[u8], store: &str) -> Result<Self> {
        let cipher =
            Aes256Gcm::new_from_slice(key).map_err(|_| StoreError::InvalidConfiguration {
                store: store.to_string(),
                reason: "Invalid encryption key length".to_string(),
            })?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext =
            cipher
                .encrypt(&nonce, plaintext)
                .map_err(|_| StoreError::ImplementationError {
                    store: store.to_string(),
                    reason: "Failed to encrypt configuration".to_string(),
                })?;
        Ok(Self {
            ciphertext,
            nonce: nonce.to_vec(),
        })
    }

    fn decrypt(&self, key: &[u8], store: &str) -> Result<SecretBytes> {
        if self.nonce.len() != AES_GCM_NONCE_SIZE {
            return Err(StoreError::InvalidConfiguration {
                store: store.to_string(),
                reason: "Invalid configuration nonce length".to_string(),
            }
            .into());
        }
        let cipher =
            Aes256Gcm::new_from_slice(key).map_err(|_| StoreError::InvalidConfiguration {
                store: store.to_string(),
                reason: "Invalid encryption key length".to_string(),
            })?;
        cipher
            .decrypt(Nonce::from_slice(&self.nonce), self.ciphertext.as_slice())
            .map(SecretBytes::from_vec)
            .map_err(|_| {
                StoreError::ImplementationError {
                    store: store.to_string(),
                    reason: "Failed to decrypt configuration - incorrect password?".to_string(),
                }
                .into()
            })
    }
}

impl PasswordStoreConfig {
    fn data_key(&self, password: &str, store: &str) -> Result<SecretBytes> {
        if !matches!(
            (self.encryption.version.as_str(), &self.wrapped_key),
            ("v0", None) | ("v1", Some(_))
        ) {
            return Err(StoreError::InvalidConfiguration {
                store: store.to_string(),
                reason: "Invalid key wrapping for KDF envelope version".to_string(),
            }
            .into());
        }
        let key = self.encryption.derive_key(password, store)?;
        let Some(wrapped_key) = &self.wrapped_key else {
            return Ok(key);
        };
        let data_key = wrapped_key.decrypt(key.as_bytes(), store)?;
        if data_key.as_bytes().len() != 32 {
            return Err(StoreError::InvalidConfiguration {
                store: store.to_string(),
                reason: "Invalid unwrapped data key length".to_string(),
            }
            .into());
        }
        Ok(data_key)
    }
}

impl From<EncryptedFragment> for Doc {
    fn from(frag: EncryptedFragment) -> Doc {
        let mut doc = Doc::new();
        doc.set("ciphertext", Base64::encode_string(&frag.ciphertext));
        doc.set("nonce", Base64::encode_string(&frag.nonce));
        doc
    }
}

impl TryFrom<&Doc> for EncryptedFragment {
    type Error = crate::Error;

    fn try_from(doc: &Doc) -> crate::Result<Self> {
        let bytes = |key: &str| -> crate::Result<Vec<u8>> {
            let b64 = doc
                .get_as::<&str>(key)
                .ok_or_else(|| CRDTError::ElementNotFound {
                    key: key.to_string(),
                })?;
            Base64::decode_vec(b64).map_err(|_| {
                CRDTError::DeserializationFailed {
                    reason: format!("{key}: invalid base64"),
                }
                .into()
            })
        };

        Ok(EncryptedFragment {
            ciphertext: bytes("ciphertext")?,
            nonce: bytes("nonce")?,
        })
    }
}

impl From<EncryptionInfo> for Doc {
    fn from(info: EncryptionInfo) -> Doc {
        // Functionally this is an atomic Doc, however we can save  small amount of space
        // by relying on this always being a sub-Doc of the PasswordStoreConfig type.
        let mut doc = Doc::new();
        doc.set("algorithm", info.algorithm);
        doc.set("kdf", info.kdf);
        doc.set("salt", info.salt);
        doc.set("version", info.version);
        if let Some(encoding) = info.salt_encoding {
            doc.set("salt_encoding", encoding);
        }
        if let Some(m) = info.argon2_m_cost {
            doc.set("argon2_m_cost", m);
        }
        if let Some(t) = info.argon2_t_cost {
            doc.set("argon2_t_cost", t);
        }
        if let Some(p) = info.argon2_p_cost {
            doc.set("argon2_p_cost", p);
        }
        doc
    }
}

impl TryFrom<&Doc> for EncryptionInfo {
    type Error = crate::Error;

    fn try_from(doc: &Doc) -> crate::Result<Self> {
        let text = |key: &str| -> crate::Result<String> {
            doc.get_as::<&str>(key).map(String::from).ok_or_else(|| {
                CRDTError::ElementNotFound {
                    key: key.to_string(),
                }
                .into()
            })
        };

        let cost = |key: &str| -> crate::Result<Option<u32>> {
            match doc.get(key) {
                None => Ok(None),
                Some(Value::Int(v)) => u32::try_from(*v).map(Some).map_err(|_| {
                    CRDTError::DeserializationFailed {
                        reason: format!("{key}: value out of u32 range"),
                    }
                    .into()
                }),
                _ => Err(CRDTError::DeserializationFailed {
                    reason: format!("{key}: expected an integer"),
                }
                .into()),
            }
        };

        Ok(EncryptionInfo {
            algorithm: text("algorithm")?,
            kdf: text("kdf")?,
            salt: text("salt")?,
            version: text("version")?,
            salt_encoding: doc
                .get("salt_encoding")
                .map(|_| text("salt_encoding"))
                .transpose()?,
            argon2_m_cost: cost("argon2_m_cost")?,
            argon2_t_cost: cost("argon2_t_cost")?,
            argon2_p_cost: cost("argon2_p_cost")?,
        })
    }
}

impl From<PasswordStoreConfig> for Doc {
    fn from(config: PasswordStoreConfig) -> Doc {
        // The config always needs to be written atomically to avoid problems with partial updates.
        let mut doc = Doc::atomic();
        doc.set("encryption", Value::Doc(config.encryption.into()));
        doc.set("wrapped_config", Value::Doc(config.wrapped_config.into()));
        if let Some(key) = config.wrapped_key {
            doc.set("wrapped_key", Value::Doc(key.into()));
        }
        doc
    }
}

impl TryFrom<&Doc> for PasswordStoreConfig {
    type Error = crate::Error;

    fn try_from(doc: &Doc) -> crate::Result<Self> {
        let sub = |key: &str| -> crate::Result<&Doc> {
            match doc.get(key) {
                Some(Value::Doc(d)) => Ok(d),
                _ => Err(CRDTError::ElementNotFound {
                    key: key.to_string(),
                }
                .into()),
            }
        };

        Ok(PasswordStoreConfig {
            encryption: sub("encryption")?.try_into()?,
            wrapped_config: sub("wrapped_config")?.try_into()?,
            wrapped_key: doc
                .get("wrapped_key")
                .map(|_| sub("wrapped_key")?.try_into())
                .transpose()?,
        })
    }
}

impl TryFrom<Doc> for PasswordStoreConfig {
    type Error = crate::Error;

    fn try_from(doc: Doc) -> crate::Result<Self> {
        Self::try_from(&doc)
    }
}

/// Wrapped store metadata serialized and encrypted during initialization.
#[derive(Serialize)]
struct WrappedStoreInfoRef<'a> {
    #[serde(rename = "type")]
    type_id: &'a str,
    config: &'a Doc,
}

/// Decrypted wrapped store metadata, parsed during `open` only to validate it.
///
/// It is dropped immediately after the type check. Its `String` and `Doc` allocations are
/// ordinary owners: dropping them does not wipe their contents.
#[derive(Deserialize)]
struct WrappedStoreInfo {
    #[serde(rename = "type")]
    type_id: String,
    #[serde(rename = "config")]
    _config: Doc,
}

/// Fixed `TypeMismatch::actual` text: the decrypted wrapped type is not echoed.
const REDACTED_WRAPPED_TYPE: &str = "a different encrypted wrapped Store type";

/// Private owner for PasswordStore plaintext bytes and temporary keys.
///
/// The current allocation is zeroized when the owner drops, including on early return and
/// panic unwinding. Zeroization covers only this allocation's current capacity: copies made by
/// callers, earlier reallocations, parsers, crypto libraries, or the allocator are outside it.
///
/// This type intentionally implements no `Debug`, `Display`, `Clone`, `Serialize` or
/// `Deserialize`. Ownership leaves only through [`SecretBytes::into_vec`], which explicitly
/// transfers the allocation to an ordinary caller-owned `Vec` without wiping or copying it.
struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    /// Guard an existing allocation, such as AES-GCM output or a parsed field.
    fn from_vec(bytes: Vec<u8>) -> Self {
        #[cfg(test)]
        let bytes = {
            let mut bytes = bytes;
            hardening_tests::observer::register_owned(&mut bytes);
            bytes
        };
        Self(Zeroizing::new(bytes))
    }

    /// A zero-filled buffer of `len` bytes, such as a derived-key output.
    fn zeroed(len: usize) -> Self {
        Self::from_vec(vec![0; len])
    }

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.0
    }

    /// Transfer the allocation to an ordinary caller-owned `Vec` without copying it.
    ///
    /// The returned bytes are no longer cleaned up by this owner.
    fn into_vec(mut self) -> Vec<u8> {
        std::mem::take(&mut *self.0)
    }

    /// Serialize `value` as JSON directly into a guarded buffer.
    ///
    /// A first pass only measures the output, so the guarded buffer is allocated once at its
    /// final size and serialization never reallocates (and abandons) partial plaintext.
    /// The guard exists before any plaintext is written, so a failed serialization wipes
    /// whatever partial output it produced.
    fn json<T: Serialize>(value: &T) -> serde_json::Result<Self> {
        let mut counter = ByteCounter(0);
        serde_json::to_writer(&mut counter, value)?;
        let mut output = Self::from_vec(Vec::with_capacity(counter.0));
        serde_json::to_writer(&mut *output.0, value)?;
        Ok(output)
    }
}

/// `io::Write` sink that only counts bytes.
struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Deserialize a completed byte array into a [`SecretBytes`] owner.
///
/// Only the completed field is guarded. If deserialization fails part-way through, serde's
/// partially built vector and its growth copies are ordinary allocations and are not wiped.
fn deserialize_secret_bytes<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<SecretBytes, D::Error> {
    Vec::<u8>::deserialize(deserializer).map(SecretBytes::from_vec)
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

/// Securely stored password with automatic zeroization.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
struct Password {
    password: String,
}

/// Password-based encryptor implementing the Encryptor trait.
/// Shares the authenticated data key with the opened wrapper, not the KDF key.
struct PasswordEncryptor {
    key: Arc<SecretBytes>,
    subtree_name: String,
    store_identity: Vec<u8>,
}

#[derive(Serialize)]
struct EncryptedRecordEnvelope<'a> {
    key: &'a [u8],
    value: &'a [u8],
}

/// Decrypted row envelope whose completed fields are guarded until transferred.
#[derive(Deserialize)]
struct DecryptedRecordEnvelope {
    #[serde(deserialize_with = "deserialize_secret_bytes")]
    key: SecretBytes,
    #[serde(deserialize_with = "deserialize_secret_bytes")]
    value: SecretBytes,
}

impl PasswordEncryptor {
    /// Create a new PasswordEncryptor
    fn new(key: Arc<SecretBytes>, subtree_name: String, database_id: &[u8]) -> Self {
        let mut store_identity = database_id.to_vec();
        store_identity.push(0);
        store_identity.extend_from_slice(subtree_name.as_bytes());
        Self {
            key,
            subtree_name,
            store_identity,
        }
    }

    fn record_key_material(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(blake3::derive_key(
            "eidetica/password-store/record-key/v1",
            self.key.as_bytes(),
        ))
    }

    fn record_value_material(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(blake3::derive_key(
            "eidetica/password-store/record-value/v1",
            self.key.as_bytes(),
        ))
    }

    /// Encrypt one row envelope with a caller-supplied 12-byte nonce.
    fn encrypt_record_with_nonce(
        &self,
        logical_key: &[u8],
        plaintext: &[u8],
        nonce: &[u8],
    ) -> Result<Vec<u8>> {
        let key = self.record_value_material();
        let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|error| {
            StoreError::ImplementationError {
                store: self.subtree_name.clone(),
                reason: format!("Failed to create record cipher: {error}"),
            }
        })?;
        let physical_key = self.physical_record_key(logical_key)?;
        let aad = physical_record_aad(&self.store_identity, &physical_key);
        let envelope = SecretBytes::json(&EncryptedRecordEnvelope {
            key: logical_key,
            value: plaintext,
        })?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: envelope.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|error| StoreError::ImplementationError {
                store: self.subtree_name.clone(),
                reason: format!("Record encryption failed: {error}"),
            })?;
        drop(envelope);
        let mut result = nonce.to_vec();
        result.extend(ciphertext);
        Ok(result)
    }
}

impl Encryptor for PasswordEncryptor {
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

        let encryption_key = self.key.as_bytes();
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
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let encryption_key = self.key.as_bytes();
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
    }

    fn physical_record_key(&self, logical_key: &[u8]) -> Result<Vec<u8>> {
        let key = self.record_key_material();
        Ok(blake3::keyed_hash(&key, logical_key).as_bytes().to_vec())
    }

    fn encrypt_record(&self, logical_key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        self.encrypt_record_with_nonce(logical_key, plaintext, nonce.as_slice())
    }

    fn decrypt_record(&self, physical_key: &[u8], ciphertext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        if ciphertext.len() < AES_GCM_NONCE_SIZE {
            return Err(StoreError::DeserializationFailed {
                store: self.subtree_name.clone(),
                reason: "Encrypted record is shorter than its nonce".to_string(),
            }
            .into());
        }
        let (nonce, ciphertext) = ciphertext.split_at(AES_GCM_NONCE_SIZE);
        let key = self.record_value_material();
        let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|error| {
            StoreError::ImplementationError {
                store: self.subtree_name.clone(),
                reason: format!("Failed to create record cipher: {error}"),
            }
        })?;
        let aad = physical_record_aad(&self.store_identity, physical_key);
        let envelope_json = SecretBytes::from_vec(
            cipher
                .decrypt(
                    Nonce::from_slice(nonce),
                    Payload {
                        msg: ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| StoreError::DataCorruption {
                    store: self.subtree_name.clone(),
                    reason: "record authentication failed".to_string(),
                })?,
        );
        // The parser error is not carried: its message can quote decrypted content.
        let envelope: DecryptedRecordEnvelope = serde_json::from_slice(envelope_json.as_bytes())
            .map_err(|_| StoreError::DeserializationFailed {
                store: self.subtree_name.clone(),
                reason: "malformed record envelope".to_string(),
            })?;
        drop(envelope_json);
        if self.physical_record_key(envelope.key.as_bytes())? != physical_key {
            return Err(StoreError::DataCorruption {
                store: self.subtree_name.clone(),
                reason: "record envelope key does not match its physical key".to_string(),
            }
            .into());
        }
        // Accepted fields move into the existing output API without copying.
        Ok((envelope.key.into_vec(), envelope.value.into_vec()))
    }

    fn projection_descriptor(&self, descriptor: ProjectionDescriptor) -> ProjectionDescriptor {
        ProjectionDescriptor {
            name: format!("eidetica/password/{}", descriptor.name),
            version: descriptor.version,
        }
    }
}

fn physical_record_aad(store_identity: &[u8], physical_key: &[u8]) -> Vec<u8> {
    let mut aad = b"eidetica/password-store/record-envelope/v1\0".to_vec();
    aad.extend_from_slice(store_identity);
    aad.push(0);
    aad.extend_from_slice(physical_key);
    aad
}

/// Password-encrypted store wrapper.
///
/// Wraps any [`Store`] type with transparent AES-256-GCM encryption using
/// password-derived keys (Argon2id). The type parameter `S` specifies the
/// wrapped store type, and `PasswordStore<S>` delegates `Store::Data` to
/// `S::Data` — encryption is a transport-level concern invisible at the type level.
///
/// # Type Parameter
///
/// * `S` - The wrapped store type (e.g., `DocStore`, `Table<T>`)
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
///
/// # Security
///
/// - **Encryption**: AES-256-GCM authenticated encryption
/// - **Key Derivation**: Argon2id memory-hard password hashing
/// - **Nonces**: Unique random nonce per encryption operation
/// - **Zeroization**: The converted password, temporary and cached keys, and local
///   plaintext JSON buffers live in zeroizing owners that clear their current allocation
///   on drop, including on errors, cancellation and unwinding
/// - **Diagnostics**: Errors about malformed decrypted metadata or rows do not include
///   decrypted content
///
/// # Limitations
///
/// - **Password Loss**: Losing the password means permanent data loss
/// - **Performance**: Encryption/decryption overhead on every operation
/// - **Metadata**: Backends can observe record counts, ciphertext sizes, and access patterns
/// - **Memory**: Cleanup is best-effort for the owners above. Caller copies, plaintext
///   returned to the application, parsed `Doc`/CRDT values, parser, KDF, cipher and
///   allocator copies are not erased. The wrapper retains its password for explicit upgrades;
///   the registered encryptor retains the data key until the last transaction clone drops
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
/// # let instance = Instance::open_backend(Box::new(backend)).await?;
/// # let (private_key, _) = generate_keypair();
/// # let db = Database::create(&instance, private_key, Doc::new()).await?;
/// let tx = db.new_transaction().await?;
/// let mut encrypted = tx.get_store::<PasswordStore<DocStore>>("secrets").await?;
/// encrypted.initialize("my_password", Doc::new()).await?;
///
/// let docstore = encrypted.inner().await?;
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
/// # let instance = Instance::open_backend(Box::new(backend)).await?;
/// # let (private_key, _) = generate_keypair();
/// # let db = Database::create(&instance, private_key, Doc::new()).await?;
/// let tx = db.new_transaction().await?;
/// let mut store = tx.get_store::<PasswordStore<DocStore>>("secrets").await?;
/// store.open("my_password")?;
///
/// let docstore = store.inner().await?;
/// let value = docstore.get("key").await?;
/// # Ok(())
/// # }
/// ```
pub struct PasswordStore<S: Store> {
    /// Subtree name
    name: String,
    /// Transaction reference
    transaction: Transaction,
    /// Encryption configuration (None if uninitialized)
    config: Option<PasswordStoreConfig>,
    /// Cached password (zeroized on drop)
    cached_password: Option<Password>,
    /// Stable data key shared with the transaction encryptor.
    data_key: Option<Arc<SecretBytes>>,
    /// Phantom type for the wrapped store
    _phantom: PhantomData<S>,
}

impl<S: Store> PasswordStore<S> {
    /// Derive the current state from internal fields
    fn state(&self) -> PasswordStoreState {
        match (&self.config, &self.cached_password) {
            (None, _) => PasswordStoreState::Uninitialized,
            (Some(_), None) => PasswordStoreState::Locked,
            (Some(_), Some(_)) => PasswordStoreState::Unlocked,
        }
    }
}

impl<S: Store> Registered for PasswordStore<S> {
    fn type_id() -> &'static str {
        "encrypted:password:v1"
    }

    fn supports_type_id(type_id: &str) -> bool {
        matches!(type_id, "encrypted:password:v0" | "encrypted:password:v1")
    }
}

#[async_trait]
impl<S: Store> Store for PasswordStore<S> {
    type Data = S::Data;

    fn state_model() -> StoreStateModel<Self::Data> {
        let inner = S::state_model();
        let descriptor = inner.descriptor();
        inner.with_descriptor(ProjectionDescriptor {
            name: format!("eidetica/password/{}", descriptor.name),
            version: descriptor.version,
        })
    }

    async fn load(txn: &Transaction, subtree_name: String) -> Result<Self> {
        // Try to load config from _index to determine state
        let index_store = txn.get_index().await?;
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
        // Empty Doc means uninitialized, non-empty means locked
        if info.config.is_empty() {
            Ok(Self {
                name: subtree_name,
                transaction: txn.clone(),
                config: None,
                cached_password: None,
                data_key: None,
                _phantom: PhantomData,
            })
        } else {
            // Parse the config from the Doc
            let config: PasswordStoreConfig =
                info.config.try_into().map_err(|e: crate::Error| {
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
                transaction: txn.clone(),
                config: Some(config),
                cached_password: None,
                data_key: None,
                _phantom: PhantomData,
            })
        }
    }

    async fn register(txn: &Transaction, subtree_name: String) -> Result<Self> {
        // Register in _index with empty config (marks as uninitialized)
        let index_store = txn.get_index().await?;
        index_store
            .set_entry(&subtree_name, Self::type_id(), Self::default_config())
            .await?;

        Ok(Self {
            name: subtree_name,
            transaction: txn.clone(),
            config: None,
            cached_password: None,
            data_key: None,
            _phantom: PhantomData,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn transaction(&self) -> &Transaction {
        &self.transaction
    }
}

impl<S: Store> PasswordStore<S> {
    /// Initialize encryption on an uninitialized store
    ///
    /// This configures encryption for a PasswordStore that was obtained via
    /// `get_store()`. The wrapped store's type (derived from `S`) and config
    /// are encrypted and stored in the PasswordStore's configuration in `_index`.
    ///
    /// After calling this method, the store transitions to the Unlocked state
    /// and is ready to use.
    ///
    /// # Arguments
    /// * `password` - Password for encryption. After the state check, it is converted and moved
    ///   into a zeroizing owner before any fallible work. An argument rejected by the state
    ///   check, and copies the caller retains, are not zeroized by this method.
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
    /// # let instance = Instance::open_backend(Box::new(backend)).await?;
    /// # let (private_key, _) = generate_keypair();
    /// # let db = Database::create(&instance, private_key, Doc::new()).await?;
    /// let tx = db.new_transaction().await?;
    /// let mut encrypted = tx.get_store::<PasswordStore<DocStore>>("secrets").await?;
    /// encrypted.initialize("my_password", Doc::new()).await?;
    ///
    /// let docstore = encrypted.inner().await?;
    /// docstore.set("key", "secret value").await?;
    /// tx.commit().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn initialize(
        &mut self,
        password: impl Into<String>,
        wrapped_config: Doc,
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

        let password = Password { password: password.into() };
        let encryption = EncryptionInfo::current(
            DEFAULT_ARGON2_M_COST,
            DEFAULT_ARGON2_T_COST,
            DEFAULT_ARGON2_P_COST,
        );
        let wrapping_key = encryption.derive_key(&password.password, &self.name)?;
        let mut data_key = SecretBytes::zeroed(32);
        OsRng.fill_bytes(data_key.as_mut_bytes());
        let wrapped_json = SecretBytes::json(&WrappedStoreInfoRef {
            type_id: S::type_id(),
            config: &wrapped_config,
        })?;
        let config = PasswordStoreConfig {
            encryption,
            wrapped_config: EncryptedFragment::encrypt(
                data_key.as_bytes(),
                wrapped_json.as_bytes(),
                &self.name,
            )?,
            wrapped_key: Some(EncryptedFragment::encrypt(
                wrapping_key.as_bytes(),
                data_key.as_bytes(),
                &self.name,
            )?),
        };
        drop(wrapped_json);
        drop(wrapped_config);
        drop(wrapping_key);
        self.set_config(config.clone().into()).await?;
        let data_key = Arc::new(data_key);
        self.transaction.register_encryptor(
            &self.name,
            Box::new(PasswordEncryptor::new(
                Arc::clone(&data_key),
                self.name.clone(),
                self.transaction.database_id().to_string().as_bytes(),
            )),
        )?;
        self.config = Some(config);
        self.cached_password = Some(password);
        self.data_key = Some(data_key);

        Ok(())
    }

    /// Open (unlock) the encrypted store with a password
    ///
    /// This derives from the stored KDF parameters, unlocks the data key and
    /// authenticates the wrapped Store configuration. It never changes the
    /// envelope; use `needs_rehash()` and explicitly call `upgrade_kdf()`.
    /// The password is retained only for an explicit KDF upgrade.
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
    /// convenience. After the state check, it is converted and moved into a zeroizing
    /// owner before any fallible work; an argument rejected by the state check, and copies
    /// the caller retains, are not zeroized by this method. Errors about malformed
    /// decrypted metadata or a mismatched wrapped type do not include decrypted content.
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
        let password = Password { password: password.into() };
        let data_key = config.data_key(&password.password, &self.name)?;
        let decrypted_config = config.wrapped_config.decrypt(data_key.as_bytes(), &self.name)?;

        // Parser errors can quote decrypted content; retain only fixed diagnostic text.
        let wrapped_info: WrappedStoreInfo =
            serde_json::from_slice(decrypted_config.as_bytes()).map_err(|_| {
                StoreError::DeserializationFailed {
                    store: self.name.clone(),
                    reason: "Failed to parse wrapped store info".to_string(),
                }
            })?;
        drop(decrypted_config);

        let supported = S::supports_type_id(&wrapped_info.type_id);
        drop(wrapped_info);
        if !supported {
            return Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: S::type_id().to_string(),
                actual: REDACTED_WRAPPED_TYPE.to_string(),
            }
            .into());
        }

        let data_key = Arc::new(data_key);
        self.transaction.register_encryptor(
            &self.name,
            Box::new(PasswordEncryptor::new(
                Arc::clone(&data_key),
                self.name.clone(),
                self.transaction.database_id().to_string().as_bytes(),
            )),
        )?;
        self.cached_password = Some(password);
        self.data_key = Some(data_key);

        Ok(())
    }

    /// Whether an authenticated Store needs the current KDF envelope or higher costs.
    ///
    /// This never changes metadata. Call after `open()` and explicitly opt into
    /// [`Self::upgrade_kdf`] to stage an upgrade. Costs above current defaults
    /// are preserved, not downgraded.
    pub fn needs_rehash(&self) -> Result<bool> {
        if !self.is_open() {
            return Err(StoreError::InvalidOperation {
                store: self.name.clone(),
                operation: "needs_rehash".to_string(),
                reason: "Store not opened - call open() first".to_string(),
            }
            .into());
        }
        let encryption = &self
            .config
            .as_ref()
            .expect("Unlocked state requires config")
            .encryption;
        let (m, t, p) = encryption.costs();
        Ok(encryption.version != "v1"
            || m < DEFAULT_ARGON2_M_COST
            || t < DEFAULT_ARGON2_T_COST
            || p < DEFAULT_ARGON2_P_COST)
    }

    /// Explicitly stage a KDF upgrade using the password authenticated by `open()`.
    ///
    /// Returns `true` if an upgrade was staged, `false` if already current.
    /// The transaction must be committed to persist it. The data key, encrypted
    /// history and cached rows are unchanged; only its password wrapping changes.
    /// This is not password rotation: old history retains old KDF metadata.
    pub async fn upgrade_kdf(&mut self) -> Result<bool> {
        if !self.needs_rehash()? {
            return Ok(false);
        }
        let mut config = self
            .config
            .as_ref()
            .expect("Unlocked state requires config")
            .clone();
        let (m, t, p) = config.encryption.costs();
        config.encryption = EncryptionInfo::current(
            m.max(DEFAULT_ARGON2_M_COST),
            t.max(DEFAULT_ARGON2_T_COST),
            p.max(DEFAULT_ARGON2_P_COST),
        );
        let wrapping_key = config.encryption.derive_key(
            &self.cached_password
                .as_ref()
                .expect("Unlocked state requires password")
                .password,
            &self.name,
        )?;
        config.wrapped_key = Some(EncryptedFragment::encrypt(
            wrapping_key.as_bytes(),
            self.data_key
                .as_ref()
                .expect("Unlocked state requires data key")
                .as_bytes(),
            &self.name,
        )?);
        drop(wrapping_key);
        self.set_config(config.clone().into()).await?;
        self.config = Some(config);
        Ok(true)
    }

    /// Check if the store is currently unlocked (password cached)
    pub fn is_open(&self) -> bool {
        self.state() == PasswordStoreState::Unlocked
    }

    /// Check if the store is initialized (has encryption configuration)
    pub fn is_initialized(&self) -> bool {
        self.state() != PasswordStoreState::Uninitialized
    }

    /// Get the wrapped store, providing transparent encryption.
    ///
    /// Returns the inner `S` store instance that transparently encrypts data
    /// on write and decrypts on read. The wrapped store is unaware of
    /// encryption — all crypto operations are handled by an encryptor
    /// registered with the transaction during `open()` or `initialize()`.
    ///
    /// # Errors
    /// - Returns error if store is not opened (call `open()` first)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::{Instance, backend::database::InMemory, crdt::Doc, Database};
    /// # use eidetica::store::{PasswordStore, DocStore};
    /// # use eidetica::auth::generate_keypair;
    /// # async fn example() -> eidetica::Result<()> {
    /// # let backend = InMemory::new();
    /// # let instance = Instance::open_backend(Box::new(backend)).await?;
    /// # let (private_key, _) = generate_keypair();
    /// # let db = Database::create(&instance, private_key, Doc::new()).await?;
    /// # let tx = db.new_transaction().await?;
    /// # let mut encrypted = tx.get_store::<PasswordStore<DocStore>>("test").await?;
    /// # encrypted.initialize("pass", Doc::new()).await?;
    /// # tx.commit().await?;
    /// # let tx2 = db.new_transaction().await?;
    /// let mut encrypted = tx2.get_store::<PasswordStore<DocStore>>("test").await?;
    /// encrypted.open("pass")?;
    ///
    /// let docstore = encrypted.inner().await?;
    /// docstore.set("key", "value").await?; // Automatically encrypted
    /// # Ok(())
    /// # }
    /// ```
    pub async fn inner(&self) -> Result<S> {
        if !self.is_open() {
            return Err(StoreError::InvalidOperation {
                store: self.name.clone(),
                operation: "inner".to_string(),
                reason: "Store not opened - call open() first".to_string(),
            }
            .into());
        }

        // Create the wrapped store. The transaction has an encryptor registered,
        // so it transparently decrypts on read and encrypts on commit.
        // We call S::load() directly, bypassing Transaction::get_store() type
        // checking, since type consistency was already verified in open().
        S::load(&self.transaction, self.name.clone()).await
    }
}

#[cfg(test)]
mod hardening_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn encryptor(store: &str) -> PasswordEncryptor {
        PasswordEncryptor::new(
            Arc::new(
                EncryptionInfo {
                    algorithm: "aes-256-gcm".to_string(),
                    kdf: "argon2id".to_string(),
                    salt: "MDEyMzQ1Njc4OUFCQ0RFRg".to_string(),
                    version: "v0".to_string(),
                    salt_encoding: None,
                    argon2_m_cost: Some(8),
                    argon2_t_cost: Some(1),
                    argon2_p_cost: Some(1),
                }
                .derive_key("record-authentication", store)
                .unwrap(),
            ),
            store.to_string(),
            b"database",
        )
    }

    #[test]
    fn record_keys_and_values_use_separate_domain_material() {
        let first = encryptor("first");
        let second = encryptor("second");
        let master = <[u8; 32]>::try_from(first.key.as_bytes()).unwrap();
        assert_ne!(*first.record_key_material(), master);
        assert_ne!(*first.record_value_material(), master);
        assert_ne!(first.record_key_material(), first.record_value_material());
        assert_eq!(*first.record_key_material(), *second.record_key_material());
        assert_eq!(
            *first.record_value_material(),
            *second.record_value_material()
        );
    }

    #[test]
    fn encrypted_records_use_unique_nonces() {
        let encryptor = encryptor("first");
        let first = encryptor.encrypt_record(b"logical", b"plaintext").unwrap();
        let second = encryptor.encrypt_record(b"logical", b"plaintext").unwrap();

        assert_ne!(&first[..AES_GCM_NONCE_SIZE], &second[..AES_GCM_NONCE_SIZE]);
        assert_ne!(first, second);
    }

    #[test]
    fn encrypted_record_rejects_wrong_store_and_physical_key() {
        let first = encryptor("first");
        let second = encryptor("second");
        let physical_key = first.physical_record_key(b"logical").unwrap();
        let ciphertext = first.encrypt_record(b"logical", b"plaintext").unwrap();
        assert_eq!(
            first.decrypt_record(&physical_key, &ciphertext).unwrap(),
            (b"logical".to_vec(), b"plaintext".to_vec())
        );
        assert!(
            first
                .decrypt_record(&first.physical_record_key(b"other").unwrap(), &ciphertext)
                .is_err(),
            "ciphertext moved to another row must be rejected"
        );
        assert!(
            second.decrypt_record(&physical_key, &ciphertext).is_err(),
            "ciphertext from another Store must be rejected"
        );
    }

    #[test]
    fn encrypted_record_rejects_malformed_and_modified_ciphertext() {
        let encryptor = encryptor("first");
        let physical_key = encryptor.physical_record_key(b"logical").unwrap();
        let mut ciphertext = encryptor.encrypt_record(b"logical", b"plaintext").unwrap();

        assert!(
            encryptor
                .decrypt_record(&physical_key, &ciphertext[..AES_GCM_NONCE_SIZE - 1])
                .is_err()
        );
        *ciphertext.last_mut().unwrap() ^= 1;
        assert!(
            encryptor
                .decrypt_record(&physical_key, &ciphertext)
                .is_err()
        );
    }

    #[test]
    fn kdf_work_ceilings_precede_salt_parsing_and_derivation() {
        for (m, t, p) in [
            (MAX_ARGON2_M_COST + 1, 1, 1),
            (8, MAX_ARGON2_T_COST + 1, 1),
            (128, 1, MAX_ARGON2_P_COST + 1),
        ] {
            let mut info = EncryptionInfo::current(m, t, p);
            info.salt = "invalid!".into();
            assert!(
                info.derive_key("password", "test")
                    .err().unwrap()
                    .to_string()
                    .contains("work ceiling")
            );
        }
        // At the inclusive ceiling validation reaches salt parsing, without
        // actually allocating a maximum-size Argon2 workspace in this test.
        let mut info =
            EncryptionInfo::current(MAX_ARGON2_M_COST, MAX_ARGON2_T_COST, MAX_ARGON2_P_COST);
        info.salt = "invalid!".into();
        assert!(
            info.derive_key("password", "test")
                .err().unwrap()
                .to_string()
                .contains("Invalid salt")
        );
        for (m, t, p) in [(0, 1, 1), (8, 0, 1), (8, 1, 0), (8, 1, 2)] {
            let info = EncryptionInfo::current(m, t, p);
            assert!(
                info.derive_key("password", "test")
                    .err().unwrap()
                    .to_string()
                    .contains("Invalid Argon2 parameters")
            );
        }
    }

    #[test]
    fn kdf_envelope_rejects_unknown_versions_encodings_and_missing_costs() {
        let good = EncryptionInfo::current(8, 1, 1);
        for (version, encoding) in [
            ("v2", Some("raw")),
            ("v1", None),
            ("v1", Some("base64-text")),
            ("v0", Some("raw")),
            ("v1", Some("unknown")),
        ] {
            let mut info = good.clone();
            info.version = version.into();
            info.salt_encoding = encoding.map(String::from);
            assert!(info.derive_key("password", "test").is_err());
        }
        for name in ["argon2_m_cost", "argon2_t_cost", "argon2_p_cost"] {
            let mut json = serde_json::to_value(&good).unwrap();
            json.as_object_mut().unwrap().remove(name);
            let missing: EncryptionInfo = serde_json::from_value(json).unwrap();
            assert!(missing.derive_key("password", "test").is_err());
        }
    }

    #[test]
    fn kdf_cost_doc_fields_reject_wrong_types_and_ranges() {
        let good = EncryptionInfo::current(8, 1, 1);
        for name in ["argon2_m_cost", "argon2_t_cost", "argon2_p_cost"] {
            for value in [
                Value::Text("8".into()),
                Value::Int(-1),
                Value::Int(u32::MAX as i64 + 1),
            ] {
                let mut doc: Doc = good.clone().into();
                doc.set(name, value);
                assert!(EncryptionInfo::try_from(&doc).is_err());
            }
        }
    }

    #[test]
    fn kdf_key_wrap_rejects_missing_corrupt_and_wrong_size_keys() {
        let encryption = EncryptionInfo::current(8, 1, 1);
        let wrapping_key = encryption.derive_key("password", "test").unwrap();
        let key = [42; 32];
        let good = PasswordStoreConfig {
            encryption,
            wrapped_key: Some(EncryptedFragment::encrypt(wrapping_key.as_bytes(), &key, "test").unwrap()),
            wrapped_config: EncryptedFragment::encrypt(&key, b"metadata", "test").unwrap(),
        };
        assert_eq!(good.data_key("password", "test").unwrap().as_bytes(), key);
        assert!(good.data_key("wrong password", "test").is_err());
        let mut bad = good.clone();
        bad.wrapped_key = None;
        assert!(bad.data_key("password", "test").is_err());
        let mut bad = good.clone();
        bad.encryption.version = "v0".into();
        assert!(bad.data_key("password", "test").is_err());
        let mut bad = good.clone();
        bad.wrapped_key.as_mut().unwrap().nonce.pop();
        assert!(bad.data_key("password", "test").is_err());
        let mut bad = good.clone();
        bad.wrapped_key.as_mut().unwrap().ciphertext[0] ^= 1;
        assert!(bad.data_key("password", "test").is_err());
        let mut bad = good;
        bad.wrapped_key =
            Some(EncryptedFragment::encrypt(wrapping_key.as_bytes(), &[42; 31], "test").unwrap());
        assert!(bad.data_key("password", "test").is_err());
    }
}
