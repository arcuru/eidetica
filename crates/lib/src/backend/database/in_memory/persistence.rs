//! Persistence operations for InMemory database
//!
//! This module handles serialization and file I/O for saving/loading
//! the in-memory database state to/from JSON files.

use std::{collections::HashMap, path::Path, sync::RwLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{InMemory, InMemoryInner, TreeTipsCache, VerifiedState};
use crate::{
    Error, Result,
    backend::{InstanceMetadata, InstanceSecrets, VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
};

/// The current persistence file format version.
/// v2 retains verified tips only; unreleased v1 prefix files are refused.
const PERSISTENCE_VERSION: u8 = 2;

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
    if version != 0 && version != PERSISTENCE_VERSION {
        return Err(serde::de::Error::custom(format!(
            "unsupported persistence version {version}; newest supported version \
             is {PERSISTENCE_VERSION} (v0 files rebuild retained state on load)"
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
    /// now scope-keyed (Shared vs Login) and bounded by an LRU; rather than
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
    /// Retained verified frontier per tree. Absent in v0 files and
    /// rebuilt on load; persisted since v2.
    #[serde(default)]
    verified: HashMap<ID, VerifiedState>,
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
                verified: inner.verified.clone(),
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

        let mut inner = InMemoryInner {
            entries: serializable.entries,
            // Derived and staging Store-state records are disposable.
            store_state_namespaces: HashMap::new(),
            verification_status: serializable.verification_status,
            instance_metadata: serializable.instance_metadata,
            instance_secrets: serializable.instance_secrets,
            tips: serializable.tips,
            verified: serializable.verified,
        };
        // v0 files predate the retained verified state: rebuild every
        // touched tree from entries and statuses so the hot path never
        // depends on state that was never persisted.
        if serializable.version == 0 {
            let mut trees: Vec<ID> = inner.tips.keys().cloned().collect();
            for entry in inner.entries.values() {
                let tree = entry.root().unwrap_or_else(|| entry.id());
                if !trees.contains(&tree) {
                    trees.push(tree);
                }
            }
            for tree in &trees {
                super::storage::repair_verification(&mut inner, tree)
                    .map_err(serde::de::Error::custom)?;
            }
        }

        Ok(InMemory {
            inner: RwLock::new(inner),
            store_state_point_reads: std::sync::atomic::AtomicUsize::new(0),
            store_state_scan_reads: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(feature = "testing")]
            store_history_reads: std::sync::atomic::AtomicUsize::new(0),
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
            verified: inner.verified.clone(),
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

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use crate::entry::Entry;

    /// Build a backend holding root → a → b with root and a Verified.
    fn verified_chain() -> (InMemory, ID, ID) {
        let backend = InMemory::new();
        let root = Entry::root_builder().build().expect("root builds");
        let root_id = root.id();
        let a = Entry::builder(root_id.clone())
            .add_parent(root_id.clone())
            .set_subtree_data("test", b"a")
            .build()
            .expect("child a builds");
        let a_id = a.id();
        let b = Entry::builder(root_id.clone())
            .add_parent(a_id.clone())
            .set_subtree_data("test", b"b")
            .build()
            .expect("child b builds");
        {
            let mut inner = backend.inner.write().unwrap();
            super::super::storage::put(&mut inner, root).unwrap();
            super::super::storage::put(&mut inner, a).unwrap();
            super::super::storage::put(&mut inner, b).unwrap();
            super::super::storage::update_verification_status(
                &mut inner,
                &root_id,
                VerificationStatus::Verified,
            )
            .unwrap();
            super::super::storage::update_verification_status(
                &mut inner,
                &a_id,
                VerificationStatus::Verified,
            )
            .unwrap();
        }
        (backend, root_id, a_id)
    }

    #[test]
    fn v2_round_trip_retains_verified_frontier() {
        let (backend, root_id, a_id) = verified_chain();
        let before =
            super::super::storage::verified_snapshot(&backend.inner.read().unwrap(), &root_id);
        assert_eq!(before.tips(), std::slice::from_ref(&a_id));

        let value = serde_json::to_value(&backend).expect("serializes");
        assert_eq!(value["_v"], 2);
        assert!(value.get("verified").is_some());

        let loaded: InMemory = serde_json::from_value(value).expect("deserializes");
        let after =
            super::super::storage::verified_snapshot(&loaded.inner.read().unwrap(), &root_id);
        assert_eq!(after.tips(), &[a_id]);
    }

    /// v0 files predate the retained state: loading rebuilds the frontier
    /// from entries and statuses instead of failing or serving empty.
    #[test]
    fn v0_file_rebuilds_verified_frontier_on_load() {
        let (backend, root_id, a_id) = verified_chain();
        let mut value = serde_json::to_value(&backend).expect("serializes");
        value.as_object_mut().unwrap().remove("verified");
        value.as_object_mut().unwrap().remove("_v");

        let loaded: InMemory = serde_json::from_value(value).expect("v0 still loads");
        let after =
            super::super::storage::verified_snapshot(&loaded.inner.read().unwrap(), &root_id);
        assert_eq!(after.tips(), &[a_id]);
    }

    /// A v2 file is trusted as persisted: an emptied retained map loads
    /// empty rather than silently rescanning history. (Repair is an
    /// explicit `repair_verification` call, not a load side effect.)
    #[test]
    fn v2_file_trusts_persisted_state() {
        let (backend, root_id, _) = verified_chain();
        let mut value = serde_json::to_value(&backend).expect("serializes");
        value["verified"] = serde_json::Value::Object(Default::default());

        let loaded: InMemory = serde_json::from_value(value).expect("deserializes");
        let after =
            super::super::storage::verified_snapshot(&loaded.inner.read().unwrap(), &root_id);
        assert!(after.is_empty());
    }
}

#[cfg(test)]
mod repair_tests {
    use super::*;
    use crate::backend::BackendImpl;

    #[tokio::test]
    async fn persisted_tips_scale_with_frontier_not_history_and_repair_is_conservative() {
        let backend = InMemory::new();
        let root = Entry::root_builder().build().unwrap();
        let tree = root.id();
        backend.put(root).await.unwrap();
        backend
            .update_verification_status(&tree, VerificationStatus::Verified)
            .await
            .unwrap();
        let mut ids = vec![tree.clone()];
        for _ in 0..128 {
            let entry = Entry::builder(tree.clone())
                .add_parent(ids.last().unwrap().clone())
                .build()
                .unwrap();
            let id = entry.id();
            backend.put(entry).await.unwrap();
            backend
                .update_verification_status(&id, VerificationStatus::Verified)
                .await
                .unwrap();
            ids.push(id);
        }
        let value = serde_json::to_value(&backend).unwrap();
        let state = &value["verified"][tree.to_string()];
        assert_eq!(state["frontier"].as_array().unwrap().len(), 1);
        assert!(state.get("prefix").is_none());
        assert_eq!(
            backend
                .get_entries_by_verification_status(VerificationStatus::Verified)
                .await
                .unwrap()
                .len(),
            129
        );
        // Corrupt only local labels, as an old mainline database can contain.
        backend
            .inner
            .write()
            .unwrap()
            .verification_status
            .insert(ids[64].clone(), VerificationStatus::Failed);
        let report = backend.repair_verification(&tree).await.unwrap();
        assert_eq!(report.demoted, 64);
        assert_eq!(report.snapshot.tips(), &[ids[63].clone()]);
        assert_eq!(
            backend.get_verification_status(&ids[64]).await.unwrap(),
            VerificationStatus::Failed
        );
        assert_eq!(backend.repair_verification(&tree).await.unwrap().demoted, 0);
        assert_eq!(backend.inner.read().unwrap().entries.len(), 129);
        let loaded: InMemory =
            serde_json::from_value(serde_json::to_value(&backend).unwrap()).unwrap();
        assert_eq!(
            loaded.verified_snapshot(&tree).await.unwrap(),
            report.snapshot
        );
    }

    #[tokio::test]
    async fn mainline_file_repairs_inconsistent_labels_and_candidate_file_is_refused() {
        let backend = InMemory::new();
        let root = Entry::root_builder().build().unwrap();
        let tree = root.id();
        let child = Entry::builder(tree.clone())
            .add_parent(tree.clone())
            .build()
            .unwrap();
        let child_id = child.id();
        backend.put(root.clone()).await.unwrap();
        backend.put(child.clone()).await.unwrap();
        let mut mainline = serde_json::json!({
            "entries": { tree.to_string(): root, child_id.to_string(): child },
            "verification_status": { tree.to_string(): "Unverified", child_id.to_string(): "Verified" },
            "tips": {}, "instance_metadata": null, "instance_secrets": null
        });
        let loaded: InMemory = serde_json::from_value(mainline.clone()).unwrap();
        assert_eq!(
            loaded.get_verification_status(&child_id).await.unwrap(),
            VerificationStatus::Unverified
        );
        assert_eq!(
            loaded.get(&child_id).await.unwrap(),
            backend.get(&child_id).await.unwrap()
        );
        loaded
            .update_verification_status(&tree, VerificationStatus::Verified)
            .await
            .unwrap();
        loaded
            .update_verification_status(&child_id, VerificationStatus::Verified)
            .await
            .unwrap();
        assert_eq!(
            loaded.verified_snapshot(&tree).await.unwrap().tips(),
            &[child_id]
        );
        mainline["_v"] = serde_json::json!(1);
        mainline["verified"] =
            serde_json::json!({ tree.to_string(): {"prefix": [tree], "frontier": []} });
        assert!(serde_json::from_value::<InMemory>(mainline).is_err());
    }
}
