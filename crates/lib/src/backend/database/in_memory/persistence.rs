//! Persistence operations for InMemory database
//!
//! This module handles serialization and file I/O for saving/loading
//! the in-memory database state to/from JSON files.

use super::{InMemory, TreeHeightsCache, TreeTipsCache};
use crate::backend::VerificationStatus;
use crate::entry::{Entry, ID};
use crate::{Error, Result};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::RwLock;

/// Serializable version of InMemory database for persistence
#[derive(Serialize, Deserialize)]
struct SerializableDatabase {
    entries: HashMap<ID, Entry>,
    #[serde(default)]
    verification_status: HashMap<ID, VerificationStatus>,
    /// Private keys stored as 32-byte arrays for serialization
    #[serde(default)]
    private_keys_bytes: HashMap<String, [u8; 32]>,
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
        let entries = self.entries.read().unwrap().clone();
        let verification_status = self.verification_status.read().unwrap().clone();
        let private_keys = self.private_keys.read().unwrap();
        let private_keys_bytes = private_keys
            .iter()
            .map(|(k, v)| (k.clone(), v.to_bytes()))
            .collect();
        let cache = self.cache.read().unwrap().clone();
        let heights = self.heights.read().unwrap().clone();
        let tips = self.tips.read().unwrap().clone();

        let serializable = SerializableDatabase {
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
pub(crate) fn save_to_file<P: AsRef<Path>>(backend: &InMemory, path: P) -> Result<()> {
    let json = serde_json::to_string_pretty(backend)
        .map_err(|e| Error::Io(std::io::Error::other(format!("Failed to serialize: {e}"))))?;
    fs::write(path, json).map_err(Error::Io)
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
    if !path.as_ref().exists() {
        return Ok(InMemory::new());
    }

    let json = fs::read_to_string(path).map_err(Error::Io)?;
    let database: InMemory = serde_json::from_str(&json)
        .map_err(|e| Error::Io(std::io::Error::other(format!("Failed to deserialize: {e}"))))?;

    Ok(database)
}
