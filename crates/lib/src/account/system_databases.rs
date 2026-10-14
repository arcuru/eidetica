//! System database initialization for the account system
//!
//! Creates and manages _users and _databases system databases.

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
    store::Table,
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
/// This database stores the account directory mapping user_id -> AccountInfo.
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
    settings.set("description", "AccountSession directory database");

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

/// Create a new account account
///
/// This function:
/// 1. Optionally hashes the account's password (if provided)
/// 2. Generates a device keypair for the account
/// 3. Creates a account database for storing keys (encrypted or unencrypted)
/// 4. Creates AccountInfo and stores it in _users database with auto-generated UUID
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
    // FIXME: Race condition - multiple concurrent creates with same username
    // can both succeed, creating duplicate accounts. This requires either:
    // 1. Distributed locking mechanism
    // 2. Backend-level unique constraints
    // 3. Periodic cleanup/reconciliation process
    // For now, duplicate detection happens at login time.

    // Check if username already exists
    let users_table = users_db
        .get_store_viewer::<Table<AccountInfo>>("users")
        .await?;
    let existing = users_table.search(|u| u.username == username).await?;
    if !existing.is_empty() {
        return Err(AccountError::LoginNameAlreadyExists {
            username: username.to_string(),
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

    // The two historical follow-up writes on the account's tree — granting the
    // device key `Read` on `_settings.auth`, and persisting the root `AccountKey`
    // metadata into the `keys` table — are deferred to first login. The
    // structural reason: this function may run over a remote (admin's)
    // session whose `session_pubkey` is *not* a member of the new account's
    // tree, so `Transaction::commit`'s reads on that tree would be denied by
    // the server-side gate (gating is by connection session_pubkey, not by
    // request identity hint). Deferring is also functionally correct: the
    // device-Read grant only matters once the daemon needs to sync the
    // account's tree on the account's behalf, and the `keys` row is layered on top
    // of the root key already carried in `AccountInfo.credentials` (which is
    // the durable source of truth and reaches the account via `_users`).
    // `build_account_session` performs the idempotent bootstrap on first login.
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

    // 5. Store AccountInfo in _users database with auto-generated UUID
    let tx = users_db.new_transaction().await?;
    let users_table = tx.get_store::<Table<AccountInfo>>("users").await?;
    let login_id = users_table.insert(account_info.clone()).await?; // Generate UUID primary key
    tx.commit().await?;

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

/// Login a account
///
/// This function:
/// 1. Searches for account by username in _users database
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
/// A AccountSession object with keys loaded
pub async fn login_account(
    users_db: &Database,
    instance: &Instance,
    username: impl AsRef<str>,
    password: Option<&str>,
) -> Result<super::AccountSession> {
    let username = username.as_ref();

    // 1. Search for account by username
    let users_table = users_db
        .get_store_viewer::<Table<AccountInfo>>("users")
        .await?;
    let results = users_table.search(|u| u.username == username).await?;

    // Check for duplicate accounts (race condition detection)
    let (login_id, account_info) = match results.len() {
        0 => {
            return Err(AccountError::AccountNotFound {
                username: username.to_string(),
            }
            .into());
        }
        1 => results.into_iter().next().unwrap(),
        count => {
            // FIXME: Multiple accounts with same username detected!
            // This indicates the race condition occurred during account creation.
            // Resolution requires manual intervention or automated cleanup.
            return Err(AccountError::DuplicateAccountsDetected {
                username: username.to_string(),
                count,
            }
            .into());
        }
    };

    // Check if account is disabled
    if account_info.status != AccountStatus::Active {
        return Err(AccountError::AccountDisabled {
            username: username.to_string(),
        }
        .into());
    }

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

    // Last-login bookkeeping. Best-effort: skip the write on remote instances
    // (no local device key to sign with — the daemon owns this side) and
    // downgrade any other failure to a debug log rather than blocking login.
    // The last_login table is treated as a low-priority access log (per the
    // older TODO noting it grows unbounded and should move to a log table);
    // a missed entry is acceptable, a failed login over a transient table
    // error would not be.
    let now = instance.clock().now_secs();
    let user_uuid_ref = login_id.clone();
    if let Err(e) = users_db
        .with_transaction(|tx| async move {
            let last_login_table = tx.get_store::<Table<i64>>("last_login").await?;
            last_login_table.set(&user_uuid_ref, now).await
        })
        .await
    {
        tracing::debug!("skipping last_login update for {username}: {e}");
    }

    Ok(user)
}

/// Build a `AccountSession` session from data already in hand: the decrypted root
/// signing key, the account's record, and an optional password (the
/// `AccountKeyManager` re-derives the KEK from it for any per-key decryption).
///
/// Used by both the local path (`login_account`, after it reads `_users` and
/// decrypts the root key from credentials) and the remote path
/// (`Instance::login_account` over a service connection, after `trusted_login`
/// has already shipped the `AccountInfo` and decrypted the root key on the
/// client). Keeping a single helper means new keys/state added to the AccountSession
/// session are picked up on both paths without divergence.
///
/// Does NOT touch `_users` — that's the caller's responsibility on the local
/// path (last-login bookkeeping). The remote path skips that intentionally:
/// the daemon would be the natural place to record it, and burning a wire
/// write per login is not worth it before the audit-log work lands.
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

    // 2. Load all keys from account database into key manager.
    // TODO: load keys lazily
    let keys_table = account_database
        .get_store_viewer::<Table<AccountKey>>("keys")
        .await?;
    let mut all_keys: Vec<AccountKey> = keys_table
        .search(|_| true)
        .await?
        .into_iter()
        .map(|(_, key)| key)
        .collect();

    // The root key may not yet be persisted in the account-tree `keys` table —
    // `create_account` no longer writes it there (the write would require reads
    // on a tree the creator's session isn't a member of). `AccountInfo.credentials`
    // in `_users` is the durable source of truth; synthesize the in-memory
    // `AccountKey` from it when the table is missing the row. The durable write
    // happens at `bootstrap_account_tree_if_needed` below, signed by the account.
    let root_in_table = all_keys
        .iter()
        .any(|k| k.key_id == account_info.credentials.root_key_id);
    if !root_in_table {
        all_keys.push(AccountKey {
            key_id: account_info.credentials.root_key_id.clone(),
            storage: account_info.credentials.root_key.clone(),
            display_name: Some("Root Key".to_string()),
            created_at: account_info.created_at,
            last_used: None,
            is_default: true,
            database_sigkeys: std::collections::HashMap::new(),
        });
    }

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
    if let Err(e) =
        bootstrap_account_tree_if_needed(instance, &account_database, account_info, root_in_table)
            .await
    {
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

/// Idempotently populate first-login state on the account's own tree.
///
/// Two writes, both Admin(0)-only and signed by the account (who is Admin(0)
/// via `Database::create`'s genesis):
///
/// - **`keys` table** — persist the root `AccountKey` row that mirrors
///   `AccountInfo.credentials`, so `AccountSession::track_database` and other writes
///   that update per-database SigKey mappings have a row to update.
/// - **`_settings.auth`** — grant this instance's device key `Read`, so the
///   daemon can sync the account's tree on the account's behalf.
///
/// Each is gated on a pre-check via the read-only viewer; on a fresh login
/// both checks miss and a single transaction commits both writes, on every
/// subsequent login both checks hit and the function returns without
/// touching the tree.
async fn bootstrap_account_tree_if_needed(
    instance: &Instance,
    account_database: &Database,
    account_info: &AccountInfo,
    root_already_persisted: bool,
) -> Result<()> {
    let device_pubkey = instance.id();

    // Check whether the device key already has an auth entry on _settings.
    // `get_settings()` builds a read-only transaction; `get_auth_key` returns
    // `Err` if the key isn't present.
    let settings_viewer = account_database.get_settings().await?;
    let device_already_granted = settings_viewer.get_auth_key(&device_pubkey).await.is_ok();

    if root_already_persisted && device_already_granted {
        return Ok(());
    }

    // Capture by-value for the move into the closure.
    let root_user_key = if root_already_persisted {
        None
    } else {
        Some(AccountKey {
            key_id: account_info.credentials.root_key_id.clone(),
            storage: account_info.credentials.root_key.clone(),
            display_name: Some("Root Key".to_string()),
            created_at: account_info.created_at,
            last_used: None,
            is_default: true,
            database_sigkeys: std::collections::HashMap::new(),
        })
    };
    let device_grant = if device_already_granted {
        None
    } else {
        Some((
            device_pubkey,
            AuthKey::active(Some("device"), Permission::Read),
        ))
    };

    account_database
        .with_transaction(|tx| async move {
            if let Some(row) = root_user_key {
                let keys_table = tx.get_store::<Table<AccountKey>>("keys").await?;
                keys_table.insert(row).await?;
            }
            if let Some((pubkey, auth_key)) = device_grant {
                let settings = tx.get_settings()?;
                settings.set_auth_key(&pubkey, auth_key).await?;
            }
            Ok(())
        })
        .await?;

    Ok(())
}

/// Look up a account's record by username, without requiring the password.
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
    let username = username.as_ref();
    let users_table = users_db
        .get_store_viewer::<Table<AccountInfo>>("users")
        .await?;
    let results = users_table.search(|u| u.username == username).await?;

    let (login_id, account_info) = match results.len() {
        0 => {
            return Err(AccountError::AccountNotFound {
                username: username.to_string(),
            }
            .into());
        }
        1 => results.into_iter().next().unwrap(),
        count => {
            return Err(AccountError::DuplicateAccountsDetected {
                username: username.to_string(),
                count,
            }
            .into());
        }
    };

    if account_info.status != AccountStatus::Active {
        return Err(AccountError::AccountDisabled {
            username: username.to_string(),
        }
        .into());
    }

    Ok((login_id, account_info))
}

/// List all accounts in the system
///
/// # Arguments
/// * `users_db` - The _users system database
///
/// # Returns
/// Vector of usernames
pub async fn list_accounts(users_db: &Database) -> Result<Vec<String>> {
    let users_table = users_db
        .get_store_viewer::<Table<AccountInfo>>("users")
        .await?;
    let users: Vec<AccountInfo> = users_table
        .search(|_| true)
        .await? // Get all users
        .into_iter()
        .map(|(_, user)| user)
        .collect();
    Ok(users.into_iter().map(|u| u.username).collect())
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

        // Create a account with password
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

        // Verify account was stored in _users database
        let users_table = users_db
            .get_store_viewer::<Table<AccountInfo>>("users")
            .await
            .unwrap();
        let stored_user = users_table.get(&login_id).await.unwrap();
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

        // Verify account was stored in _users database
        let users_table = users_db
            .get_store_viewer::<Table<AccountInfo>>("users")
            .await
            .unwrap();
        let stored_user = users_table.get(&login_id).await.unwrap();
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

        // Create a account with password
        create_account(&users_db, &instance, "bob", Some("bobpassword"))
            .await
            .unwrap();

        // Login account
        let user = login_account(&users_db, &instance, "bob", Some("bobpassword"))
            .await
            .unwrap();

        // Verify account session
        assert_eq!(user.username(), "bob");

        // Verify last_login was recorded in separate table
        let last_login_table = users_db
            .get_store_viewer::<Table<i64>>("last_login")
            .await
            .unwrap();
        let last_login = last_login_table.get(user.login_id()).await.unwrap();
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

        // Verify last_login was recorded
        let last_login_table = users_db
            .get_store_viewer::<Table<i64>>("last_login")
            .await
            .unwrap();
        let last_login = last_login_table.get(user.login_id()).await.unwrap();
        assert!(last_login > 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_login_wrong_password() {
        let (instance, device_key, _admin) = setup_instance().await;
        let users_db = create_accounts_database(&instance, &device_key)
            .await
            .unwrap();

        // Create a account
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

        // Initially no accounts
        let users = list_accounts(&users_db).await.unwrap();
        assert_eq!(users.len(), 0);

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
        assert_eq!(users.len(), 3);
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
        // the durable source of truth. The account-tree `keys` table is
        // populated only at first login (deferred bootstrap), so reading
        // it here would race a `bob` that hasn't logged in yet.
        let users_table = users_db
            .get_store_viewer::<Table<AccountInfo>>("users")
            .await
            .unwrap();
        let bob_info = users_table.get(&bob_uuid).await.unwrap();
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
