use std::sync::Arc;

use axum::{
    Form, Router,
    extract::{Json as ExtractJson, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};

use eidetica::{
    Instance,
    backend::database::InMemory,
    sync::{
        handler::SyncHandlerImpl,
        protocol::{SyncRequest, SyncResponse},
    },
};
use serde::Deserialize;
use signal_hook::flag as signal_flag;
use tower_cookies::{Cookie, CookieManagerLayer, Cookies};
use tracing_subscriber::EnvFilter;

mod session;
mod templates;

use session::SessionStore;
use templates::DatabaseInfo;

const DB_FILE: &str = "eidetica.json";
const PORT: u16 = 3000;
const DEFAULT_USER: &str = "default";
const SESSION_COOKIE: &str = "eidetica_session";

/// Shared application state
#[derive(Clone)]
struct AppState {
    instance: Arc<Instance>,
    sync_handler: Arc<dyn eidetica::sync::handler::SyncHandler>,
    sessions: SessionStore,
}

/// Login form data
#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: Option<String>,
}

/// Registration form data
#[derive(Deserialize)]
struct RegisterForm {
    username: String,
    password: Option<String>,
    password_confirm: Option<String>,
}

/// Track database form data (bootstrap request)
#[derive(Deserialize)]
struct TrackDatabaseForm {
    database_id: String,
    peer_address: String,
    permission: String, // "read", "write", or "admin"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=info".parse().unwrap()),
        )
        .init();

    // Load or create database instance
    let backend_box: Box<dyn eidetica::backend::BackendImpl> =
        match InMemory::load_from_file(DB_FILE) {
            Ok(backend) => {
                tracing::info!("Loaded database from {DB_FILE}");
                Box::new(backend)
            }
            Err(e) => {
                tracing::warn!("Failed to load database: {e:?}. Creating a new one.");
                Box::new(InMemory::new())
            }
        };

    // Initialize Instance using open API
    let instance = Instance::open(backend_box)?;

    // Enable Sync on the instance (creates/loads sync tree)
    instance.enable_sync()?;
    tracing::info!("Sync enabled on instance");

    // Ensure default user exists (for single-user server mode)
    let user_exists = instance.list_users()?.iter().any(|u| u == DEFAULT_USER);

    if !user_exists {
        tracing::info!("Creating default user '{DEFAULT_USER}'");
        instance.create_user(DEFAULT_USER, None)?;
    }

    // Login as default user to get device key
    let mut default_user = instance.login_user(DEFAULT_USER, None)?;

    // Ensure default user has at least one key
    let user_keys = default_user.list_keys()?;
    let device_key_id = if user_keys.is_empty() {
        tracing::info!("Creating initial device key for default user");
        default_user.add_private_key(Some("Server Device Key"))?
    } else {
        user_keys[0].clone()
    };

    tracing::info!("Using device key: {device_key_id}");

    // Get sync object and enable Iroh transport for peer communication
    let sync = instance.sync().ok_or("Sync not enabled on instance")?;

    // Enable Iroh transport for P2P communication with NAT traversal
    sync.enable_iroh_transport()?;
    tracing::info!("Iroh transport enabled for sync");

    // Start Iroh server for incoming sync requests
    sync.start_server_async("iroh").await?;
    let iroh_address = sync.get_server_address_async().await?;
    tracing::info!("Iroh server started: {}", iroh_address);

    let sync_tree_id = sync.sync_tree_root_id().clone();

    let sync_handler: Arc<dyn eidetica::sync::handler::SyncHandler> =
        Arc::new(SyncHandlerImpl::new(instance.clone(), sync_tree_id));

    // Create session store
    let sessions = SessionStore::new();

    // Create shared application state
    let app_state = AppState {
        instance: Arc::new(instance),
        sync_handler,
        sessions,
    };

    // Build router
    let app = Router::new()
        .route("/", get(handle_root_request))
        .route("/login", get(handle_login_page).post(handle_login_submit))
        .route(
            "/register",
            get(handle_register_page).post(handle_register_submit),
        )
        .route("/logout", post(handle_logout))
        .route("/dashboard", get(handle_dashboard))
        .route("/dashboard/database", get(handle_database_detail))
        .route("/dashboard/track", post(handle_track_database))
        .route("/stats", get(handle_stats_request))
        .route("/api/v0", post(handle_sync_request))
        .layer(CookieManagerLayer::new())
        .with_state(app_state.clone());

    // Set up graceful shutdown signal handling
    let term_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in signal_hook::consts::TERM_SIGNALS {
        let _ = signal_flag::register(*signal, Arc::clone(&term_signal));
    }

    // Bind server
    let addr = format!("0.0.0.0:{PORT}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;

    // Print startup message
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║              Eidetica Sync Server Started                     ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("🌐 Web Interface: http://localhost:{}", local_addr.port());
    println!("🔗 Iroh Node ID:  {}", iroh_address);
    println!();
    println!("Available endpoints:");
    println!("  GET  /             - Redirect to login or dashboard");
    println!("  GET  /login        - Login page");
    println!("  POST /login        - Login submission");
    println!("  GET  /register     - User registration page");
    println!("  POST /register     - User registration submission");
    println!("  GET  /dashboard    - User dashboard (requires login)");
    println!("  POST /dashboard/track - Request database access (requires login)");
    println!("  GET  /stats        - Server statistics");
    println!("  POST /api/v0       - Eidetica sync protocol endpoint");
    println!();
    println!("📝 To connect from chat app, use your Iroh Node ID above");
    println!();
    println!("Press Ctrl+C to shutdown");

    // Clone instance and term_signal for shutdown handler
    let instance_for_shutdown = app_state.instance.clone();
    let term_signal_for_shutdown = term_signal.clone();

    // Start server with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for shutdown signal
            while !term_signal_for_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }

            tracing::info!("Shutdown signal received, saving database...");

            // Save database on shutdown
            if let Some(in_memory_backend) = instance_for_shutdown
                .backend()
                .as_any()
                .downcast_ref::<InMemory>()
            {
                match in_memory_backend.save_to_file(DB_FILE) {
                    Ok(_) => {
                        tracing::info!("Database saved successfully");
                        println!("\nDatabase saved successfully");
                    }
                    Err(e) => {
                        tracing::error!("Failed to save database: {e:?}");
                        eprintln!("Failed to save database: {e:?}");
                    }
                }
            }
        })
        .await?;

    println!("Server shut down");
    Ok(())
}

