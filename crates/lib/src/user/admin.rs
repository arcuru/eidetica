//! Instance-admin capability view.
//!
//! [`InstanceAdmin`] is the gateway for operations gated by `Admin` on the
//! `_users` / `_databases` system databases — creating users, listing users,
//! promoting other admins.
//!
//! It is obtained via [`User::admin`](crate::user::User::admin), which only
//! constructs it when the user actually holds instance-admin. Because the
//! permission is checked at construction, the operations here perform no
//! further check of their own, and the privilege boundary is explicit at the
//! call site:
//!
//! ```ignore
//! let admin = user.admin().await?;          // Err if not an instance admin
//! admin.create_user("alice", None).await?;
//! ```
//!
//! Every operation signs `_users` / `_databases` writes with the user's
//! **session key** (never the device key), so the same calls work on both
//! local and remote instances.

use super::{session::User, system_databases};
use crate::{Database, Result, auth::crypto::PublicKey, entry::ID};

/// Instance-admin capability view over a [`User`] session.
///
/// Obtain via [`User::admin`](crate::user::User::admin). See the
/// [module docs](self) for the rationale behind the separate type.
#[derive(Debug)]
pub struct InstanceAdmin<'a> {
    user: &'a User,
}

impl<'a> InstanceAdmin<'a> {
    /// Wrap a user session as an admin view.
    ///
    /// Only [`User::admin`](crate::user::User::admin) calls this, and only
    /// after confirming the user holds instance-admin — do not construct
    /// `InstanceAdmin` any other way.
    pub(crate) fn new(user: &'a User) -> Self {
        Self { user }
    }

    /// Create a new user account.
    ///
    /// Signs the `_users` write with this admin's session key and submits
    /// through the existing signed-entry wire surface, so it works on both
    /// local and remote instances. This is the canonical way to create users.
    ///
    /// # Arguments
    /// * `username` - Unique user identifier
    /// * `password` - Optional password. `None` creates a passwordless user
    ///   (instant login, no key encryption).
    ///
    /// # Returns
    /// The new user's UUID (stable internal identifier).
    pub async fn create_user(&self, username: &str, password: Option<&str>) -> Result<String> {
        let instance = self.user.instance();

        // On a connected instance the genesis + user-database follow-up
        // writes can't yet be authored over the connection's session
        // end-to-end (transitional: the wire-submit path is unblocked by
        // verification-gated submit, but `system_databases::create_user`'s
        // multi-tree authorship has remaining identity-routing edges that
        // a follow-up will close). The daemon owns this flow for now: send
        // the admin-gated `CreateUser` RPC and let the server run
        // `system_databases::create_user` locally.
        if let Some(conn) = instance.remote_connection() {
            return conn.create_user(username, password).await;
        }

        let signing_key = self.user.default_signing_key()?;
        let users_db = instance.users_db_for_session(&signing_key).await?;
        let (user_uuid, _) =
            system_databases::create_user(&users_db, instance, username, password).await?;
        Ok(user_uuid)
    }

    /// List all user IDs.
    ///
    /// Reads `_users` via the admin's session key, so it works on both local
    /// and remote instances.
    pub async fn list_users(&self) -> Result<Vec<String>> {
        let signing_key = self.user.default_signing_key()?;
        let users_db = self
            .user
            .instance()
            .users_db_for_session(&signing_key)
            .await?;
        system_databases::list_users(&users_db).await
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
    /// Unlike `User::open_database`, this does not require a tracked-database
    /// SigKey mapping: the instance-admin bootstrap writes the user's pubkey
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
