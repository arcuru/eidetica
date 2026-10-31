//! Real HTTP requests against the router used by both serve and daemon --dashboard.
use super::*;
use eidetica::{
    Database, NewAccount,
    account::{AccountKey, AccountSession},
    auth::{AuthKey, SigKey},
    backend::{
        BackendImpl,
        database::{Postgres, Sqlite},
    },
    crdt::Doc,
    path,
    service::ServiceServer,
    store::{DocStore, Table},
};
use reqwest::{Client, redirect::Policy};
use tokio::{sync::watch, task::JoinHandle};

struct TestApp {
    instance: Instance,
    client_instance: Instance,
    user: AccountSession,
    database: Database,
    http: Client,
    origin: String,
    cookie: String,
    web: JoinHandle<()>,
    service: Option<JoinHandle<eidetica::Result<()>>>,
    _shutdown: watch::Sender<()>,
    _dir: tempfile::TempDir,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.web.abort();
        if let Some(service) = &self.service {
            service.abort();
        }
    }
}

impl TestApp {
    async fn new() -> Self {
        let backend: Box<dyn BackendImpl> = match std::env::var("TEST_BACKEND").as_deref() {
            Ok("sqlite") => Box::new(Sqlite::in_memory().await.unwrap()),
            Ok("postgres") => Box::new(
                Postgres::connect_isolated(
                    &std::env::var("TEST_POSTGRES_URL").expect("postgres test URL"),
                )
                .await
                .unwrap(),
            ),
            Ok("service" | "inmemory" | "") | Err(_) => Box::new(InMemory::new()),
            Ok(other) => panic!("unsupported test backend: {other}"),
        };
        let (instance, _) = Instance::create_backend(backend, NewAccount::passwordless("alice"))
            .await
            .unwrap();
        instance.enable_sync().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (shutdown, rx) = watch::channel(());
        let (client_instance, service) =
            if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
                let socket = dir.path().join("preferences.sock");
                let server = ServiceServer::bind(instance.clone(), &socket)
                    .await
                    .unwrap();
                let task = tokio::spawn(server.run(rx));
                let client = Instance::connect(format!("unix://{}", socket.display()))
                    .await
                    .unwrap();
                (client, Some(task))
            } else {
                (instance.clone(), None)
            };
        let mut user = client_instance.login_account("alice", None).await.unwrap();
        let key = user.get_default_key().unwrap();
        let database = user.create_database(Doc::new(), &key).await.unwrap();
        let mut settings = SyncSettings::disabled();
        settings
            .properties
            .insert("keep".into(), "unrelated value".into());
        user.track_database(database.root_id(), &key, settings)
            .await
            .unwrap();
        user.set_reference("example", "keep", database.root_id())
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = web_router(instance.clone(), false);
        let web = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let http = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let cookie = login(&http, &origin, "alice").await;
        Self {
            instance,
            client_instance,
            user,
            database,
            http,
            origin,
            cookie,
            web,
            service,
            _shutdown: shutdown,
            _dir: dir,
        }
    }

    async fn account(&self) -> Database {
        open_account(&self.client_instance, &self.user).await
    }

    fn values(&self, enabled: &str, on_commit: &str, interval: &str) -> Vec<(&str, String)> {
        vec![
            ("id", self.database.root_id().to_string()),
            ("enabled", enabled.into()),
            ("sync_on_commit", on_commit.into()),
            ("interval_seconds", interval.into()),
        ]
    }

    async fn post(&self, values: &[(&str, String)]) -> reqwest::Response {
        self.http
            .post(format!("{}/dashboard/database/sync", self.origin))
            .header("Origin", &self.origin)
            .header("Cookie", &self.cookie)
            .form(values)
            .send()
            .await
            .unwrap()
    }

    async fn reload(&self) -> String {
        let response = self
            .http
            .get(format!(
                "{}/dashboard/database?id={}",
                self.origin,
                self.database.root_id()
            ))
            .header("Cookie", &self.cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.text().await.unwrap()
    }

    async fn combined(&self) -> Option<SyncSettings> {
        let sync = self.instance.sync().unwrap();
        let db = Database::open(&self.instance, sync.sync_tree_root_id())
            .await
            .unwrap();
        let store = db
            .get_store_viewer::<DocStore>("database_users")
            .await
            .unwrap();
        match store
            .get_path_as::<String>(path!(
                &self.database.root_id().to_string(),
                "combined_settings"
            ))
            .await
        {
            Ok(json) => Some(serde_json::from_str(&json).unwrap()),
            Err(e) if e.is_not_found() => None,
            Err(e) => panic!("failed to read daemon combined settings: {e}"),
        }
    }

    async fn wait_combined(&self, expected: Option<(bool, Option<u64>)>) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let actual = self
                    .combined()
                    .await
                    .map(|settings| (settings.sync_on_commit, settings.interval_seconds));
                if actual == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("daemon did not reconcile preferences");
    }
}

// Inspect persisted Account state using this test account's own signing key.
async fn open_account(instance: &Instance, user: &AccountSession) -> Database {
    Database::open(instance, user.account().id())
        .await
        .unwrap()
        .with_key(
            user.get_signing_key(&user.get_default_key().unwrap())
                .unwrap(),
        )
}

