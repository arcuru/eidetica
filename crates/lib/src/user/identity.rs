//! Identity database wrapper.
//!
//! An identity is an ordinary database whose authentication settings contain the
//! identity's member keys. Its root ID is the stable address used by delegations.

use std::ops::Deref;

use crate::{
    Database, Result,
    auth::{
        crypto::{PrivateKey, PublicKey},
        settings::AuthSettings,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, PermissionBounds, SigKey, TreeReference,
        },
    },
    database::DatabaseKey,
    entry::ID,
    store::Table,
    sync::DatabaseTicket,
    user::{IdentityStatus, TrackedIdentity, UserError},
};

/// A locally tracked identity backed by a dedicated database.
#[derive(Debug)]
pub struct Identity {
    database: Database,
    key_id: PublicKey,
    signing_key: PrivateKey,
    name: String,
    user_database: Database,
}

impl Deref for Identity {
    type Target = Database;

    fn deref(&self) -> &Database {
        &self.database
    }
}

impl Identity {
    pub(crate) fn new(
        database: Database,
        key_id: PublicKey,
        signing_key: PrivateKey,
        name: String,
        user_database: Database,
    ) -> Self {
        Self {
            database,
            key_id,
            signing_key,
            name,
            user_database,
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

    /// Select another user-held member key for identity operations.
    ///
    /// The key must match `signing_key` and must currently be authorized by the
    /// identity database. The wrapped database is rebound immediately, so later
    /// identity writes are signed by the newly selected key.
    pub async fn set_key(&mut self, key_id: PublicKey, signing_key: PrivateKey) -> Result<()> {
        let actual = signing_key.public_key();
        if actual != key_id {
            return Err(UserError::IdentityKeyMismatch {
                expected: key_id.to_string(),
                actual: actual.to_string(),
            }
            .into());
        }

        let member = self
            .database
            .get_settings()
            .await?
            .auth_snapshot()
            .await?
            .get_key_by_pubkey(&key_id)
            .map_err(|_| UserError::NoSigKeyFound {
                key_id: key_id.to_string(),
                database_id: self.root_id().clone(),
            })?;
        if !member.is_active() {
            return Err(UserError::NoSigKeyFound {
                key_id: key_id.to_string(),
                database_id: self.root_id().clone(),
            }
            .into());
        }

        let sigkey = SigKey::from_pubkey(&key_id);
        let instance = self.database.instance()?;
        let rebound =
            Self::open_with_identity(&instance, self.root_id(), signing_key.clone(), sigkey)
                .await?;
        rebound.current_permission().await?;

        let tx = self.user_database.new_transaction().await?;
        let identities = tx.get_store::<Table<TrackedIdentity>>("identities").await?;
        identities
            .set(
                &self.name,
                TrackedIdentity {
                    root_id: self.root_id().clone(),
                    status: IdentityStatus::Active,
                    key_id: key_id.clone(),
                },
            )
            .await?;
        tx.commit().await?;

        self.database = rebound;
        self.key_id = key_id;
        self.signing_key = signing_key;
        Ok(())
    }

    /// Open a database specifically through this identity root.
    ///
    /// Direct and global paths for the same public key are deliberately ignored:
    /// the selected path must start at this identity's root.
    pub async fn open_database(&self, root_id: &ID) -> Result<Database> {
        let instance = self.database.instance()?;
        let sigkey = self.delegation_key().await?;
        let database =
            Self::open_with_identity(&instance, root_id, self.signing_key.clone(), sigkey).await?;
        // A connected handle was already authorized by the daemon during
        // `open_remote`; resolving the delegation again client-side would
        // require a local engine the connected client deliberately lacks.
        #[cfg(all(unix, feature = "service"))]
        if instance.remote_connection().is_none() {
            database.current_permission().await?;
        }
        #[cfg(not(all(unix, feature = "service")))]
        database.current_permission().await?;
        Ok(database)
    }

    async fn open_with_identity(
        instance: &crate::Instance,
        root_id: &ID,
        signing_key: PrivateKey,
        sigkey: SigKey,
    ) -> Result<Database> {
        let key = DatabaseKey::with_identity(signing_key.clone(), sigkey.clone());
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = instance.remote_connection() {
            conn.register_session_key(&signing_key).await?;
            return Ok(Database::open_remote(instance, conn, root_id, sigkey)
                .await?
                .with_key(key));
        }
        Ok(Database::open(instance, root_id).await?.with_key(key))
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

    /// Create a ticket for the identity database.
    pub fn ticket(&self) -> DatabaseTicket {
        DatabaseTicket::new(self.root_id().clone())
    }
}
