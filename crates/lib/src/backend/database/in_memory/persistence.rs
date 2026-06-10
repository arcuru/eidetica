//! Persistence operations for InMemory database
//!
//! This module handles serialization and file I/O for saving/loading
//! the in-memory database state to/from JSON files.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::RwLock,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{InMemory, InMemoryInner, TreeTipsCache, cache::InMemoryCrdtCache};
use crate::{
    Error, Result,
    backend::{InstanceMetadata, InstanceSecrets, VerificationStatus, errors::BackendError},
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
    /// Instance metadata containing device public key and system database IDs
    #[serde(default)]
    instance_metadata: Option<InstanceMetadata>,
    /// Instance secrets containing the device signing key
    #[serde(default)]
    instance_secrets: Option<InstanceSecrets>,
    /// CRDT state cache *was* serialized here pre-unification. The cache is
    /// now scope-keyed (Shared vs User) and bounded by an LRU; rather than
    /// serializing an opaque LRU snapshot, we treat the cache as ephemeral
    /// performance state and rebuild lazily on load. Field retained as
    /// `#[serde(default, skip_serializing)]` so old snapshots still
    /// deserialize cleanly; the bytes are discarded.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    cache: Option<serde_json::Value>,
    /// Cached tips grouped by tree
    #[serde(default)]
    tips: HashMap<ID, TreeTipsCache>,
    /// Content-addressed blobs. Durable owned content (unlike the CRDT cache),
    /// so it IS persisted. `#[serde(default)]` keeps pre-blob snapshots loading.
    #[serde(default)]
    blobs: HashMap<ID, Vec<u8>>,
    /// Per-blob last-access time (epoch ms) for LRU eviction (§6).
    #[serde(default)]
    blob_accessed: HashMap<ID, i64>,
    /// Blob pins (`(user_id, database_id, blob_cid)`): the local GC root set.
    #[serde(default)]
    blob_pins: HashSet<(String, String, ID)>,
}

impl Serialize for InMemory {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Clone data under locks, then release before serializing.
        // The CRDT cache is deliberately not persisted; see the
        // SerializableDatabase docs.
        let serializable = {
            let inner = self.inner.read().unwrap();
            SerializableDatabase {
                version: PERSISTENCE_VERSION,
                entries: inner.entries.clone(),
                verification_status: inner.verification_status.clone(),
                instance_metadata: inner.instance_metadata.clone(),
                instance_secrets: inner.instance_secrets.clone(),
                cache: None,
                tips: inner.tips.clone(),
                blobs: inner.blobs.clone(),
                blob_accessed: inner.blob_accessed.clone(),
                blob_pins: inner.blob_pins.clone(),
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
                instance_secrets: serializable.instance_secrets,
                tips: serializable.tips,
                blobs: serializable.blobs,
                blob_accessed: serializable.blob_accessed,
                blob_pins: serializable.blob_pins,
            }),
            // Cache rebuilds lazily as reads materialize state — see the
            // `cache` field's doc on SerializableDatabase.
            crdt_cache: std::sync::Mutex::new(InMemoryCrdtCache::new()),
        })
    }
}

/// Saves the entire database state (all entries) to a specified file as JSON.
///
/// **Atomicity:** the write goes to `<path>.tmp` first, then renames into
/// place. On POSIX the final rename is atomic — a process crash mid-write
/// leaves the previous snapshot intact and any stale `.tmp` is overwritten
/// on the next save. On Windows the rename is not atomic when the
/// destination already exists, so a crash during the rename can leave a
/// stale `.tmp` and an out-of-date snapshot.
///
/// # Arguments
/// * `backend` - The InMemory database to save
/// * `path` - The path to the file where the state should be saved.
///
/// # Returns
/// A `Result` indicating success or an I/O or serialization error.
pub(crate) fn save_to_file<P: AsRef<Path>>(backend: &InMemory, path: P) -> Result<()> {
    // Clone data under locks, then release before file I/O. Cache
    // deliberately not persisted; see SerializableDatabase docs.
    let serializable = {
        let inner = backend.inner.read().unwrap();
        SerializableDatabase {
            version: PERSISTENCE_VERSION,
            entries: inner.entries.clone(),
            verification_status: inner.verification_status.clone(),
            instance_metadata: inner.instance_metadata.clone(),
            instance_secrets: inner.instance_secrets.clone(),
            cache: None,
            tips: inner.tips.clone(),
            blobs: inner.blobs.clone(),
            blob_accessed: inner.blob_accessed.clone(),
            blob_pins: inner.blob_pins.clone(),
        }
    };

    let json = serde_json::to_string_pretty(&serializable)
        .map_err(|e| -> Error { BackendError::SerializationFailed { source: e }.into() })?;

    // Write to a sibling tempfile, then atomic rename. `<path>.tmp` is the
    // standard convention; a stale tempfile from a crashed previous run is
    // overwritten on the next save.
    let path = path.as_ref();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp);

    std::fs::write(&tmp_path, json.as_bytes())
        .map_err(|e| -> Error { BackendError::FileIo { source: e }.into() })?;
    std::fs::rename(&tmp_path, path).map_err(|e| -> Error {
        // Best-effort cleanup of the tempfile if rename failed; ignore
        // any cleanup error (the original failure is what the caller
        // needs to see).
        let _ = std::fs::remove_file(&tmp_path);
        BackendError::FileIo { source: e }.into()
    })
}

/// Attempts to load the database state from a specified JSON file.
///
/// Returns `Ok(None)` when the file does not exist; the caller decides
/// whether that's a fresh-start signal or an error (strict load vs.
/// bootstrap). Other I/O errors and deserialisation errors surface
/// directly.
///
/// Reading the bytes and parsing happen in a single call so there's no
/// TOCTOU window between an external "does this snapshot exist?" check
/// and the actual read.
pub(crate) fn try_load_from_file<P: AsRef<Path>>(path: P) -> Result<Option<InMemory>> {
    match std::fs::read_to_string(path) {
        Ok(json) => {
            let database: InMemory = serde_json::from_str(&json).map_err(|e| -> Error {
                BackendError::DeserializationFailed { source: e }.into()
            })?;
            Ok(Some(database))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(BackendError::FileIo { source: e }.into()),
    }
}

/// Loads the database state from a specified JSON file.
///
/// If the file does not exist, a new, empty `InMemory` database is returned.
/// Callers that need to distinguish "missing" from "loaded empty" should
/// use [`try_load_from_file`] instead.
///
/// # Arguments
/// * `path` - The path to the file from which to load the state.
///
/// # Returns
/// A `Result` containing the loaded `InMemory` database or an I/O or deserialization error.
pub(crate) fn load_from_file<P: AsRef<Path>>(path: P) -> Result<InMemory> {
    Ok(try_load_from_file(path)?.unwrap_or_else(InMemory::new))
}