// ============================================================================
// Authentication Handlers
// ============================================================================

/// Handler for GET / - Root redirect
async fn handle_root_request(State(state): State<AppState>, cookies: Cookies) -> Redirect {
    // Check if user has a valid session
    if let Some(cookie) = cookies.get(SESSION_COOKIE)
        && state.sessions.get_user(cookie.value()).await.is_some()
    {
        return Redirect::to("/dashboard");
    }
    Redirect::to("/login")
}

/// Handler for GET /login - Show login page
async fn handle_login_page(State(state): State<AppState>, cookies: Cookies) -> Response {
    // If already logged in, redirect to dashboard
    if let Some(cookie) = cookies.get(SESSION_COOKIE)
        && state.sessions.get_user(cookie.value()).await.is_some()
    {
        return Redirect::to("/dashboard").into_response();
    }

    Html(templates::login_page(None)).into_response()
}

/// Handler for POST /login - Process login
async fn handle_login_submit(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(form): Form<LoginForm>,
) -> Response {
    // Attempt to login
    let password = form.password.as_deref();
    let login_result = state.instance.login_user(&form.username, password);

    match login_result {
        Ok(user) => {
            // Enable background sync for this user's databases
            // Might be unnecessary after updates to the sync tracking system
            if let Some(sync) = state.instance.sync() {
                let user_uuid = user.user_uuid();
                let user_db_id = user.user_database().root_id().clone();
                if let Err(e) = sync.sync_user(user_uuid, &user_db_id) {
                    tracing::warn!(
                        "Failed to enable background sync for user {}: {}",
                        form.username,
                        e
                    );
                }
            }

            // Create session
            let session_token = state.sessions.create_session(user).await;

            // Set cookie (HTTP-only, Secure if behind HTTPS proxy)
            let mut cookie = Cookie::new(SESSION_COOKIE, session_token);
            cookie.set_http_only(true);
            cookie.set_path("/");
            // Note: Set Secure flag in production behind HTTPS
            // cookie.set_secure(true);

            cookies.add(cookie);

            // Redirect to dashboard
            Redirect::to("/dashboard").into_response()
        }
        Err(e) => {
            // Show error on login page
            let error_msg = format!("Login failed: {}", e);
            Html(templates::login_page(Some(&error_msg))).into_response()
        }
    }
}

