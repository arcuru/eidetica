//! Snapshot-bound State computation for embedded and serving engines.
//!
//! The caller holds the signing key or the adapter proves its possession before
//! entering this module. Credentials, database Read, and admission to
//! computation are independent. The returned boundary is not a lease or a grant.

pub(crate) mod work;

use crate::{
    Database, ID, Instance, Registered, Result, Snapshot,
    auth::{
        AuthSettings, SigKey, crypto::PublicKey,
        validation::permissions::resolve_state_identity_permission,
    },
    backend::{BackendImpl, Reachability},
    constants::{INDEX, SETTINGS},
    crdt::{CRDT, Codec, Doc, doc::Value},
    database::DatabaseKey,
    store::{DocStore, Table},
};
use serde::{Deserialize, Serialize};
use work::ReadWork;

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
    /// The request exceeded an admission bound (work, bytes, time or fanout).
    #[error("State resource limit exceeded")]
    Limit,
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

/// State reads under one caller identity. Discovering the latest boundary needs
/// current Read. A requested Verified cut can also be read with Read at that cut.
/// Neither operation grants Write authority.
pub struct StateReader<'a> {
    instance: &'a Instance,
    pubkey: PublicKey,
    identity: &'a SigKey,
    admission: Option<&'a StateAdmission>,
    work: Option<&'a ReadWork>,
}

