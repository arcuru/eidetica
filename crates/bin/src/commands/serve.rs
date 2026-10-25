//! Serve command - runs the Eidetica web server with sync.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Form, Router,
    extract::{ConnectInfo, Json as ExtractJson, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use tokio::signal::unix::{SignalKind, signal};
use tower_cookies::{Cookie, CookieManagerLayer, Cookies, cookie::SameSite};
use tracing_subscriber::EnvFilter;

use eidetica::{
    Instance,
    account::{AccountSession, SyncSettings},
    auth::{Permission, crypto::PublicKey},
    backend::database::{DbKind, InMemory, SqlxBackend},
    entry::ID,
    sync::{
        DatabaseTicket, SyncError,
        handler::{SyncHandler, SyncHandlerImpl},
        peer_types::Address,
        protocol::{RequestContext, SyncRequest, SyncResponse},
        transports::{http::HttpTransport, iroh::IrohTransport},
    },
};

use crate::backend::create_backend;
use crate::cli::ServeArgs;
use crate::session::SessionStore;
use crate::templates::DatabaseInfo;

// The `DEFAULT_USER = "default"` auto-creation has been removed alongside
// the `admin/admin` bootstrap. Operators initialise the instance with an
// explicit user via `eidetica daemon init`.
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
    ticket: String,
    permission: String,
}

/// A decision never accepts a replacement requester, permission or priority.
#[derive(Deserialize)]
struct AccessDecisionForm {
    database_id: String,
    request_id: String,
    decision: String,
}

