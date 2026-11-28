//! Standalone HTTP adapter for the shared dashboard presentation.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Extension, Form, Json, Router,
    extract::{Query, RawQuery, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use eidetica::{
    ID,
    service::dashboard::{Connection, DatabaseSummary, JoinOutcome, Login},
    sync::DatabaseTicket,
};
use serde::Deserialize;
use tower_cookies::{Cookie, CookieManagerLayer, Cookies, cookie::SameSite};
use uuid::Uuid;

use crate::{
    commands::serve::{accounts, form_origin, handle_register_submit},
    forms::{
        self, AccessDecisionForm, DatabaseQuery, LoginForm, SyncPreferencesForm, TrackDatabaseForm,
    },
    templates::{self, DashboardView, DatabaseInfo},
};

const COOKIE: &str = "eidetica_dashboard_session";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(45);
const INTENT_TTL: Duration = Duration::from_secs(300);
const MAX_INTENTS: usize = 1024;

/// Run the local dashboard against an explicitly enabled daemon endpoint.
#[derive(clap::Parser, Debug)]
#[command(about)]
pub struct Args {
    /// The socket selected by `eidetica daemon --dashboard-socket`.
    #[arg(long)]
    socket: PathBuf,
    /// Local HTTP bind address (loopback only).
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(long, default_value_t = 5942)]
    port: u16,
}

pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    if !args.host.is_loopback() {
        return Err("the standalone dashboard listens on loopback only".into());
    }
    Connection::connect(&args.socket).await?.ping().await?;
    let listener = tokio::net::TcpListener::bind((args.host, args.port)).await?;
    let addr = listener.local_addr()?;
    println!("Dashboard listening on http://{addr}");
    axum::serve(listener, router(args.socket, addr)).await?;
    Ok(())
}

#[derive(Clone)]
struct Web {
    socket: Arc<PathBuf>,
    address: SocketAddr,
    browsers: Arc<Mutex<Browsers>>,
}

#[derive(Default)]
struct Browsers {
    intents: HashMap<String, Intent>,
    sessions: HashMap<String, Session>,
}

struct Intent {
    generation: String,
    expires: Instant,
    started: bool,
}

#[derive(Clone)]
struct Session {
    login: Arc<Login>,
    generation: Arc<str>,
}

fn router(socket: PathBuf, address: SocketAddr) -> Router {
    let state = Web {
        socket: Arc::new(socket),
        address,
        browsers: Default::default(),
    };
    let protected = Router::new()
        .route("/dashboard", get(dashboard))
        .route("/dashboard/accounts", get(browse))
        .route("/dashboard/database", get(database))
        .route("/dashboard/database/sync", post(preferences))
        .route("/dashboard/track", post(track))
        .route("/dashboard/requests", get(requests))
        .route("/dashboard/requests/decide", post(decide))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    Router::new()
        .merge(protected)
        .route("/", get(root))
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .route("/register", get(register).post(handle_register_submit))
        .route("/health", get(health))
        .route("/stats", get(stats))
        .layer(middleware::from_fn_with_state(state.clone(), local_host))
        .layer(CookieManagerLayer::new())
        .with_state(state)
}