async fn login(http: &Client, origin: &str, username: &str) -> String {
    let response = http
        .post(format!("{origin}/login"))
        .header("Origin", origin)
        .form(&[("username", username)])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    cookie.split(';').next().unwrap().to_string()
}

fn assert_form(html: &str, enabled: bool, on_commit: bool, interval: &str) {
    assert!(html.contains("Another account may still"));
    assert!(html.contains("<form method=\"POST\" action=\"/dashboard/database/sync\">"));
    for (field, expected) in [("enabled", enabled), ("sync_on_commit", on_commit)] {
        let select = html
            .split(&format!("<select id=\"{field}\""))
            .nth(1)
            .unwrap()
            .split("</select>")
            .next()
            .unwrap();
        assert!(
            select.contains(&format!("<option value=\"{expected}\" selected>")),
            "{select}"
        );
    }
    assert!(html.contains(&format!("min=\"1\" step=\"1\" value=\"{interval}\"")));
}

#[tokio::test]
async fn sync_preferences_http_roundtrip_preserves_state_and_reconciles() {
    let mut app = TestApp::new().await;
    let id = app.database.root_id().clone();
    let before = app.user.database(&id).await.unwrap();
    let keys = app
        .account()
        .await
        .get_store_viewer::<Table<AccountKey>>("keys")
        .await
        .unwrap();
    let keys_before = serde_json::to_value(keys.search(|_| true).await.unwrap()).unwrap();
    assert_form(&app.reload().await, false, false, "");
    for (enabled, on_commit, interval) in [
        (true, false, Some(41)),
        (true, true, Some(17)),
        (false, true, Some(17)),
        (true, false, None),
        (false, false, None),
    ] {
        let interval_text = interval.map(|n: u64| n.to_string()).unwrap_or_default();
        let values = app.values(&enabled.to_string(), &on_commit.to_string(), &interval_text);
        let response = app.post(&values).await;
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "{}",
            response.text().await.unwrap()
        );
        assert_eq!(
            response.headers()["location"],
            format!("/dashboard/database?id={id}")
        );
        // A fresh session over the selected backend reads the persisted signed state.
        let reloaded = app
            .client_instance
            .login_account("alice", None)
            .await
            .unwrap();
        let tracked = reloaded.database(&id).await.unwrap();
        assert_eq!(tracked.key_id, before.key_id);
        assert_eq!(
            tracked.sync_settings.properties,
            before.sync_settings.properties
        );
        assert_eq!(
            (
                tracked.sync_settings.sync_enabled,
                tracked.sync_settings.sync_on_commit,
                tracked.sync_settings.interval_seconds
            ),
            (enabled, on_commit, interval)
        );
        assert_eq!(
            reloaded.reference("example", "keep").await.unwrap(),
            Some(id.clone())
        );
        let keys = open_account(&app.client_instance, &reloaded)
            .await
            .get_store_viewer::<Table<AccountKey>>("keys")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(keys.search(|_| true).await.unwrap()).unwrap(),
            keys_before
        );
        assert_form(&app.reload().await, enabled, on_commit, &interval_text);
        app.wait_combined(if enabled {
            Some((on_commit, interval))
        } else {
            None
        })
        .await;
    }
    // The same preference-only operation also travels as a signed service-client write.
    app.user
        .set_sync_preferences(&id, true, true, Some(23))
        .await
        .unwrap();
    app.wait_combined(Some((true, Some(23)))).await;
    let snapshot = app.account().await.snapshot().await.unwrap();
    app.post(&app.values("true", "true", "23"))
        .await
        .error_for_status()
        .unwrap();
    assert_eq!(
        app.account().await.snapshot().await.unwrap(),
        snapshot,
        "identical save wrote history"
    );
}