/// Run the Eidetica server
pub async fn run(args: &ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=warn".parse().unwrap()),
        )
        .init();

    // Create the storage backend
    let backend_box = create_backend(&args.backend_config).await?;

    // Load the existing Instance — initialise via `eidetica daemon init`
    // first if the backend is empty. The auto-bootstrapped `admin/admin`
    // pair is gone.
    let instance = match Instance::open_backend(backend_box).await {
        Ok(instance) => instance,
        Err(e) => {
            if let eidetica::Error::Instance(boxed) = &e
                && matches!(
                    boxed.as_ref(),
                    eidetica::instance::InstanceError::NotInitialized
                )
            {
                return Err(format!(
                    "Backend at {} is not initialised.\nRun `eidetica daemon init --username <NAME> [--password PASS | --passwordless]` first.",
                    crate::backend::backend_label(&args.backend_config)
                )
                .into());
            }
            return Err(Box::new(e));
        }
    };

    // Enable Sync on the instance (creates/loads sync tree)
    instance.enable_sync().await?;
    tracing::info!("Sync enabled on instance");

    // Get sync object
    let sync = instance.sync().ok_or("Sync not enabled on instance")?;

    // Register transports for sync
    sync.register_transport("iroh", IrohTransport::builder())
        .await?;
    sync.register_transport("http", HttpTransport::builder())
        .await?;

    // Start accepting incoming sync connections
    sync.accept_connections().await?;
    let iroh_address = sync.get_server_address_for("iroh").await?;
    tracing::info!("Iroh server started: {}", iroh_address);

    let app = web_router(instance.clone(), true);

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
    println!("  GET  /register     - Account registration page");
    println!("  POST /register     - Account registration submission");
    println!("  GET  /dashboard    - Account dashboard (requires login)");
    println!("  POST /dashboard/track - Request database access (requires login)");
    println!("  GET  /dashboard/requests - Pending access requests (requires database Admin)");
    println!("  POST /dashboard/requests/decide - Approve or reject an exact request");
    println!("  GET  /stats        - Server statistics");
    println!("  POST /api/v0       - Eidetica sync protocol endpoint");
    println!();
    println!("📝 To connect from chat app, use your Iroh Node ID above");
    println!();
    println!("Press Ctrl+C to shutdown");

    let data_dir = args
        .backend_config
        .data_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));

    // Start server with graceful shutdown
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to set up SIGTERM handler");
        let mut sigint =
            signal(SignalKind::interrupt()).expect("failed to set up SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => tracing::info!("Received SIGTERM, initiating graceful shutdown..."),
            _ = sigint.recv() => tracing::info!("Received SIGINT, initiating graceful shutdown..."),
        }

        // Flush pending sync operations with timeout
        let flush_timeout = Duration::from_secs(10);
        if let Err(e) = tokio::time::timeout(flush_timeout, instance.flush_sync()).await {
            tracing::warn!("Sync flush timed out: {e}");
        }

        // Save database on shutdown (only needed for InMemory backend)
        let engine = instance.backend().local_engine();
        if let Some(in_memory_backend) =
            engine.as_ref().and_then(|e| e.as_any().downcast_ref::<InMemory>())
        {
            let json_path = data_dir.join("eidetica.json");
            match in_memory_backend.save_to_file(&json_path) {
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

/// Build only the existing web surface; never mount the trusted service RPC here.
/// The daemon disables the legacy HTTP sync endpoint: peer sync still runs on Iroh.
pub(super) fn web_router(instance: Instance, include_http_sync: bool) -> Router {
    let sync_handler: Arc<dyn SyncHandler> = Arc::new(SyncHandlerImpl::new(
        instance.clone(),
        instance
            .sync()
            .expect("sync enabled")
            .sync_tree_root_id()
            .clone(),
    ));
    let state = AppState {
        instance: Arc::new(instance),
        sync_handler,
        sessions: SessionStore::new(),
    };
    dashboard_router(state, include_http_sync)
}

fn dashboard_router(state: AppState, include_http_sync: bool) -> Router {
    let mut app = Router::new()
        .route("/", get(handle_root_request))
        .route("/health", get(handle_health_endpoint))
        .route("/login", get(handle_login_page).post(handle_login_submit))
        .route(
            "/register",
            get(handle_register_page).post(handle_register_submit),
        )
        .route("/logout", post(handle_logout))
        .route("/dashboard", get(handle_dashboard))
        .route("/dashboard/database", get(handle_database_detail))
        .route("/dashboard/track", post(handle_track_database))
        .route("/dashboard/requests", get(handle_access_requests))
        .route("/dashboard/requests/decide", post(handle_access_decision))
        .route("/stats", get(handle_stats_request));
    if include_http_sync {
        app = app.route("/api/v0", post(handle_sync_request));
    }
    app.layer(CookieManagerLayer::new()).with_state(state)
}

// All browser form writes (including login) require an Origin matching the
// request's own Host. The sync endpoint is not cookie-authenticated and is
// deliberately excluded.
fn form_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    let parsed = url::Url::parse(origin).map_err(|_| StatusCode::FORBIDDEN)?;
    let default_port = if parsed.scheme() == "https" { 443 } else { 80 };
    let host = if host.contains(':') && !host.ends_with(']') {
        host.to_string()
    } else {
        format!("{host}:{default_port}")
    };
    if matches!(parsed.scheme(), "http" | "https")
        && parsed.path() == "/"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed
            .host_str()
            .is_some_and(|h| format!("{h}:{}", parsed.port_or_known_default().unwrap_or(0)) == host)
    {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

// ============================================================================
// Authentication Handlers
// ============================================================================

/// Handler for GET / - Root redirect
async fn handle_root_request(State(state): State<AppState>, cookies: Cookies) -> Redirect {
    if let Some(cookie) = cookies.get(SESSION_COOKIE)
        && state.sessions.get_user(cookie.value()).await.is_some()
    {
        return Redirect::to("/dashboard");
    }
    Redirect::to("/login")
}

/// Handler for GET /login - Show login page
async fn handle_login_page(State(state): State<AppState>, cookies: Cookies) -> Response {
    if let Some(cookie) = cookies.get(SESSION_COOKIE)
        && state.sessions.get_user(cookie.value()).await.is_some()
    {
        return Redirect::to("/dashboard").into_response();
    }

    Html(crate::templates::login_page(None)).into_response()
}

/// Handler for POST /login - Process login
async fn handle_login_submit(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    if let Err(code) = form_origin(&headers) {
        return code.into_response();
    }
    let instance = state.instance.clone();
    let sessions = state.sessions.clone();
    let username = form.username.clone();
    let password = form.password.clone();

    let result = match instance.login_account(&username, password.as_deref()).await {
        Ok(user) => {
            // Enable background sync for this user's databases
            if let Some(sync) = instance.sync() {
                let login_id = user.login_id();
                let user_db_id = user.account().id().clone();
                if let Err(e) = sync.sync_account(login_id, &user_db_id).await {
                    tracing::warn!(
                        "Failed to enable background sync for user {}: {}",
                        username,
                        e
                    );
                }
            }

            let session_token = sessions.create_session(user).await;
            Ok(session_token)
        }
        Err(e) => Err(format!("Login failed: {e}")),
    };

    match result {
        Ok(session_token) => {
            let mut cookie = Cookie::new(SESSION_COOKIE, session_token);
            cookie.set_http_only(true);
            cookie.set_same_site(SameSite::Strict);
            cookie.set_secure(
                headers
                    .get(axum::http::header::ORIGIN)
                    .and_then(|h| h.to_str().ok())
                    .is_some_and(|o| o.starts_with("https://")),
            );
            cookie.set_path("/");
            cookies.add(cookie);
            Redirect::to("/dashboard").into_response()
        }
        Err(error_msg) => Html(crate::templates::login_page(Some(&error_msg))).into_response(),
    }
}

/// Handler for POST /logout - Logout and destroy session
async fn handle_logout(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
) -> Response {
    if let Err(code) = form_origin(&headers) {
        return code.into_response();
    }
    if let Some(cookie) = cookies.get(SESSION_COOKIE) {
        state.sessions.destroy_session(cookie.value()).await;
        cookies.remove(Cookie::from(SESSION_COOKIE));
    }
    Redirect::to("/login").into_response()
}

/// Handler for GET /register - Show registration page
async fn handle_register_page(State(state): State<AppState>, cookies: Cookies) -> Response {
    if let Some(cookie) = cookies.get(SESSION_COOKIE)
        && state.sessions.get_user(cookie.value()).await.is_some()
    {
        return Redirect::to("/dashboard").into_response();
    }

    Html(crate::templates::register_page(None)).into_response()
}

/// Handler for POST /register - Process registration
async fn handle_register_submit(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    Form(form): Form<RegisterForm>,
) -> Response {
    if let Err(code) = form_origin(&headers) {
        return code.into_response();
    }
    if form.username.is_empty() {
        return Html(crate::templates::register_page(Some(
            "Username cannot be empty",
        )))
        .into_response();
    }

    let _password: Option<String> = if let Some(ref pwd) = form.password {
        if pwd.is_empty() {
            None
        } else {
            if form.password_confirm.as_deref() != Some(pwd.as_str()) {
                return Html(crate::templates::register_page(Some(
                    "Passwords do not match",
                )))
                .into_response();
            }
            Some(pwd.clone())
        }
    } else {
        None
    };

    let _instance = state.instance.clone();
    let _sessions = state.sessions.clone();
    let _username = form.username.clone();

    // TODO: Self-service registration needs an admin session on the server
    // side to drive `InstanceAdmin::create_account`. The previous
    // `admin/admin` auto-bootstrap is gone; until `serve` accepts admin
    // credentials via flags/env vars (a follow-up task), web-form
    // registration is disabled. Operators should create users via
    // `eidetica daemon init` (initial admin) or programmatically against
    // an already-initialised instance.
    let result: Result<Option<String>, String> = Err("Self-service registration is disabled. \
         Create users via `eidetica daemon init` or contact an administrator."
        .to_string());

    match result {
        Ok(Some(session_token)) => {
            let mut cookie = Cookie::new(SESSION_COOKIE, session_token);
            cookie.set_http_only(true);
            cookie.set_same_site(SameSite::Strict);
            cookie.set_secure(
                headers
                    .get(axum::http::header::ORIGIN)
                    .and_then(|h| h.to_str().ok())
                    .is_some_and(|o| o.starts_with("https://")),
            );
            cookie.set_path("/");
            cookies.add(cookie);
            Redirect::to("/dashboard").into_response()
        }
        Ok(None) => Redirect::to("/login").into_response(),
        Err(error_msg) => Html(crate::templates::register_page(Some(&error_msg))).into_response(),
    }
}

// ============================================================================
// Dashboard Handlers
// ============================================================================

/// Handler for GET /dashboard - Show user dashboard
async fn handle_dashboard(State(state): State<AppState>, cookies: Cookies) -> Response {
    let session_token = match cookies.get(SESSION_COOKIE) {
        Some(cookie) => cookie.value().to_string(),
        None => return Redirect::to("/login").into_response(),
    };

    let user_lock = match state.sessions.get_user(&session_token).await {
        Some(user) => user,
        None => return Redirect::to("/login").into_response(),
    };

    let user = user_lock.read().await;

    let tracked_dbs = user.databases().await.unwrap_or_default();

    let mut databases = Vec::new();
    for tracked in &tracked_dbs {
        let db = user.open_database(&tracked.database_id).await.ok();
        databases.push(DatabaseInfo::from_tracked(tracked, db.as_ref()).await);
    }

    let html = crate::templates::dashboard_page(&user, databases);
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
    let session_token = match cookies.get(SESSION_COOKIE) {
        Some(cookie) => cookie.value().to_string(),
        None => return Redirect::to("/login").into_response(),
    };

    let user_lock = match state.sessions.get_user(&session_token).await {
        Some(user) => user,
        None => return Redirect::to("/login").into_response(),
    };

    let database_id = match ID::parse(&query.id) {
        Ok(id) => id,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid database ID").into_response();
        }
    };

    let user = user_lock.read().await;

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

    let db_info = DatabaseInfo::from_tracked(&tracked, Some(&db)).await;

    let entries: Vec<String> = db
        .get_all_entries()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.id().to_string())
        .collect();

    Html(crate::templates::database_detail_page(
        &user, db_info, entries,
    ))
    .into_response()
}

/// Only a held, mapped account key with current database Admin authority may
/// inspect its queue. Instance administration is not database authority.
async fn bootstrap_admin_key(
    user: &AccountSession,
    database_id: &ID,
) -> eidetica::Result<Option<PublicKey>> {
    let mut best: Option<(PublicKey, Permission)> = None;
    for key in user.list_keys()? {
        if let Ok(database) = user.open_database_with_key(database_id, &key).await
            && let Ok(permission) = database.current_permission().await
            && permission.can_admin()
            && best
                .as_ref()
                .is_none_or(|(_, current)| permission > *current)
        {
            best = Some((key, permission));
        }
    }
    Ok(best.map(|(key, _)| key))
}

async fn handle_access_requests(State(state): State<AppState>, cookies: Cookies) -> Response {
    let Some(cookie) = cookies.get(SESSION_COOKIE) else {
        return Redirect::to("/login").into_response();
    };
    let Some(user_lock) = state.sessions.get_user(cookie.value()).await else {
        return Redirect::to("/login").into_response();
    };
    let Some(sync) = state.instance.sync() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user = user_lock.read().await;
    let pending = match user.pending_bootstrap_requests(&sync).await {
        Ok(pending) => pending,
        Err(e) => {
            tracing::warn!("Could not load bootstrap requests: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not load access requests",
            )
                .into_response();
        }
    };
    let mut permitted = Vec::new();
    for (id, request) in pending {
        match bootstrap_admin_key(&user, &request.tree_id).await {
            Ok(Some(_)) => permitted.push((id, request)),
            Ok(None) => {}
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
    Html(crate::templates::access_requests_page(&permitted)).into_response()
}

async fn handle_access_decision(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    Form(form): Form<AccessDecisionForm>,
) -> Response {
    if let Err(code) = form_origin(&headers) {
        return code.into_response();
    }
    let Some(cookie) = cookies.get(SESSION_COOKIE) else {
        return Redirect::to("/login").into_response();
    };
    let Some(user_lock) = state.sessions.get_user(cookie.value()).await else {
        return Redirect::to("/login").into_response();
    };
    if !matches!(form.decision.as_str(), "approve" | "reject") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(database_id) = ID::parse(&form.database_id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(sync) = state.instance.sync() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user = user_lock.read().await;
    // Check authorization before looking up a request, so forbidden and absent
    // queues have the same response and never disclose requester details.
    let key = match bootstrap_admin_key(&user, &database_id).await {
        Ok(Some(key)) => key,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let pending = match user.pending_bootstrap_requests(&sync).await {
        Ok(pending) => pending,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if !pending
        .iter()
        .any(|(id, request)| id == &form.request_id && request.tree_id == database_id)
    {
        return (
            StatusCode::NOT_FOUND,
            "No matching pending request; refresh the list",
        )
            .into_response();
    }
    // Native APIs recheck current Auth, priority bounds and terminal state.
    // These are local decisions, not peer network waits.
    let result = if form.decision == "approve" {
        user.approve_bootstrap_request(&sync, &form.request_id, &key)
            .await
    } else {
        user.reject_bootstrap_request(&sync, &form.request_id, &key)
            .await
    };
    match result {
        Ok(()) => Html(crate::templates::access_result_page(
            if form.decision == "approve" { "Request approved" } else { "Request rejected" },
            if form.decision == "approve" {
                "Approval was recorded. The requester must explicitly retry to transfer the database and record local access."
            } else {
                "Rejection was recorded and is terminal for this request. No access was granted by this decision."
            },
            None,
        )).into_response(),
        Err(e) => (StatusCode::CONFLICT, Html(crate::templates::access_result_page(
            "Decision not confirmed",
            &format!("{e}. A grant may have persisted before a later bookkeeping error. Refresh the queue and check current database Auth before retrying; this is not confirmation of a new grant."),
            None,
        ))).into_response(),
    }
}

/// Handler for POST /dashboard/track - Request database access (bootstrap)
async fn handle_track_database(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    Form(form): Form<TrackDatabaseForm>,
) -> Response {
    if let Err(code) = form_origin(&headers) {
        return code.into_response();
    }
    let session_token = match cookies.get(SESSION_COOKIE) {
        Some(cookie) => cookie.value().to_string(),
        None => return Redirect::to("/login").into_response(),
    };

    let user_lock = match state.sessions.get_user(&session_token).await {
        Some(user) => user,
        None => return Redirect::to("/login").into_response(),
    };

    let ticket: DatabaseTicket = match form.ticket.parse() {
        Ok(t) => t,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid ticket URL: {e}")).into_response();
        }
    };

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

    let (key_id, signing_key, original) = {
        let user = user_lock.read().await;
        let key_id = match user.get_default_key() {
            Ok(key) => key,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to get default key: {e}"),
                )
                    .into_response();
            }
        };
        // Taken under the same lock: the bootstrap request is signed with this
        // key to prove we hold it.
        let original = match user.databases().await {
            Ok(databases) => databases
                .into_iter()
                .find(|db| &db.database_id == ticket.database_id()),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html(crate::templates::access_result_page(
                        "Request not sent",
                        &format!("Could not read existing Account preferences: {e}"),
                        None,
                    )),
                )
                    .into_response();
            }
        };
        match user.get_signing_key(&key_id) {
            Ok(signing_key) => (key_id, signing_key, original),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to get signing key: {e}"),
                )
                    .into_response();
            }
        }
    };

    // Run the network bootstrap WITHOUT holding the per-session user lock — a
    // slow or hung peer must not freeze the rest of this session's requests.
    let key_name = key_id.to_string();
    let sync_settings = SyncSettings::enabled().with_interval(13);
    let network_result = sync
        .bootstrap_with_ticket(&ticket, &signing_key, &key_name, permission, None)
        .await;

    if let Err(e) = network_result {
        let (status, title, message, retry) = match &e {
            eidetica::Error::Sync(error) => match error.as_ref() {
                SyncError::BootstrapPending { request_id, .. } => (
                    StatusCode::ACCEPTED,
                    "Access pending approval",
                    format!(
                        "Request {request_id} for {} is pending. No new local access mapping or preferences were recorded. Retry explicitly after approval.",
                        ticket.database_id()
                    ),
                    true,
                ),
                SyncError::BootstrapRejected { request_id, .. } => (
                    StatusCode::FORBIDDEN,
                    "Access request rejected",
                    format!(
                        "Request {request_id} for {} was rejected. Rejection is terminal for this request; no local access was recorded.",
                        ticket.database_id()
                    ),
                    false,
                ),
                _ => (
                    StatusCode::BAD_GATEWAY,
                    "Access not confirmed",
                    format!(
                        "{e}. No local access was recorded. A peer request or grant may still exist; retry explicitly to check."
                    ),
                    true,
                ),
            },
            _ => (
                StatusCode::BAD_GATEWAY,
                "Access not confirmed",
                format!(
                    "{e}. No local access was recorded. Retry explicitly to check the peer outcome."
                ),
                true,
            ),
        };
        return (
            status,
            Html(crate::templates::access_result_page(
                title,
                &message,
                retry.then_some((form.ticket.as_str(), form.permission.as_str())),
            )),
        )
            .into_response();
    }

    // Re-acquire only for local bookkeeping. Reuse the native observed-state
    // check so a preference change during the network wait isn't overwritten.
    let bootstrap_result = {
        let mut user = user_lock.write().await;
        user.record_database_access_if_unchanged(
            ticket.database_id(),
            &key_id,
            sync_settings,
            Ok(()),
            original,
        )
        .await
    };

    match bootstrap_result {
        Ok(()) => Html(crate::templates::access_result_page(
            "Access granted and recorded locally",
            &format!("Database {} was transferred and its local access bookkeeping completed. This does not enroll a device or promise ongoing private replication.", ticket.database_id()),
            None,
        )).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Html(crate::templates::access_result_page(
            "Access transferred; local bookkeeping not confirmed",
            &format!("Database {} was transferred, but recording Account access returned: {e}. Local metadata may already have persisted. Retry explicitly to repair; this is not confirmation of usable local access.", ticket.database_id()),
            Some((&form.ticket, &form.permission)),
        ))).into_response(),
    }
}

