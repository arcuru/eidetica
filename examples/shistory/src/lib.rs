use std::{
    collections::{BTreeMap, HashSet},
    io::{self, Write},
};

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
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 1000;
pub const COMMAND_DISPLAY_LIMIT: usize = 80;
pub const CWD_DISPLAY_LIMIT: usize = 40;
pub const HOST_DISPLAY_LIMIT: usize = 20;

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
    #[serde(skip)]
    pub entry_id: String,
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySummary {
    pub total: usize,
    pub first_started_at: Option<DateTime<Utc>>,
    pub last_started_at: Option<DateTime<Utc>>,
    pub successes: usize,
    pub failures: usize,
    pub incomplete: usize,
    pub most_common_command: Option<(String, usize)>,
    pub longest_runtime: Option<HistoryEntry>,
    pub machine_counts: Vec<(Uuid, String, usize)>,
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
            entry_id: String::new(),
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
    duplicates: bool,
) -> Result<Vec<HistoryEntry>> {
    validate_limit(limit)?;
    let mut entries = matching_entries(database, host_id, text).await?;
    if !duplicates {
        let mut commands = HashSet::new();
        entries.retain(|entry| commands.insert(entry.command.clone()));
    }
    entries.truncate(limit);
    Ok(entries)
}

pub async fn summarize(database: &Database, host_id: Option<Uuid>) -> Result<HistorySummary> {
    let entries = matching_entries(database, host_id, None).await?;
    Ok(HistorySummary::from_entries(entries))
}

pub fn print_entries(mut output: impl Write, entries: &[HistoryEntry]) -> io::Result<()> {
    writeln!(
        output,
        "start time\thost\texit status\tduration (ms)\tworking directory\tcommand"
    )?;
    for entry in entries {
        let status = entry
            .exit_status
            .map_or_else(|| "incomplete".to_owned(), |status| status.to_string());
        writeln!(
            output,
            "{}\t{}\t{}\t{}\t{}\t{}",
            entry.started_at.to_rfc3339(),
            bounded_escaped(&entry.host_name, HOST_DISPLAY_LIMIT),
            status,
            entry
                .duration_ms
                .map_or_else(|| "-".to_owned(), |ms| ms.to_string()),
            bounded_escaped(&entry.cwd, CWD_DISPLAY_LIMIT),
            bounded_escaped(&entry.command, COMMAND_DISPLAY_LIMIT),
        )?;
    }
    Ok(())
}

pub fn print_summary(mut output: impl Write, summary: &HistorySummary) -> io::Result<()> {
    writeln!(output, "total records: {}", summary.total)?;
    if let (Some(first), Some(last)) = (summary.first_started_at, summary.last_started_at) {
        writeln!(
            output,
            "date range: {} to {}",
            first.to_rfc3339(),
            last.to_rfc3339()
        )?;
    }
    writeln!(
        output,
        "status: {} success, {} failure, {} incomplete",
        summary.successes, summary.failures, summary.incomplete
    )?;
    if let Some((command, count)) = &summary.most_common_command {
        writeln!(
            output,
            "most common command: {} ({count})",
            bounded_escaped(command, COMMAND_DISPLAY_LIMIT)
        )?;
    }
    if let Some(entry) = &summary.longest_runtime {
        writeln!(
            output,
            "longest runtime: {} ms\t{}\t{}",
            entry.duration_ms.unwrap_or_default(),
            bounded_escaped(&entry.host_name, HOST_DISPLAY_LIMIT),
            bounded_escaped(&entry.command, COMMAND_DISPLAY_LIMIT),
        )?;
    }
    writeln!(output, "records by machine:")?;
    for (id, name, count) in &summary.machine_counts {
        writeln!(
            output,
            "{}\t{}\t{count}",
            bounded_escaped(name, HOST_DISPLAY_LIMIT),
            id,
        )?;
    }
    Ok(())
}

