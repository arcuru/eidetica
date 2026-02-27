//! HTML templates for web interface
//!
//! Simple inline HTML templates without a template engine.

use eidetica::{
    Database,
    user::{TrackedDatabase, User},
};

/// Common CSS styles for all pages
const COMMON_STYLES: &str = r#"
    body {
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
        max-width: 1000px;
        margin: 40px auto;
        padding: 0 20px;
        background: #f5f5f5;
    }
    .container {
        background: white;
        padding: 30px;
        border-radius: 8px;
        box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    }
    h1 {
        color: #333;
        border-bottom: 2px solid #0066cc;
        padding-bottom: 10px;
    }
    h2 {
        color: #555;
        margin-top: 30px;
    }
    .info-row {
        margin: 10px 0;
        padding: 8px;
        background: #f9f9f9;
        border-radius: 4px;
    }
    .label {
        font-weight: bold;
        color: #666;
        display: inline-block;
        width: 150px;
    }
    .value {
        color: #0066cc;
    }
    form {
        margin: 20px 0;
    }
    .form-group {
        margin: 15px 0;
    }
    label {
        display: block;
        font-weight: bold;
        margin-bottom: 5px;
        color: #333;
    }
    input[type="text"],
    input[type="password"],
    textarea {
        width: 100%;
        padding: 10px;
        border: 1px solid #ddd;
        border-radius: 4px;
        font-size: 14px;
        box-sizing: border-box;
    }
    textarea {
        font-family: monospace;
        resize: vertical;
    }
    button {
        background: #0066cc;
        color: white;
        padding: 10px 20px;
        border: none;
        border-radius: 4px;
        cursor: pointer;
        font-size: 14px;
        font-weight: bold;
    }
    button:hover {
        background: #0052a3;
    }
    .logout-btn {
        background: #999;
        float: right;
    }
    .logout-btn:hover {
        background: #777;
    }
    table {
        width: 100%;
        border-collapse: collapse;
        margin: 20px 0;
    }
    th, td {
        text-align: left;
        padding: 12px;
        border-bottom: 1px solid #ddd;
    }
    th {
        background: #f0f0f0;
        font-weight: bold;
        color: #333;
    }
    tr:hover {
        background: #f9f9f9;
    }
    .error {
        color: #d9534f;
        background: #f2dede;
        padding: 10px;
        border-radius: 4px;
        margin: 10px 0;
    }
    .code {
        font-family: monospace;
        background: #f5f5f5;
        padding: 2px 6px;
        border-radius: 3px;
        font-size: 13px;
    }
"#;

/// Render the login page
pub fn login_page(error: Option<&str>) -> String {
    let error_html = error.map_or(String::new(), |e| {
        format!(r#"<div class="error">{}</div>"#, html_escape(e))
    });

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Eidetica - Login</title>
    <style>{COMMON_STYLES}</style>
</head>
<body>
    <div class="container">
        <h1>Eidetica Sync Server</h1>
        <h2>Login</h2>
        {error_html}
        <form method="POST" action="/login">
            <div class="form-group">
                <label for="username">Username:</label>
                <input type="text" id="username" name="username" required autofocus>
            </div>
            <div class="form-group">
                <label for="password">Password:</label>
                <input type="password" id="password" name="password">
                <small style="color: #666;">Leave blank for passwordless users</small>
            </div>
            <button type="submit">Login</button>
        </form>
        <p style="margin-top: 20px; text-align: center;">
            Don't have an account? <a href="/register">Register here</a>
        </p>
    </div>
</body>
</html>"#
    )
}

/// Render the registration page
pub fn register_page(error: Option<&str>) -> String {
    let error_html = error.map_or(String::new(), |e| {
        format!(r#"<div class="error">{}</div>"#, html_escape(e))
    });

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Eidetica - Register</title>
    <style>{COMMON_STYLES}</style>
</head>
<body>
    <div class="container">
        <h1>Eidetica Sync Server</h1>
        <h2>Register New Account</h2>
        {error_html}
        <form method="POST" action="/register">
            <div class="form-group">
                <label for="username">Username:</label>
                <input type="text" id="username" name="username" required autofocus
                       pattern="[a-zA-Z0-9_-]+"
                       title="Username must contain only letters, numbers, underscores, and hyphens">
                <small style="color: #666;">Letters, numbers, underscores, and hyphens only</small>
            </div>
            <div class="form-group">
                <label for="password">Password (optional):</label>
                <input type="password" id="password" name="password">
                <small style="color: #666;">Leave blank to create a passwordless account</small>
            </div>
            <div class="form-group">
                <label for="password_confirm">Confirm Password:</label>
                <input type="password" id="password_confirm" name="password_confirm">
                <small style="color: #666;">Required only if you set a password</small>
            </div>
            <button type="submit">Create Account</button>
        </form>
        <p style="margin-top: 20px; text-align: center;">
            Already have an account? <a href="/login">Login here</a>
        </p>
    </div>
</body>
</html>"#
    )
}