// ============================================================================
// Health, Stats and Sync Handlers
// ============================================================================

/// Health check response
#[derive(serde::Serialize)]
struct HealthResponse {
    status: &'static str,
    backend: &'static str,
}

/// Handler for GET /health - Health check endpoint
async fn handle_health_endpoint(State(state): State<AppState>) -> axum::Json<HealthResponse> {
    let engine = state.instance.backend().local_engine();
    let backend_type = if let Some(sqlx) = engine
        .as_ref()
        .and_then(|e| e.as_any().downcast_ref::<SqlxBackend>())
    {
        match sqlx.kind() {
            DbKind::Sqlite => "sqlite",
            DbKind::Postgres => "postgres",
        }
    } else if engine.as_ref().is_some_and(|e| e.as_any().is::<InMemory>()) {
        "inmemory"
    } else {
        "unknown"
    };

    axum::Json(HealthResponse {
        status: "healthy",
        backend: backend_type,
    })
}

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

    html.push_str("<h2>Instance Overview</h2>\n");

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

#[cfg(test)]
mod access_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            headers.insert(axum::http::header::ORIGIN, origin.parse().unwrap());
        }
        headers
    }

    #[test]
    fn form_posts_require_same_origin() {
        let host = "127.0.0.1:3000";
        assert_eq!(
            form_origin(&headers(host, None)),
            Err(StatusCode::FORBIDDEN)
        );
        for bad in ["http://elsewhere:3000", "http://127.0.0.1:3001", "null"] {
            assert_eq!(
                form_origin(&headers(host, Some(bad))),
                Err(StatusCode::FORBIDDEN),
                "accepted {bad}"
            );
        }
        assert_eq!(
            form_origin(&headers(host, Some("http://127.0.0.1:3000"))),
            Ok(())
        );
    }

    #[test]
    fn form_origin_applies_default_ports() {
        let proxied = headers("db.example.com", Some("https://db.example.com"));
        assert_eq!(form_origin(&proxied), Ok(()));
        let downgraded = headers("db.example.com", Some("http://db.example.com:443"));
        assert_eq!(form_origin(&downgraded), Err(StatusCode::FORBIDDEN));
        assert_eq!(
            form_origin(&headers("[::1]:3000", Some("http://[::1]:3000"))),
            Ok(())
        );
        assert_eq!(form_origin(&headers("[::1]", Some("http://[::1]"))), Ok(()));
    }
}
