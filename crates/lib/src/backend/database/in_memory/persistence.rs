//! Persistence operations for InMemory database
//!
//! This module handles serialization and file I/O for saving/loading
//! the in-memory database state to/from JSON files.

use std::{collections::HashMap, path::Path, sync::RwLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{InMemory, InMemoryInner, TreeTipsCache};
use crate::{
    Error, Result,
    backend::{InstanceMetadata, VerificationStatus, errors::BackendError},
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
    /// Instance metadata containing device key and system database IDs
    #[serde(default)]
    instance_metadata: Option<InstanceMetadata>,
    /// Generic key-value cache (not serialized - cache is rebuilt on load)
    #[serde(default)]
    cache: HashMap<String, String>,
    /// Cached tips grouped by tree
    #[serde(default)]
    tips: HashMap<ID, TreeTipsCache>,
}

impl Serialize for InMemory {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Clone data under locks, then release before serializing
        let serializable = {
            let inner = self.inner.read().unwrap();
            let crdt_cache = self.crdt_cache.read().unwrap();
            SerializableDatabase {
                version: PERSISTENCE_VERSION,
                entries: inner.entries.clone(),
                verification_status: inner.verification_status.clone(),
                instance_metadata: inner.instance_metadata.clone(),
                cache: crdt_cache.clone(),
                tips: inner.tips.clone(),
            }
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

        Ok(InMemory {
            inner: RwLock::new(InMemoryInner {
                entries: serializable.entries,
                verification_status: serializable.verification_status,
                instance_metadata: serializable.instance_metadata,
                tips: serializable.tips,
            }),
            crdt_cache: RwLock::new(serializable.cache),
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
pub(crate) fn save_to_file<P: AsRef<Path>>(backend: &InMemory, path: P) -> Result<()> {
    // Clone data under locks, then release before file I/O
    let serializable = {
        let inner = backend.inner.read().unwrap();
        let crdt_cache = backend.crdt_cache.read().unwrap();
        SerializableDatabase {
            version: PERSISTENCE_VERSION,
            entries: inner.entries.clone(),
            verification_status: inner.verification_status.clone(),
            instance_metadata: inner.instance_metadata.clone(),
            cache: crdt_cache.clone(),
            tips: inner.tips.clone(),
        }
    };

    let json = serde_json::to_string_pretty(&serializable)
        .map_err(|e| -> Error { BackendError::SerializationFailed { source: e }.into() })?;
    std::fs::write(path, json).map_err(|e| -> Error { BackendError::FileIo { source: e }.into() })
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
pub(crate) fn load_from_file<P: AsRef<Path>>(path: P) -> Result<InMemory> {
    match std::fs::read_to_string(path) {
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
