//! Web UI regression tests: rendered dashboard correctness and handler
//! behaviour against the current Account/AccountSession API.
//!
//! These pin the contract that failures and disabled features must never
//! masquerade as working or empty states: a failed tracked-database list is
//! not "no databases", a failed read is not a zero count, and registration
//! states the operator/admin-only boundary instead of presenting a dead form.

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use eidetica::{
    Instance, NewAccount, account::TrackedDatabase, backend::database::InMemory, crdt::Doc,
};
use tower::ServiceExt;

use super::serve::web_router;
use crate::templates::{
    DatabaseInfo, dashboard_page, database_detail_page, login_page, register_page,
};

const HOST: &str = "127.0.0.1:3000";

/// A fresh in-memory instance (initial admin `alice`) plus its web router,
/// and a separately logged-in session for asserting expected page content.
struct TestServer {
    instance: Arc<Instance>,
    app: Router,
}

async fn server() -> TestServer {
    let (instance, _initial) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("alice"))
            .await
            .unwrap();
    instance.enable_sync().await.unwrap();
    let instance = Arc::new(instance);
    let app = web_router((*instance).clone(), false);
    TestServer { instance, app }
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap()
}

fn get_with_cookie(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::COOKIE, format!("eidetica_session={cookie}"))
        .body(Body::empty())
        .unwrap()
}

fn form_post(uri: &str, body: &str, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("eidetica_session={cookie}"));
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn session_cookie(response: &axum::response::Response) -> String {
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("response sets a session cookie")
        .to_str()
        .unwrap();
    set_cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("eidetica_session=")
        .expect("cookie name")
        .to_string()
}

// ---------------------------------------------------------------------------
// Rendered template regressions
// ---------------------------------------------------------------------------

async fn alice_session() -> eidetica::account::AccountSession {
    let (instance, alice) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("alice"))
            .await
            .unwrap();
    let _ = instance;
    alice
}

#[tokio::test]
async fn dashboard_labels_stable_account_id_separately_from_login_id() {
    let alice = alice_session().await;
    let html = dashboard_page(&alice, Vec::new(), None);

    let login_id = alice.login_id().to_string();
    let account_root_id = alice.account().id().to_string();
    assert_ne!(
        login_id, account_root_id,
        "instance-local login ID and stable Account root ID must be distinct values"
    );
    assert!(html.contains("Username:"));
    assert!(html.contains(&format!("<span class=\"value\">{}</span>", "alice")));
    assert!(html.contains("Login ID:"));
    assert!(html.contains(&login_id));
    assert!(html.contains("Account Root ID:"));
    assert!(html.contains(&account_root_id));
}

#[tokio::test]
async fn dashboard_explains_account_vs_identity_without_one_to_one_claim() {
    let alice = alice_session().await;
    let html = dashboard_page(&alice, Vec::new(), None);

    assert!(html.contains("An Identity is a separate concept"));
    assert!(html.contains("need not"));
    assert!(html.contains("correspond one-to-one"));
}

#[tokio::test]
async fn dashboard_lists_public_key_ids_with_default_marker() {
    let alice = alice_session().await;
    let default_key = alice.get_default_key().unwrap();
    let html = dashboard_page(&alice, Vec::new(), None);

    assert!(html.contains(&default_key.to_string()));
    assert!(html.contains("(default)"));
    assert!(
        html.contains("key material is never shown"),
        "the page should state that only key IDs are displayed"
    );
}

#[tokio::test]
async fn dashboard_distinguishes_empty_list_from_failed_read() {
    let alice = alice_session().await;

    let empty = dashboard_page(&alice, Vec::new(), None);
    assert!(empty.contains("No databases tracked yet."));
    assert!(!empty.contains("Failed to read the tracked-database list"));

    let failed = dashboard_page(&alice, Vec::new(), Some("store read failed <boom>"));
    assert!(failed.contains("Failed to read the tracked-database list"));
    assert!(failed.contains("read failure, not an empty list"));
    assert!(failed.contains("store read failed &lt;boom&gt;"));
    assert!(
        !failed.contains("No databases tracked yet."),
        "a failed list read must not render as an empty list"
    );
}