/// Handler for POST /logout - Logout and destroy session
async fn handle_logout(State(state): State<AppState>, cookies: Cookies) -> Redirect {
    if let Some(cookie) = cookies.get(SESSION_COOKIE) {
        state.sessions.destroy_session(cookie.value()).await;
        cookies.remove(Cookie::from(SESSION_COOKIE));
    }
    Redirect::to("/login")
}

/// Handler for GET /register - Show registration page
async fn handle_register_page(State(state): State<AppState>, cookies: Cookies) -> Response {
    // If already logged in, redirect to dashboard
    if let Some(cookie) = cookies.get(SESSION_COOKIE)
        && state.sessions.get_user(cookie.value()).await.is_some()
    {
        return Redirect::to("/dashboard").into_response();
    }

    Html(templates::register_page(None)).into_response()
}

/// Handler for POST /register - Process registration
async fn handle_register_submit(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(form): Form<RegisterForm>,
) -> Response {
    // Validate username
    if form.username.is_empty() {
        return Html(templates::register_page(Some("Username cannot be empty"))).into_response();
    }

    // Check if user already exists
    if let Ok(users) = state.instance.list_users()
        && users.iter().any(|u| u == &form.username)
    {
        return Html(templates::register_page(Some("Username already exists"))).into_response();
    }

    // Handle password validation
    let password = if let Some(ref pwd) = form.password {
        if pwd.is_empty() {
            // Treat empty password as None (passwordless)
            None
        } else {
            // Validate password confirmation
            if form.password_confirm.as_deref() != Some(pwd.as_str()) {
                return Html(templates::register_page(Some("Passwords do not match")))
                    .into_response();
            }
            Some(pwd.as_str())
        }
    } else {
        None
    };

    // Create the user
    match state.instance.create_user(&form.username, password) {
        Ok(_) => {
            tracing::info!("Created new user: {}", form.username);

            // Log in the new user automatically
            match state.instance.login_user(&form.username, password) {
                Ok(user) => {
                    // Create session
                    let session_token = state.sessions.create_session(user).await;

                    // Set cookie
                    let mut cookie = Cookie::new(SESSION_COOKIE, session_token);
                    cookie.set_http_only(true);
                    cookie.set_path("/");

                    cookies.add(cookie);

                    // Redirect to dashboard
                    Redirect::to("/dashboard").into_response()
                }
                Err(e) => {
                    // User created but login failed - redirect to login page
                    tracing::error!("User created but auto-login failed: {}", e);
                    Redirect::to("/login").into_response()
                }
            }
        }
        Err(e) => {
            let error_msg = format!("Registration failed: {}", e);
            Html(templates::register_page(Some(&error_msg))).into_response()
        }
    }
}

// ============================================================================
// Dashboard Handlers
// ============================================================================