async fn local_host(State(state): State<Web>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if !matches!(host, Some(host) if host == state.address.to_string() || host == format!("localhost:{}", state.address.port()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    response
}

fn session(state: &Web, cookies: &Cookies) -> Option<Session> {
    state
        .browsers
        .lock()
        .unwrap()
        .sessions
        .get(cookies.get(COOKIE)?.value())
        .filter(|session| !session.login.is_closed())
        .cloned()
}

async fn require_session(
    State(state): State<Web>,
    cookies: Cookies,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(session) = session(&state, &cookies) else {
        return Redirect::to("/login").into_response();
    };
    let write = request.method() == Method::POST;
    request.extensions_mut().insert(session.clone());
    let response = next.run(request).await;
    // Do not return protected content from a request that finished after its
    // generation was removed. In particular, never set a new cookie here.
    if self::session(&state, &cookies)
        .is_some_and(|current| current.generation == session.generation)
    {
        response
    } else if write {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Html(templates::access_result_page(
                "Session ended; outcome unconfirmed",
                "Sign in again and check the current state before repeating the action.",
                None,
            )),
        )
            .into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

async fn root(State(state): State<Web>, cookies: Cookies) -> Redirect {
    if session(&state, &cookies).is_some() {
        Redirect::to("/dashboard")
    } else {
        Redirect::to("/login")
    }
}

fn random() -> String {
    Uuid::new_v4().to_string()
}

fn set_cookie(cookies: &Cookies, token: String, headers: &HeaderMap) {
    let mut cookie = Cookie::new(COOKIE, token);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    cookie.set_secure(
        headers
            .get(header::ORIGIN)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|o| o.starts_with("https://")),
    );
    cookies.add(cookie);
}

fn render_login(
    state: &Web,
    cookies: &Cookies,
    headers: &HeaderMap,
    error: Option<&str>,
) -> Response {
    let now = Instant::now();
    let mut browsers = state.browsers.lock().unwrap();
    if let Some(cookie) = cookies.get(COOKIE) {
        if browsers
            .sessions
            .get(cookie.value())
            .is_some_and(|s| !s.login.is_closed())
        {
            return Redirect::to("/dashboard").into_response();
        }
        if let Some(intent) = browsers
            .intents
            .get(cookie.value())
            .filter(|i| i.expires > now)
        {
            if intent.started {
                return (
                    StatusCode::CONFLICT,
                    "Sign-in is already in progress in this browser.",
                )
                    .into_response();
            }
            return Html(templates::login_form(error, Some(&intent.generation))).into_response();
        }
    }
    browsers.intents.retain(|_, intent| intent.expires > now);
    if browsers.intents.len() >= MAX_INTENTS {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let cookie = random();
    let generation = random();
    browsers.intents.insert(
        cookie.clone(),
        Intent {
            generation: generation.clone(),
            expires: now + INTENT_TTL,
            started: false,
        },
    );
    set_cookie(cookies, cookie, headers);
    Html(templates::login_form(error, Some(&generation))).into_response()
}

async fn login_page(State(state): State<Web>, cookies: Cookies, headers: HeaderMap) -> Response {
    render_login(&state, &cookies, &headers, None)
}

async fn login(
    State(state): State<Web>,
    cookies: Cookies,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    if let Err(status) = form_origin(&headers) {
        return status.into_response();
    }
    let Some(cookie) = cookies.get(COOKIE).map(|c| c.value().to_owned()) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    {
        let mut browsers = state.browsers.lock().unwrap();
        let Some(intent) = browsers.intents.get_mut(&cookie).filter(|i| {
            !i.started
                && form.generation.as_deref() == Some(&i.generation)
                && i.expires > Instant::now()
        }) else {
            return StatusCode::FORBIDDEN.into_response();
        };
        intent.started = true;
    }
    let password = zeroize::Zeroizing::new(form.password);
    let authenticate = async {
        Connection::connect(&state.socket)
            .await?
            .login(&form.username, &password)
            .await
    };
    let result = tokio::time::timeout(LOGIN_TIMEOUT, authenticate).await;
    drop(password);
    let mut browsers = state.browsers.lock().unwrap();
    if browsers
        .intents
        .remove(&cookie)
        .is_none_or(|intent| intent.expires <= Instant::now())
    {
        return (StatusCode::CONFLICT, "Sign-in expired; load /login again.").into_response();
    }
    match result {
        Ok(Ok(login)) if !login.is_closed() => {
            let token = random();
            let login = Arc::new(login);
            browsers.sessions.insert(
                token.clone(),
                Session {
                    login: login.clone(),
                    generation: Arc::from(random()),
                },
            );
            let weak = Arc::downgrade(&state.browsers);
            let eviction_token = token.clone();
            tokio::spawn(async move {
                login.disconnected().await;
                if let Some(browsers) = weak.upgrade() {
                    browsers.lock().unwrap().sessions.remove(&eviction_token);
                }
            });
            set_cookie(&cookies, token, &headers);
            Redirect::to("/dashboard").into_response()
        }
        _ => {
            drop(browsers);
            render_login(
                &state,
                &cookies,
                &headers,
                Some("Sign-in failed. Check your credentials and the dashboard daemon."),
            )
        }
    }
}

fn protect_form(
    headers: &HeaderMap,
    session: &Session,
    generation: Option<&str>,
) -> Result<(), StatusCode> {
    form_origin(headers)?;
    if generation != Some(&session.generation) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

#[derive(Deserialize)]
struct Logout {
    generation: String,
}

async fn logout(
    State(state): State<Web>,
    cookies: Cookies,
    headers: HeaderMap,
    Form(input): Form<Logout>,
) -> Response {
    if let Err(status) = form_origin(&headers) {
        return status.into_response();
    }
    let Some(cookie) = cookies.get(COOKIE) else {
        return Redirect::to("/login").into_response();
    };
    let mut browsers = state.browsers.lock().unwrap();
    if let Some(session) = browsers.sessions.get(cookie.value()) {
        if input.generation != *session.generation {
            return StatusCode::FORBIDDEN.into_response();
        }
        let session = browsers.sessions.remove(cookie.value()).unwrap();
        session.login.retire();
    } else if let Some(intent) = browsers.intents.get(cookie.value()) {
        if input.generation != intent.generation {
            return StatusCode::FORBIDDEN.into_response();
        }
        browsers.intents.remove(cookie.value());
    }
    let mut removal = Cookie::from(COOKIE);
    removal.set_path("/");
    cookies.remove(removal);
    Redirect::to("/login").into_response()
}

async fn register(State(state): State<Web>, cookies: Cookies) -> Response {
    if session(&state, &cookies).is_some() {
        return Redirect::to("/dashboard").into_response();
    }
    Html(templates::register_page()).into_response()
}

async fn health(State(state): State<Web>) -> impl IntoResponse {
    let reachable = match Connection::connect(&state.socket).await {
        Ok(connection) => tokio::time::timeout(Duration::from_secs(5), connection.ping())
            .await
            .is_ok_and(|probe| probe.is_ok()),
        Err(_) => false,
    };
    (
        if reachable {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(serde_json::json!({
            "status": if reachable { "healthy" } else { "disconnected" },
            "backend": "daemon",
        })),
    )
}

async fn stats(State(state): State<Web>) -> Html<String> {
    templates::stats_page(state.browsers.lock().unwrap().sessions.len())
}

fn database_info(row: &DatabaseSummary) -> DatabaseInfo {
    DatabaseInfo {
        root_id: row.tracked.database_id.to_string(),
        name: row.name.clone(),
        entry_count: row
            .entries
            .as_ref()
            .map(|(count, _)| *count)
            .map_err(String::clone),
        sync_settings: row.tracked.sync_settings.clone(),
        key_id: row.tracked.key_id.to_string(),
    }
}

async fn dashboard(Extension(session): Extension<Session>) -> Html<String> {
    let (rows, error) = match session.login.databases().await {
        Ok(rows) => (rows.iter().map(database_info).collect(), None),
        Err(_) => (
            Vec::new(),
            Some("Could not read tracked databases on the daemon"),
        ),
    };
    Html(templates::dashboard_view(
        DashboardView {
            username: &session.login.username,
            login_id: &session.login.login_id,
            root: &session.login.root.to_string(),
            keys: Ok(&session.login.keys),
            token: Some(&session.generation),
        },
        rows,
        error,
    ))
}

async fn database(
    Extension(session): Extension<Session>,
    Query(query): Query<DatabaseQuery>,
) -> Response {
    let Ok(id) = ID::parse(&query.id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match session.login.database(id).await {
        Ok(Some(row)) => Html(templates::database_detail_view(
            database_info(&row),
            row.entries
                .map(|(_, ids)| ids.iter().map(ToString::to_string).collect()),
            Some(&session.generation),
        ))
        .into_response(),
        Ok(None) => unavailable(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn browse(Extension(session): Extension<Session>, RawQuery(query): RawQuery) -> Response {
    let location = accounts::parse_location(query.as_deref());
    let page = match location.clone() {
        Some(location) => match session.login.browse(location).await {
            Ok(page) => page.map(|page| *page),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        None => None,
    };
    accounts::render(&session.login.username, location, page)
}

async fn preferences(
    Extension(session): Extension<Session>,
    headers: HeaderMap,
    Form(input): Form<SyncPreferencesForm>,
) -> Response {
    if let Err(status) = protect_form(&headers, &session, input.generation.as_deref()) {
        return status.into_response();
    }
    let Ok(root) = ID::parse(&input.id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let interval = match forms::interval(&input.interval_seconds) {
        Ok(interval) => interval,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    if session
        .login
        .preferences(root.clone(), input.enabled, input.sync_on_commit, interval)
        .await
        .is_ok()
    {
        Redirect::to(&format!("/dashboard/database?id={root}")).into_response()
    } else {
        (
            StatusCode::CONFLICT,
            Html(templates::sync_preferences_error(
                "Target unavailable or update not confirmed",
                &root.to_string(),
            )),
        )
            .into_response()
    }
}

async fn track(
    Extension(session): Extension<Session>,
    headers: HeaderMap,
    Form(input): Form<TrackDatabaseForm>,
) -> Response {
    if let Err(status) = protect_form(&headers, &session, input.generation.as_deref()) {
        return status.into_response();
    }
    let Ok(ticket) = input.ticket.parse::<DatabaseTicket>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(permission) = forms::requested_permission(&input.permission) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let root = ticket.database_id().clone();
    let (status, title, message, retry) = match session.login.join(ticket, permission).await {
        Ok(JoinOutcome::Complete) => (StatusCode::OK, "Access granted and recorded locally", format!("Database {root} was transferred and its local access bookkeeping completed. This does not enroll a device or promise ongoing private replication."), false),
        Ok(JoinOutcome::Pending(id)) => (StatusCode::ACCEPTED, "Access pending approval", format!("Request {id} for {root} is pending. No new local access was recorded. Retry explicitly after approval."), true),
        Ok(JoinOutcome::Rejected(id)) => (StatusCode::FORBIDDEN, "Access request rejected", format!("Rejection of {id} for {root} is terminal. No new local access was recorded."), false),
        Ok(JoinOutcome::BookkeepingUnconfirmed) => (StatusCode::CONFLICT, "Access transferred; local bookkeeping not confirmed", format!("Database {root} was transferred. Reload to check whether its local access was recorded before repeating the request."), true),
        Err(_) => (StatusCode::BAD_GATEWAY, "Access not confirmed", "The operation was denied or its outcome is unknown. Check the current state before retrying.".into(), true),
    };
    (
        status,
        Html(templates::access_result_view(
            title,
            &message,
            retry.then_some((&input.ticket, &input.permission)),
            Some(&session.generation),
        )),
    )
        .into_response()
}

async fn requests(Extension(session): Extension<Session>) -> Response {
    match session.login.access_requests().await {
        Ok(requests) => Html(templates::access_requests_view(
            &requests,
            Some(&session.generation),
        ))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn decide(
    Extension(session): Extension<Session>,
    headers: HeaderMap,
    Form(input): Form<AccessDecisionForm>,
) -> Response {
    if let Err(status) = protect_form(&headers, &session, input.generation.as_deref()) {
        return status.into_response();
    }
    let Ok(root) = ID::parse(&input.database_id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let reject = match input.decision.as_str() {
        "approve" => false,
        "reject" => true,
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    match session.login.decide(root, input.request_id, reject).await {
        Ok(()) if reject => Html(templates::access_result_page("Request rejected", "Rejection is terminal; no access was granted by this decision.", None)).into_response(),
        Ok(()) => Html(templates::access_result_page("Request approved", "The requester must explicitly retry to transfer the database and record local access.", None)).into_response(),
        Err(_) => (StatusCode::CONFLICT, Html(templates::access_result_page("Decision not confirmed", "The request was inaccessible or the outcome is uncertain. Refresh the queue and check current database Auth before retrying.", None))).into_response(),
    }
}

fn unavailable() -> Response {
    (StatusCode::NOT_FOUND, "Target inaccessible or unavailable.").into_response()
}
