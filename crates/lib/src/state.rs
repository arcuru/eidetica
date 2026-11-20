//! Snapshot-bound State computation for embedded and serving engines.
//!
//! The caller holds the signing key or the adapter proves its possession before
//! entering this module. Credentials, current database Read, and admission to
//! computation are independent. The returned boundary is not a lease or a grant.

use crate::{
    Database, ID, Instance, Registered, Result, Snapshot,
    auth::{SigKey, crypto::PublicKey, validation::permissions::resolve_identity_permission},
    backend::{BackendImpl, Reachability},
    constants::{INDEX, SETTINGS},
    crdt::{CRDT, Codec, Doc, doc::Value},
    database::DatabaseKey,
    store::{DocStore, Table},
};
use serde::{Deserialize, Serialize};

/// Verification view of a database-root boundary, separate from the tip-set value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReadScope {
    /// The retained, ancestor-closed Verified view of the serving engine.
    #[default]
    Verified,
    /// Diagnostic raw frontier with Failed tips removed, not a claim about ancestors.
    AllowUnverified,
}

/// A database-root Snapshot and its read posture. Neither is an authorization grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadContext {
    database: ID,
    snapshot: Snapshot,
    scope: ReadScope,
}

impl ReadContext {
    /// Describe a requested historical root boundary. The serving operation
    /// checks it; a Store's own frontier is not a root-boundary input.
    pub fn at(database: ID, snapshot: Snapshot, scope: ReadScope) -> Self {
        Self {
            database,
            snapshot,
            scope,
        }
    }

    pub fn database(&self) -> &ID {
        &self.database
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn scope(&self) -> ReadScope {
        self.scope
    }
}

/// A complete Doc projection computed by the serving engine, at the exact
/// context carried with the value. This is not evidence of independent history
/// validation by the consumer. A remote adapter must also bind its producer
/// identity before a consumer can make any trust decision.
#[derive(Debug)]
pub struct ComputedDoc {
    pub context: ReadContext,
    pub store: String,
    pub value: Doc,
}

impl ComputedDoc {
    /// The built-in whole-Doc projection. This is not a typed row projection.
    pub const PROJECTION: &str = "eidetica/doc:v0";
}

/// Failure to acquire a serving boundary or complete a State computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StateError {
    /// This engine declined the work. No authorization or existence is implied.
    #[error("State computation refused")]
    Refused,
    #[error("State read permission denied")]
    Denied,
    #[error("Invalid State read boundary")]
    InvalidBoundary,
    #[error("State history unavailable at the requested boundary")]
    UnavailableHistory,
    #[error("State projection is not supported for this store or read scope")]
    UnsupportedProjection,
}

impl From<StateError> for crate::Error {
    fn from(value: StateError) -> Self {
        Self::State(value)
    }
}

/// Work being considered by the engine, before any protected result is produced.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum StateOperation<'a> {
    Capture,
    ReadDoc { store: &'a str },
}

/// A serving engine's optional, nonblocking admission decision. A rejection can
/// be for any reason and does not create an enrollment or change database Auth.
pub type StateAdmission = dyn Fn(&ID, StateOperation<'_>) -> bool + Send + Sync;

/// State reads under one caller identity. Re-checks current database Read on
/// every operation, including historical reads. It grants no Write authority.
pub struct StateReader<'a> {
    instance: &'a Instance,
    pubkey: PublicKey,
    identity: &'a SigKey,
    admission: Option<&'a StateAdmission>,
}

impl<'a> StateReader<'a> {
    /// Read from an embedded engine using a caller-held key. No credential
    /// bundle is consulted. Remote clients use their service adapter instead.
    pub fn new(instance: &'a Instance, key: &'a DatabaseKey) -> Self {
        Self::from_proven_signer(instance, key.public_key(), key.identity())
    }

    /// Only call this after an adapter has authenticated this exact key.
    pub(crate) fn from_proven_signer(
        instance: &'a Instance,
        pubkey: PublicKey,
        identity: &'a SigKey,
    ) -> Self {
        Self {
            instance,
            pubkey,
            identity,
            admission: None,
        }
    }

    /// Use the serving engine's independent State admission decision.
    pub fn with_admission(mut self, admission: Option<&'a StateAdmission>) -> Self {
        self.admission = admission;
        self
    }

    /// Authorize Read and capture one root cut without running verification or
    /// preparing a write. Only the authority check needs Settings. No registry,
    /// application Store, or parent-height bundle is read by this operation.
    /// To stage writes locally, pass the captured snapshot to
    /// [`Database::new_transaction_at`]; staging cannot alter the cut or the
    /// pre-write authorization enforced when signing/committing.
    pub async fn capture(&self, database: &ID, scope: ReadScope) -> Result<ReadContext> {
        self.admit(database, StateOperation::Capture)?;
        let (db, verified) = self.authorize(database).await?;
        let snapshot = match scope {
            ReadScope::Verified => verified,
            ReadScope::AllowUnverified => {
                let raw = db.ops().snapshot(database).await?;
                db.drop_failed(raw.tips()).await?
            }
        };
        if snapshot.is_empty() {
            return Err(StateError::UnavailableHistory.into());
        }
        Ok(ReadContext::at(database.clone(), snapshot, scope))
    }

