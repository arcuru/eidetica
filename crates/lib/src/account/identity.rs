//! Identity database wrapper.
//!
//! An identity is an ordinary database whose authentication settings contain the
//! identity's member keys. Its root ID is the stable address used by delegations.

use crate::{
    Database, Result,
    account::AccountSigner,
    auth::{
        crypto::PublicKey,
        settings::AuthSettings,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, PermissionBounds, SigKey, TreeReference,
        },
    },
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
        let instance = self.database.instance()?;
        self.signer
            .with_identity(self.delegation_key().await?)
            .open_database(&instance, root_id)
            .await
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
        Ok(SigKey::Delegation {
            path: vec![DelegationStep {
                tree: self.root_id().clone(),
                tips: self.database.snapshot().await?.into_tips(),
            }],
            hint: crate::auth::types::KeyHint::from_pubkey(&self.key_id),
        })
    }

    /// Retrieve the ordinary ticket locator after enabling sharing on this handle.
    pub async fn ticket(&self) -> Result<DatabaseTicket> {
        self.database.ticket().await
    }
}