impl HistorySummary {
    fn from_entries(entries: Vec<HistoryEntry>) -> Self {
        let mut command_counts = BTreeMap::new();
        let mut machine_counts = BTreeMap::new();
        let mut successes = 0;
        let mut failures = 0;
        let mut incomplete = 0;
        let mut longest_runtime: Option<HistoryEntry> = None;

        for entry in &entries {
            match entry.exit_status {
                Some(0) => successes += 1,
                Some(_) => failures += 1,
                None => incomplete += 1,
            }
            let command = entry.command.split_whitespace().next().unwrap_or("(empty)");
            *command_counts.entry(command.to_owned()).or_insert(0) += 1;
            let machine = machine_counts
                .entry(entry.host_id)
                .or_insert_with(|| (entry.host_name.clone(), 0));
            machine.1 += 1;
            if entry.duration_ms.is_some_and(|duration| {
                longest_runtime.as_ref().is_none_or(|longest| {
                    duration > longest.duration_ms.unwrap_or_default()
                        || (duration == longest.duration_ms.unwrap_or_default()
                            && entry_tie_key(entry) < entry_tie_key(longest))
                })
            }) {
                longest_runtime = Some(entry.clone());
            }
        }

        let most_common_command = command_counts.into_iter().max_by(
            |(left_command, left_count), (right_command, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| right_command.cmp(left_command))
            },
        );
        let first_started_at = entries.last().map(|entry| entry.started_at);
        let last_started_at = entries.first().map(|entry| entry.started_at);
        Self {
            total: entries.len(),
            first_started_at,
            last_started_at,
            successes,
            failures,
            incomplete,
            most_common_command,
            longest_runtime,
            machine_counts: machine_counts
                .into_iter()
                .map(|(id, (name, count))| (id, name, count))
                .collect(),
        }
    }
}

fn entry_tie_key(entry: &HistoryEntry) -> (&str, Uuid, DateTime<Utc>, &str, &str, &str) {
    (
        &entry.command,
        entry.host_id,
        entry.started_at,
        &entry.cwd,
        &entry.session,
        &entry.entry_id,
    )
}

async fn matching_entries(
    database: &Database,
    host_id: Option<Uuid>,
    text: Option<&str>,
) -> Result<Vec<HistoryEntry>> {
    let mut host_ids = match host_id {
        Some(host_id) => vec![host_id],
        None => configured_hosts(database).await?,
    };
    host_ids.sort_unstable();
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
                .map(|(entry_id, mut entry)| {
                    entry.host_name = host_name.clone();
                    entry.entry_id = entry_id;
                    entry
                }),
        );
    }
    entries.sort_by(|left, right| {
        right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| left.host_id.cmp(&right.host_id))
            .then_with(|| left.command.cmp(&right.command))
            .then_with(|| left.cwd.cmp(&right.cwd))
            .then_with(|| left.session.cmp(&right.session))
            .then_with(|| left.exit_status.cmp(&right.exit_status))
            .then_with(|| left.duration_ms.cmp(&right.duration_ms))
            .then_with(|| left.entry_id.cmp(&right.entry_id))
    });
    Ok(entries)
}

fn validate_limit(limit: usize) -> Result<()> {
    if limit == 0 || limit > MAX_LIMIT {
        return Err(eidetica::store::StoreError::InvalidConfiguration {
            store: "shistory".to_owned(),
            reason: format!("limit must be between 1 and {MAX_LIMIT}"),
        }
        .into());
    }
    Ok(())
}

fn bounded_escaped(value: &str, limit: usize) -> String {
    let mut display = String::new();
    for character in value.chars() {
        let escaped = if character.is_control() {
            character.escape_default().to_string()
        } else {
            character.to_string()
        };
        if display.chars().count() + escaped.chars().count() > limit {
            while display.chars().count() >= limit {
                display.pop();
            }
            display.push('…');
            break;
        }
        display.push_str(&escaped);
    }
    display
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
