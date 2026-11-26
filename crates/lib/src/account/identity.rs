//! Identity database wrapper.
//!
//! An identity is an ordinary database whose authentication settings contain the
//! identity's member keys. Its root ID is the stable address used by delegations.

use crate::{
    Database, Result, Snapshot,
    account::{Account, AccountSigner},
    auth::{
        crypto::{PrivateKey, PublicKey},
        errors::AuthError,
        settings::AuthSettings,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, PermissionBounds, SigKey, TreeReference,
        },
        validation::{delegation::MAX_DELEGATION_TIPS, floors::DerivedFloors},
    },
    backend::Reachability,
    entry::ID,
    sync::DatabaseTicket,
};

/// A locally tracked identity backed by a dedicated database.
#[derive(Debug)]
pub struct Identity {
    database: Database,
    key_id: PublicKey,
    signer: AccountSigner,
    name: String,
}

/// The outcome of registering an existing identity.
// Return the ordinary handle directly, like create_identity, without another allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum IdentityRegistration {
    /// Membership is visible locally and the returned handle is ready to use.
    Active(Identity),
    /// Approval or its synced state is still pending; activate explicitly later.
    Pending,
}

/// The local result for one database in an [`AccountSession`](super::AccountSession) sweep.
#[derive(Debug)]
pub enum DelegationAdvance {
    /// The empty signed data entry was committed locally.
    Updated(ID),
    /// The effective floor already covered the chosen cut; nothing was appended.
    AlreadyCovered,
    /// The account could read the target, and it did not delegate to this Identity.
    Skipped,
    /// No advance was confirmed. The cause is retained for this database.
    Failed(crate::Error),
}

impl Identity {
    pub(crate) fn new(
        database: Database,
        key_id: PublicKey,
        signer: AccountSigner,
        name: String,
    ) -> Self {
        Self {
            database,
            key_id,
            signer,
            name,
        }
    }

    /// The stable identity address.
    pub fn root_id(&self) -> &ID {
        self.database.root_id()
    }

    /// The selected local signing key's public key.
    pub fn key_id(&self) -> &PublicKey {
        &self.key_id
    }

    /// The local tracking name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The underlying identity database.
    pub fn database(&self) -> &Database {
        &self.database
    }

    /// Open a database specifically through this identity root.
    ///
    /// Direct and global paths for the same public key are deliberately ignored:
    /// the selected path must start at this identity's root.
    pub async fn open_database(&self, root_id: &ID) -> Result<Database> {
        let cut = self.database.snapshot().await?;
        self.open_database_at(root_id, &cut).await
    }

    pub(super) async fn open_database_at(&self, root_id: &ID, cut: &Snapshot) -> Result<Database> {
        let instance = self.database.instance()?;
        self.signer
            .with_identity(self.delegation_key_at(cut.tips().to_vec()))
            .open_database(&instance, root_id)
            .await
    }

    /// Advance a directly delegating database's causal floor with an empty
    /// data entry signed through this exact Identity at its Verified snapshot.
    ///
    /// Returns the entry ID, or `None` if the effective floor already covers
    /// that snapshot. A write needs effective delegated Write permission.
    /// This never edits Settings or falls back to a direct key. The result
    /// describes the local operation, not acceptance by other replicas.
    ///
    /// Returns an error if the chosen snapshot cannot be proved to cover the
    /// existing floor (for example, if it misses a concurrent Identity branch).
    pub async fn advance_delegation(&self, root_id: &ID) -> Result<Option<ID>> {
        let cut = self.delegation_cut().await?;
        let target = self.open_database_at(root_id, &cut).await?;
        self.advance_opened_delegation(&target, &cut).await
    }

    pub(super) async fn delegation_cut(&self) -> Result<Snapshot> {
        let cut = self.database.snapshot().await?;
        if cut.is_empty() {
            return Err(AuthError::DelegatedTreeUnsynced {
                tree_id: self.root_id().clone(),
                missing: vec![self.root_id().clone()],
            }
            .into());
        }
        Ok(cut)
    }

