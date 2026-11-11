//! Independent primary and derived-cache storage.
//!
//! [`SplitBackend`] composes any primary [`BackendImpl`] with any
//! [`DerivedCacheStorage`]. Entries, verification, instance metadata/secrets,
//! local credentials and Authoritative Store state stay in the primary. Only
//! disposable Derived Store state and its private builds use the cache. Each
//! engine keeps its own resolve/stage/publish/read/scan/clear and
//! budget/eviction implementation, dialect, schema and ownership; the split
//! only routes requests and opaque handles.

use std::any::Any;

use async_trait::async_trait;

use crate::{
    Result,
    backend::{
        BackendImpl, InstanceMetadata, InstanceSecrets, Reachability, RecordMutations, RecordPage,
        RecordRange, RecordView, StagingToken, StoreStateLifecycle, StoreStateRequest,
        VerificationRepair, VerificationStatus, errors::BackendError,
    },
    entry::{Entry, ID},
    snapshot::Snapshot,
};

/// Storage for disposable Derived Store state, selectable independently of
/// the primary backend.
///
/// The methods have the same contracts as their [`BackendImpl`] namesakes,
/// including atomic publication, same-target adoption, policy admission and
/// eviction, live-view reads after unlink/eviction, and two-phase clear. An
/// implementation receives only [`StoreStateLifecycle::Derived`] targets and
/// owns its handles; it holds no authoritative or verification state.
#[async_trait]
pub trait DerivedCacheStorage: Send + Sync {
    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>>;
    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken>;
    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()>;
    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView>;
    async fn abort_store_state(&self, token: StagingToken) -> Result<()>;
    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>>;
    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage>;
    /// Online two-phase clear: unlink published state, reclaim the previous
    /// unlinked generation.
    async fn clear_derived_store_state(&self) -> Result<()>;
    /// Offline discard of every Derived namespace and build, for trust reset.
    /// Requires no live readers or writers.
    async fn reset_derived_store_state(&self) -> Result<()>;
}

/// Implement [`DerivedCacheStorage`] by delegating to a dedicated backend
/// whose only populated state is Derived. `reset_derived_store_state` uses its
/// offline reset, which also discards every disposable namespace.
macro_rules! derived_cache_storage_via {
    ($ty:ty, |$this:ident| $inner:expr) => {
        #[async_trait::async_trait]
        impl $crate::backend::DerivedCacheStorage for $ty {
            async fn resolve_store_state(
                &self,
                request: &$crate::backend::StoreStateRequest,
            ) -> $crate::Result<Option<$crate::backend::RecordView>> {
                let $this = self;
                $crate::backend::BackendImpl::resolve_store_state($inner, request).await
            }
            async fn begin_store_state_staging(
                &self,
                request: $crate::backend::StoreStateRequest,
            ) -> $crate::Result<$crate::backend::StagingToken> {
                let $this = self;
                $crate::backend::BackendImpl::begin_store_state_staging($inner, request).await
            }
            async fn stage_store_state_records(
                &self,
                token: &$crate::backend::StagingToken,
                records: $crate::backend::RecordMutations,
            ) -> $crate::Result<()> {
                let $this = self;
                $crate::backend::BackendImpl::stage_store_state_records($inner, token, records)
                    .await
            }
            async fn publish_store_state(
                &self,
                token: $crate::backend::StagingToken,
            ) -> $crate::Result<$crate::backend::RecordView> {
                let $this = self;
                $crate::backend::BackendImpl::publish_store_state($inner, token).await
            }
            async fn abort_store_state(
                &self,
                token: $crate::backend::StagingToken,
            ) -> $crate::Result<()> {
                let $this = self;
                $crate::backend::BackendImpl::abort_store_state($inner, token).await
            }
            async fn store_state_record_get(
                &self,
                view: &$crate::backend::RecordView,
                key: &[u8],
            ) -> $crate::Result<Option<Vec<u8>>> {
                let $this = self;
                $crate::backend::BackendImpl::store_state_record_get($inner, view, key).await
            }
            async fn store_state_record_scan(
                &self,
                view: &$crate::backend::RecordView,
                range: &$crate::backend::RecordRange,
                after: Option<&[u8]>,
                limit: usize,
            ) -> $crate::Result<$crate::backend::RecordPage> {
                let $this = self;
                $crate::backend::BackendImpl::store_state_record_scan(
                    $inner, view, range, after, limit,
                )
                .await
            }
            async fn clear_derived_store_state(&self) -> $crate::Result<()> {
                let $this = self;
                $crate::backend::BackendImpl::clear_derived_store_state($inner).await
            }
            async fn reset_derived_store_state(&self) -> $crate::Result<()> {
                let $this = self;
                $crate::backend::BackendImpl::reset_local_verification($inner).await
            }
        }
    };
}
pub(crate) use derived_cache_storage_via;

