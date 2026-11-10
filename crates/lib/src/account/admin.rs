//! Instance-admin capability view.
//!
//! [`InstanceAdmin`] is the gateway for operations gated by `Admin` on the
//! `_users` / `_databases` system databases — creating accounts, listing accounts,
//! promoting other admins.
//!
//! It is obtained via [`AccountSession::admin`](crate::account::AccountSession::admin), which only
//! constructs it when the account actually holds instance-admin. Because the
//! permission is checked at construction, the operations here perform no
//! further check of their own, and the privilege boundary is explicit at the
//! call site:
//!
//! ```ignore
//! let admin = account.admin().await?;          // Err if not an instance admin
//! admin.create_account(NewAccount::passwordless("alice")).await?;
//! ```
//!
//! Promotion signs system Auth with the account's session key. Native login
//! registration/listing use instance-admin-gated RPC on connected instances;
//! passwords and plaintext signing keys remain client-side.

use super::{session::AccountSession, system_databases};
use crate::{Database, NewAccount, Result, auth::crypto::PublicKey, entry::ID};

/// Instance-admin capability view over an [`AccountSession`] session.
///
/// Obtain via [`AccountSession::admin`](crate::account::AccountSession::admin). See the
/// [module docs](self) for the rationale behind the separate type.
#[derive(Debug)]
pub struct InstanceAdmin<'a> {
    user: &'a AccountSession,
}

impl<'a> InstanceAdmin<'a> {
    /// Wrap an account session as an admin view.
    ///
    /// Only [`AccountSession::admin`](crate::account::AccountSession::admin) calls this, and only
    /// after confirming the account holds instance-admin — do not construct
    /// `InstanceAdmin` any other way.
    pub(crate) fn new(user: &'a AccountSession) -> Self {
        Self { user }
    }

    /// Create a new account.
    ///
    /// Authors the Account database with its new key and registers its wrapped
    /// credentials in native local storage. Connected registration is gated by
    /// the admin's session key; passwords and plaintext keys are not
    /// sent to the daemon. First login retains the existing device-Read grant.
    ///
    /// Returns the new instance-local login ID (see
    /// [`AccountSession::login_id`]), not the Account root ID. Use
    /// [`Instance::login_account`] to obtain an [`AccountSession`] session for the new
    /// account; materialising it directly here would require the admin's
    /// connection to read the new account's own tree over the wire, where the
    /// session gate (correctly) rejects the admin as a non-member.
    ///
    /// # Arguments
    /// * `new_account` - Username and password string (including empty) for the account to create.
    ///   See [`NewAccount`].
    ///
    /// [`Instance::login_account`]: crate::Instance::login_account
    pub async fn create_account(&self, new_account: NewAccount) -> Result<String> {
        let instance = self.user.instance();
        let signing_key = self.user.default_signing_key()?;
        let users_db = instance.users_db_for_session(&signing_key).await?;
        let (login_id, _user_info, _root_key) = system_databases::create_account(
            &users_db,
            instance,
            &new_account.username,
            &new_account.password,
        )
        .await?;
        Ok(login_id)
    }

    /// List the login names of all accounts registered on this instance.
    ///
    /// Reads native local login names. Connected requests require instance-admin
    /// on the daemon's `_users` Auth and return no credential records.
    pub async fn list_accounts(&self) -> Result<Vec<String>> {
        let signing_key = self.user.default_signing_key()?;
        let users_db = self
            .user
            .instance()
            .users_db_for_session(&signing_key)
            .await?;
        system_databases::list_accounts(&users_db).await
    }

    /// Grant instance-admin to another key.
    ///
    /// Adds `new_admin` as `Admin(0)` on the system databases that gate
    /// instance-level admin operations (`_users` and `_databases`), with the
    /// write signed by this admin's own key. The first instance admin is
    /// created automatically during instance bootstrap; this is how every
    /// subsequent admin is promoted.
    ///
    /// Idempotent: re-granting an existing admin re-asserts the same
    /// `Admin(0)` entry.
    pub async fn grant_instance_admin(&self, new_admin: &PublicKey) -> Result<()> {
        let users_db = self
            .admin_keyed_system_db(self.user.instance().users_db_id())
            .await?;
        let databases_db = self
            .admin_keyed_system_db(self.user.instance().databases_db_id())
            .await?;
        system_databases::grant_admin_on_system_dbs(&users_db, &databases_db, new_admin).await
    }

    /// Open a system database keyed by this admin's default signing key.
    ///
    /// Unlike `AccountSession::open_database`, this does not require a tracked-database
    /// SigKey mapping: the instance-admin bootstrap writes the account's pubkey
    /// straight into the system DB's `auth_settings`, so the default-pubkey
    /// identity resolves directly. Routes through
    /// [`Instance::open_system_db_for_session`], so reads go over the wire on
    /// a remote instance instead of hitting the client's empty local backend.
    async fn admin_keyed_system_db(&self, root_id: &ID) -> Result<Database> {
        let signing_key = self.user.default_signing_key()?;
        self.user
            .instance()
            .open_system_db_for_session(root_id, &signing_key)
            .await
    }
}
