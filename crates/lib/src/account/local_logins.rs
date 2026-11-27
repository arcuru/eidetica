//! Native installation login directory. Nothing here is an Entry or sync input.
use super::{AccountCredentials, AccountError, AccountInfo, AccountStatus, KeyStorage};
use crate::{Database, ID, Instance, Result, auth::crypto::PrivateKey};
use serde::{Deserialize, Serialize};

const PREFIX: &str = "login:";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRecord {
    login_id: String,
    info: AccountInfo,
}

/// Wrap exactly one installation key for the ordinary login path.
pub(crate) async fn register_key(
    instance: &Instance,
    account_id: &ID,
    username: &str,
    password: &str,
    key: PrivateKey,
) -> Result<(String, AccountInfo)> {
    let info = AccountInfo {
        username: username.to_owned(),
        account_id: account_id.clone(),
        credentials: AccountCredentials {
            root_key_id: key.public_key(),
            bundle: KeyStorage::seal(account_id, &[key], password)?,
        },
        created_at: instance.clock().now_secs(),
        status: AccountStatus::Active,
    };
    let login_id = uuid::Uuid::new_v4().to_string();
    register(instance, &login_id, &info).await?;
    Ok((login_id, info))
}

pub(crate) async fn register(
    instance: &Instance,
    login_id: &str,
    info: &AccountInfo,
) -> Result<()> {
    #[cfg(all(unix, feature = "service"))]
    if let Some(connection) = instance.remote_connection() {
        return connection.register_local_account(login_id, info).await;
    }
    let bytes = serde_json::to_vec(&LoginRecord {
        login_id: login_id.to_owned(),
        info: info.clone(),
    })
    .map_err(|_| AccountError::InvalidLocalState)?;
    if !instance
        .require_local_engine()?
        .compare_exchange_local_account_record(&format!("{PREFIX}{}", info.username), None, &bytes)
        .await?
    {
        return Err(AccountError::LoginNameAlreadyExists {
            username: info.username.clone(),
        }
        .into());
    }
    Ok(())
}

pub(crate) async fn lookup(instance: &Instance, username: &str) -> Result<(String, AccountInfo)> {
    let bytes = instance
        .require_local_engine()?
        .local_account_record(&format!("{PREFIX}{username}"))
        .await?
        .ok_or_else(|| AccountError::AccountNotFound {
            username: username.to_owned(),
        })?;
    let record: LoginRecord =
        serde_json::from_slice(&bytes).map_err(|_| AccountError::InvalidLocalState)?;
    if record.info.username != username || record.login_id.is_empty() {
        return Err(AccountError::InvalidLocalState.into());
    }
    if record.info.status != AccountStatus::Active {
        return Err(AccountError::AccountDisabled {
            username: username.to_owned(),
        }
        .into());
    }
    Ok((record.login_id, record.info))
}

pub(crate) async fn list(users_db: &Database) -> Result<Vec<String>> {
    let instance = users_db.instance()?;
    #[cfg(all(unix, feature = "service"))]
    if let Some(connection) = instance.remote_connection() {
        return connection.list_local_accounts().await;
    }
    Ok(instance
        .require_local_engine()?
        .local_account_names()
        .await?
        .into_iter()
        .filter_map(|name| name.strip_prefix(PREFIX).map(str::to_owned))
        .collect())
}

/// Internal daemon enumeration includes disabled logins without unlocking keys.
pub(crate) async fn records(instance: &Instance) -> Result<Vec<(String, AccountInfo)>> {
    let backend = instance.require_local_engine()?;
    let mut records = Vec::new();
    for name in backend.local_account_names().await? {
        if let Some(username) = name.strip_prefix(PREFIX) {
            let bytes = backend
                .local_account_record(&name)
                .await?
                .ok_or(AccountError::InvalidLocalState)?;
            let record: LoginRecord =
                serde_json::from_slice(&bytes).map_err(|_| AccountError::InvalidLocalState)?;
            if record.info.username != username || record.login_id.is_empty() {
                return Err(AccountError::InvalidLocalState.into());
            }
            records.push((record.login_id, record.info));
        }
    }
    Ok(records)
}