// A dedicated InMemory handle serves as an in-memory cache engine.
derived_cache_storage_via!(crate::backend::database::InMemory, |this| this);

/// Owner tags keep cache and primary handles distinct, whatever identifiers
/// each engine mints, so every view or token returns to the storage that
/// issued it.
const CACHE_TAG: &str = "cache:";
const PRIMARY_TAG: &str = "primary:";

enum Owner {
    Cache,
    Primary,
}

fn tag(owner: Owner, namespace_id: String) -> String {
    match owner {
        Owner::Cache => format!("{CACHE_TAG}{namespace_id}"),
        Owner::Primary => format!("{PRIMARY_TAG}{namespace_id}"),
    }
}

fn untag(namespace_id: &str) -> Option<(Owner, String)> {
    if let Some(inner) = namespace_id.strip_prefix(CACHE_TAG) {
        Some((Owner::Cache, inner.to_owned()))
    } else {
        namespace_id
            .strip_prefix(PRIMARY_TAG)
            .map(|inner| (Owner::Primary, inner.to_owned()))
    }
}

fn tag_view(owner: Owner, view: RecordView) -> RecordView {
    RecordView {
        namespace_id: tag(owner, view.namespace_id),
    }
}

fn tag_token(owner: Owner, token: StagingToken) -> StagingToken {
    StagingToken {
        namespace_id: tag(owner, token.namespace_id),
        target: token.target,
    }
}

fn untag_view(view: &RecordView) -> Result<(Owner, RecordView)> {
    let (owner, namespace_id) =
        untag(&view.namespace_id).ok_or(BackendError::InvalidStoreStateView)?;
    Ok((owner, RecordView { namespace_id }))
}

