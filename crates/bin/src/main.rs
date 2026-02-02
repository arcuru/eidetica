use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Form, Router,
    extract::{ConnectInfo, Json as ExtractJson, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use clap::{Parser, ValueEnum};

use eidetica::{
    Instance,
    auth::Permission,
    backend::{
        BackendImpl,
        database::{InMemory, Postgres, Sqlite},
    },
    entry::ID,
    sync::{
        handler::{SyncHandler, SyncHandlerImpl},
        peer_types::Address,
        protocol::{RequestContext, SyncRequest, SyncResponse},
        transports::{http::HttpTransport, iroh::IrohTransport},
    },
    user::{SyncSettings, TrackedDatabase},
};

/// Storage backend type
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    /// SQLite database (default, production-ready)
    Sqlite,
    /// PostgreSQL database (for distributed deployments)
    Postgres,
    /// In-memory with JSON persistence (for development and ephemeral deployments)
    Inmemory,
}

/// Eidetica decentralized database server
#[derive(Parser, Debug)]
#[command(name = "eidetica")]
#[command(about = "Eidetica: Remember Everything - Decentralized Database Server")]
#[command(version)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value_t = 3000, env = "EIDETICA_PORT")]
    port: u16,

    /// Bind address
    #[arg(long, default_value = "0.0.0.0", env = "EIDETICA_HOST")]
    host: String,

    /// Storage backend to use
    #[arg(short, long, default_value = "sqlite", env = "EIDETICA_BACKEND")]
    backend: Backend,

    /// Data directory for storage files.
    /// For SQLite: stores eidetica.db
    /// For InMemory: stores eidetica.json
    #[arg(short = 'D', long, env = "EIDETICA_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// PostgreSQL connection URL (required when backend=postgres)
    #[arg(long, env = "EIDETICA_POSTGRES_URL")]
    postgres_url: Option<String>,
}

/// Redact credentials from a PostgreSQL connection URL for safe logging
fn redact_postgres_url(url: &str) -> String {
    // Try to parse and redact credentials, fall back to generic message on failure
    if let Ok(parsed) = url::Url::parse(url) {
        let mut redacted = parsed.clone();
        if !parsed.username().is_empty() {
            let _ = redacted.set_username("***");
        }
        if parsed.password().is_some() {
            let _ = redacted.set_password(Some("***"));
        }
        redacted.to_string()
    } else {
        // Can't parse URL, just indicate we have a postgres URL
        "postgres://***@<unparsable-url>".to_string()
    }
}

/// Create the appropriate backend based on configuration
async fn create_backend(args: &Args) -> Result<Box<dyn BackendImpl>, Box<dyn std::error::Error>> {
    let data_dir = args.data_dir.clone().unwrap_or_else(|| PathBuf::from("."));

    // Ensure data directory exists
    tokio::fs::create_dir_all(&data_dir).await?;

    match args.backend {
        Backend::Sqlite => {
            let db_path = data_dir.join("eidetica.db");
            tracing::info!("Using SQLite backend at {}", db_path.display());
            Ok(Box::new(Sqlite::open(&db_path).await?))
        }
        Backend::Postgres => {
            let url = args
                .postgres_url
                .as_ref()
                .ok_or("PostgreSQL backend requires --postgres-url or EIDETICA_POSTGRES_URL")?;

            // Log connection info without credentials
            let display_url = redact_postgres_url(url);
            tracing::info!("Connecting to PostgreSQL backend at {}", display_url);

            match Postgres::connect(url).await {
                Ok(backend) => {
                    tracing::info!("Connected to PostgreSQL successfully");
                    Ok(Box::new(backend))
                }
                Err(e) => {
                    Err(format!("Failed to connect to PostgreSQL at {}: {}", display_url, e).into())
                }
            }
        }
        Backend::Inmemory => {
            let json_path = data_dir.join("eidetica.json");
            tracing::info!(
                "Using in-memory backend with persistence at {}",
                json_path.display()
            );
            match InMemory::load_from_file(&json_path).await {
                Ok(backend) => {
                    tracing::info!("Loaded existing data from {}", json_path.display());
                    Ok(Box::new(backend))
                }
                Err(_) => {
                    tracing::info!("Starting with fresh database");
                    Ok(Box::new(InMemory::new()))
                }
            }
        }
    }
}
use serde::Deserialize;
use signal_hook::flag as signal_flag;
use tower_cookies::{Cookie, CookieManagerLayer, Cookies};
use tracing_subscriber::EnvFilter;

mod session;
mod templates;

use session::SessionStore;
use templates::DatabaseInfo;

const DEFAULT_USER: &str = "default";
const SESSION_COOKIE: &str = "eidetica_session";

