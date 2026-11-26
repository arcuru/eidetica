//! Form fields shared by both dashboard adapters.
use serde::Deserialize;

/// Login form data
#[derive(Deserialize)]
pub(crate) struct LoginForm {
    pub username: String,
    #[serde(default)]
    pub password: String,
    pub generation: Option<String>,
}

/// Track database form data (bootstrap request)
#[derive(Deserialize)]
pub(crate) struct TrackDatabaseForm {
    pub ticket: String,
    pub permission: String,
    pub generation: Option<String>,
}

/// A decision never accepts a replacement requester, permission or priority.
#[derive(Deserialize)]
pub(crate) struct AccessDecisionForm {
    pub database_id: String,
    pub request_id: String,
    pub decision: String,
    pub generation: Option<String>,
}

/// Existing per-account preferences only; never accepts an account or key selector.
#[derive(Deserialize)]
pub(crate) struct SyncPreferencesForm {
    pub id: String,
    pub enabled: bool,
    pub sync_on_commit: bool,
    pub interval_seconds: String,
    pub generation: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DatabaseQuery {
    pub id: String,
}

pub(crate) fn requested_permission(value: &str) -> Option<eidetica::auth::Permission> {
    use eidetica::auth::Permission;
    match value {
        "read" => Some(Permission::Read),
        "write" => Some(Permission::Write(10)),
        "admin" => Some(Permission::Admin(0)),
        _ => None,
    }
}

pub(crate) fn interval(value: &str) -> Result<Option<u64>, &'static str> {
    if value.is_empty() {
        return Ok(None);
    }
    let seconds = value.parse::<u64>().ok().filter(|seconds| {
        *seconds > 0
            && std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(*seconds))
                .is_some()
    });
    seconds.map(Some).ok_or(
        "Interval must be a positive, timer-representable number of seconds, or blank for default",
    )
}