#[tokio::test]
async fn dashboard_row_failures_do_not_masquerade_as_placeholders() {
    let alice = alice_session().await;
    let broken = DatabaseInfo {
        root_id: "sha256:broken".to_string(),
        name: Err("open failed: key not held".to_string()),
        entry_count: Err("entry read failed".to_string()),
        sync_settings: eidetica::account::SyncSettings::disabled(),
        key_id: "sha256:key".to_string(),
    };
    let html = dashboard_page(&alice, vec![broken], None);

    assert!(html.contains("unavailable: open failed: key not held"));
    assert!(html.contains("unknown (entry read failed)"));
    assert!(
        !html.contains("Unknown</td>"),
        "a failed read must not render the old Unknown placeholder"
    );
    assert!(
        !html.contains("<td>0</td>"),
        "a failed entry read must not render a zero count"
    );
}

#[tokio::test]
async fn dashboard_escapes_user_controlled_values() {
    let alice = alice_session().await;
    let hostile = DatabaseInfo {
        root_id: "sha256:hostile".to_string(),
        name: Ok("<script>alert(1)</script>".to_string()),
        entry_count: Ok(2),
        sync_settings: eidetica::account::SyncSettings::enabled(),
        key_id: "sha256:key2".to_string(),
    };
    // Row values are escaped on a healthy list render.
    let with_row = dashboard_page(&alice, vec![hostile], None);
    assert!(with_row.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!with_row.contains("<script>alert(1)</script>"));

    // The list-error banner escapes its error text.
    let with_error = dashboard_page(&alice, Vec::new(), Some("<injected>"));
    assert!(with_error.contains("&lt;injected&gt;"));
    assert!(!with_error.contains("<injected>"));
}

#[tokio::test]
async fn dashboard_labels_sync_preference_not_runtime_health() {
    let alice = alice_session().await;
    let html = dashboard_page(&alice, Vec::new(), None);

    assert!(html.contains("Sync Preference"));
    assert!(!html.contains("Sync Status"));
    assert!(html.contains("does not by itself indicate"));
}

#[tokio::test]
async fn registration_pages_state_operator_boundary() {
    let notice = register_page(None);
    assert!(notice.contains("Self-service registration is not available"));
    assert!(notice.contains("eidetica daemon init"));
    assert!(
        !notice.contains("action=\"/register\""),
        "the dead self-service form must be gone"
    );

    let login = login_page(None);
    assert!(
        !login.contains("href=\"/register\""),
        "the login page must not invite self-service registration"
    );
    assert!(login.contains("created by the operator"));
}

#[tokio::test]
async fn detail_page_distinguishes_empty_entries_from_failed_read() {
    let alice = alice_session().await;
    let info = DatabaseInfo {
        root_id: "sha256:db1".to_string(),
        name: Ok("db1".to_string()),
        entry_count: Ok(0),
        sync_settings: eidetica::account::SyncSettings::disabled(),
        key_id: "sha256:k".to_string(),
    };

    let empty = database_detail_page(&alice, info.clone(), Ok(Vec::new()));
    assert!(empty.contains("No entries in this database yet."));
    assert!(!empty.contains("Failed to read entries"));

    let failed = database_detail_page(&alice, info, Err("backend exploded <x>".to_string()));
    assert!(failed.contains("Failed to read entries"));
    assert!(failed.contains("backend exploded &lt;x&gt;"));
    assert!(
        !failed.contains("No entries in this database yet."),
        "a failed entry read must not render as an empty database"
    );
}

#[tokio::test]
async fn detail_page_labels_sync_preference() {
    let alice = alice_session().await;
    let info = DatabaseInfo {
        root_id: "sha256:db1".to_string(),
        name: Ok("db1".to_string()),
        entry_count: Ok(0),
        sync_settings: eidetica::account::SyncSettings::enabled(),
        key_id: "sha256:k".to_string(),
    };
    let html = database_detail_page(&alice, info, Ok(Vec::new()));
    assert!(html.contains("Sync Preference"));
    assert!(!html.contains("Sync Status"));
    assert!(!html.contains("Background sync is active"));
}

