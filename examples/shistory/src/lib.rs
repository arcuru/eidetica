use std::io::{self, Write};

use chrono::{DateTime, Utc};
use eidetica::{
    Database, Instance, Result,
    auth::{AuthKey, Permission},
    crdt::Doc,
    service::default_socket_url,
    store::Table,
    sync::DatabaseTicket,
    user::{SyncSettings, User},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    pub host_id: Uuid,
    #[serde(skip)]
    pub host_name: String,
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub id: Uuid,
    pub name: String,
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

pub async fn setup(user: &mut User, ticket: Option<&DatabaseTicket>, name: &str) -> Result<Host> {
    validate_name(name)?;
    let database = match ticket {
        Some(ticket) => user.join_database(ticket, Permission::Write(10)).await?,
        None => create_database(user).await?,
    };
    let host = Host {
        id: Uuid::new_v4(),
        name: name.to_owned(),
    };
    save_host(&database, &host).await?;
    Ok(host)
}

pub async fn ticket(user: &mut User) -> Result<DatabaseTicket> {
    let database = open_database(user).await?;
    user.share_database(database.root_id()).await
}

pub async fn rename(database: &Database, host_id: Uuid, name: &str) -> Result<()> {
    validate_name(name)?;
    let transaction = database.new_transaction().await?;
    let hosts = transaction.get_store::<Table<Host>>(HOSTS_STORE).await?;
    let mut host = hosts.get(&host_id.to_string()).await?;
    host.name = name.to_owned();
    hosts.set(&host_id.to_string(), host).await?;
    transaction.commit().await.map(|_| ())
}

async fn save_host(database: &Database, host: &Host) -> Result<()> {
    let transaction = database.new_transaction().await?;
    transaction
        .get_store::<Table<Host>>(HOSTS_STORE)
        .await?
        .set(&host.id.to_string(), host.clone())
        .await?;
    transaction.commit().await.map(|_| ())
}

pub async fn host(database: &Database, host_id: Uuid) -> Result<Host> {
    database
        .get_store_viewer::<Table<Host>>(HOSTS_STORE)
        .await?
        .get(&host_id.to_string())
        .await
}

pub async fn start(
    database: &Database,
    host_id: Uuid,
    session: &str,
    cwd: &str,
    command: &str,
    started_at: DateTime<Utc>,
) -> Result<Option<String>> {
    if command.starts_with(' ') {
        return Ok(None);
    }
    host(database, host_id).await?;
    let transaction = database.new_transaction().await?;
    let id = transaction
        .get_store::<Table<HistoryEntry>>(&history_store(host_id))
        .await?
        .insert(HistoryEntry {
            command: command.to_owned(),
            started_at,
            cwd: cwd.to_owned(),
            duration_ms: None,
            exit_status: None,
            host_id,
            host_name: String::new(),
            session: session.to_owned(),
        })
        .await?;
    transaction.commit().await?;
    Ok(Some(id))
}

pub async fn finish(
    database: &Database,
    host_id: Uuid,
    id: &str,
    duration_ms: i64,
    exit_status: i32,
) -> Result<()> {
    let transaction = database.new_transaction().await?;
    let history = transaction
        .get_store::<Table<HistoryEntry>>(&history_store(host_id))
        .await?;
    let mut entry = history.get(id).await?;
    entry.duration_ms = Some(duration_ms.max(0));
    entry.exit_status = Some(exit_status);
    history.set(id, entry).await?;
    transaction.commit().await.map(|_| ())
}

pub async fn query(
    database: &Database,
    host_id: Option<Uuid>,
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
    let host_ids = match host_id {
        Some(host_id) => vec![host_id],
        None => configured_hosts(database).await?,
    };
    let mut entries = Vec::new();
    for host_id in host_ids {
        let host_name = host(database, host_id).await?.name;
        let history = database
            .get_store_viewer::<Table<HistoryEntry>>(&history_store(host_id))
            .await?;
        entries.extend(
            history
                .search(|entry| text.is_none_or(|text| entry.command.contains(text)))
                .await?
                .into_iter()
                .map(|(_, mut entry)| {
                    entry.host_name = host_name.clone();
                    entry
                }),
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
        write!(
            output,
            "{}\t{}\t{}\t{}\t",
            entry.started_at.to_rfc3339(),
            entry.host_name,
            status,
            entry
                .duration_ms
                .map_or_else(|| "-".to_owned(), |ms| ms.to_string()),
        )?;
        write_escaped(&mut output, &entry.cwd)?;
        write!(output, "\t")?;
        write_escaped(&mut output, &entry.command)?;
        writeln!(output)?;
    }
    Ok(())
}

fn write_escaped(mut output: impl Write, value: &str) -> io::Result<()> {
    for character in value.chars() {
        if character.is_control() {
            write!(output, "{}", character.escape_default())?;
        } else {
            write!(output, "{character}")?;
        }
    }
    Ok(())
}

async fn create_database(user: &mut User) -> Result<Database> {
    match user.find_database(DATABASE_NAME).await {
        Ok(_) => Err(eidetica::store::StoreError::InvalidConfiguration {
            store: DATABASE_NAME.to_owned(),
            reason: "history database already exists; omit the ticket only on the first host"
                .to_owned(),
        }
        .into()),
        Err(error) if error.is_not_found() => {
            let mut settings = Doc::new();
            settings.set("name", DATABASE_NAME);
            let key = user.get_default_key()?;
            let database = user.create_database(settings, &key).await?;
            let transaction = database.new_transaction().await?;
            transaction
                .get_settings()?
                .set_auth_key(&key, AuthKey::active(None, Permission::Admin(0)))
                .await?;
            transaction
                .get_settings()?
                .set_global_auth_key(AuthKey::active(None, Permission::Write(10)))
                .await?;
            transaction.commit().await?;
            user.track_database(database.root_id().clone(), &key, SyncSettings::on_commit())
                .await?;
            Ok(database)
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

async fn configured_hosts(database: &Database) -> Result<Vec<Uuid>> {
    database
        .get_store_viewer::<Table<Host>>(HOSTS_STORE)
        .await?
        .search(|_| true)
        .await?
        .into_iter()
        .map(|(_, host)| Ok(host.id))
        .collect()
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(eidetica::store::StoreError::InvalidConfiguration {
            store: HOSTS_STORE.to_owned(),
            reason: "host display name must not be empty".to_owned(),
        }
        .into());
    }
    Ok(())
}

fn history_store(host_id: Uuid) -> String {
    format!("{HISTORY_STORE_PREFIX}{host_id}")
}