/// A token's owner must agree with its target lifecycle.
fn untag_token(token: &StagingToken) -> Result<(Owner, StagingToken)> {
    let (owner, namespace_id) =
        untag(&token.namespace_id).ok_or(BackendError::InvalidStoreStateStagingToken)?;
    let derived = token.target.lifecycle == StoreStateLifecycle::Derived;
    if derived != matches!(owner, Owner::Cache) {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    Ok((
        owner,
        StagingToken {
            namespace_id,
            target: token.target.clone(),
        },
    ))
}

/// A primary backend with a separately selected Derived-state cache.
///
/// Every operation except Derived Store state goes to the primary. Derived
/// resolution, builds, reads, clear and budget/eviction go to the cache, so the
/// cache's dialect, schema, location and bounds are independent of the
/// primary's. Opaque [`RecordView`]s and [`StagingToken`]s carry their owner
/// and are refused if presented to the wrong storage.
///
/// The primary's own Derived storage is not used; open it with retention
/// disabled (PostgreSQL then reclaims leftover Derived rows on open). Offline trust reset discards
/// the cache before resetting the primary, so a failure can leave a cold cache
/// but never a successful reset with old cached state.
///
/// Both stores must be exclusively owned by this handle. A persistent cache
/// requires a persistent primary and must not be shared between instances; see
/// the backend guide. [`as_any`](BackendImpl::as_any) exposes the primary so
/// engine-specific inspection and persistence keep working.
pub struct SplitBackend {
    primary: Box<dyn BackendImpl>,
    cache: Box<dyn DerivedCacheStorage>,
}

impl SplitBackend {
    pub fn new(primary: Box<dyn BackendImpl>, cache: Box<dyn DerivedCacheStorage>) -> Self {
        Self { primary, cache }
    }

    /// The primary backend.
    pub fn primary(&self) -> &dyn BackendImpl {
        &*self.primary
    }
}

#[async_trait]
impl BackendImpl for SplitBackend {
    async fn local_account_record(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.primary.local_account_record(name).await
    }

    async fn compare_exchange_local_account_record(
        &self,
        name: &str,
        expected: Option<&[u8]>,
        replacement: &[u8],
    ) -> Result<bool> {
        self.primary
            .compare_exchange_local_account_record(name, expected, replacement)
            .await
    }

    async fn local_account_names(&self) -> Result<Vec<String>> {
        self.primary.local_account_names().await
    }

    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        if request.lifecycle == StoreStateLifecycle::Derived {
            let view = self.cache.resolve_store_state(request).await?;
            Ok(view.map(|view| tag_view(Owner::Cache, view)))
        } else {
            let view = self.primary.resolve_store_state(request).await?;
            Ok(view.map(|view| tag_view(Owner::Primary, view)))
        }
    }

    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken> {
        if request.lifecycle == StoreStateLifecycle::Derived {
            let token = self.cache.begin_store_state_staging(request).await?;
            Ok(tag_token(Owner::Cache, token))
        } else {
            let token = self.primary.begin_store_state_staging(request).await?;
            Ok(tag_token(Owner::Primary, token))
        }
    }

    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        match untag_token(token)? {
            (Owner::Cache, token) => self.cache.stage_store_state_records(&token, records).await,
            (Owner::Primary, token) => {
                self.primary
                    .stage_store_state_records(&token, records)
                    .await
            }
        }
    }

    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        match untag_token(&token)? {
            (Owner::Cache, token) => {
                let view = self.cache.publish_store_state(token).await?;
                Ok(tag_view(Owner::Cache, view))
            }
            (Owner::Primary, token) => {
                let view = self.primary.publish_store_state(token).await?;
                Ok(tag_view(Owner::Primary, view))
            }
        }
    }

    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        match untag_token(&token)? {
            (Owner::Cache, token) => self.cache.abort_store_state(token).await,
            (Owner::Primary, token) => self.primary.abort_store_state(token).await,
        }
    }

    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        match untag_view(view)? {
            (Owner::Cache, view) => self.cache.store_state_record_get(&view, key).await,
            (Owner::Primary, view) => self.primary.store_state_record_get(&view, key).await,
        }
    }

    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        match untag_view(view)? {
            (Owner::Cache, view) => {
                self.cache
                    .store_state_record_scan(&view, range, after, limit)
                    .await
            }
            (Owner::Primary, view) => {
                self.primary
                    .store_state_record_scan(&view, range, after, limit)
                    .await
            }
        }
    }

    async fn clear_derived_store_state(&self) -> Result<()> {
        self.cache.clear_derived_store_state().await
    }

    /// Cache first: interruption can lose cached values, never keep them
    /// across a successful reset. The primary reset then resets trust labels
    /// and frontiers together and discards its own disposable state.
    async fn reset_local_verification(&self) -> Result<()> {
        self.cache.reset_derived_store_state().await?;
        self.primary.reset_local_verification().await
    }

    async fn get(&self, id: &ID) -> Result<Entry> {
        self.primary.get(id).await
    }

    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        self.primary.get_verification_status(id).await
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        self.primary.put(entry).await
    }

    async fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> Result<()> {
        self.primary
            .update_verification_status(id, verification_status)
            .await
    }

    async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        self.primary
            .get_entries_by_verification_status(status)
            .await
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        self.primary.snapshot(tree).await
    }

    async fn verified_snapshot(&self, tree: &ID) -> Result<Snapshot> {
        self.primary.verified_snapshot(tree).await
    }

    async fn repair_verification(&self, tree: &ID) -> Result<VerificationRepair> {
        self.primary.repair_verification(tree).await
    }

    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        self.primary.store_snapshot(tree, store).await
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        self.primary
            .store_snapshot_at(tree, store, main_snapshot)
            .await
    }

    async fn all_roots(&self) -> Result<Vec<ID>> {
        self.primary.all_roots().await
    }

    async fn find_merge_base(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<Option<ID>> {
        self.primary.find_merge_base(tree, store, entry_ids).await
    }

    fn as_any(&self) -> &dyn Any {
        self.primary.as_any()
    }

    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        self.primary.get_tree(tree).await
    }

    async fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>> {
        self.primary.get_store(tree, store).await
    }

    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        self.primary.get_tree_from_tips(tree, tips).await
    }

    async fn check_targets_reachable_from(
        &self,
        tree: &ID,
        from: &[ID],
        targets: &[ID],
    ) -> Result<Reachability> {
        self.primary
            .check_targets_reachable_from(tree, from, targets)
            .await
    }

    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.primary.store_at(tree, store, snapshot).await
    }

    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        self.primary
            .get_sorted_store_parents(tree_id, entry_id, store)
            .await
    }

    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: Option<&ID>,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        self.primary
            .get_path_from_to(tree_id, store, from_id, to_ids)
            .await
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.primary.get_instance_metadata().await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.primary.set_instance_metadata(metadata).await
    }

    async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        self.primary.get_instance_secrets().await
    }

    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()> {
        self.primary.set_instance_secrets(secrets).await
    }
}

#[cfg(all(test, feature = "sqlite", feature = "testing"))]
mod tests;
