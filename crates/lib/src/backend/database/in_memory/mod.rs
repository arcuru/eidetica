//! In-memory database backend implementation
//!
//! This module provides an in-memory implementation of the Database trait,
//! suitable for testing, development, or scenarios where data persistence
//! is not strictly required or is handled externally.

mod persistence;
mod storage;
mod traversal;

use std::{
    any::Any,
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{
        RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    backend::{
        BackendImpl, InstanceMetadata, InstanceSecrets, RecordMutation, RecordMutations,
        RecordPage, RecordRange, RecordView, StagingStatus, StagingToken, StoreStateLifecycle,
        StoreStateRequest, VerificationStatus, errors::BackendError,
    },
    entry::{Entry, ID},
    snapshot::Snapshot,
};

use crate::backend::database::sorting;

fn staging_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_secs() as i64
}

/// Grouped tree tips cache: (tree_tips, subtree_name -> subtree_tips)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct TreeTipsCache {
    pub(crate) tree_tips: HashSet<ID>,
    pub(crate) subtree_tips: HashMap<String, HashSet<ID>>,
}

/// Core data protected by a single lock.
///
/// All fields that participate in entry storage and tip tracking are grouped
/// together to eliminate lock ordering concerns. A single `RwLock` on the
/// outer `InMemory` struct protects all fields atomically.
#[derive(Debug)]
pub(crate) struct InMemoryInner {
    pub(crate) entries: HashMap<ID, Entry>,
    pub(crate) store_state_namespaces: HashMap<String, RecordNamespace>,
    pub(crate) staging_tokens: HashMap<String, MemoryStagingToken>,
    pub(crate) verification_status: HashMap<ID, VerificationStatus>,
    /// Instance metadata containing device public key and system database IDs.
    ///
    /// When `None`, the backend is uninitialized. When `Some`, contains the
    /// device public key and root IDs for system databases.
    pub(crate) instance_metadata: Option<InstanceMetadata>,
    /// Instance secrets containing the device signing key.
    ///
    /// **Security Warning**: The signing key is stored in memory without encryption.
    /// This is suitable for development/testing only. Production systems should use
    /// proper key management with encryption at rest.
    pub(crate) instance_secrets: Option<InstanceSecrets>,
    /// Cached tips grouped by tree: tree_id -> (tree_tips, subtree_name -> subtree_tips)
    pub(crate) tips: HashMap<ID, TreeTipsCache>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemoryStagingToken {
    pub(crate) target: StoreStateRequest,
    pub(crate) status: StagingStatus,
    pub(crate) last_activity: i64,
    #[serde(default)]
    pub(crate) next_sequence: u64,
    pub(crate) last_digest: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RecordNamespace {
    pub(crate) request: StoreStateRequest,
    pub(crate) ready: bool,
    /// Unlinked by a derived clear: no longer resolvable, still readable
    /// through views resolved before the clear.
    pub(crate) unlinked: bool,
    pub(crate) records: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
}

/// A simple in-memory database implementation using a `HashMap` for storage.
///
/// This database is suitable for testing, development, or scenarios where
/// data persistence is not strictly required or is handled externally
/// (e.g., by saving/loading the entire state to/from a file).
///
/// It provides basic persistence capabilities via `save_to_file` and
/// `load_from_file`, serializing the `HashMap` to JSON.
///
/// **Security Note**: The device key is stored in memory in plaintext in this implementation.
/// This is acceptable for development and testing but should not be used in production
/// without proper encryption or hardware security module integration.
#[derive(Debug)]
pub struct InMemory {
    /// Core data protected by a single lock for atomic access and
    /// to eliminate lock ordering concerns between entries, verification
    /// status, and tips.
    pub(crate) inner: RwLock<InMemoryInner>,
    store_state_point_reads: AtomicUsize,
    store_state_scan_reads: AtomicUsize,
    #[cfg(feature = "testing")]
    store_history_reads: AtomicUsize,
}

impl InMemory {
    #[cfg(feature = "testing")]
    pub fn store_state_record_count(&self, database: &ID, store: &str) -> usize {
        self.inner
            .read()
            .unwrap()
            .store_state_namespaces
            .values()
            .filter(|record_set| {
                record_set.ready
                    && record_set.request.database == *database
                    && record_set.request.store == store
                    && matches!(
                        record_set.request.projection.name.as_str(),
                        "eidetica/table/rows/canonical-json:v0"
                            | "eidetica/password/eidetica/table/rows/canonical-json:v0"
                    )
            })
            .map(|record_set| record_set.records.len())
            .max()
            .unwrap_or(0)
    }

    #[cfg(feature = "testing")]
    pub fn store_state_records(
        &self,
        database: &ID,
        store: &str,
    ) -> Option<crate::backend::RecordMutations> {
        self.inner
            .read()
            .unwrap()
            .store_state_namespaces
            .values()
            .find(|namespace| {
                namespace.ready
                    && namespace.request.database == *database
                    && namespace.request.store == store
            })
            .map(|namespace| namespace.records.clone())
    }

    #[cfg(feature = "testing")]
    pub fn store_state_read_counts(&self) -> (usize, usize) {
        (
            self.store_state_point_reads.load(Ordering::Relaxed),
            self.store_state_scan_reads.load(Ordering::Relaxed),
        )
    }

    #[cfg(feature = "testing")]
    pub fn store_history_read_count(&self) -> usize {
        self.store_history_reads.load(Ordering::Relaxed)
    }
}

impl InMemory {
    /// Creates a new, empty `InMemory` database.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(InMemoryInner {
                entries: HashMap::new(),
                store_state_namespaces: HashMap::new(),
                staging_tokens: HashMap::new(),
                verification_status: HashMap::new(),
                instance_metadata: None,
                instance_secrets: None,
                tips: HashMap::new(),
            }),
            store_state_point_reads: AtomicUsize::new(0),
            store_state_scan_reads: AtomicUsize::new(0),
            #[cfg(feature = "testing")]
            store_history_reads: AtomicUsize::new(0),
        }
    }

    /// Returns a vector containing the IDs of all entries currently stored in the database.
    pub async fn all_ids(&self) -> Vec<ID> {
        let inner = self.inner.read().unwrap();
        inner.entries.keys().cloned().collect()
    }

    /// Saves the entire database state (all entries) to a specified file as JSON.
    ///
    /// The write is atomic on POSIX (writes to `<path>.tmp` then renames
    /// into place). On Windows the final rename is not atomic when the
    /// destination already exists.
    ///
    /// This is synchronous — the body is just `std::fs::write` + `rename`
    /// with no await points — so it's safe to call from `Drop` impls and
    /// other non-async contexts. Callers on a tokio runtime should be
    /// aware that the write briefly blocks the calling worker thread.
    ///
    /// # Arguments
    /// * `path` - The path to the file where the state should be saved.
    ///
    /// # Returns
    /// A `Result` indicating success or an I/O or serialization error.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        persistence::save_to_file(self, path)
    }

    /// Loads the database state from a specified JSON file.
    ///
    /// If the file does not exist, a new, empty `InMemory` database is returned.
    /// Callers that need to tell "missing" apart from "loaded empty" should
    /// use [`Self::try_load_from_file`].
    ///
    /// # Arguments
    /// * `path` - The path to the file from which to load the state.
    ///
    /// # Returns
    /// A `Result` containing the loaded `InMemory` database or an I/O or deserialization error.
    pub async fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        persistence::load_from_file(path)
    }

    /// Like [`Self::load_from_file`], but returns `Ok(None)` when the file
    /// does not exist instead of falling back to an empty backend. Lets
    /// callers distinguish "no snapshot yet" from "snapshot loaded as
    /// empty" without a separate `path.exists()` round-trip (and the TOCTOU
    /// window that comes with it).
    pub async fn try_load_from_file<P: AsRef<Path>>(path: P) -> Result<Option<Self>> {
        persistence::try_load_from_file(path)
    }

    /// Sort entries by their height within a tree (exposed for testing)
    ///
    /// Heights are stored directly in entries, so this just reads and sorts.
    ///
    /// # Arguments
    /// * `_tree` - The ID of the tree context (unused, kept for API compatibility)
    /// * `entries` - The vector of entries to be sorted in place
    pub fn sort_entries_by_height(&self, _tree: &ID, entries: &mut [Entry]) {
        sorting::sort_entries_by_height(entries)
    }

    /// Sort entries by their height within a subtree (exposed for testing)
    ///
    /// Heights are stored directly in entries, so this just reads and sorts.
    ///
    /// # Arguments
    /// * `_tree` - The ID of the tree context (unused, kept for API compatibility)
    /// * `subtree` - The name of the subtree context
    /// * `entries` - The vector of entries to be sorted in place
    pub fn sort_entries_by_subtree_height(&self, _tree: &ID, subtree: &str, entries: &mut [Entry]) {
        sorting::sort_entries_by_store_height(subtree, entries)
    }

    /// Check if an entry is a tip within its tree (exposed for benchmarks)
    ///
    /// An entry is a tip if no other entry in the same tree lists it as a parent.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree to check within
    /// * `entry_id` - The ID of the entry to check
    ///
    /// # Returns
    /// `true` if the entry is a tip, `false` otherwise
    pub async fn is_tip(&self, tree: &ID, entry_id: &ID) -> bool {
        let inner = self.inner.read().unwrap();
        storage::is_tip(&inner.entries, tree, entry_id)
    }
}