/// Handler for GET /dashboard - Show user dashboard
async fn handle_dashboard(State(state): State<AppState>, cookies: Cookies) -> Response {
    // Check session
    let session_token = match cookies.get(SESSION_COOKIE) {
        Some(cookie) => cookie.value().to_string(),
        None => return Redirect::to("/login").into_response(),
    };

    let user_lock = match state.sessions.get_user(&session_token).await {
        Some(user) => user,
        None => return Redirect::to("/login").into_response(),
    };

    let user = user_lock.read().await;

    // Get user's tracked database preferences
    let database_prefs = user.list_database_prefs().unwrap_or_default();

    // Convert preferences to display info
    let databases: Vec<DatabaseInfo> = database_prefs
        .iter()
        .map(|prefs| {
            // Try to open the database to get current info
            let db = user.open_database(&prefs.database_id).ok();
            DatabaseInfo::from_user_prefs(prefs, db.as_ref())
        })
        .collect();

    Html(templates::dashboard_page(&user, databases)).into_response()
}

/// Query parameters for database detail
#[derive(Deserialize)]
struct DatabaseQuery {
    id: String,
}

/// Handler for GET /dashboard/database?id=... - Show database details
async fn handle_database_detail(
    State(state): State<AppState>,
    cookies: Cookies,
    Query(query): Query<DatabaseQuery>,
) -> Response {
    // Check session
    let session_token = match cookies.get(SESSION_COOKIE) {
        Some(cookie) => cookie.value().to_string(),
        None => return Redirect::to("/login").into_response(),
    };

    let user_lock = match state.sessions.get_user(&session_token).await {
        Some(user) => user,
        None => return Redirect::to("/login").into_response(),
    };

    let user = user_lock.read().await;

    // Parse database ID
    let database_id = match eidetica::entry::ID::parse(&query.id) {
        Ok(id) => id,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid database ID").into_response();
        }
    };

    // Get database preferences
    let prefs = match user.database_prefs(&database_id) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get database preferences: {}", e),
            )
                .into_response();
        }
    };

    // Open the database
    let db = match user.open_database(&database_id) {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to open database: {}", e),
            )
                .into_response();
        }
    };

    // Get database info
    let db_info = DatabaseInfo::from_user_prefs(&prefs, Some(&db));

    // Get all entries
    let entries: Vec<String> = db
        .get_all_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.id().to_string())
        .collect();

    Html(templates::database_detail_page(&user, db_info, entries)).into_response()
}

/// Handler for POST /dashboard/track - Request database access (bootstrap)
async fn handle_track_database(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(form): Form<TrackDatabaseForm>,
) -> Response {
    // Check session
    let session_token = match cookies.get(SESSION_COOKIE) {
        Some(cookie) => cookie.value().to_string(),
        None => return Redirect::to("/login").into_response(),
    };

    let user_lock = match state.sessions.get_user(&session_token).await {
        Some(user) => user,
        None => return Redirect::to("/login").into_response(),
    };

    let user = user_lock.read().await;

    // Parse the database ID
    let database_id = match eidetica::entry::ID::parse(&form.database_id) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid database ID: {:?}", e),
            )
                .into_response();
        }
    };

    // Get user's default key
    let key_id = match user.get_default_key() {
        Ok(key) => key,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get default key: {}", e),
            )
                .into_response();
        }
    };

    // Parse requested permission
    let permission = match form.permission.as_str() {
        "read" => eidetica::auth::Permission::Read,
        "write" => eidetica::auth::Permission::Write(10),
        "admin" => eidetica::auth::Permission::Admin(0),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid permission: {}", form.permission),
            )
                .into_response();
        }
    };

    // Get sync object from instance
    let sync = match state.instance.sync() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Sync not enabled on instance",
            )
                .into_response();
        }
    };

    // Request database access via bootstrap
    let bootstrap_result = user
        .request_database_access(&sync, &form.peer_address, &database_id, &key_id, permission)
        .await;

    // Drop read lock before acquiring write lock
    drop(user);

    match bootstrap_result {
        Ok(_) => {
            // Bootstrap succeeded - now add database to user's tracked list
            let mut user = user_lock.write().await;

            // Create database preferences with sync enabled and 13-second polling
            let mut properties = std::collections::HashMap::new();
            properties.insert("peer_address".to_string(), form.peer_address.clone());

            let prefs = eidetica::user::DatabasePreferences {
                database_id: database_id.clone(),
                key_id: key_id.clone(),
                sync_settings: eidetica::user::SyncSettings {
                    sync_enabled: true,
                    sync_on_commit: false,
                    interval_seconds: Some(13),
                    properties,
                },
            };

            match user.add_database(prefs) {
                Ok(_) => {
                    tracing::info!(
                        "Successfully bootstrapped and tracked database {} for user {}",
                        form.database_id,
                        user.username()
                    );
                    Redirect::to("/dashboard").into_response()
                }
                Err(e) => {
                    tracing::warn!(
                        "Bootstrapped database {} but failed to add to tracking: {}",
                        form.database_id,
                        e
                    );
                    // Database is bootstrapped but not tracked - still redirect to dashboard
                    Redirect::to("/dashboard").into_response()
                }
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to request database access: {}", e),
        )
            .into_response(),
    }
}

