use std::io::{self, Write};

use chrono::{DateTime, Utc};
use eidetica::{
    Database, Instance, Result, crdt::Doc, service::default_socket_url, store::Table, user::User,
};
use serde::{Deserialize, Serialize};

const DATABASE_NAME: &str = "shistory";
const HOSTS_STORE: &str = "hosts";
const HISTORY_STORE_PREFIX: &str = "history-";
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub command: String,
    pub started_at: DateTime<Utc>,
    pub cwd: String,
    pub duration_ms: Option<i64>,
    pub exit_status: Option<i32>,
    pub host: String,
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Host {
    label: String,
}

pub async fn connect(username: &str) -> Result<(Instance, User)> {
    connect_to(default_socket_url(), username).await
}

pub async fn connect_to(socket_url: impl AsRef<str>, username: &str) -> Result<(Instance, User)> {
    let socket_url = socket_url.as_ref();
    let instance = Instance::connect(socket_url).await.map_err(|error| {
        eidetica::store::StoreError::InvalidOperation {
            store: "shistory".to_owned(),
            operation: "connect".to_owned(),
            reason: format!("Eidetica daemon at {socket_url} is unavailable: {error}"),
        }
    })?;
    let user = instance.login_user(username, None).await?;
    Ok((instance, user))
}

pub async fn setup(user: &mut User, host: &str) -> Result<Database> {
    validate_host(host)?;
    let database = load_or_create_database(user).await?;
    let transaction = database.new_transaction().await?;
    transaction
        .get_store::<Table<Host>>(HOSTS_STORE)
        .await?
        .set(
            host,
            Host {
                label: host.to_owned(),
            },
        )
        .await?;
    transaction.commit().await?;
    Ok(database)
}

pub async fn start(
    database: &Database,
    host: &str,
    session: &str,
    cwd: &str,
    command: &str,
    started_at: DateTime<Utc>,
) -> Result<Option<String>> {
    validate_host(host)?;
    if command.starts_with(' ') {
        return Ok(None);
    }

    let transaction = database.new_transaction().await?;
    let id = transaction
        .get_store::<Table<HistoryEntry>>(&history_store(host))
        .await?
        .insert(HistoryEntry {
            command: command.to_owned(),
            started_at,
            cwd: cwd.to_owned(),
            duration_ms: None,
            exit_status: None,
            host: host.to_owned(),
            session: session.to_owned(),
        })
        .await?;
    transaction.commit().await?;
    Ok(Some(id))
}

pub async fn finish(
    database: &Database,
    host: &str,
    id: &str,
    duration_ms: i64,
    exit_status: i32,
) -> Result<()> {
    validate_host(host)?;
    let transaction = database.new_transaction().await?;
    let history = transaction
        .get_store::<Table<HistoryEntry>>(&history_store(host))
        .await?;
    let mut entry = history.get(id).await?;
    if entry.host != host {
        return Err(eidetica::store::StoreError::InvalidOperation {
            store: history_store(host),
            operation: "finish".to_owned(),
            reason: "record belongs to a different host".to_owned(),
        }
        .into());
    }
    entry.duration_ms = Some(duration_ms.max(0));
    entry.exit_status = Some(exit_status);
    history.set(id, entry).await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn query(
    database: &Database,
    host: Option<&str>,
    text: Option<&str>,
    limit: usize,
) -> Result<Vec<HistoryEntry>> {
    if limit == 0 || limit > MAX_LIMIT {
        return Err(eidetica::store::StoreError::InvalidConfiguration {
            store: "shistory".to_owned(),
            reason: format!("limit must be between 1 and {MAX_LIMIT}"),
        }
        .into());
    }

    let hosts = match host {
        Some(host) => {
            validate_host(host)?;
            vec![host.to_owned()]
        }
        None => configured_hosts(database).await?,
    };
    let mut entries = Vec::new();
    for host in hosts {
        let history = database
            .get_store_viewer::<Table<HistoryEntry>>(&history_store(&host))
            .await?;
        entries.extend(
            history
                .search(|entry| text.is_none_or(|text| entry.command.contains(text)))
                .await?
                .into_iter()
                .map(|(_, entry)| entry),
        );
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.started_at));
    entries.truncate(limit);
    Ok(entries)
}

pub fn print_entries(mut output: impl Write, entries: &[HistoryEntry]) -> io::Result<()> {
    for entry in entries {
        let status = entry
            .exit_status
            .map_or_else(|| "incomplete".to_owned(), |status| status.to_string());
        writeln!(
            output,
            "{}\t{}\t{}\t{}\t{}\t{}",
            entry.started_at.to_rfc3339(),
            entry.host,
            status,
            entry
                .duration_ms
                .map_or_else(|| "-".to_owned(), |ms| ms.to_string()),
            entry.cwd,
            entry.command
        )?;
    }
    Ok(())
}

async fn load_or_create_database(user: &mut User) -> Result<Database> {
    match user.find_database(DATABASE_NAME).await {
        Ok(mut databases) if databases.len() == 1 => Ok(databases.pop().unwrap()),
        Ok(databases) => Err(eidetica::store::StoreError::InvalidConfiguration {
            store: DATABASE_NAME.to_owned(),
            reason: format!(
                "found {} tracked databases named {DATABASE_NAME}; keep exactly one",
                databases.len()
            ),
        }
        .into()),
        Err(error) if error.is_not_found() => {
            let mut settings = Doc::new();
            settings.set("name", DATABASE_NAME);
            let key = user.get_default_key()?;
            user.create_database(settings, &key).await
        }
        Err(error) => Err(error),
    }
}

pub async fn open_database(user: &User) -> Result<Database> {
    let mut databases = user.find_database(DATABASE_NAME).await?;
    if databases.len() != 1 {
        return Err(eidetica::store::StoreError::InvalidConfiguration {
            store: DATABASE_NAME.to_owned(),
            reason: format!(
                "found {} tracked databases named {DATABASE_NAME}; keep exactly one",
                databases.len()
            ),
        }
        .into());
    }
    Ok(databases.pop().unwrap())
}

async fn configured_hosts(database: &Database) -> Result<Vec<String>> {
    let hosts = database
        .get_store_viewer::<Table<Host>>(HOSTS_STORE)
        .await?
        .search(|_| true)
        .await?;
    let mut labels = hosts
        .into_iter()
        .map(|(_, host)| host.label)
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    Ok(labels)
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(eidetica::store::StoreError::InvalidConfiguration {
            store: HOSTS_STORE.to_owned(),
            reason:
                "host must be a non-empty label containing only letters, digits, '.', '-', or '_'"
                    .to_owned(),
        }
        .into());
    }
    Ok(())
}

fn history_store(host: &str) -> String {
    format!("{HISTORY_STORE_PREFIX}{host}")
}