impl Default for InMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BackendImpl for InMemory {
    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .store_state_namespaces
            .iter()
            .find(|(_, namespace)| {
                namespace.ready && !namespace.unlinked && namespace.request == *request
            })
            .map(|(namespace_id, _)| RecordView {
                namespace_id: namespace_id.clone(),
            }))
    }

    async fn begin_store_state_staging(
        &self,
        mut request: StoreStateRequest,
    ) -> Result<StagingToken> {
        if request.lifecycle == StoreStateLifecycle::Staging {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        let target = request.clone();
        request.lifecycle = StoreStateLifecycle::Staging;
        let namespace_id = uuid::Uuid::now_v7().to_string();
        let mut inner = self.inner.write().unwrap();
        inner.staging_tokens.insert(
            namespace_id.clone(),
            MemoryStagingToken {
                target: target.clone(),
                status: StagingStatus::Active,
                last_activity: staging_now(),
                next_sequence: 0,
                last_digest: None,
            },
        );
        inner.store_state_namespaces.insert(
            namespace_id.clone(),
            RecordNamespace {
                request,
                ready: false,
                unlinked: false,
                records: BTreeMap::new(),
            },
        );
        Ok(StagingToken {
            namespace_id,
            target,
        })
    }

    async fn replace_unknown_store_state_staging(
        &self,
        previous: &StagingToken,
    ) -> Result<Option<StagingToken>> {
        let mut inner = self.inner.write().unwrap();
        let now = staging_now();
        if !crate::backend::forgotten_token_is_old(&previous.namespace_id, now)
            || inner.staging_tokens.contains_key(&previous.namespace_id)
            || inner
                .store_state_namespaces
                .values()
                .any(|ns| ns.ready && !ns.unlinked && ns.request == previous.target)
            || inner.staging_tokens.values().any(|state| {
                state.target == previous.target
                    && state.status == StagingStatus::Active
                    && state.last_activity >= now - 300
            })
        {
            return Ok(None);
        }
        let namespace_id = uuid::Uuid::now_v7().to_string();
        let mut staging = previous.target.clone();
        staging.lifecycle = StoreStateLifecycle::Staging;
        inner.staging_tokens.insert(
            namespace_id.clone(),
            MemoryStagingToken {
                target: previous.target.clone(),
                status: StagingStatus::Active,
                last_activity: now,
                next_sequence: 0,
                last_digest: None,
            },
        );
        inner.store_state_namespaces.insert(
            namespace_id.clone(),
            RecordNamespace {
                request: staging,
                ready: false,
                unlinked: false,
                records: BTreeMap::new(),
            },
        );
        Ok(Some(StagingToken {
            namespace_id,
            target: previous.target.clone(),
        }))
    }

    async fn store_state_staging_status(
        &self,
        token: &StagingToken,
    ) -> Result<Option<StagingStatus>> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .staging_tokens
            .get(&token.namespace_id)
            .filter(|state| state.target == token.target)
            .map(|state| state.status.clone()))
    }

    async fn store_state_staging_token(
        &self,
        id: &str,
    ) -> Result<Option<(StagingToken, StagingStatus)>> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .staging_tokens
            .get(id)
            .map(|state| {
                (
                    StagingToken {
                        namespace_id: id.to_owned(),
                        target: state.target.clone(),
                    },
                    state.status.clone(),
                )
            }))
    }

    async fn stage_store_state_chunk(
        &self,
        token: &StagingToken,
        sequence: u64,
        digest: &[u8],
        records: RecordMutations,
    ) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        let state = inner
            .staging_tokens
            .get_mut(&token.namespace_id)
            .filter(|state| {
                state.target == token.target
                    && state.status == StagingStatus::Active
                    && state.last_activity > staging_now() - 300
            })
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if sequence.checked_add(1) == Some(state.next_sequence)
            && state.last_digest.as_deref() == Some(digest)
        {
            return Ok(());
        }
        if sequence != state.next_sequence {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        let next = sequence
            .checked_add(1)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        let namespace = inner
            .store_state_namespaces
            .get_mut(&token.namespace_id)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        namespace.records.extend(records);
        let state = inner.staging_tokens.get_mut(&token.namespace_id).unwrap();
        state.next_sequence = next;
        state.last_digest = Some(digest.to_vec());
        state.last_activity = staging_now();
        Ok(())
    }

    async fn stage_store_state_ordered_chunk(
        &self,
        token: &StagingToken,
        sequence: u64,
        digest: &[u8],
        mutations: Vec<RecordMutation>,
    ) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        let state = inner
            .staging_tokens
            .get(&token.namespace_id)
            .filter(|state| {
                state.target == token.target
                    && state.status == StagingStatus::Active
                    && state.last_activity > staging_now() - 300
            })
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if sequence.checked_add(1) == Some(state.next_sequence)
            && state.last_digest.as_deref() == Some(digest)
        {
            return Ok(());
        }
        if sequence != state.next_sequence {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        let next = sequence
            .checked_add(1)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        let namespace = inner
            .store_state_namespaces
            .get_mut(&token.namespace_id)
            .filter(|ns| !ns.ready && ns.request.lifecycle == StoreStateLifecycle::Staging)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        for mutation in mutations {
            match mutation {
                RecordMutation::Put { key, value } => {
                    namespace.records.insert(key, Some(value));
                }
                RecordMutation::Delete { key } => {
                    namespace.records.remove(&key);
                }
            }
        }
        let state = inner.staging_tokens.get_mut(&token.namespace_id).unwrap();
        state.next_sequence = next;
        state.last_digest = Some(digest.to_vec());
        state.last_activity = staging_now();
        Ok(())
    }

    async fn renew_store_state_staging(&self, token: &StagingToken) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        let state = inner
            .staging_tokens
            .get_mut(&token.namespace_id)
            .filter(|state| {
                state.target == token.target
                    && state.status == StagingStatus::Active
                    && state.last_activity > staging_now() - 300
            })
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        state.last_activity = staging_now();
        Ok(())
    }

    #[cfg(feature = "testing")]
    async fn testing_age_store_state_staging(
        &self,
        token: &StagingToken,
        seconds: i64,
    ) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        let state = inner
            .staging_tokens
            .get_mut(&token.namespace_id)
            .filter(|state| state.target == token.target)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        state.last_activity -= seconds;
        Ok(())
    }

    async fn reclaim_expired_store_state(&self) -> Result<u64> {
        let mut inner = self.inner.write().unwrap();
        let expired: Vec<_> = inner
            .staging_tokens
            .iter()
            .filter(|(_, state)| {
                state.status == StagingStatus::Active && state.last_activity < staging_now() - 600
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            let state = inner.staging_tokens.get_mut(id).unwrap();
            state.status = StagingStatus::Expired;
            state.last_activity = staging_now();
            inner.store_state_namespaces.remove(id);
        }
        let now = staging_now();
        inner.staging_tokens.retain(|_, state| {
            state.status == StagingStatus::Active
                || state.last_activity > now - crate::backend::STAGING_RETENTION_SECS
        });
        Ok(expired.len() as u64)
    }

    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        let state = inner
            .staging_tokens
            .get_mut(&token.namespace_id)
            .filter(|state| {
                state.target == token.target
                    && state.status == StagingStatus::Active
                    && state.last_activity > staging_now() - 300
            })
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        state.last_activity = staging_now();
        let namespace = inner
            .store_state_namespaces
            .get_mut(&token.namespace_id)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if namespace.ready || namespace.request.lifecycle != StoreStateLifecycle::Staging {
            return Err(BackendError::StoreStateNamespaceImmutable.into());
        }
        namespace.records.extend(records);
        Ok(())
    }

    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        let mut inner = self.inner.write().unwrap();
        let state = inner
            .staging_tokens
            .get(&token.namespace_id)
            .filter(|state| state.target == token.target)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        match &state.status {
            StagingStatus::Published(view) | StagingStatus::Adopted(view) => {
                return Ok(view.clone());
            }
            StagingStatus::Active if state.last_activity > staging_now() - 300 => {}
            _ => return Err(BackendError::InvalidStoreStateStagingToken.into()),
        }
        if let Some((winner, _)) = inner
            .store_state_namespaces
            .iter()
            .find(|(_, ns)| ns.ready && !ns.unlinked && ns.request == token.target)
        {
            let view = RecordView {
                namespace_id: winner.clone(),
            };
            inner.store_state_namespaces.remove(&token.namespace_id);
            inner
                .staging_tokens
                .get_mut(&token.namespace_id)
                .unwrap()
                .status = StagingStatus::Adopted(view.clone());
            inner
                .staging_tokens
                .get_mut(&token.namespace_id)
                .unwrap()
                .last_activity = staging_now();
            return Ok(view);
        }
        let namespace = inner
            .store_state_namespaces
            .get_mut(&token.namespace_id)
            .filter(|ns| !ns.ready && ns.request.lifecycle == StoreStateLifecycle::Staging)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if namespace.records.values().any(Option::is_none) {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        namespace.request = token.target;
        namespace.ready = true;
        let view = RecordView {
            namespace_id: token.namespace_id.clone(),
        };
        inner
            .staging_tokens
            .get_mut(&token.namespace_id)
            .unwrap()
            .status = StagingStatus::Published(view.clone());
        inner
            .staging_tokens
            .get_mut(&token.namespace_id)
            .unwrap()
            .last_activity = staging_now();
        Ok(view)
    }

    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        if let Some(state) = inner
            .staging_tokens
            .get_mut(&token.namespace_id)
            .filter(|state| state.target == token.target && state.status == StagingStatus::Active)
        {
            state.status = StagingStatus::Aborted;
            state.last_activity = staging_now();
            inner.store_state_namespaces.remove(&token.namespace_id);
        }
        Ok(())
    }

    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        self.store_state_point_reads.fetch_add(1, Ordering::Relaxed);
        let inner = self.inner.read().unwrap();
        let namespace = inner
            .store_state_namespaces
            .get(&view.namespace_id)
            .filter(|namespace| namespace.ready)
            .ok_or(BackendError::InvalidStoreStateView)?;
        Ok(namespace.records.get(key).and_then(Clone::clone))
    }

    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        let inner = self.inner.read().unwrap();
        let namespace = inner
            .store_state_namespaces
            .get(&view.namespace_id)
            .filter(|namespace| namespace.ready)
            .ok_or(BackendError::InvalidStoreStateView)?;
        if limit == 0 {
            return Ok(RecordPage::default());
        }
        self.store_state_scan_reads.fetch_add(1, Ordering::Relaxed);
        let mut records = namespace
            .records
            .iter()
            .filter(|(key, value)| {
                value.is_some()
                    && range
                        .start
                        .as_deref()
                        .is_none_or(|start| key.as_slice() >= start)
                    && range.end.as_deref().is_none_or(|end| key.as_slice() < end)
                    && after.is_none_or(|after| key.as_slice() > after)
            })
            .take(limit.saturating_add(1))
            .filter_map(|(key, value)| value.clone().map(|value| (key.clone(), value)))
            .collect::<Vec<_>>();
        let has_more = records.len() > limit;
        records.truncate(limit);
        let next = if has_more {
            records.last().map(|record| record.0.clone())
        } else {
            None
        };
        Ok(RecordPage { records, next })
    }

    /// Unlink every ready derived namespace and reclaim the previously
    /// unlinked generation.
    ///
    /// Clearing is two-phase because a reader that already resolved a view
    /// keeps reading through it: unlinking removes the namespace from
    /// resolution, so the next miss rebuilds, while the records stay readable
    /// until the following clear reclaims them. Authoritative namespaces are
    /// never selected.
    async fn clear_derived_store_state(&self) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        inner.store_state_namespaces.retain(|_, namespace| {
            !(namespace.unlinked && namespace.request.lifecycle == StoreStateLifecycle::Derived)
        });
        for namespace in inner.store_state_namespaces.values_mut() {
            if namespace.ready && namespace.request.lifecycle == StoreStateLifecycle::Derived {
                namespace.unlinked = true;
            }
        }
        Ok(())
    }
    /// Retrieves an entry by its unique content-addressable ID.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to retrieve.
    ///
    /// # Returns
    /// A `Result` containing the `Entry` if found, or a `DatabaseError::EntryNotFound` otherwise.
    /// Returns an owned copy to support concurrent access with internal synchronization.
    async fn get(&self, id: &ID) -> Result<Entry> {
        let inner = self.inner.read().unwrap();
        storage::get(&inner, id)
    }

    /// Gets the verification status of an entry.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to check.
    ///
    /// # Returns
    /// A `Result` containing the `VerificationStatus` if the entry exists, or a `DatabaseError::VerificationStatusNotFound` otherwise.
    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        let inner = self.inner.read().unwrap();
        inner
            .verification_status
            .get(id)
            .copied()
            .ok_or_else(|| BackendError::VerificationStatusNotFound { id: id.clone() }.into())
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        // Validate before acquiring write lock to fail fast
        entry.validate()?;
        let mut inner = self.inner.write().unwrap();
        storage::put(&mut inner, entry)
    }

    /// Updates the verification status of an existing entry.
    ///
    /// This allows the authentication system to mark entries as verified or failed
    /// after they have been stored. Useful for batch verification operations.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to update
    /// * `verification_status` - The new verification status
    ///
    /// # Returns
    /// A `Result` indicating success or `DatabaseError::EntryNotFound` if the entry doesn't exist.
    async fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        if inner.verification_status.contains_key(id) {
            inner
                .verification_status
                .insert(id.clone(), verification_status);
            Ok(())
        } else {
            Err(BackendError::EntryNotFound { id: id.clone() }.into())
        }
    }

    /// Gets all entries with a specific verification status.
    ///
    /// This is useful for finding unverified entries that need authentication
    /// or for security audits.
    ///
    /// # Arguments
    /// * `status` - The verification status to filter by
    ///
    /// # Returns
    /// A `Result` containing a vector of entry IDs with the specified status.
    async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        let ids = inner
            .verification_status
            .iter()
            .filter(|&(_, entry_status)| *entry_status == status)
            .map(|(id, _)| id.clone())
            .collect();
        Ok(ids)
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        // Fast path: check cache with read lock
        {
            let inner = self.inner.read().unwrap();
            if let Some(cache) = inner.tips.get(tree) {
                return Ok(Snapshot::new(cache.tree_tips.iter().cloned().collect()));
            }
        }
        // Slow path: compute and cache with write lock
        let mut inner = self.inner.write().unwrap();
        traversal::snapshot(&mut inner, tree).map(Snapshot::new)
    }

    async fn store_snapshot(&self, tree: &ID, subtree: &str) -> Result<Snapshot> {
        // Fast path: check cache with read lock
        {
            let inner = self.inner.read().unwrap();
            if let Some(cache) = inner.tips.get(tree)
                && let Some(subtree_tips) = cache.subtree_tips.get(subtree)
            {
                return Ok(Snapshot::new(subtree_tips.iter().cloned().collect()));
            }
        }
        // Slow path: compute and cache with write lock
        let mut inner = self.inner.write().unwrap();
        traversal::store_snapshot(&mut inner, tree, subtree).map(Snapshot::new)
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        subtree: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        let mut inner = self.inner.write().unwrap();
        traversal::store_snapshot_at(&mut inner, tree, subtree, main_snapshot.tips())
            .map(Snapshot::new)
    }

    /// Retrieves the IDs of all top-level root entries stored in the database.
    ///
    /// Top-level roots are entries that are themselves roots of a tree
    /// (i.e., `entry.is_root()` is true) and are not part of a larger tree structure
    /// tracked by the backend. These represent the starting points
    /// of distinct trees managed by the database.
    ///
    /// # Returns
    /// A `Result` containing a vector of top-level root entry IDs or an error.
    async fn all_roots(&self) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        let roots: Vec<ID> = inner
            .entries
            .values()
            .filter(|entry| entry.is_root())
            .map(|entry| entry.id())
            .collect();
        Ok(roots)
    }

    async fn find_merge_base(
        &self,
        tree: &ID,
        subtree: &str,
        entry_ids: &[ID],
    ) -> Result<Option<ID>> {
        let inner = self.inner.read().unwrap();
        traversal::find_merge_base(&inner, tree, subtree, entry_ids)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        let inner = self.inner.read().unwrap();
        storage::get_tree(&inner, tree)
    }

    async fn get_store(&self, tree: &ID, subtree: &str) -> Result<Vec<Entry>> {
        let inner = self.inner.read().unwrap();
        storage::get_store(&inner, tree, subtree)
    }

    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        let inner = self.inner.read().unwrap();
        storage::get_tree_from_tips(&inner, tree, tips)
    }

    async fn store_at(&self, tree: &ID, subtree: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        #[cfg(feature = "testing")]
        self.store_history_reads.fetch_add(1, Ordering::Relaxed);
        let inner = self.inner.read().unwrap();
        storage::store_at(&inner, tree, subtree, snapshot.tips())
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        let inner = self.inner.read().unwrap();
        Ok(inner.instance_metadata.clone())
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        inner.instance_metadata = Some(metadata.clone());
        Ok(())
    }

    async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        let inner = self.inner.read().unwrap();
        Ok(inner.instance_secrets.clone())
    }

    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        inner.instance_secrets = Some(secrets.clone());
        Ok(())
    }

    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        subtree: &str,
    ) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        traversal::get_sorted_store_parents(&inner, tree_id, entry_id, subtree)
    }

    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        subtree: &str,
        from_id: Option<&ID>,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        traversal::get_path_from_to(&inner, tree_id, subtree, from_id, to_ids)
    }
}