// ---------------------------------------------------------------------------
// Handler regressions through the router
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unauthenticated_requests_redirect_to_login() {
    let TestServer { instance: _, app } = server().await;

    for uri in ["/", "/dashboard"] {
        let response = app.clone().oneshot(get(uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|l| l.to_str().ok()),
            Some("/login"),
            "{uri} without a session must redirect to /login"
        );
    }
}

#[tokio::test]
async fn login_dashboard_logout_roundtrip() {
    let TestServer { instance, app } = server().await;

    let login = app
        .clone()
        .oneshot(form_post("/login", "username=alice", None))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::SEE_OTHER);
    assert_eq!(login.headers().get(header::LOCATION).unwrap(), "/dashboard");
    let cookie = session_cookie(&login);
    assert!(!cookie.is_empty());

    let dashboard = app
        .clone()
        .oneshot(get_with_cookie("/dashboard", &cookie))
        .await
        .unwrap();
    assert_eq!(dashboard.status(), StatusCode::OK);
    let html = body_string(dashboard).await;

    // Expected values from an independent login of the same account.
    let alice = instance.login_account("alice", None).await.unwrap();
    let login_id = alice.login_id().to_string();
    let account_root_id = alice.account().id().to_string();
    assert_ne!(login_id, account_root_id);
    assert!(html.contains("Account Root ID:"));
    assert!(html.contains(&login_id));
    assert!(html.contains(&account_root_id));
    assert!(html.contains("No databases tracked yet."));

    let logout = app
        .clone()
        .oneshot(form_post("/logout", "", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::SEE_OTHER);
    assert_eq!(logout.headers().get(header::LOCATION).unwrap(), "/login");

    let after = app
        .clone()
        .oneshot(get_with_cookie("/dashboard", &cookie))
        .await
        .unwrap();
    assert_eq!(
        after.status(),
        StatusCode::SEE_OTHER,
        "a destroyed session must not reach the dashboard"
    );
    assert_eq!(after.headers().get(header::LOCATION).unwrap(), "/login");
}

#[tokio::test]
async fn register_endpoints_reject_without_creating_sessions() {
    let TestServer { instance: _, app } = server().await;

    let notice = app.clone().oneshot(get("/register")).await.unwrap();
    assert_eq!(notice.status(), StatusCode::OK);
    let html = body_string(notice).await;
    assert!(html.contains("Self-service registration is not available"));
    assert!(!html.contains("action=\"/register\""));

    let submit = app
        .clone()
        .oneshot(form_post("/register", "username=attacker", None))
        .await
        .unwrap();
    assert_eq!(submit.status(), StatusCode::OK);
    assert!(
        submit.headers().get(header::SET_COOKIE).is_none(),
        "registration must not create a session"
    );
    let html = body_string(submit).await;
    assert!(html.contains("Self-service registration is disabled"));
    // The account must not exist afterwards.
    assert!(
        instance_login_fails(&app, "attacker").await,
        "a rejected registration must not create a login"
    );
}

async fn instance_login_fails(app: &Router, username: &str) -> bool {
    let response = app
        .clone()
        .oneshot(form_post("/login", &format!("username={username}"), None))
        .await
        .unwrap();
    response.status() != StatusCode::SEE_OTHER
}