/// Render the dashboard page
pub fn dashboard_page(user: &User, databases: Vec<DatabaseInfo>) -> String {
    let databases_html = if databases.is_empty() {
        r#"<p style="color: #666; font-style: italic;">No databases tracked yet.</p>"#.to_string()
    } else {
        let rows: String = databases
            .iter()
            .map(|db| {
                let sync_status = if db.sync_enabled {
                    r#"<span style="color: #28a745;">✓ Enabled</span>"#
                } else {
                    r#"<span style="color: #999;">✗ Disabled</span>"#
                };
                let view_link = format!(
                    r#"<a href="/dashboard/database?id={}" style="color: #0066cc; text-decoration: none;">View</a>"#,
                    html_escape(&db.root_id)
                );
                format!(
                    r#"<tr>
                    <td class="code">{}</td>
                    <td>{}</td>
                    <td>{}</td>
                    <td>{}</td>
                    <td>{}</td>
                </tr>"#,
                    html_escape(&db.root_id),
                    html_escape(&db.name),
                    db.entry_count,
                    sync_status,
                    view_link
                )
            })
            .collect();

        format!(
            r#"<table>
            <tr>
                <th>Database ID</th>
                <th>Name</th>
                <th>Entries</th>
                <th>Sync Status</th>
                <th>Actions</th>
            </tr>
            {rows}
        </table>"#
        )
    };

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Eidetica - Dashboard</title>
    <style>{}</style>
</head>
<body>
    <div class="container">
        <h1>Eidetica Dashboard
            <form method="POST" action="/logout" style="display: inline;">
                <button type="submit" class="logout-btn">Logout</button>
            </form>
        </h1>

        <h2>User Information</h2>
        <div class="info-row">
            <span class="label">Username:</span>
            <span class="value">{}</span>
        </div>
        <div class="info-row">
            <span class="label">User UUID:</span>
            <span class="value code">{}</span>
        </div>

        <h2>Tracked Databases</h2>
        {}

        <h2>Request Database Access</h2>
        <form method="POST" action="/dashboard/track">
            <div class="form-group">
                <label for="ticket">Database Ticket URL:</label>
                <input type="text" id="ticket" name="ticket"
                       placeholder="eidetica:?db=sha256:...&pr=http:host:port" required>
                <small style="color: #666;">
                    Paste a ticket URL shared by the database owner
                </small>
            </div>
            <div class="form-group">
                <label for="permission">Requested Permission:</label>
                <select id="permission" name="permission" required>
                    <option value="read" selected>Read Only</option>
                    <option value="write">Write Access</option>
                    <option value="admin">Admin Access</option>
                </select>
                <small style="color: #666;">
                    The permission level you're requesting from the database owner
                </small>
            </div>
            <button type="submit">Request Access</button>
        </form>
    </div>
</body>
</html>"#,
        COMMON_STYLES,
        html_escape(user.username()),
        html_escape(user.user_uuid()),
        databases_html
    )
}

/// Render the database detail page
pub fn database_detail_page(_user: &User, db_info: DatabaseInfo, entries: Vec<String>) -> String {
    let entries_html = if entries.is_empty() {
        r#"<p style="color: #666; font-style: italic;">No entries in this database yet.</p>"#
            .to_string()
    } else {
        let entry_rows: String = entries
            .iter()
            .take(100) // Limit to first 100 entries
            .map(|entry_id| {
                format!(
                    r#"<tr><td class="code">{}</td></tr>"#,
                    html_escape(entry_id)
                )
            })
            .collect();

        let more_msg = if entries.len() > 100 {
            format!(
                "<p style=\"color: #666;\">Showing first 100 of {} entries</p>",
                entries.len()
            )
        } else {
            String::new()
        };

        format!(
            r#"{more_msg}
            <table>
                <tr><th>Entry ID</th></tr>
                {entry_rows}
            </table>"#
        )
    };

    let sync_status = if db_info.sync_enabled {
        r#"<span style="color: #28a745;">✓ Enabled</span>"#
    } else {
        r#"<span style="color: #999;">✗ Disabled</span>"#
    };

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Eidetica - Database Detail</title>
    <style>{}</style>
</head>
<body>
    <div class="container">
        <h1>
            <a href="/dashboard" style="color: #0066cc; text-decoration: none;">← Dashboard</a>
            <button onclick="location.reload()" style="float: right; padding: 8px 16px; cursor: pointer;">
                🔄 Refresh
            </button>
        </h1>

        <h2>Database: {}</h2>
        <p style="color: #666; font-size: 0.9em; margin-top: -10px;">
            Background sync is active. New entries from peers will appear automatically. Click Refresh to update view.
        </p>

        <div class="info-row">
            <span class="label">Database ID:</span>
            <span class="value code">{}</span>
        </div>
        <div class="info-row">
            <span class="label">Entry Count:</span>
            <span class="value">{}</span>
        </div>
        <div class="info-row">
            <span class="label">Sync Status:</span>
            <span class="value">{}</span>
        </div>
        <div class="info-row">
            <span class="label">Key ID:</span>
            <span class="value code">{}</span>
        </div>

        <h2>Entries</h2>
        {}
    </div>
</body>
</html>"#,
        COMMON_STYLES,
        html_escape(&db_info.name),
        html_escape(&db_info.root_id),
        db_info.entry_count,
        sync_status,
        html_escape(&db_info.key_id),
        entries_html
    )
}

/// Information about a database for display
pub struct DatabaseInfo {
    pub root_id: String,
    pub name: String,
    pub entry_count: usize,
    pub sync_enabled: bool,
    pub key_id: String,
}

impl DatabaseInfo {
    /// Create DatabaseInfo from tracked database and database
    pub async fn from_tracked(tracked: &TrackedDatabase, db: Option<&Database>) -> Self {
        let name = if let Some(d) = db {
            d.get_name().await.ok()
        } else {
            None
        }
        .unwrap_or_else(|| "Unknown".to_string());

        let entry_count = if let Some(d) = db {
            d.get_all_entries()
                .await
                .ok()
                .map(|entries| entries.len())
                .unwrap_or(0)
        } else {
            0
        };

        Self {
            root_id: tracked.database_id.to_string(),
            name,
            entry_count,
            sync_enabled: tracked.sync_settings.sync_enabled,
            key_id: tracked.key_id.clone(),
        }
    }
}

/// Escape HTML special characters
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