// ============================================================================
// Stats and Sync Handlers
// ============================================================================

/// Handler for GET /stats - Statistics page
async fn handle_stats_request(State(state): State<AppState>) -> Html<String> {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Eidetica Server Statistics</title>
    <style>
        body { font-family: monospace; max-width: 800px; margin: 40px auto; padding: 0 20px; }
        h1 { border-bottom: 2px solid #333; padding-bottom: 10px; }
        h2 { margin-top: 30px; border-bottom: 1px solid #666; padding-bottom: 5px; }
        .stat { margin: 10px 0; }
        .label { font-weight: bold; display: inline-block; width: 200px; }
        .value { color: #0066cc; }
        table { border-collapse: collapse; width: 100%; margin: 15px 0; }
        th, td { text-align: left; padding: 8px; border-bottom: 1px solid #ddd; }
        th { background-color: #f0f0f0; }
    </style>
</head>
<body>
    <h1>Eidetica Server Statistics</h1>
"#,
    );

    // Get instance statistics
    html.push_str("<h2>Instance Overview</h2>\n");

    // Count active sessions
    let session_count = state.sessions.session_count().await;
    html.push_str(&format!(
        r#"<div class="stat"><span class="label">Active Sessions:</span> <span class="value">{}</span></div>"#,
        session_count
    ));

    // Count databases
    let database_count = state
        .instance
        .all_databases()
        .map(|dbs| dbs.len())
        .unwrap_or(0);
    html.push_str(&format!(
        r#"<div class="stat"><span class="label">Total Databases:</span> <span class="value">{}</span></div>"#,
        database_count
    ));

    // List databases
    if let Ok(databases) = state.instance.all_databases() {
        html.push_str("<h2>Databases</h2>\n<table>\n");
        html.push_str("<tr><th>Name</th><th>Root ID</th><th>Entry Count</th></tr>\n");

        for db in databases {
            let name = db.get_name().unwrap_or_else(|_| "Unknown".to_string());
            let root_id = db.root_id().to_string();
            let entry_count = db
                .get_all_entries()
                .map(|entries| entries.len())
                .unwrap_or(0);

            html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                name, root_id, entry_count
            ));
        }

        html.push_str("</table>\n");
    }

    html.push_str("</body>\n</html>");
    Html(html)
}

/// Handler for POST /api/v0 - Eidetica sync endpoint
async fn handle_sync_request(
    State(state): State<AppState>,
    ExtractJson(request): ExtractJson<SyncRequest>,
) -> axum::Json<SyncResponse> {
    let response = state.sync_handler.handle_request(&request).await;
    axum::Json(response)
}