#[tokio::test]
async fn dashboard_shows_own_database_and_hides_other_accounts() {
    let TestServer { instance, app } = server().await;

    // Bob is created by the instance admin (operator path), not self-service.
    let mut alice = instance.login_account("alice", None).await.unwrap();
    alice
        .admin()
        .await
        .unwrap()
        .create_account(NewAccount::passwordless("bob"))
        .await
        .unwrap();
    let mut bob = instance.login_account("bob", None).await.unwrap();

    // Bob owns a named database; Alice owns a different one.
    let bob_key = bob.get_default_key().unwrap();
    let bob_db = bob.create_database(Doc::new(), &bob_key).await.unwrap();
    let txn = bob_db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_name("bob-secret-<name>")
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let alice_key = alice.get_default_key().unwrap();
    let alice_db = alice.create_database(Doc::new(), &alice_key).await.unwrap();
    let txn = alice_db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_name("alice-public")
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let bob_login = app
        .clone()
        .oneshot(form_post("/login", "username=bob", None))
        .await
        .unwrap();
    let bob_cookie = session_cookie(&bob_login);
    let bob_dashboard = app
        .clone()
        .oneshot(get_with_cookie("/dashboard", &bob_cookie))
        .await
        .unwrap();
    assert_eq!(bob_dashboard.status(), StatusCode::OK);
    let bob_html = body_string(bob_dashboard).await;
    assert!(bob_html.contains(&bob_db.root_id().to_string()));
    assert!(bob_html.contains("bob-secret-&lt;name&gt;"));

    let alice_login = app
        .clone()
        .oneshot(form_post("/login", "username=alice", None))
        .await
        .unwrap();
    let alice_cookie = session_cookie(&alice_login);
    let alice_dashboard = app
        .clone()
        .oneshot(get_with_cookie("/dashboard", &alice_cookie))
        .await
        .unwrap();
    assert_eq!(alice_dashboard.status(), StatusCode::OK);
    let alice_html = body_string(alice_dashboard).await;
    assert!(alice_html.contains(&alice_db.root_id().to_string()));
    assert!(
        !alice_html.contains(&bob_db.root_id().to_string()),
        "another account's database must not be listed"
    );
    assert!(
        !alice_html.contains("bob-secret"),
        "another account's database name must not leak"
    );

    // Direct fetch of another account's database is refused, not rendered.
    let detail = app
        .clone()
        .oneshot(get_with_cookie(
            &format!("/dashboard/database?id={}", bob_db.root_id()),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn database_detail_shows_stored_state() {
    let TestServer { instance, app } = server().await;

    let mut alice = instance.login_account("alice", None).await.unwrap();
    let key = alice.get_default_key().unwrap();
    let db = alice.create_database(Doc::new(), &key).await.unwrap();
    let txn = db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_name("alice-db")
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let login = app
        .clone()
        .oneshot(form_post("/login", "username=alice", None))
        .await
        .unwrap();
    let cookie = session_cookie(&login);

    let detail = app
        .clone()
        .oneshot(get_with_cookie(
            &format!("/dashboard/database?id={}", db.root_id()),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let html = body_string(detail).await;
    assert!(html.contains("alice-db"));
    assert!(html.contains(&db.root_id().to_string()));
    // A named database carries its settings entry, so entries are shown.
    assert!(!html.contains("No entries in this database yet."));
    assert!(html.contains("Entry ID"));

    let bad = app
        .clone()
        .oneshot(get_with_cookie("/dashboard/database?id=not-an-id", &cookie))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn from_tracked_keeps_read_outcomes() {
    let (instance, mut alice) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("alice"))
            .await
            .unwrap();
    let key = alice.get_default_key().unwrap();
    let db = alice.create_database(Doc::new(), &key).await.unwrap();
    instance.enable_sync().await.unwrap();
    alice
        .track_database(
            db.root_id(),
            &key,
            eidetica::account::SyncSettings::on_commit(),
        )
        .await
        .unwrap();
    let tracked: TrackedDatabase = alice.database(db.root_id()).await.unwrap();

    // A healthy open yields real values (empty name is a value, not a failure;
    // a fresh database already carries its settings entry).
    let expected_entries = db.get_all_entries().await.unwrap().len();
    let healthy = DatabaseInfo::from_tracked(&tracked, Ok(&db)).await;
    assert_eq!(healthy.name, Ok(String::new()));
    assert_eq!(healthy.entry_count, Ok(expected_entries));

    // An open failure propagates into both fields instead of placeholders.
    let failed = DatabaseInfo::from_tracked(&tracked, Err("open failed: no key")).await;
    assert_eq!(failed.name, Err("open failed: no key".to_string()));
    assert_eq!(failed.entry_count, Err("open failed: no key".to_string()));
}