    // `target` must have been opened through `open_database_at` at `cut`.
    pub(super) async fn advance_opened_delegation(
        &self,
        target: &Database,
        cut: &Snapshot,
    ) -> Result<Option<ID>> {
        let parents = target.snapshot().await?;
        if parents.is_empty() {
            return Err(AuthError::DelegatedTreeUnsynced {
                tree_id: target.root_id().clone(),
                missing: vec![target.root_id().clone()],
            }
            .into());
        }
        let settings = target.auth_settings_at(&parents).await?;
        let declaration = settings.get_delegated_tree(self.root_id())?;
        let floor = DerivedFloors::at_verified_frontier(target, &parents, &declaration).await?;
        if self.covers(&floor, cut).await? {
            return Ok(None);
        }
        if !self.covers(cut, &floor).await? {
            return Err(AuthError::DelegationSnapshotRegressed {
                tree_id: Box::new(self.root_id().clone()),
                claimed_tips: cut.tips().to_vec(),
            }
            .into());
        }
        if cut.len() > MAX_DELEGATION_TIPS {
            return Err(AuthError::DelegationTipsTooMany {
                tree_id: self.root_id().clone(),
                len: cut.len(),
                max: MAX_DELEGATION_TIPS,
            }
            .into());
        }

        // Avoid submitting a known read-only write to a connected daemon.
        // The commit still runs ordinary signature, floor and permission checks.
        let member = self
            .database
            .auth_settings_at(cut)
            .await?
            .get_key_by_pubkey(&self.key_id)?;
        if !member.is_active()
            || !member
                .permissions()
                .clamp_to_bounds(&declaration.permission_bounds)
                .can_write()
        {
            return Err(AuthError::PermissionDenied {
                reason: format!(
                    "Identity {} does not grant Write on {} at this cut",
                    self.root_id(),
                    target.root_id()
                ),
            }
            .into());
        }
        // Anchor both the preflight and the signed entry to the same parents.
        // If another writer races, its branch is not overwritten.
        target
            .new_transaction_at(&parents)
            .await?
            .commit()
            .await
            .map(Some)
    }

    async fn covers(&self, frontier: &Snapshot, tips: &Snapshot) -> Result<bool> {
        if let Some(engine) = self.database.ops().local_engine() {
            return match engine
                .check_targets_reachable_from(self.root_id(), frontier, tips)
                .await?
            {
                Reachability::Reachable => Ok(true),
                Reachability::Unreachable => Ok(false),
                Reachability::Indeterminate { missing } => Err(AuthError::DelegatedTreeUnsynced {
                    tree_id: self.root_id().clone(),
                    missing,
                }
                .into()),
            };
        }
        let ancestry: std::collections::HashSet<_> = self
            .database
            .ids_added(&Snapshot::EMPTY, frontier)
            .await?
            .into_iter()
            .collect();
        Ok(tips.iter().all(|tip| ancestry.contains(tip)))
    }

    /// Open a referenced Account through this exact Identity, without logging in to it.
    pub async fn open_account(&self, root_id: &ID) -> Result<Account> {
        self.open_database(root_id).await.map(Account::new)
    }

    /// Add or update a member key.
    pub async fn add_key(&self, pubkey: &PublicKey, auth_key: AuthKey) -> Result<()> {
        let tx = self.database.new_transaction().await?;
        tx.get_settings()?.set_auth_key(pubkey, auth_key).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Revoke a member key.
    pub async fn revoke_key(&self, pubkey: &PublicKey) -> Result<()> {
        let tx = self.database.new_transaction().await?;
        tx.get_settings()?.revoke_auth_key(pubkey).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Read the identity's authentication settings.
    pub async fn keys(&self) -> Result<AuthSettings> {
        self.database.get_settings().await?.auth_snapshot().await
    }

    /// Build a delegation reference to the identity's current snapshot.
    pub async fn as_delegation(
        &self,
        permission_bounds: PermissionBounds,
    ) -> Result<DelegatedTreeRef> {
        Ok(DelegatedTreeRef {
            permission_bounds,
            tree: TreeReference {
                root: self.root_id().clone(),
                tips: self.database.snapshot().await?.into_tips(),
            },
        })
    }

    /// Build the exact signing identity rooted at this identity database.
    pub async fn delegation_key(&self) -> Result<SigKey> {
        Ok(self.delegation_key_at(self.database.snapshot().await?.into_tips()))
    }

    pub(crate) fn delegation_key_at(&self, tips: Vec<ID>) -> SigKey {
        SigKey::Delegation {
            path: vec![DelegationStep {
                tree: self.root_id().clone(),
                tips,
            }],
            hint: crate::auth::types::KeyHint::from_pubkey(&self.key_id),
        }
    }

    pub(crate) fn signing_key(&self) -> &PrivateKey {
        self.signer.signing_key()
    }

    /// Retrieve the ordinary ticket locator after enabling sharing on this handle.
    pub async fn ticket(&self) -> Result<DatabaseTicket> {
        self.database.ticket().await
    }
}