    /// Materialize one supported plaintext Store at the explicit Verified cut.
    /// Diagnostic boundaries and encrypted or custom projections do not use
    /// this method; authorized raw history can be consumed separately.
    pub async fn read_doc(&self, context: &ReadContext, store: &str) -> Result<ComputedDoc> {
        self.admit(context.database(), StateOperation::ReadDoc { store })?;
        let (_, verified) = self.authorize(context.database()).await?;
        let backend = self.instance.require_local_engine()?;
        if context.scope() != ReadScope::Verified {
            return Err(StateError::UnsupportedProjection.into());
        }
        if context.snapshot().is_empty() {
            return Err(StateError::InvalidBoundary.into());
        }
        if context.snapshot() != &verified {
            require_reachable(
                &*backend,
                context.database(),
                &verified,
                context.snapshot().tips(),
            )
            .await?;
        }
        compute_doc(&*backend, context, store).await
    }

    // Keep the legacy JSON adapter's implicit-latest shape out of new State
    // operations. Use one authorization and capture, then the pinned compute.
    #[cfg(all(unix, feature = "service"))]
    pub(crate) async fn read_current_doc(&self, database: &ID, store: &str) -> Result<ComputedDoc> {
        self.admit(database, StateOperation::ReadDoc { store })?;
        let (_, verified) = self.authorize(database).await?;
        let context = ReadContext::at(database.clone(), verified, ReadScope::Verified);
        let backend = self.instance.require_local_engine()?;
        compute_doc(&*backend, &context, store).await
    }

    fn admit(&self, database: &ID, op: StateOperation<'_>) -> Result<()> {
        if self.admission.is_some_and(|admit| !admit(database, op)) {
            return Err(StateError::Refused.into());
        }
        Ok(())
    }

    async fn authorize(&self, database: &ID) -> Result<(Database, Snapshot)> {
        let backend = self.instance.require_local_engine()?;
        let verified = backend.verified_snapshot(database).await?;
        if verified.is_empty() {
            return Err(StateError::Denied.into());
        }
        let db = Database::open(self.instance, database)
            .await
            .map_err(|_| StateError::Denied)?;
        let settings = db
            .auth_settings_at(&verified)
            .await
            .map_err(|_| StateError::Denied)?;
        // All resolved Permission variants grant at least Read. Delegations
        // resolve against current accepted authority, not the requested cut.
        resolve_identity_permission(&self.pubkey, self.identity, &settings, Some(self.instance))
            .await
            .map_err(|_| StateError::Denied)?;
        Ok((db, verified))
    }
}

async fn require_reachable(
    backend: &dyn BackendImpl,
    database: &ID,
    from: &Snapshot,
    targets: &[ID],
) -> Result<()> {
    match backend
        .check_targets_reachable_from(database, from.tips(), targets)
        .await
    {
        Ok(Reachability::Reachable) => Ok(()),
        Ok(Reachability::Unreachable) => Err(StateError::InvalidBoundary.into()),
        Ok(Reachability::Indeterminate { .. }) => Err(StateError::UnavailableHistory.into()),
        Err(crate::Error::Backend(error))
            if matches!(
                error.as_ref(),
                crate::backend::BackendError::EntryNotInTree { .. }
            ) =>
        {
            Err(StateError::InvalidBoundary.into())
        }
        Err(err) => Err(err),
    }
}

async fn compute_doc(
    backend: &dyn BackendImpl,
    context: &ReadContext,
    store: &str,
) -> Result<ComputedDoc> {
    if !matches!(store, INDEX | SETTINGS) {
        if store.is_empty() {
            return Err(StateError::UnsupportedProjection.into());
        }
        let index = fold_doc(backend, context, INDEX).await?;
        let declared_type = index
            .get(store)
            .and_then(Value::as_doc)
            .and_then(|metadata| metadata.get("type"))
            .and_then(Value::as_text);
        if !matches!(declared_type, Some(t) if t == DocStore::type_id() || t == Table::<()>::type_id())
        {
            return Err(StateError::UnsupportedProjection.into());
        }
    }
    let value = fold_doc(backend, context, store).await?;
    Ok(ComputedDoc {
        context: context.clone(),
        store: store.to_owned(),
        value,
    })
}

/// Fold signed deltas from just this Store. Do not consult owner-scoped
/// uploaded projections or decode ciphertext under a caller-provided codec.
async fn fold_doc(backend: &dyn BackendImpl, context: &ReadContext, store: &str) -> Result<Doc> {
    let database = context.database();
    let tips = backend
        .store_snapshot_at(database, store, context.snapshot())
        .await?;
    let entries = backend.store_at(database, store, &tips).await?;
    let mut ids: Vec<_> = entries.iter().map(crate::Entry::id).collect();
    ids.sort_unstable();
    // Store links are not the main DAG: do not fold a missing link as empty or
    // follow a link outside the captured root history, even on a Verified cut.
    if !tips.tips().iter().all(|id| ids.binary_search(id).is_ok()) {
        return Err(StateError::UnavailableHistory.into());
    }
    require_reachable(backend, database, context.snapshot(), &ids).await?;
    let mut value = Doc::new();
    for entry in entries {
        if !entry.in_tree(database) || !entry.in_subtree(store) {
            return Err(StateError::InvalidBoundary.into());
        }
        if !entry
            .subtree_parents(store)?
            .iter()
            .all(|id| ids.binary_search(id).is_ok())
        {
            return Err(StateError::UnavailableHistory.into());
        }
        if let Ok(data) = entry.data(store) {
            value = value.merge(&Doc::decode(data)?)?;
        }
    }
    Ok(value)
}
