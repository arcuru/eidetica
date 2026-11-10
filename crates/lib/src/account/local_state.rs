//! Installation-local key metadata and activation choices, never Account Entries.

use super::{AccountKey, TrackedDatabase};
use crate::{ID, Instance, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalAccountState {
    pub credentials: Option<super::KeyStorage>,
    pub keys: BTreeMap<String, AccountKey>,
    pub last_login: Option<i64>,
    pub databases: BTreeMap<String, TrackedDatabase>,
}

fn record_name(login_id: &str) -> String {
    format!("account-state:{login_id}")
}

fn invalid_state() -> crate::Error {
    super::AccountError::InvalidLocalState.into()
}

pub(crate) async fn read_raw(instance: &Instance, login_id: &str) -> Result<Option<Vec<u8>>> {
    #[cfg(all(unix, feature = "service"))]
    if let Some(connection) = instance.remote_connection() {
        return connection.local_account_state(login_id).await;
    }
    instance
        .require_local_engine()?
        .local_account_record(&record_name(login_id))
        .await
}

pub(crate) async fn exchange_raw(
    instance: &Instance,
    login_id: &str,
    expected: Option<&[u8]>,
    value: &[u8],
) -> Result<bool> {
    // Refuse malformed state before enabling a durable writer. No old DAG data
    // is imported or deleted; missing native state starts empty.
    let _: LocalAccountState = serde_json::from_slice(value).map_err(|_| invalid_state())?;
    #[cfg(all(unix, feature = "service"))]
    if let Some(connection) = instance.remote_connection() {
        return connection
            .exchange_local_account_state(login_id, expected, value)
            .await;
    }
    instance
        .require_local_engine()?
        .compare_exchange_local_account_record(&record_name(login_id), expected, value)
        .await
}

pub(crate) async fn read(instance: &Instance, login_id: &str) -> Result<LocalAccountState> {
    match read_raw(instance, login_id).await? {
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| invalid_state()),
        None => Ok(LocalAccountState::default()),
    }
}

/// Retry against the observed bytes, so independent sessions do not lose each
/// other's keys, mappings or preferences. The closure only edits local data and
/// may run more than once; network operations and key generation stay outside.
pub(crate) async fn update(
    instance: &Instance,
    login_id: &str,
    mut edit: impl FnMut(&mut LocalAccountState) -> Result<()>,
) -> Result<LocalAccountState> {
    loop {
        let before = read_raw(instance, login_id).await?;
        let mut state: LocalAccountState = match &before {
            Some(bytes) => serde_json::from_slice(bytes).map_err(|_| invalid_state())?,
            None => LocalAccountState::default(),
        };
        edit(&mut state)?;
        let bytes = serde_json::to_vec(&state).map_err(|_| invalid_state())?;
        if exchange_raw(instance, login_id, before.as_deref(), &bytes).await? {
            return Ok(state);
        }
    }
}

pub(crate) async fn refresh_sync(
    instance: &Instance,
    login_id: &str,
    account_id: &ID,
) -> Result<()> {
    if let Some(sync) = instance.sync() {
        sync.sync_account(login_id, account_id).await?;
    }
    Ok(())
}

#[cfg(all(test, unix, feature = "service"))]
mod tests {
    use super::*;
    use crate::{
        Database, NewAccount,
        account::{AccountCredentials, AccountInfo, AccountStatus, KeyStorage, SyncSettings},
        auth::{
            AuthKey, Permission,
            crypto::{generate_keypair, verify_entry_signature},
        },
        backend::database::InMemory,
        crdt::Doc,
        service::ServiceServer,
        store::DocStore,
    };
    use std::os::unix::fs::PermissionsExt;

