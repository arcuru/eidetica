//! System database initialization for the account system
//!
//! Creates system Auth databases and native installation login records.

use handle_trait::Handle;

use super::{
    AccountSession,
    crypto::{derive_encryption_key, encrypt_private_key},
    errors::AccountError,
    key_manager::AccountKeyManager,
    types::{AccountCredentials, AccountInfo, AccountKey, AccountStatus, KeyStorage},
};
use crate::{
    Database, Instance, Result,
    auth::{
        crypto::{PrivateKey, PublicKey, generate_keypair},
        types::{AuthKey, Permission},
    },
    constants::{DATABASES, INSTANCE, USERS},
    crdt::Doc,
};

/// Whether `_users.auth_settings` already lists an instance admin.
///
/// "Instance admin" here means any non-device-key entry with `Admin` permission
/// in `_users`'s auth settings. The device key is `Admin(0)` on every system
/// DB by construction (it bootstrapped them), so we exclude it explicitly —
/// otherwise every fresh instance would look like it already had an admin.
pub(crate) async fn has_instance_admin(
    users_db: &Database,
    device_pubkey: &PublicKey,
) -> Result<bool> {
    let tx = users_db.new_transaction().await?;
    let settings = tx.get_settings()?;
    let auth = settings.auth_snapshot().await?;
    let device_pubkey_str = device_pubkey.to_string();
    for (pubkey_str, key) in auth.get_all_keys()? {
        if pubkey_str == device_pubkey_str {
            continue;
        }
        if matches!(key.permissions(), Permission::Admin(_)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Add `pubkey` as `Admin(0)` to every system database that gates instance-level
/// admin operations.
///
/// Today that's `_users` (account directory + auth bootstrap) and `_databases`
/// (instance-wide database registry). `_sync` is lazily created on
/// `enable_sync()` and out of scope here.
///
/// The Database handles passed in must already be opened with a signing key
/// that holds `Admin` on each respective DB. The first-admin bootstrap path
/// (`create_account` below) passes device-keyed handles — `Database::create`
/// registered the device key as `Admin(0)` on each system DB. The admin
/// promotion path (`InstanceAdmin::grant_instance_admin`) passes handles keyed by an
/// existing admin's own key; the same write then resolves against that
/// admin's identity.
pub(crate) async fn grant_admin_on_system_dbs(
    users_db: &Database,
    databases_db: &Database,
    pubkey: &PublicKey,
) -> Result<()> {
    for database in [users_db, databases_db] {
        database
            .with_transaction(|tx| async move {
                let settings = tx.get_settings()?;
                settings
                    .set_auth_key(pubkey, AuthKey::active(Some("admin"), Permission::Admin(0)))
                    .await
            })
            .await?;
    }
    Ok(())
}

/// Create the _instance system database
///
/// This database stores Instance-level configuration and metadata.
/// Auth is bootstrapped by `Database::create` with the device key as Admin(0).
///
/// # Arguments
/// * `instance` - The Instance handle
/// * `device_signing_key` - The device's Ed25519 signing key
///
/// # Returns
/// The _instance Database
pub async fn create_instance_database(
    instance: &Instance,
    device_signing_key: &PrivateKey,
) -> Result<Database> {
    let mut settings = Doc::new();
    settings.set("name", INSTANCE);
    settings.set("type", "system");
    settings.set("description", "Instance configuration and management");

    Database::create(instance, device_signing_key.clone(), settings).await
}

/// Create the _users system database
///
/// This database gates instance-admin capabilities. Login credentials are native local records.
/// Auth is bootstrapped by `Database::create` with the device key as Admin(0).
///
/// # Arguments
/// * `instance` - The Instance handle
/// * `device_signing_key` - The device's Ed25519 signing key
///
/// # Returns
/// The created _users Database
pub async fn create_accounts_database(
    instance: &Instance,
    device_signing_key: &PrivateKey,
) -> Result<Database> {
    let mut settings = Doc::new();
    settings.set("name", USERS);
    settings.set("type", "system");
    settings.set("description", "Instance account administration");

    Database::create(instance, device_signing_key.clone(), settings).await
}

/// Create the _databases tracking database
///
/// This database stores the database tracking information mapping
/// database_id -> DatabaseTracking.
/// Auth is bootstrapped by `Database::create` with the device key as Admin(0).
///
/// # Arguments
/// * `instance` - The Instance handle
/// * `device_signing_key` - The device's Ed25519 signing key
///
/// # Returns
/// The created _databases Database
pub async fn create_databases_tracking(
    instance: &Instance,
    device_signing_key: &PrivateKey,
) -> Result<Database> {
    let mut settings = Doc::new();
    settings.set("name", DATABASES);
    settings.set("type", "system");
    settings.set("description", "Database tracking and registry");

    Database::create(instance, device_signing_key.clone(), settings).await
}

/// Create a new account
///
/// This function:
/// 1. Optionally hashes the account's password (if provided)
/// 2. Generates a device keypair for the account
/// 3. Creates the portable Account database (no private keys)
/// 4. Registers AccountInfo in native local storage with a login UUID
/// 5. **First-admin bootstrap**: if no instance admin exists yet in `_users.auth_settings`,
///    promotes this account by adding their pubkey as `Admin(0)` to the
///    instance-admin system DBs (`_users` and `_databases`). Subsequent
///    accounts land as non-admins.
///
/// # Arguments
/// * `users_db` - The _users system database
/// * `instance` - The Instance handle
/// * `username` - Unique username for login
/// * `password` - Optional password. If None, creates passwordless account (instant login, no encryption)
///
/// # Returns
/// A tuple of `(login_id, AccountInfo, root_private_key)`. The private key is
/// the one just generated for this account; callers (e.g.
/// [`Instance::create_backend`](crate::Instance::create_backend) or
/// [`InstanceAdmin::create_account`](crate::account::InstanceAdmin::create_account))
/// can hand it straight to [`build_account_session`] to materialise a logged-in
/// [`AccountSession`](crate::account::AccountSession) without re-decrypting the key from storage.
pub async fn create_account(
    users_db: &Database,
    instance: &Instance,
    username: impl AsRef<str>,
    password: Option<&str>,
) -> Result<(String, AccountInfo, crate::auth::crypto::PrivateKey)> {
    let username = username.as_ref();
    // Names are unique within this installation. The native insertion below
    // is atomic; no credential row is ever written into _users or AccountDB.
    if super::local_logins::list(users_db)
        .await?
        .iter()
        .any(|name| name == username)
    {
        return Err(AccountError::LoginNameAlreadyExists {
            username: username.to_owned(),
        }
        .into());
    }

    // 1. Generate default keypair for this account
    let (user_private_key, user_public_key) = generate_keypair();
    // Keep a clone for the caller — the passwordless branch below moves
    // `user_private_key` into `KeyStorage::Unencrypted`, so we hand back a
    // detached copy. Ed25519 keys are 32 bytes; the clone is negligible.
    let returned_private_key = user_private_key.clone();

    // 2. Create account database with the account's key as owner (Admin(0))
    let mut user_db_settings = Doc::new();
    user_db_settings.set("name", format!("_user_{username}"));
    user_db_settings.set("type", "user");
    user_db_settings.set("description", format!("Account database for {username}"));

    let account_database =
        Database::create(instance, user_private_key.clone(), user_db_settings).await?;
    let account_id = account_database.root_id().clone();

    // First login grants the existing daemon device Read access to AccountDB.
    // It also initializes native key metadata from this local root credential;
    // neither root nor additional private keys are authored into AccountDB.
    let device_pubkey = instance.id();

    // 3. Build AccountCredentials — encrypt root key if password provided
    let (root_key, password_salt) = match password {
        Some(pwd) => {
            let salt_string = super::crypto::generate_salt();
            let encryption_key = derive_encryption_key(pwd, &salt_string)?;
            let (ciphertext, nonce) = encrypt_private_key(&user_private_key, &encryption_key)?;
            (
                KeyStorage::Encrypted {
                    algorithm: "aes-256-gcm".to_string(),
                    ciphertext,
                    nonce,
                },
                Some(salt_string),
            )
        }
        None => (
            KeyStorage::Unencrypted {
                key: user_private_key,
            },
            None,
        ),
    };

    let credentials = AccountCredentials {
        root_key_id: user_public_key.clone(),
        root_key,
        password_salt,
    };

    // 4. Create AccountInfo
    let account_info = AccountInfo {
        username: username.to_string(),
        account_id,
        credentials,
        created_at: instance.clock().now_secs(),
        status: AccountStatus::Active,
    };

    let login_id = uuid::Uuid::new_v4().to_string();
    super::local_logins::register(instance, &login_id, &account_info).await?;

    // 6. First-admin bootstrap: if no instance admin exists yet, promote this
    // account. Done last so that a failure here doesn't leave behind a partially
    // created account. The auth-settings write itself is signed by the device
    // key (the only Admin on the system DBs at bootstrap time); opening
    // `databases_db()` here mirrors the `users_db` argument passed in and
    // keeps the device-key signing detail inside `Instance`.
    if !has_instance_admin(users_db, &device_pubkey).await? {
        let databases_db = instance.databases_db().await?;
        grant_admin_on_system_dbs(users_db, &databases_db, &user_public_key).await?;
    }

    Ok((login_id, account_info, returned_private_key))
}

/// Login an account
///
/// This function:
/// 1. Finds the installation-local login record by name
/// 2. Verifies password (if provided and required)
/// 3. Opens account's private database
/// 4. Loads and decrypts account keys (or loads unencrypted for passwordless accounts)
/// 5. Creates AccountKeyManager with keys
/// 6. Returns AccountSession object
///
/// # Arguments
/// * `users_db` - The _users system database
/// * `instance` - The Instance handle
/// * `username` - Username for login
/// * `password` - Optional password. None for passwordless accounts.
///
/// # Returns
/// An AccountSession object with keys loaded
pub async fn login_account(
    _users_db: &Database,
    instance: &Instance,
    username: impl AsRef<str>,
    password: Option<&str>,
) -> Result<super::AccountSession> {
    let username = username.as_ref();

    let (login_id, account_info) = super::local_logins::lookup(instance, username).await?;

    // 2. Verify password compatibility and construct root AccountKey from credentials
    let creds = &account_info.credentials;
    let is_passwordless = creds.password_salt.is_none();
    match (password, is_passwordless) {
        (Some(_), false) => {
            // Password provided for password-protected account: OK
            // Decryption of the root key IS password verification
        }
        (None, true) => {
            // No password for passwordless account: OK
        }
        (Some(_), true) => {
            // Password provided for passwordless account: reject
            return Err(AccountError::InvalidPassword.into());
        }
        (None, false) => {
            // No password for password-protected account: reject
            return Err(AccountError::PasswordRequired {
                operation: "login for password-protected user".to_string(),
            }
            .into());
        }
    }

    // 3. Decrypt root signing key from credentials (decryption IS password verification)
    let root_signing_key = match (&creds.root_key, password) {
        (
            KeyStorage::Encrypted {
                ciphertext, nonce, ..
            },
            Some(pwd),
        ) => {
            let salt =
                creds
                    .password_salt
                    .as_ref()
                    .ok_or_else(|| AccountError::PasswordRequired {
                        operation: "decrypt keys for password-protected user".to_string(),
                    })?;
            let encryption_key = derive_encryption_key(pwd, salt)?;
            super::crypto::decrypt_private_key(ciphertext, nonce, &encryption_key)?
        }
        (KeyStorage::Unencrypted { key }, None) => key.clone(),
        _ => return Err(AccountError::InvalidPassword.into()),
    };

    let user = build_account_session(
        instance,
        &login_id,
        &account_info,
        root_signing_key,
        password,
    )
    .await?;

    Ok(user)
}

/// Build an `AccountSession` session from data already in hand: the decrypted root
/// signing key, the account's record, and an optional password (the
/// `AccountKeyManager` re-derives the KEK from it for any per-key decryption).
///
/// Shared by local login and service login after client-side root unlock.
/// Additional keys/mappings are read only from this installation's native record.
/// The private Account database supplies references and Auth, never credentials.
pub(crate) async fn build_account_session(
    instance: &Instance,
    login_id: &str,
    account_info: &AccountInfo,
    root_signing_key: crate::auth::crypto::PrivateKey,
    password: Option<&str>,
) -> Result<super::AccountSession> {
    // 1. Open account database authenticated with decrypted root key.
    let account_database = Database::open(instance, &account_info.account_id)
        .await?
        .with_key(root_signing_key);

    // The installation's native record is the only key/mapping store. Never
    // import historical Account keys: old histories remain untouched, not safe.
    let state = super::local_state::update(instance, login_id, |state| {
        state.last_login = Some(instance.clock().now_secs());
        state
            .keys
            .entry(account_info.credentials.root_key_id.to_string())
            .or_insert_with(|| AccountKey {
                key_id: account_info.credentials.root_key_id.clone(),
                storage: account_info.credentials.root_key.clone(),
                display_name: Some("Root Key".to_string()),
                created_at: account_info.created_at,
                last_used: None,
                is_default: true,
                database_sigkeys: std::collections::HashMap::new(),
            });
        Ok(())
    })
    .await?;
    let all_keys = state.keys.into_values().collect();

    let key_manager = if let Some(pwd) = password {
        let salt = account_info
            .credentials
            .password_salt
            .as_ref()
            .ok_or(AccountError::InvalidPassword)?;
        AccountKeyManager::new(pwd, salt, all_keys)?
    } else {
        AccountKeyManager::new_passwordless(all_keys)?
    };

    // 3. First-login (and recovery) bootstrap on the account's own tree.
    // Idempotent: each check short-circuits on subsequent logins. Best
    // effort — failure here is logged and the login proceeds, because the
    // in-memory `key_manager` already has the root key and the next login
    // will retry on the same conditions.
    if let Err(e) = bootstrap_account_tree_if_needed(instance, &account_database).await {
        tracing::debug!(
            user = %account_info.username,
            error = %e,
            "user-tree first-login bootstrap skipped"
        );
    }

    Ok(AccountSession::new(
        login_id.to_string(),
        account_info.clone(),
        account_database,
        instance.handle(),
        key_manager,
    ))
}

/// Preserve the existing device-Read grant for daemon replication. This is
/// public Auth, not credential storage or a redesign of background authority.
async fn bootstrap_account_tree_if_needed(
    instance: &Instance,
    account_database: &Database,
) -> Result<()> {
    let device_pubkey = instance.id();
    if account_database
        .get_settings()
        .await?
        .get_auth_key(&device_pubkey)
        .await
        .is_ok()
    {
        return Ok(());
    }
    account_database
        .with_transaction(|tx| async move {
            tx.get_settings()?
                .set_auth_key(
                    &device_pubkey,
                    AuthKey::active(Some("device"), Permission::Read),
                )
                .await
        })
        .await?;
    Ok(())
}

/// Look up an account's record by username, without requiring the password.
///
/// Used by the service daemon's challenge-response login flow: the daemon
/// fetches the account's full `AccountInfo` (including encrypted credentials and the
/// account's private-database id) and ships it to the client so the client can
/// derive the KEK locally, decrypt the root key, sign the challenge, and then
/// build the `AccountSession` session entirely from the data already carried by the
/// `TrustedLoginChallenge` response — no second wire read of `_users` is
/// required. The encrypted blob is designed to survive at rest; shipping it
/// over the local socket is the same trust boundary as filesystem read. See
/// the Service Architecture doc § Trusted login threat model for the full
/// rationale.
///
/// # Returns
/// Tuple of `(login_id, AccountInfo)` if the account exists and is active.
pub async fn lookup_account_record(
    users_db: &Database,
    username: impl AsRef<str>,
) -> Result<(String, AccountInfo)> {
    super::local_logins::lookup(&users_db.instance()?, username.as_ref()).await
}

/// List all accounts in the system
///
/// # Arguments
/// * `users_db` - The _users system database
///
/// # Returns
/// Vector of usernames
pub async fn list_accounts(users_db: &Database) -> Result<Vec<String>> {
    super::local_logins::list(users_db).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Instance;
    use crate::backend::database::InMemory;
    use crate::store::DocStore;
    use crate::store::SettingsStore;

    use std::sync::Arc;

    /// Test helper: Create Instance with `admin` as the initial account and a
    /// pinned clock, returning the instance, its device signing key, and the
    /// just-bootstrapped admin AccountSession.
    ///
    /// Tests that only need the device key destructure `(instance,
    /// device_key, _admin)`; tests that need to drive admin operations
    /// destructure `(instance, _device_key, admin)`. Uses [`FixedClock`] for
    /// reproducible timestamps.
    async fn setup_instance() -> (Instance, PrivateKey, crate::account::AccountSession) {
        use crate::clock::FixedClock;

        let (instance, admin) = Instance::create_backend_with_clock(
            Box::new(InMemory::new()),
            Arc::new(FixedClock::default()),
            crate::NewAccount::passwordless("admin"),
        )
        .await
        .unwrap();

        // Get the device key from the instance
        let device_key = instance.signing_key().unwrap().clone();

        (instance, device_key, admin)
    }

    #[tokio::test]
    async fn test_create_instance_database() {
        let (instance, device_key, _admin) = setup_instance().await;

        let instance_db = create_instance_database(&instance, &device_key)
            .await
            .unwrap();

        // Verify database was created
        assert!(!instance_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = instance_db.new_transaction().await.unwrap();
        let doc_store = transaction
            .get_store::<DocStore>("_settings")
            .await
            .unwrap();
        let name = doc_store.get_string("name").await.unwrap();
        assert_eq!(name, INSTANCE);

        // Verify auth settings - key is stored by pubkey
        let device_pubkey = instance.id();
        let settings_store = SettingsStore::new(&transaction).unwrap();
        let auth_settings = settings_store.auth_snapshot().await.unwrap();
        let device_key = auth_settings.get_key_by_pubkey(&device_pubkey).unwrap();
        assert_eq!(device_key.permissions(), &Permission::Admin(0));
        assert_eq!(device_key.name(), None);
    }

    #[tokio::test]
    async fn test_create_users_database() {
        let (instance, device_key, _admin) = setup_instance().await;

        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Verify database was created
        assert!(!users_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = users_db.new_transaction().await.unwrap();
        let doc_store = transaction
            .get_store::<DocStore>("_settings")
            .await
            .unwrap();
        let name = doc_store.get_string("name").await.unwrap();
        assert_eq!(name, USERS);
    }

    #[tokio::test]
    async fn test_create_databases_tracking() {
        let (instance, device_key, _admin) = setup_instance().await;

        let databases_db = create_databases_tracking(&instance, &device_key)
            .await
            .unwrap();

        // Verify database was created
        assert!(!databases_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = databases_db.new_transaction().await.unwrap();
        let doc_store = transaction
            .get_store::<DocStore>("_settings")
            .await
            .unwrap();
        let name = doc_store.get_string("name").await.unwrap();
        assert_eq!(name, DATABASES);
    }

    #[tokio::test]
    async fn test_system_databases_haveadmin_auth() {
        let (instance, device_key, _admin) = setup_instance().await;

        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Verify device key has admin access - key is stored by pubkey
        let device_pubkey = instance.id();
        let transaction = users_db.new_transaction().await.unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();
        let auth_settings = settings_store.auth_snapshot().await.unwrap();
        let device_key = auth_settings.get_key_by_pubkey(&device_pubkey).unwrap();

        assert_eq!(device_key.permissions(), &Permission::Admin(0));
        assert_eq!(device_key.name(), None);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_create_user() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Create an account with password
        let (login_id, account_info, _) =
            create_account(&users_db, &instance, "alice", Some("password123"))
                .await
                .unwrap();

        // Verify account info
        assert_eq!(account_info.username, "alice");
        assert_eq!(account_info.status, AccountStatus::Active);
        assert!(account_info.credentials.password_salt.is_some());
        assert!(matches!(
            account_info.credentials.root_key,
            KeyStorage::Encrypted { .. }
        ));
        assert!(!login_id.is_empty());

        // Verify native registration is durable
        let (stored_id, stored_user) = lookup_account_record(&users_db, &account_info.username)
            .await
            .unwrap();
        assert_eq!(stored_id, login_id);
        assert_eq!(stored_user.username, "alice");
    }

    #[tokio::test]
    async fn test_create_user_passwordless() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Create a passwordless account
        let (login_id, account_info, _) = create_account(&users_db, &instance, "bob", None)
            .await
            .unwrap();

        // Verify account info
        assert_eq!(account_info.username, "bob");
        assert_eq!(account_info.status, AccountStatus::Active);
        assert!(account_info.credentials.password_salt.is_none());
        assert!(matches!(
            account_info.credentials.root_key,
            KeyStorage::Unencrypted { .. }
        ));
        assert!(!login_id.is_empty());

        // Verify native registration is durable
        let (stored_id, stored_user) = lookup_account_record(&users_db, &account_info.username)
            .await
            .unwrap();
        assert_eq!(stored_id, login_id);
        assert_eq!(stored_user.username, "bob");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_create_duplicate_user() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Create first account
        create_account(&users_db, &instance, "alice", Some("password123"))
            .await
            .unwrap();

        // Try to create duplicate
        let result = create_account(&users_db, &instance, "alice", Some("password456")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_login_user() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Create an account with password
        create_account(&users_db, &instance, "bob", Some("bobpassword"))
            .await
            .unwrap();

        // Login account
        let user = login_account(&users_db, &instance, "bob", Some("bobpassword"))
            .await
            .unwrap();

        // Verify account session
        assert_eq!(user.username(), "bob");

        // Verify native last_login was recorded in native local state
        let last_login = super::super::local_state::read(&instance, user.login_id())
            .await
            .unwrap()
            .last_login
            .unwrap();
        assert!(last_login > 0);
    }

    #[tokio::test]
    async fn test_login_user_passwordless() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Create a passwordless account
        create_account(&users_db, &instance, "charlie", None)
            .await
            .unwrap();

        // Login account without password
        let user = login_account(&users_db, &instance, "charlie", None)
            .await
            .unwrap();

        // Verify account session
        assert_eq!(user.username(), "charlie");

        // Verify native last_login was recorded
        let last_login = super::super::local_state::read(&instance, user.login_id())
            .await
            .unwrap()
            .last_login
            .unwrap();
        assert!(last_login > 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_login_wrong_password() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Create an account
        create_account(&users_db, &instance, "dave", Some("correct_password"))
            .await
            .unwrap();

        // Try to login with wrong password
        let result = login_account(&users_db, &instance, "dave", Some("wrong_password")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_login_password_mismatch() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Create a passwordless account
        create_account(&users_db, &instance, "eve", None)
            .await
            .unwrap();

        // Try to login with password (should fail)
        let result = login_account(&users_db, &instance, "eve", Some("password")).await;
        assert!(result.is_err());

        // Create a password-protected account
        create_account(&users_db, &instance, "frank", Some("password"))
            .await
            .unwrap();

        // Try to login without password (should fail)
        let result = login_account(&users_db, &instance, "frank", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_login_nonexistent_user() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Try to login account that doesn't exist
        let result = login_account(&users_db, &instance, "nonexistent", Some("password")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_list_users() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // The installation already has its bootstrap admin.
        let users = list_accounts(&users_db).await.unwrap();
        assert_eq!(users, ["admin"]);

        // Create some accounts (mix of password-protected and passwordless)
        create_account(&users_db, &instance, "alice", Some("pass1"))
            .await
            .unwrap();
        create_account(&users_db, &instance, "bob", None)
            .await
            .unwrap();
        create_account(&users_db, &instance, "charlie", Some("pass3"))
            .await
            .unwrap();

        // List accounts
        let users = list_accounts(&users_db).await.unwrap();
        assert_eq!(users.len(), 4);
        assert!(users.contains(&"alice".into()));
        assert!(users.contains(&"bob".into()));
        assert!(users.contains(&"charlie".into()));
    }

    /// Read the auth permission for `pubkey` from a system database. Returns
    /// `None` if the key isn't registered. Test-only helper.
    async fn read_admin_permission(database: &Database, pubkey: &PublicKey) -> Option<Permission> {
        let tx = database.new_transaction().await.unwrap();
        let settings = SettingsStore::new(&tx).unwrap();
        let auth = settings.auth_snapshot().await.unwrap();
        auth.get_key_by_pubkey(pubkey)
            .ok()
            .map(|k| *k.permissions())
    }

    /// The bootstrapped admin/admin account is the instance admin: its pubkey
    /// lands as `Admin(0)` in both `_users` and `_databases`.
    #[tokio::test]
    async fn test_first_user_becomes_instance_admin() {
        let (instance, _device_key, _admin) = setup_instance().await;

        // Admin is already bootstrapped — login and verify
        let admin_user = instance.login_account("admin", None).await.unwrap();

        let users_db = instance.users_db().await.unwrap();
        let databases_db = instance.databases_db().await.unwrap();

        let admin_pubkey = admin_user.key_manager().get_default_key_id().unwrap();

        assert_eq!(
            read_admin_permission(&users_db, &admin_pubkey).await,
            Some(Permission::Admin(0)),
            "bootstrapped admin should be Admin(0) in _users"
        );
        assert_eq!(
            read_admin_permission(&databases_db, &admin_pubkey).await,
            Some(Permission::Admin(0)),
            "bootstrapped admin should be Admin(0) in _databases"
        );
    }

    /// After the first account has bootstrapped, subsequent accounts land as
    /// non-admins. This test locks in that the auto-bootstrap doesn't fire a
    /// second time.
    #[tokio::test]
    async fn test_subsequent_users_are_not_admin() {
        let (instance, _device_key, admin) = setup_instance().await;

        // Admin creates two more accounts via the admin path. They land as
        // non-admins because admin (the first account) already holds Admin(0)
        // on the system DBs.
        let admin_view = admin.admin().await.unwrap();
        admin_view
            .create_account(crate::NewAccount::passwordless("alice"))
            .await
            .unwrap();
        let bob_uuid = admin_view
            .create_account(crate::NewAccount::passwordless("bob"))
            .await
            .unwrap();

        let users_db = instance.users_db().await.unwrap();
        let databases_db = instance.databases_db().await.unwrap();

        // Bob's root pubkey lives in `AccountInfo.credentials.root_key_id` —
        // the native durable source of truth, even before first login.
        let (stored_id, bob_info) = lookup_account_record(&users_db, "bob").await.unwrap();
        assert_eq!(stored_id, bob_uuid);
        let bob_pubkey = bob_info.credentials.root_key_id.clone();

        assert!(
            read_admin_permission(&users_db, &bob_pubkey)
                .await
                .is_none(),
            "second user must not have any entry in _users.auth_settings"
        );
        assert!(
            read_admin_permission(&databases_db, &bob_pubkey)
                .await
                .is_none(),
            "second user must not have any entry in _databases.auth_settings"
        );
    }

    /// `AccountSession::is_admin()` query: the bootstrapped admin is admin, newly created
    /// accounts are not.
    #[tokio::test]
    async fn test_user_is_admin_query() {
        let (instance, _device_key, admin) = setup_instance().await;

        // Bootstrapped admin reports is_admin = true
        assert!(
            admin.is_admin().await.unwrap(),
            "bootstrapped admin must report is_admin = true"
        );

        // A newly created account reports is_admin = false
        admin
            .admin()
            .await
            .unwrap()
            .create_account(crate::NewAccount::passwordless("alice"))
            .await
            .unwrap();
        let alice = instance.login_account("alice", None).await.unwrap();
        assert!(
            !alice.is_admin().await.unwrap(),
            "newly created user must report is_admin = false"
        );
    }

    /// The bootstrapped admin can promote another account: the new admin's pubkey
    /// lands as `Admin(0)` on both `_users` and `_databases`, and the
    /// promoted account then reports `is_admin()`.
    #[tokio::test]
    async fn test_admin_can_promote_user() {
        let (instance, _device_key, admin) = setup_instance().await;

        admin
            .admin()
            .await
            .unwrap()
            .create_account(crate::NewAccount::passwordless("bob"))
            .await
            .unwrap();
        let bob = instance.login_account("bob", None).await.unwrap();
        let bob_pubkey = bob.key_manager().get_default_key_id().unwrap();

        assert!(
            !bob.is_admin().await.unwrap(),
            "precondition: bob starts as a non-admin"
        );

        // Admin promotes bob
        admin
            .admin()
            .await
            .unwrap()
            .grant_instance_admin(&bob_pubkey)
            .await
            .unwrap();

        let users_db = instance.users_db().await.unwrap();
        let databases_db = instance.databases_db().await.unwrap();
        assert_eq!(
            read_admin_permission(&users_db, &bob_pubkey).await,
            Some(Permission::Admin(0)),
            "promoted user should be Admin(0) in _users"
        );
        assert_eq!(
            read_admin_permission(&databases_db, &bob_pubkey).await,
            Some(Permission::Admin(0)),
            "promoted user should be Admin(0) in _databases"
        );
        assert!(
            bob.is_admin().await.unwrap(),
            "promoted user must now report is_admin = true"
        );
    }

    /// A non-admin cannot promote anyone: the attempt fails with
    /// `InsufficientPermissions` and writes nothing.
    #[tokio::test]
    async fn test_non_admin_cannot_promote() {
        let (instance, _device_key, admin) = setup_instance().await;

        // The bootstrapped `admin` is the first/only admin. Admin creates
        // bob and charlie as non-admin accounts.
        let admin_view = admin.admin().await.unwrap();
        admin_view
            .create_account(crate::NewAccount::passwordless("bob"))
            .await
            .unwrap();
        admin_view
            .create_account(crate::NewAccount::passwordless("charlie"))
            .await
            .unwrap();
        let bob = instance.login_account("bob", None).await.unwrap();
        let charlie = instance.login_account("charlie", None).await.unwrap();
        let charlie_pubkey = charlie.key_manager().get_default_key_id().unwrap();

        // A non-admin can't even obtain the admin view — the privilege
        // boundary is enforced at `AccountSession::admin`, before any write.
        let err = bob
            .admin()
            .await
            .expect_err("non-admin must not be able to promote");
        assert!(
            matches!(
                &err,
                crate::Error::Account(e)
                    if matches!(e.as_ref(), crate::account::AccountError::InsufficientPermissions)
            ),
            "expected InsufficientPermissions, got: {err:?}"
        );

        let users_db = instance.users_db().await.unwrap();
        assert!(
            read_admin_permission(&users_db, &charlie_pubkey)
                .await
                .is_none(),
            "a failed promotion must not write to _users.auth_settings"
        );
    }

    /// Promotion is idempotent, and a freshly-promoted admin can itself
    /// promote further admins — exercising the admin-keyed write path
    /// (not the device key) end to end.
    #[tokio::test]
    async fn test_grant_instance_admin_idempotent_and_chains() {
        let (instance, _device_key, admin) = setup_instance().await;

        admin
            .admin()
            .await
            .unwrap()
            .create_account(crate::NewAccount::passwordless("bob"))
            .await
            .unwrap();
        let bob = instance.login_account("bob", None).await.unwrap();
        let bob_pubkey = bob.key_manager().get_default_key_id().unwrap();

        admin
            .admin()
            .await
            .unwrap()
            .grant_instance_admin(&bob_pubkey)
            .await
            .unwrap();
        admin
            .admin()
            .await
            .unwrap()
            .grant_instance_admin(&bob_pubkey)
            .await
            .unwrap(); // idempotent: no error

        let users_db = instance.users_db().await.unwrap();
        assert_eq!(
            read_admin_permission(&users_db, &bob_pubkey).await,
            Some(Permission::Admin(0)),
            "re-granting an existing admin re-asserts the same entry"
        );

        // bob, now an admin, promotes charlie signing with bob's *own* key —
        // the device key is never involved here.
        bob.admin()
            .await
            .unwrap()
            .create_account(crate::NewAccount::passwordless("charlie"))
            .await
            .unwrap();
        let charlie = instance.login_account("charlie", None).await.unwrap();
        let charlie_pubkey = charlie.key_manager().get_default_key_id().unwrap();

        bob.admin()
            .await
            .unwrap()
            .grant_instance_admin(&charlie_pubkey)
            .await
            .unwrap();

        let databases_db = instance.databases_db().await.unwrap();
        assert_eq!(
            read_admin_permission(&databases_db, &charlie_pubkey).await,
            Some(Permission::Admin(0)),
            "a promoted admin can promote further admins via their own key"
        );
    }
}