#[tokio::test]
async fn sync_preferences_http_accounts_are_independent() {
    let app = TestApp::new().await;
    app.user
        .admin()
        .await
        .unwrap()
        .create_account(NewAccount::passwordless("bob"))
        .await
        .unwrap();
    // Service authentication is connection-local; independent accounts need
    // independent socket connections, just as independent client processes do.
    let bob_instance = if app.service.is_some() {
        Instance::connect(format!(
            "unix://{}",
            app._dir.path().join("preferences.sock").display()
        ))
        .await
        .unwrap()
    } else {
        app.instance.clone()
    };
    let mut bob = bob_instance.login_account("bob", None).await.unwrap();
    app.database
        .with_transaction(|tx| async move {
            tx.get_settings()?
                .set_global_auth_key(AuthKey::active(None, Permission::Read))
                .await
        })
        .await
        .unwrap();
    let key = bob.get_default_key().unwrap();
    bob.track_database(
        app.database.root_id(),
        &key,
        SyncSettings::on_commit().with_interval(11),
    )
    .await
    .unwrap();
    let bob_before = bob.database(app.database.root_id()).await.unwrap();
    let mut values = app.values("true", "false", "47");
    values.push(("account_id", bob.account().id().to_string()));
    assert_eq!(app.post(&values).await.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        bob.database(app.database.root_id()).await.unwrap(),
        bob_before
    );
    app.wait_combined(Some((true, Some(11)))).await;
    assert_eq!(
        app.post(&app.values("false", "false", "")).await.status(),
        StatusCode::SEE_OTHER
    );
    app.wait_combined(Some((true, Some(11)))).await; // Alice's disable is not a global stop.
    let cookie = login(&app.http, &app.origin, "bob").await;
    let html = app
        .http
        .get(format!(
            "{}/dashboard/database?id={}",
            app.origin,
            app.database.root_id()
        ))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_form(&html, true, true, "11");

    let foreign = bob.create_database(Doc::new(), &key).await.unwrap();
    let snapshot = app.account().await.snapshot().await.unwrap();
    values[0].1 = foreign.root_id().to_string();
    assert_eq!(app.post(&values).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(app.account().await.snapshot().await.unwrap(), snapshot);
    assert!(app.user.database(foreign.root_id()).await.is_err());
}

#[tokio::test]
async fn sync_preferences_http_rejects_invalid_input_sessions_and_origins() {
    let app = TestApp::new().await;
    let before = app.account().await.snapshot().await.unwrap();
    let values = app.values("true", "true", "19");
    for (field, bad) in [
        ("id", "not-an-id"),
        ("id", &ID::from_bytes("untracked").to_string()),
        ("enabled", "on"),
        ("sync_on_commit", "yes"),
        ("interval_seconds", "0"),
        ("interval_seconds", "-1"),
        ("interval_seconds", "1.5"),
        ("interval_seconds", "abc"),
        ("interval_seconds", "18446744073709551616"),
        ("interval_seconds", "18446744073709551615"),
    ] {
        let mut invalid = values.clone();
        invalid
            .iter_mut()
            .find(|(name, _)| *name == field)
            .unwrap()
            .1 = bad.into();
        let response = app.post(&invalid).await;
        assert!(
            response.status().is_client_error(),
            "accepted {field}={bad}"
        );
    }
    for field in ["id", "enabled", "sync_on_commit", "interval_seconds"] {
        let invalid: Vec<_> = values
            .iter()
            .filter(|(name, _)| *name != field)
            .cloned()
            .collect();
        assert!(app.post(&invalid).await.status().is_client_error());
    }
    for origin in [None, Some("http://foreign.example"), Some("null")] {
        let mut request = app
            .http
            .post(format!("{}/dashboard/database/sync", app.origin))
            .header("Cookie", &app.cookie)
            .form(&values);
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    for cookie in [None, Some("eidetica_session=forged")] {
        let mut request = app
            .http
            .post(format!("{}/dashboard/database/sync", app.origin))
            .header("Origin", &app.origin)
            .form(&values);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], "/login");
    }
    assert_eq!(app.account().await.snapshot().await.unwrap(), before);
}

#[tokio::test]
async fn sync_preferences_http_never_repairs_an_unauthorized_mapping() {
    let mut app = TestApp::new().await;
    let key = app.user.get_default_key().unwrap();
    // The default pubkey remains authorized, but the explicitly recorded global
    // identity is not. track_database would rediscover and replace this mapping.
    app.user
        .map_key(&key, app.database.root_id(), SigKey::global(&key))
        .await
        .unwrap();
    // Web sessions cache mappings at login, so load this exact recorded identity.
    app.cookie = login(&app.http, &app.origin, "alice").await;
    let before = app.account().await.snapshot().await.unwrap();
    let response = app.post(&app.values("true", "true", "31")).await;
    assert!(response.status().is_server_error());
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Could not confirm sync preferences")
    );
    assert_eq!(app.account().await.snapshot().await.unwrap(), before);
    assert_eq!(
        app.user.key_mapping(&key, app.database.root_id()).unwrap(),
        Some(SigKey::global(&key))
    );
}

#[tokio::test]
async fn sync_preferences_http_failed_signed_write_is_visible_and_does_not_save() {
    let app = TestApp::new().await;
    let key = app.user.get_default_key().unwrap();
    let account = app.account().await;
    account
        .with_transaction(|tx| async move {
            tx.get_settings()?
                .set_auth_key(&key, AuthKey::active(None, Permission::Read))
                .await
        })
        .await
        .unwrap();
    let before = account.snapshot().await.unwrap();
    let response = app.post(&app.values("true", "true", "29")).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error = response.text().await.unwrap();
    assert!(error.contains("Could not confirm sync preferences"));
    assert!(error.contains("Reload the current preferences before retrying"));
    assert_eq!(account.snapshot().await.unwrap(), before);
    assert!(
        !app.user
            .database(app.database.root_id())
            .await
            .unwrap()
            .sync_settings
            .sync_enabled
    );
}

#[test]
fn sync_preferences_error_escapes_untrusted_text() {
    let html = crate::templates::sync_preferences_error("<script>\"&'", "<id>");
    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;script&gt;&quot;&amp;&#x27;"));
    assert!(html.contains("id=&lt;id&gt;"));
}
