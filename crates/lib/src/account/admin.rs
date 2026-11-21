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

use super::{
    Account, AccountError, AccountSigner, Identity, local_logins, session::AccountSession,
    system_databases,
};
use crate::{
    Database, NewAccount, Result,
    auth::{
        AuthKey, AuthSettings, Permission,
        crypto::{PrivateKey, PublicKey},
        types::{DelegatedTreeRef, PermissionBounds, TreeReference},
    },
    database::DatabaseKey,
    entry::ID,
};

const ACCOUNT_REFERENCES: &str = "accounts";
const APP_ADMIN: Permission = Permission::Admin(20);

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

    /// Register a normal local login for an existing Account and an already
    /// authorized direct key from this installation. This creates no Account,
    /// grants no Auth, and refuses to replace a login of the same name.
    /// Public readability or a delegated grant alone is not a direct key grant.
    /// Password wrapping stays client-side, including on a daemon connection.
    /// Returns the installation-local login ID, not the Account root ID.
    pub async fn register_account_login(
        &self,
        account_id: &ID,
        key: PrivateKey,
        login: NewAccount,
    ) -> Result<String> {
        let instance = self.user.instance();
        let signer = AccountSigner::new(account_id.clone(), key.clone());
        let account = signer.open_account(instance).await?;
        let direct = account.database().live_auth_settings().await?;
        if !direct
            .get_key_by_pubkey(&signer.key_id())
            .is_ok_and(|entry| entry.is_active())
        {
            return Err(AccountError::InsufficientPermissions.into());
        }
        let (login_id, _) =
            local_logins::register_key(instance, account_id, &login.username, &login.password, key)
                .await?;
        Ok(login_id)
    }

    /// Provision one owned app Account, a fresh scoped key and its ordinary
    /// local login. The chosen Identity must have an active member stronger
    /// than the app's `Admin(20)` grant; that exact member signs genesis through
    /// the Identity. The app key is not enrolled in the Identity.
    ///
    /// On success the administering Account remembers the root in `accounts`
    /// under `alias`. Serialize setup for this alias: reference writes are
    /// ordinary CRDT writes, not distributed create-if-absent. Reuse this root
    /// on later installations rather than calling this method again.
    ///
    /// Genesis, local login registration and linking are separate writes. If a
    /// later step fails, [`crate::Error::created_database_id`] identifies the
    /// known created root. A link error means login registration succeeded;
    /// after a transport error, read back login/reference state before retrying.
    pub async fn provision_owned_app(
        &self,
        alias: &str,
        owner: &Identity,
        login: NewAccount,
    ) -> Result<ID> {
        if self
            .user
            .reference(ACCOUNT_REFERENCES, alias)
            .await?
            .is_some()
        {
            return Err(AccountError::AccountLinkAlreadyExists {
                name: alias.to_owned(),
            }
            .into());
        }
        if self.list_accounts().await?.contains(&login.username) {
            return Err(AccountError::LoginNameAlreadyExists {
                username: login.username,
            }
            .into());
        }

        let snapshot = owner.database().snapshot().await?;
        // Read membership and build the grant and signing path from one cut.
        // Do not select a different key if the supplied member lacks priority.
        let membership = owner
            .database()
            .new_transaction_at(&snapshot)
            .await?
            .get_settings()?
            .auth_snapshot()
            .await?;
        if !membership
            .get_key_by_pubkey(owner.key_id())
            .is_ok_and(|entry| entry.is_active() && *entry.permissions() > APP_ADMIN)
        {
            return Err(AccountError::InsufficientPermissions.into());
        }
        let tips = snapshot.into_tips();
        let signer = DatabaseKey::with_identity(
            owner.signing_key().clone(),
            owner.delegation_key_at(tips.clone()),
        );
        let app_key = PrivateKey::generate();
        let mut auth = AuthSettings::new();
        auth.add_delegated_tree(DelegatedTreeRef {
            tree: TreeReference {
                root: owner.root_id().clone(),
                tips,
            },
            permission_bounds: PermissionBounds {
                max: Permission::Admin(0),
                min: None,
            },
        })?;
        auth.add_key(&app_key.public_key(), AuthKey::active(None, APP_ADMIN))?;
        let account = Account::create(self.user.instance(), signer, alias, auth).await?;
        let id = account.id();

        let post_genesis = |step, source| {
            crate::Error::from(AccountError::OwnedAppSetup {
                step,
                source: Box::new(source),
            })
            .after_database_creation(id)
        };
        // The key is already explicitly present in the verified genesis we just
        // created. Register it without transferring the owner's Identity key.
        local_logins::register_key(
            self.user.instance(),
            id,
            &login.username,
            &login.password,
            app_key,
        )
        .await
        .map_err(|source| post_genesis("login registration", source))?;
        self.user
            .set_reference(ACCOUNT_REFERENCES, alias, id)
            .await
            .map_err(|source| post_genesis("Account link", source))?;
        Ok(id.clone())
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