/// A publish carrying a token whose target does not match the namespace it
/// names must never disturb a ready namespace.
///
/// `StagingToken` is `Clone` with crate-visible fields, so a caller can hold
/// a token for one target while naming another namespace (or vice versa).
/// A mismatched token is rejected without changing either namespace.
#[cfg(test)]
mod store_state_token_tests {
    use std::collections::BTreeMap;

    use super::InMemory;
    use crate::backend::{
        BackendImpl, CacheScope, ProjectionDescriptor, StagingToken, StoreStateLifecycle,
        StoreStateRequest,
    };
    use crate::entry::ID;

    fn request(store: &str) -> StoreStateRequest {
        StoreStateRequest {
            database: ID::from_bytes("db"),
            store: store.to_string(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: ProjectionDescriptor {
                name: "test/opaque".to_string(),
                version: 0,
            },
            source_key: b"snapshot".to_vec(),
        }
    }

    #[tokio::test]
    async fn mismatched_target_publish_preserves_ready_namespace() {
        let backend = InMemory::new();

        // Ready namespace for target A.
        let request_a = request("store-a");
        let token_a = backend
            .begin_store_state_staging(request_a.clone())
            .await
            .unwrap();
        backend
            .stage_store_state_records(
                &token_a,
                BTreeMap::from([(b"key".to_vec(), Some(b"value-a".to_vec()))]),
            )
            .await
            .unwrap();
        let view_a = backend.publish_store_state(token_a).await.unwrap();

        // Staging namespace for target B.
        let request_b = request("store-b");
        let token_b = backend
            .begin_store_state_staging(request_b.clone())
            .await
            .unwrap();
        backend
            .stage_store_state_records(
                &token_b,
                BTreeMap::from([(b"key".to_vec(), Some(b"value-b".to_vec()))]),
            )
            .await
            .unwrap();

        // A mismatched target cannot publish or adopt a different build.
        let bad = StagingToken {
            namespace_id: token_b.namespace_id.clone(),
            target: request_a.clone(),
        };
        assert!(backend.publish_store_state(bad).await.is_err());
        assert_eq!(
            backend
                .store_state_record_get(&view_a, b"key")
                .await
                .unwrap(),
            Some(b"value-a".to_vec())
        );
        assert_eq!(backend.resolve_store_state(&request_b).await.unwrap(), None);

        // Malformed clone: A's (ready) namespace id with a target that
        // resolves nowhere. The publish fails and the ready namespace is
        // re-inserted with its records intact.
        let nowhere = request("store-nowhere");
        let bad_ready = StagingToken {
            namespace_id: view_a.namespace_id.clone(),
            target: nowhere.clone(),
        };
        assert!(backend.publish_store_state(bad_ready).await.is_err());
        assert_eq!(
            backend.resolve_store_state(&request_a).await.unwrap(),
            Some(view_a.clone())
        );
        assert_eq!(
            backend
                .store_state_record_get(&view_a, b"key")
                .await
                .unwrap(),
            Some(b"value-a".to_vec())
        );
        assert_eq!(backend.resolve_store_state(&nowhere).await.unwrap(), None);
    }
}
