//! Immutable acting handles, independent of bundle storage and login names.

use crate::{
    Database, ID, Instance, Result,
    auth::{SigKey, crypto::PublicKey},
    database::DatabaseKey,
};

/// An Account locator paired with one actual signer and explicit Auth identity.
///
/// The locator is organizational context, never authorization. A direct key
/// needs no Identity database. Delegated use passes the exact `DatabaseKey`
/// identity produced by the Identity API; no referenced Identity or default
/// signer is implicitly selected. Constructing another handle never rebinds
/// this one, nor does dropping a bundle invalidate a retained signer handle.
#[derive(Clone, Debug)]
pub struct AccountSigner {
    account_id: ID,
    key: DatabaseKey,
}

impl AccountSigner {
    /// Bind an explicit signer and Account locator. This performs no storage
    /// lookup, authorization grant, key import, or daemon login.
    pub fn new(account_id: ID, key: impl Into<DatabaseKey>) -> Self {
        let (signer, mut identity) = key.into().into_parts();
        // A name-only hint must not fall back to the service connection's login
        // key. Narrow the hint to this actual signer, without changing its path.
        if identity.hint().pubkey.is_none() {
            identity.hint_mut().pubkey = Some(signer.public_key());
        }
        Self {
            account_id,
            key: DatabaseKey::with_identity(signer, identity),
        }
    }

    pub(crate) fn signing_key(&self) -> &crate::auth::crypto::PrivateKey {
        self.key.signing_key()
    }

    pub fn account_id(&self) -> &ID {
        &self.account_id
    }
    pub fn key_id(&self) -> PublicKey {
        self.key.public_key()
    }
    pub fn identity(&self) -> &SigKey {
        self.key.identity()
    }

    /// Return a newly bound handle using this actual key and an explicit Auth
    /// identity. The old handle retains its original identity and signer.
    pub fn with_identity(&self, identity: SigKey) -> Self {
        Self::new(
            self.account_id.clone(),
            DatabaseKey::with_identity(self.key.signing_key().clone(), identity),
        )
    }

    /// Open an already-initialized instance and prove signer possession to a
    /// service without fetching or registering a daemon-held login bundle.
    /// Embedded connections use the same signer API without a wire handshake.
    /// Each call creates its own connection; no existing connection is rebound.
    pub async fn connect(&self, url: impl AsRef<str>) -> Result<Instance> {
        let instance = Instance::connect(url).await?;
        #[cfg(all(unix, feature = "service"))]
        if let Some(connection) = instance.remote_connection() {
            connection
                .authenticate_signer(self.key.signing_key())
                .await?;
        }
        Ok(instance)
    }

    /// Open this locator with this signer under ordinary Account Auth.
    pub async fn open_account(&self, instance: &Instance) -> Result<super::Account> {
        self.open_database(instance, &self.account_id)
            .await
            .map(super::Account::new)
    }

    /// Open exactly this target with this actual signer/identity. A locator or
    /// reference is not a grant; target Auth and proof validation are unchanged.
    pub async fn open_database(&self, instance: &Instance, root_id: &ID) -> Result<Database> {
        if self.key.identity().hint().pubkey.as_ref() != Some(&self.key.public_key()) {
            return Err(crate::auth::AuthError::InvalidAuthConfiguration {
                reason: "Signer does not match the Auth identity public-key hint".to_owned(),
            }
            .into());
        }
        #[cfg(all(unix, feature = "service"))]
        if let Some(connection) = instance.remote_connection() {
            connection
                .register_session_key(self.key.signing_key())
                .await?;
            return Ok(Database::open_remote(
                instance,
                connection.with_signer(self.key.signing_key().clone()),
                root_id,
                self.key.identity().clone(),
            )
            .await?
            .with_key(self.key.clone()));
        }
        let database = Database::open(instance, root_id)
            .await?
            .with_key(self.key.clone());
        // Local opening alone is a handle construction. Check the selected Auth
        // path as the remote open gate does; do not return a fallback identity.
        database.current_permission().await?;
        Ok(database)
    }
}