struct VerifiedDatabase {
    at: Snapshot,
    // The embedded/legacy adapter keeps its existing transaction auth path.
    legacy: Option<Database>,
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
            work: None,
        }
    }

    /// Use the serving engine's independent State admission decision.
    pub fn with_admission(mut self, admission: Option<&'a StateAdmission>) -> Self {
        self.admission = admission;
        self
    }

    #[cfg(all(unix, feature = "service"))]
    pub(crate) fn with_work(mut self, work: &'a ReadWork) -> Self {
        self.work = Some(work);
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
        let verified = self.authorize(database).await?;
        let snapshot = match scope {
            ReadScope::Verified => verified.at,
            ReadScope::AllowUnverified => {
                let db = verified.legacy.ok_or(StateError::UnsupportedProjection)?;
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
    /// Require Read at that cut or the serving engine's latest Verified frontier.
    /// Diagnostic boundaries, encrypted Stores and custom projections are not
    /// supported; authorized raw history can be consumed separately.
    pub async fn read_doc(&self, context: &ReadContext, store: &str) -> Result<ComputedDoc> {
        self.admit(context.database(), StateOperation::ReadDoc { store })?;
        let backend = self.instance.require_local_engine()?;
        let verified = self.verified_database(context.database()).await?;
        let current_read = match self.authorize_current(context.database(), &verified).await {
            Err(error) if is_admission_failure(&error) => return Err(error),
            result => result,
        };
        if context.scope() != ReadScope::Verified {
            current_read?;
            return Err(StateError::UnsupportedProjection.into());
        }

        let boundary = if context.snapshot().is_empty() {
            Err(StateError::InvalidBoundary.into())
        } else if context.snapshot() != &verified.at {
            self.require_reachable(
                &*backend,
                context.database(),
                &verified.at,
                context.snapshot().tips(),
            )
            .await
        } else {
            Ok(())
        };
        if current_read.is_ok() {
            boundary?;
        } else {
            // Without current Read, don't report boundary details until the
            // requested cut has itself been validated and authorized.
            boundary.map_err(deny_except_admission)?;
            let settings = self
                .fold(&*backend, context, SETTINGS)
                .await
                .map_err(deny_except_admission)?;
            self.authorize_settings(&auth_from_doc(&settings), true)
                .await?;
            if store == SETTINGS {
                return Ok(ComputedDoc {
                    context: context.clone(),
                    store: store.to_owned(),
                    value: settings,
                });
            }
        }
        self.compute_doc(&*backend, context, store).await
    }

    // Keep the legacy JSON adapter's implicit-latest shape out of new State
    // operations. Use one authorization and capture, then the pinned compute.
    #[cfg(all(unix, feature = "service"))]
    pub(crate) async fn read_current_doc(&self, database: &ID, store: &str) -> Result<ComputedDoc> {
        self.admit(database, StateOperation::ReadDoc { store })?;
        let verified = self.authorize(database).await?;
        let context = ReadContext::at(database.clone(), verified.at, ReadScope::Verified);
        let backend = self.instance.require_local_engine()?;
        self.compute_doc(&*backend, &context, store).await
    }

    pub(crate) fn admit(&self, database: &ID, op: StateOperation<'_>) -> Result<()> {
        if self.admission.is_some_and(|admit| !admit(database, op)) {
            return Err(StateError::Refused.into());
        }
        Ok(())
    }

    async fn authorize(&self, database: &ID) -> Result<VerifiedDatabase> {
        let verified = self.verified_database(database).await?;
        self.authorize_current(database, &verified).await?;
        Ok(verified)
    }

    async fn verified_database(&self, database: &ID) -> Result<VerifiedDatabase> {
        let backend = self.instance.require_local_engine()?;
        let at = if let Some(work) = self.work {
            work.verified(&*backend, database).await?
        } else {
            backend.verified_snapshot(database).await?
        };
        if at.is_empty() {
            return Err(StateError::Denied.into());
        }
        let legacy = if let Some(work) = self.work {
            let root = work
                .entry(&*backend, database)
                .await
                .map_err(deny_except_admission)?;
            if root.id_ref() != database || !root.is_root() {
                return Err(StateError::Denied.into());
            }
            None
        } else {
            Some(
                Database::open(self.instance, database)
                    .await
                    .map_err(|_| StateError::Denied)?,
            )
        };
        Ok(VerifiedDatabase { at, legacy })
    }

    async fn authorize_current(&self, database: &ID, verified: &VerifiedDatabase) -> Result<()> {
        let settings = if let Some(work) = self.work {
            let backend = self.instance.require_local_engine()?;
            auth_from_doc(
                &work
                    .fold(&*backend, database, &verified.at, SETTINGS, false)
                    .await
                    .map_err(deny_except_admission)?,
            )
        } else {
            verified
                .legacy
                .as_ref()
                .ok_or(StateError::Denied)?
                .auth_settings_at(&verified.at)
                .await
                .map_err(|_| StateError::Denied)?
        };
        self.authorize_settings(&settings, false).await
    }

    async fn authorize_settings(&self, settings: &AuthSettings, at_cut: bool) -> Result<()> {
        // Every resolved Permission grants at least Read. The live arm uses
        // current accepted authority at every delegation hop.
        resolve_state_identity_permission(
            &self.pubkey,
            self.identity,
            settings,
            self.instance,
            at_cut,
            self.work,
        )
        .await
        .map_err(deny_except_admission)?;
        Ok(())
    }

    /// History delivery does not inherit the historical State grant rule.
    #[cfg(all(unix, feature = "service"))]
    pub(crate) async fn authorize_history(&self, context: &ReadContext) -> Result<()> {
        let verified = self.authorize(context.database()).await?;
        if context.scope() != ReadScope::Verified || context.snapshot().is_empty() {
            return Err(StateError::InvalidBoundary.into());
        }
        if context.snapshot() != &verified.at {
            let backend = self.instance.require_local_engine()?;
            self.require_reachable(
                &*backend,
                context.database(),
                &verified.at,
                context.snapshot().tips(),
            )
            .await?;
        }
        Ok(())
    }

    async fn require_reachable(
        &self,
        backend: &dyn BackendImpl,
        database: &ID,
        from: &Snapshot,
        targets: &[ID],
    ) -> Result<()> {
        if let Some(work) = self.work {
            work.require_reachable(backend, database, from, targets)
                .await
        } else {
            require_reachable(backend, database, from, targets).await
        }
    }

    async fn fold(
        &self,
        backend: &dyn BackendImpl,
        context: &ReadContext,
        store: &str,
    ) -> Result<Doc> {
        if let Some(work) = self.work {
            work.fold(
                backend,
                context.database(),
                context.snapshot(),
                store,
                false,
            )
            .await
        } else {
            fold_doc(backend, context, store).await
        }
    }

    async fn compute_doc(
        &self,
        backend: &dyn BackendImpl,
        context: &ReadContext,
        store: &str,
    ) -> Result<ComputedDoc> {
        if !matches!(store, INDEX | SETTINGS) {
            if store.is_empty() {
                return Err(StateError::UnsupportedProjection.into());
            }
            let index = self.fold(backend, context, INDEX).await?;
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
        let value = self.fold(backend, context, store).await?;
        Ok(ComputedDoc {
            context: context.clone(),
            store: store.to_owned(),
            value,
        })
    }
}

pub(crate) fn auth_from_doc(doc: &Doc) -> AuthSettings {
    doc.get("auth")
        .and_then(Value::as_doc)
        .cloned()
        .unwrap_or_default()
        .into()
}

fn is_admission_failure(error: &crate::Error) -> bool {
    matches!(
        error,
        crate::Error::State(StateError::Limit | StateError::Refused)
    )
}

fn deny_except_admission(error: crate::Error) -> crate::Error {
    if is_admission_failure(&error) {
        error
    } else {
        StateError::Denied.into()
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