    async fn two_installations(connected: bool) -> Result<()> {
        let (first_server, _) = Instance::create_backend(
            Box::new(InMemory::new()),
            NewAccount::with_password("device-one", "first-password"),
        )
        .await?;
        let (second_server, _) = Instance::create_backend(
            Box::new(InMemory::new()),
            NewAccount::passwordless("second-admin"),
        )
        .await?;
        let mut first = first_server
            .login_account("device-one", "first-password")
            .await?;
        let a = first.get_default_key()?;
        let (b_secret, b) = generate_keypair();
        let account = first.account().database().clone();
        let target = first.create_database(Doc::new(), &a).await?;
        for database in [&account, &target] {
            database
                .with_transaction(|tx| {
                    let b = b.clone();
                    async move {
                        tx.get_settings()?
                            .set_auth_key(
                                &b,
                                AuthKey::active(Some("second-device"), Permission::Admin(0)),
                            )
                            .await
                    }
                })
                .await?;
        }
        first.set_reference("app", "root", target.root_id()).await?;
        // Preloaded, independently authorized enrollment fixture. Use the real
        // remote-ingest/verification path, not copied private credentials or
        // synthetic Verified labels. Enrollment UX belongs to its own API.
        for database in [&account, &target] {
            let entries = first_server
                .require_local_engine()?
                .get_tree(database.root_id())
                .await?;
            second_server
                .put_remote_entries(database.root_id(), entries)
                .await?;
            assert_eq!(
                Database::open(&second_server, database.root_id())
                    .await?
                    .with_key(b_secret.clone())
                    .snapshot()
                    .await?,
                database.snapshot().await?
            );
        }
        let info = AccountInfo {
            username: "device-two".to_owned(),
            account_id: account.root_id().clone(),
            credentials: AccountCredentials {
                root_key_id: b.clone(),
                bundle: KeyStorage::seal(account.root_id(), &[b_secret], "second-password")?,
            },
            created_at: second_server.clock().now_secs(),
            status: AccountStatus::Active,
        };
        let second_login = uuid::Uuid::new_v4().to_string();
        super::super::local_logins::register(&second_server, &second_login, &info).await?;
        first_server.enable_sync().await?;
        second_server.enable_sync().await?;
        let dir = tempfile::tempdir()?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let mut shutdowns = Vec::new();
        let mut tasks = Vec::new();
        let mut first_client = first_server.clone();
        let mut second_client = second_server.clone();
        if connected {
            for (server, name, client) in [
                (&first_server, "first.sock", &mut first_client),
                (&second_server, "second.sock", &mut second_client),
            ] {
                let path = dir.path().join(name);
                let service = ServiceServer::bind(server.clone(), &path).await?;
                let (shutdown, rx) = tokio::sync::watch::channel(());
                tasks.push(tokio::spawn(service.run(rx)));
                shutdowns.push(shutdown);
                *client = Instance::connect(format!("unix://{}", path.display())).await?;
            }
        }
        let mut first = first_client
            .login_account("device-one", "first-password")
            .await?;
        let mut second = second_client
            .login_account("device-two", "second-password")
            .await?;
        assert_eq!(first.account().id(), second.account().id());
        assert_eq!(first.list_keys()?, vec![a.clone()]);
        assert_eq!(second.list_keys()?, vec![b.clone()]);
        assert!(second.get_signing_key(&a).is_err());
        assert!(first.get_signing_key(&b).is_err());
        assert_eq!(
            second.reference("app", "root").await?,
            Some(target.root_id().clone())
        );
        assert!(
            second.open_reference("app", "root").await.is_err(),
            "discovery activated a local signer mapping"
        );
        first
            .track_database(target.root_id(), &a, SyncSettings::enabled())
            .await?;
        second
            .track_database(target.root_id(), &b, SyncSettings::disabled())
            .await?;
        assert!(first.is_sync_enabled(target.root_id()).await?);
        assert!(!second.is_sync_enabled(target.root_id()).await?);
        for (user, expected) in [(&first, &a), (&second, &b)] {
            let database = user.open_reference("app", "root").await?.unwrap();
            let tx = database.new_transaction().await?;
            tx.get_store::<DocStore>("data")
                .await?
                .set(user.username(), "written")
                .await?;
            let id = tx.commit().await?;
            verify_entry_signature(&database.get_entry(id).await?, expected)?;
        }
        first.logout()?;
        second.logout()?;
        let first = first_client
            .login_account("device-one", "first-password")
            .await?;
        let second = second_client
            .login_account("device-two", "second-password")
            .await?;
        assert!(first.is_sync_enabled(target.root_id()).await?);
        assert!(!second.is_sync_enabled(target.root_id()).await?);
        assert_eq!(second.get_default_key()?, b);
        for server in [&first_server, &second_server] {
            for entry in server
                .require_local_engine()?
                .get_tree(account.root_id())
                .await?
            {
                assert!(!entry.in_subtree("keys"));
                assert!(!entry.in_subtree("databases"));
            }
        }
        drop(first);
        drop(second);
        drop(first_client);
        drop(second_client);
        drop(shutdowns);
        for task in tasks {
            task.await.unwrap()?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn two_installations_keep_distinct_signers_and_local_activation_embedded() -> Result<()> {
        two_installations(false).await
    }
    #[tokio::test]
    async fn two_installations_keep_distinct_signers_and_local_activation_connected() -> Result<()>
    {
        two_installations(true).await
    }
}
