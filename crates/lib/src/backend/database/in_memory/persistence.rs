//! Persistence operations for InMemory database
//!
//! This module handles serialization and file I/O for saving/loading
//! the in-memory database state to/from JSON files.

use std::{collections::HashMap, path::Path};

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::RwLock;

use super::{InMemory, TreeHeightsCache, TreeTipsCache};
use crate::{
    Error, Result,
    auth::crypto::ED25519_PRIVATE_KEY_SIZE,
    backend::{VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
};

/// The current persistence file format version.
/// v0 indicates this is an unstable format subject to breaking changes.
const PERSISTENCE_VERSION: u8 = 0;

/// Helper to check if version is default (0) for serde skip_serializing_if
fn is_v0(v: &u8) -> bool {
    *v == 0
}

/// Validates the persistence version during deserialization.
fn validate_persistence_version<'de, D>(deserializer: D) -> std::result::Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::Deserialize;
    let version = u8::deserialize(deserializer)?;
    if version != PERSISTENCE_VERSION {
        return Err(serde::de::Error::custom(format!(
            "unsupported persistence version {version}; only version {PERSISTENCE_VERSION} is supported"
        )));
    }
    Ok(version)
}

/// Serializable version of InMemory database for persistence
#[derive(Serialize, Deserialize)]
struct SerializableDatabase {
    /// File format version for compatibility checking
    #[serde(
        rename = "_v",
        default,
        skip_serializing_if = "is_v0",
        deserialize_with = "validate_persistence_version"
    )]
    version: u8,
    entries: HashMap<ID, Entry>,
    #[serde(default)]
    verification_status: HashMap<ID, VerificationStatus>,
    /// Private keys stored as ED25519_PRIVATE_KEY_SIZE-byte arrays for serialization
    #[serde(default)]
    private_keys_bytes: HashMap<String, [u8; ED25519_PRIVATE_KEY_SIZE]>,
    /// Generic key-value cache (not serialized - cache is rebuilt on load)
    #[serde(default)]
    cache: HashMap<String, String>,
    /// Cached heights grouped by tree
    #[serde(default)]
    heights: HashMap<ID, TreeHeightsCache>,
    /// Cached tips grouped by tree
    #[serde(default)]
    tips: HashMap<ID, TreeTipsCache>,
}

impl Serialize for InMemory {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Use blocking_read since serde's Serialize is sync
        let entries = self.entries.blocking_read().clone();
        let verification_status = self.verification_status.blocking_read().clone();
        let private_keys = self.private_keys.blocking_read();
        let private_keys_bytes = private_keys
            .iter()
            .map(|(k, v)| (k.clone(), v.to_bytes()))
            .collect();
        let cache = self.cache.blocking_read().clone();
        let heights = self.heights.blocking_read().clone();
        let tips = self.tips.blocking_read().clone();

        let serializable = SerializableDatabase {
            version: PERSISTENCE_VERSION,
            entries,
            verification_status,
            private_keys_bytes,
            cache,
            heights,
            tips,
        };

        serializable.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InMemory {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Version validation happens via deserialize_with on SerializableDatabase._v
        let serializable = SerializableDatabase::deserialize(deserializer)?;

        let private_keys = serializable
            .private_keys_bytes
            .into_iter()
            .map(|(k, bytes)| {
                let signing_key = SigningKey::from_bytes(&bytes);
                (k, signing_key)
            })
            .collect();

        Ok(InMemory {
            entries: RwLock::new(serializable.entries),
            verification_status: RwLock::new(serializable.verification_status),
            private_keys: RwLock::new(private_keys),
            cache: RwLock::new(serializable.cache),
            heights: RwLock::new(serializable.heights),
            tips: RwLock::new(serializable.tips),
        })
    }
}

/// Saves the entire database state (all entries) to a specified file as JSON.
///
/// # Arguments
/// * `backend` - The InMemory database to save
/// * `path` - The path to the file where the state should be saved.
///
/// # Returns
/// A `Result` indicating success or an I/O or serialization error.
pub(crate) async fn save_to_file<P: AsRef<Path>>(backend: &InMemory, path: P) -> Result<()> {
    // Extract data from locks asynchronously (can't use blocking_read in async context)
    let entries = backend.entries.read().await.clone();
    let verification_status = backend.verification_status.read().await.clone();
    let private_keys = backend.private_keys.read().await;
    let private_keys_bytes = private_keys
        .iter()
        .map(|(k, v)| (k.clone(), v.to_bytes()))
        .collect();
    drop(private_keys);
    let cache = backend.cache.read().await.clone();
    let heights = backend.heights.read().await.clone();
    let tips = backend.tips.read().await.clone();

    let serializable = SerializableDatabase {
        version: PERSISTENCE_VERSION,
        entries,
        verification_status,
        private_keys_bytes,
        cache,
        heights,
        tips,
    };

    let json = serde_json::to_string_pretty(&serializable)
        .map_err(|e| -> Error { BackendError::SerializationFailed { source: e }.into() })?;
    tokio::fs::write(path, json)
        .await
        .map_err(|e| -> Error { BackendError::FileIo { source: e }.into() })
}

/// Loads the database state from a specified JSON file.
///
/// If the file does not exist, a new, empty `InMemory` database is returned.
///
/// # Arguments
/// * `path` - The path to the file from which to load the state.
///
/// # Returns
/// A `Result` containing the loaded `InMemory` database or an I/O or deserialization error.
pub(crate) async fn load_from_file<P: AsRef<Path>>(path: P) -> Result<InMemory> {
    match tokio::fs::read_to_string(path).await {
        Ok(json) => {
            let database: InMemory = serde_json::from_str(&json).map_err(|e| -> Error {
                BackendError::DeserializationFailed { source: e }.into()
            })?;
            Ok(database)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(InMemory::new()),
        Err(e) => Err(BackendError::FileIo { source: e }.into()),
    }
}