/// Shared application state
#[derive(Clone)]
struct AppState {
    instance: Arc<Instance>,
    sync_handler: Arc<dyn SyncHandler>,
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
    // Parse command line arguments
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=warn".parse().unwrap()),
        )
        .init();

    // Create the storage backend
    let backend_box = create_backend(&args).await?;

    // Initialize Instance using open API
    let instance = Instance::open(backend_box).await?;

    // Enable Sync on the instance (creates/loads sync tree)
    instance.enable_sync().await?;
    tracing::info!("Sync enabled on instance");

    // Ensure default user exists (for single-user server mode)
    let user_exists = instance
        .list_users()
        .await?
        .iter()
        .any(|u| u == DEFAULT_USER);

    if !user_exists {
        tracing::info!("Creating default user '{DEFAULT_USER}'");
        instance.create_user(DEFAULT_USER, None).await?;
    }

    // Login as default user to get device key
    let mut default_user = instance.login_user(DEFAULT_USER, None).await?;

    // Ensure default user has at least one key
    let user_keys = default_user.list_keys()?;
    let device_key_id = if user_keys.is_empty() {
        tracing::info!("Creating initial device key for default user");
        default_user
            .add_private_key(Some("Server Device Key"))
            .await?
    } else {
        user_keys[0].clone()
    };

    tracing::info!("Using device key: {device_key_id}");

    // Get sync object
    let sync = instance.sync().ok_or("Sync not enabled on instance")?;

    // Register transports for sync
    // - Iroh: peer-to-peer sync, starts its own server
    // - HTTP: client-only here (no bind address) for outbound connections to HTTP peers
    //   Inbound HTTP sync is handled by this binary's web server via /api/v0
    sync.register_transport("iroh", IrohTransport::builder())
        .await?;
    sync.register_transport("http", HttpTransport::builder())
        .await?;

    // Start accepting incoming sync connections
    sync.accept_connections().await?;
    let iroh_address = sync.get_server_address_for("iroh").await?;
    tracing::info!("Iroh server started: {}", iroh_address);

    let sync_tree_id = sync.sync_tree_root_id().clone();

    let sync_handler: Arc<dyn SyncHandler> =
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
    let addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;

    // Print startup message
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║              Eidetica Sync Server Started                     ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("🌐 Web Interface: http://localhost:{}", local_addr.port());
    println!("🔗 Iroh Node ID:  {iroh_address}");
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

    // Clone values for shutdown handler
    let instance_for_shutdown = app_state.instance.clone();
    let term_signal_for_shutdown = term_signal.clone();
    // If the data_dir is not set use the CWD
    let data_dir_for_shutdown = args.data_dir.clone().unwrap_or_else(|| PathBuf::from("."));

    // Start server with graceful shutdown
    // Convert app to service with ConnectInfo support
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        // Wait for shutdown signal
        while !term_signal_for_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        tracing::info!("Shutdown signal received...");

        // Save database on shutdown (only needed for InMemory backend)
        if let Some(in_memory_backend) = instance_for_shutdown
            .backend()
            .as_any()
            .downcast_ref::<InMemory>()
        {
            let json_path = data_dir_for_shutdown.join("eidetica.json");
            match in_memory_backend.save_to_file(&json_path).await {
                Ok(_) => {
                    tracing::info!("Database saved to {}", json_path.display());
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
    let instance = state.instance.clone();
    let sessions = state.sessions.clone();
    let username = form.username.clone();
    let password = form.password.clone();

    // Attempt to login
    let result = match instance.login_user(&username, password.as_deref()).await {
        Ok(user) => {
            // Enable background sync for this user's databases
            if let Some(sync) = instance.sync() {
                let user_uuid = user.user_uuid();
                let user_db_id = user.user_database().root_id().clone();
                if let Err(e) = sync.sync_user(user_uuid, &user_db_id).await {
                    tracing::warn!(
                        "Failed to enable background sync for user {}: {}",
                        username,
                        e
                    );
                }
            }

            // Create session
            let session_token = sessions.create_session(user).await;
            Ok(session_token)
        }
        Err(e) => Err(format!("Login failed: {e}")),
    };

    match result {
        Ok(session_token) => {
            // Set cookie (HTTP-only, Secure if behind HTTPS proxy)
            let mut cookie = Cookie::new(SESSION_COOKIE, session_token);
            cookie.set_http_only(true);
            cookie.set_path("/");
            cookies.add(cookie);
            Redirect::to("/dashboard").into_response()
        }
        Err(error_msg) => Html(templates::login_page(Some(&error_msg))).into_response(),
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

    // Handle password validation
    let password: Option<String> = if let Some(ref pwd) = form.password {
        if pwd.is_empty() {
            None
        } else {
            if form.password_confirm.as_deref() != Some(pwd.as_str()) {
                return Html(templates::register_page(Some("Passwords do not match")))
                    .into_response();
            }
            Some(pwd.clone())
        }
    } else {
        None
    };

    let instance = state.instance.clone();
    let sessions = state.sessions.clone();
    let username = form.username.clone();

    // Check if user already exists
    let result = if let Ok(users) = instance.list_users().await
        && users.iter().any(|u| u == &username)
    {
        Err("Username already exists".to_string())
    } else {
        // Create the user
        let pwd_ref = password.as_deref();
        if let Err(e) = instance.create_user(&username, pwd_ref).await {
            Err(format!("Registration failed: {e}"))
        } else {
            tracing::info!("Created new user: {}", username);

            // Log in the new user automatically
            match instance.login_user(&username, pwd_ref).await {
                Ok(user) => {
                    let session_token = sessions.create_session(user).await;
                    Ok(Some(session_token))
                }
                Err(e) => {
                    tracing::error!("User created but auto-login failed: {}", e);
                    Ok(None) // User created, but login failed
                }
            }
        }
    };

    match result {
        Ok(Some(session_token)) => {
            let mut cookie = Cookie::new(SESSION_COOKIE, session_token);
            cookie.set_http_only(true);
            cookie.set_path("/");
            cookies.add(cookie);
            Redirect::to("/dashboard").into_response()
        }
        Ok(None) => {
            // User created but login failed - redirect to login page
            Redirect::to("/login").into_response()
        }
        Err(error_msg) => Html(templates::register_page(Some(&error_msg))).into_response(),
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

    // Get user's tracked databases
    let tracked_dbs = user.databases().await.unwrap_or_default();

    // Convert to display info
    let mut databases = Vec::new();
    for tracked in &tracked_dbs {
        let db = user.open_database(&tracked.database_id).await.ok();
        databases.push(DatabaseInfo::from_tracked(tracked, db.as_ref()).await);
    }

    let html = templates::dashboard_page(&user, databases);
    Html(html).into_response()
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

    // Parse database ID
    let database_id = match ID::parse(&query.id) {
        Ok(id) => id,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid database ID").into_response();
        }
    };

    let user = user_lock.read().await;

    // Get tracked database info
    let tracked = match user.database(&database_id).await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get tracked database: {e}"),
            )
                .into_response();
        }
    };

    // Open the database
    let db = match user.open_database(&database_id).await {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to open database: {e}"),
            )
                .into_response();
        }
    };

    // Get database info
    let db_info = DatabaseInfo::from_tracked(&tracked, Some(&db)).await;

    // Get all entries
    let entries: Vec<String> = db
        .get_all_entries()
        .await
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

    // Parse the database ID
    let database_id = match ID::parse(&form.database_id) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid database ID: {e:?}"),
            )
                .into_response();
        }
    };

    // Parse requested permission
    let permission = match form.permission.as_str() {
        "read" => Permission::Read,
        "write" => Permission::Write(10),
        "admin" => Permission::Admin(0),
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

    let peer_address = form.peer_address.clone();
    let database_id_str = form.database_id.clone();

    // Get user's default key
    let key_id = {
        let user = user_lock.read().await;
        match user.get_default_key() {
            Ok(key) => key,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to get default key: {e}"),
                )
                    .into_response();
            }
        }
    };

    // Request database access via bootstrap
    let bootstrap_result = {
        let user = user_lock.read().await;
        user.request_database_access(&sync, &peer_address, &database_id, &key_id, permission)
            .await
    };

    match bootstrap_result {
        Ok(_) => {
            // Bootstrap succeeded - now add database to user's tracked list
            let mut user = user_lock.write().await;

            let mut properties = std::collections::HashMap::new();
            properties.insert("peer_address".to_string(), peer_address);

            let tracked = TrackedDatabase {
                database_id: database_id.clone(),
                key_id: key_id.clone(),
                sync_settings: SyncSettings {
                    sync_enabled: true,
                    sync_on_commit: false,
                    interval_seconds: Some(13),
                    properties,
                },
            };

            match user.track_database(tracked).await {
                Ok(_) => {
                    tracing::info!(
                        "Successfully bootstrapped and tracked database {} for user {}",
                        database_id_str,
                        user.username()
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Bootstrapped database {} but failed to add to tracking: {}",
                        database_id_str,
                        e
                    );
                }
            }
            Redirect::to("/dashboard").into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to request database access: {e}"),
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
        r#"<div class="stat"><span class="label">Active Sessions:</span> <span class="value">{session_count}</span></div>"#
    ));

    html.push_str("<p><em>Database details available on authenticated dashboard.</em></p>\n");

    html.push_str("</body>\n</html>");
    Html(html)
}

/// Handler for POST /api/v0 - Eidetica sync endpoint
async fn handle_sync_request(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    ExtractJson(request): ExtractJson<SyncRequest>,
) -> axum::Json<SyncResponse> {
    // Create request context with remote address
    let context = RequestContext {
        remote_address: Some(Address {
            transport_type: "http".to_string(),
            address: addr.to_string(),
        }),
        peer_pubkey: None,
    };

    let response = state.sync_handler.handle_request(&request, &context).await;
    axum::Json(response)
}
