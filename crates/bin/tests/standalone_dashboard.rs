//! Process-boundary coverage. The same test can use the Nix-installed pair.
#![cfg(unix)]

use std::{
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use eidetica::{
    Database, ID, Instance, NewAccount,
    auth::{AuthKey, AuthSettings, Permission, types::PermissionBounds},
    crdt::Doc,
    service::dashboard::Connection,
    store::Table,
};
use reqwest::{Client, Response, StatusCode};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

const ALICE: &str = "alice disposable password";
const BOB: &str = "bob disposable password";

fn daemon_binary() -> OsString {
    std::env::var_os("EIDETICA_TEST_DAEMON")
        .or_else(|| std::env::var_os("NEXTEST_BIN_EXE_eidetica"))
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_eidetica").into())
}

fn dashboard_binary() -> OsString {
    std::env::var_os("EIDETICA_TEST_DASHBOARD")
        .or_else(|| std::env::var_os("NEXTEST_BIN_EXE_eidetica-dashboard"))
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_eidetica-dashboard").into())
}

struct Installation {
    directory: tempfile::TempDir,
    native: PathBuf,
    dashboard: PathBuf,
}

impl Installation {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let this = Self {
            native: directory.path().join("service.sock"),
            dashboard: directory.path().join("dashboard.sock"),
            directory,
        };
        let completed = this
            .command()
            .args(["daemon", "init", "--username", "alice", "--password", ALICE])
            .output()
            .await
            .unwrap();
        assert!(
            completed.status.success(),
            "daemon init failed: {}",
            String::from_utf8_lossy(&completed.stderr)
        );
        this
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(daemon_binary());
        cmd.env("EIDETICA_BACKEND", "sqlite")
            .env("EIDETICA_DATA_DIR", self.directory.path().join("data"))
            .env("EIDETICA_CACHE_BACKEND", "inmemory")
            .env("EIDETICA_CACHE_DIR", self.directory.path().join("cache"))
            .env("EIDETICA_SOCKET", &self.native)
            .env_remove("EIDETICA_DEVICE_KEY_DIR")
            .env_remove("EIDETICA_DASHBOARD")
            .env_remove("EIDETICA_SYNC_TICKETS")
            .kill_on_drop(true);
        cmd
    }

    async fn start(&self, standalone: bool) -> Child {
        let mut cmd = self.command();
        cmd.arg("daemon");
        if standalone {
            cmd.arg("--dashboard-socket").arg(&self.dashboard);
        }
        let mut child = cmd.stdout(Stdio::piped()).spawn().unwrap();
        ready(
            &mut child,
            if standalone {
                "Dashboard socket listening on "
            } else {
                "Eidetica daemon listening on "
            },
        )
        .await;
        child
    }

    async fn headless_write(&self, value: &str) -> ID {
        let instance = Instance::connect(format!("unix://{}", self.native.display()))
            .await
            .unwrap();
        let mut alice = instance.login_account("alice", ALICE).await.unwrap();
        let mut settings = Doc::new();
        settings.set("name", value);
        let key = alice.get_default_key().unwrap();
        let db = alice.create_database(settings, &key).await.unwrap();
        assert_eq!(db.get_name().await.unwrap(), value);
        db.root_id().clone()
    }
}

async fn ready(child: &mut Child, prefix: &str) -> String {
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let found = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if let Some(rest) = line.strip_prefix(prefix) {
                return rest.to_owned();
            }
        }
        panic!("process exited before printing {prefix}");
    })
    .await
    .expect("process readiness timed out");
    // Drain remaining output; closing the pipe could otherwise kill a
    // correct process the next time it writes a diagnostic.
    tokio::spawn(async move { while output.next_line().await.is_ok_and(|line| line.is_some()) {} });
    found
}

async fn start_gui(socket: &Path) -> (Child, Browser) {
    let mut child = Command::new(dashboard_binary())
        .arg("--socket")
        .arg(socket)
        .args(["--port", "0"])
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let base = ready(&mut child, "Dashboard listening on ").await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    (child, Browser { client, base })
}

struct Browser {
    client: Client,
    base: String,
}

impl Browser {
    async fn get(&self, path: &str, jar: &str) -> Response {
        self.client
            .get(format!("{}{path}", self.base))
            .header("Cookie", jar)
            .send()
            .await
            .unwrap()
    }

    async fn post(&self, path: &str, jar: &str, fields: &[(&str, &str)]) -> Response {
        self.client
            .post(format!("{}{path}", self.base))
            .header("Cookie", jar)
            .header("Origin", &self.base)
            .form(fields)
            .send()
            .await
            .unwrap()
    }

    async fn login(&self, username: &str, password: &str) -> String {
        let start = self.get("/login", "").await;
        assert_eq!(start.status(), StatusCode::OK);
        let intent = cookie(&start);
        let start = start.text().await.unwrap();
        let response = self
            .post(
                "/login",
                &intent,
                &[
                    ("username", username),
                    ("password", password),
                    ("generation", generation(&start)),
                ],
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "login rejected");
        assert_eq!(response.headers()["location"], "/dashboard");
        let headers = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            headers.contains("samesite=strict")
                && headers.contains("httponly")
                && !headers.contains("; secure")
        );
        let session = cookie(&response);
        assert_ne!(intent, session);
        session
    }

    async fn page(&self, path: &str, jar: &str) -> String {
        let response = self.get(path, jar).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        response.text().await.unwrap()
    }
}

fn cookie(response: &Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn generation(page: &str) -> &str {
    page.split_once("name=\"generation\" value=\"")
        .unwrap()
        .1
        .split('"')
        .next()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standalone_packaged_login_browsing_and_restarts() {
    let installation = Installation::new().await;
    let mut headless = installation.start(false).await;
    assert!(!installation.dashboard.exists());
    let alice_db = installation.headless_write("Alice only database").await;
    headless.kill().await.unwrap();
    let mut daemon = installation.start(true).await;

    let wrong_profile = Command::new(dashboard_binary())
        .arg("--socket")
        .arg(&installation.native)
        .args(["--port", "0"])
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(!wrong_profile.status.success());
    assert!(String::from_utf8_lossy(&wrong_profile.stderr).contains("Protocol version mismatch"));
    assert!(
        eidetica::service::RemoteConnection::connect(&installation.dashboard)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::metadata(&installation.dashboard)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    Connection::connect(&installation.dashboard)
        .await
        .unwrap()
        .ping()
        .await
        .unwrap();

    let instance = Instance::connect(format!("unix://{}", installation.native.display()))
        .await
        .unwrap();
    let mut alice = instance.login_account("alice", ALICE).await.unwrap();
    let owner_key = alice.add_private_key(None).await.unwrap();
    let identity = alice.create_identity("owner", &owner_key).await.unwrap();
    let admin = alice.admin().await.unwrap();
    let app_root = admin
        .provision_owned_app("Notes", &identity, NewAccount::passwordless("notes"))
        .await
        .unwrap();
    admin
        .create_account(NewAccount::with_password("bob", BOB))
        .await
        .unwrap();
    let mut bob = instance.login_account("bob", BOB).await.unwrap();
    let bob_key = bob.get_default_key().unwrap();
    let mut settings = Doc::new();
    settings.set("name", "Bob only database");
    let bob_db = bob.create_database(settings, &bob_key).await.unwrap();
    // A foreign login may hold public locators. It still cannot borrow the
    // selected Identity or see any linked data through a saved URL.
    bob.set_reference("accounts", "Notes", &app_root)
        .await
        .unwrap();

    let app_conn = Instance::connect(format!("unix://{}", installation.native.display()))
        .await
        .unwrap();
    let app = app_conn.login_account("notes", "").await.unwrap();
    for (name, payload, delegated) in [
        ("Shared", "<standalone-visible>", true),
        ("Direct", "DIRECT_SECRET", false),
    ] {
        let key = app
            .get_signing_key(&app.get_default_key().unwrap())
            .unwrap();
        let mut auth = AuthSettings::new();
        auth.add_key(
            &key.public_key(),
            AuthKey::active(None, Permission::Admin(0)),
        )
        .unwrap();
        if delegated {
            auth.add_delegated_tree(
                identity
                    .as_delegation(PermissionBounds::default())
                    .await
                    .unwrap(),
            )
            .unwrap();
        } else {
            auth.add_key(&owner_key, AuthKey::active(None, Permission::Read))
                .unwrap();
        }
        let mut settings = Doc::new();
        settings.set("name", name);
        settings.set("auth", auth.as_doc().clone());
        let db = Database::create(&app_conn, key, settings).await.unwrap();
        let tx = db.new_transaction().await.unwrap();
        tx.get_store::<Table<String>>("rows")
            .await
            .unwrap()
            .set("first", payload.to_owned())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        app.set_reference("data", name, db.root_id()).await.unwrap();
    }

    let (mut gui, browser) = start_gui(&installation.dashboard).await;
    assert_eq!(browser.get("/health", "").await.status(), StatusCode::OK);
    assert_eq!(
        browser.post("/api/v0", "", &[]).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        browser.get("/dashboard", "").await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        browser
            .client
            .get(format!("{}/login", browser.base))
            .header("Host", "other.test")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let bad = browser.get("/login", "").await;
    let intent = cookie(&bad);
    let bad = bad.text().await.unwrap();
    let failure = browser
        .post(
            "/login",
            &intent,
            &[
                ("username", "alice"),
                ("password", "wrong"),
                ("generation", generation(&bad)),
            ],
        )
        .await;
    assert!(failure.status().is_success());
    let fresh_intent = cookie(&failure);
    assert!(failure.text().await.unwrap().contains("Sign-in failed"));
    assert_eq!(
        browser.get("/dashboard", &fresh_intent).await.status(),
        StatusCode::SEE_OTHER
    );
    let (a, b, another_a) = tokio::join!(
        browser.login("alice", ALICE),
        browser.login("bob", BOB),
        browser.login("alice", ALICE)
    );
    let (a_page, b_page) = tokio::join!(
        browser.page("/dashboard", &a),
        browser.page("/dashboard", &b)
    );
    assert!(a_page.contains("Alice only database") && !a_page.contains("Bob only database"));
    assert!(b_page.contains("Bob only database") && !b_page.contains("Alice only database"));
    assert_eq!(
        browser
            .post(
                "/login",
                &a,
                &[
                    ("username", "bob"),
                    ("password", BOB),
                    ("generation", generation(&a_page))
                ]
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    let detail = format!("/dashboard/database?id={alice_db}");
    let observed = browser.page(&detail, &a).await;
    assert!(observed.contains("Verified entries on this daemon"));
    assert!(observed.contains("<span class=\"value\">1</span>"));
    assert_eq!(
        browser.get(&detail, &b).await.status(),
        StatusCode::NOT_FOUND
    );
    let prefs = [
        ("generation", generation(&a_page)),
        ("id", &bob_db.root_id().to_string()),
        ("enabled", "true"),
        ("sync_on_commit", "false"),
        ("interval_seconds", "29"),
    ];
    assert_eq!(
        browser
            .post("/dashboard/database/sync", &b, &prefs)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut valid = prefs;
    valid[0].1 = generation(&b_page);
    assert_eq!(
        browser
            .post("/dashboard/database/sync", &b, &valid)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        bob.database(bob_db.root_id())
            .await
            .unwrap()
            .sync_settings
            .interval_seconds,
        Some(29)
    );

    let selection = json!({"name":"owner", "root":identity.root_id(), "key":owner_key}).to_string();
    let path = json!([{"name":"Notes", "root":app_root}]).to_string();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("identity", &selection)
        .append_pair("path", &path)
        .finish();
    let linked = format!("/dashboard/accounts?{query}");
    let page = browser.page(&linked, &a).await;
    assert!(page.contains("Acting Identity") && page.contains("Viewed Account"));
    assert_eq!(
        browser.get(&linked, &b).await.status(),
        StatusCode::NOT_FOUND
    );
    let saved = |name: &str| {
        let (before, _) = page.split_once(&format!(">{name}</a>")).unwrap();
        before
            .rsplit_once("href=\"")
            .unwrap()
            .1
            .strip_suffix('"')
            .unwrap()
            .replace("&amp;", "&")
    };
    let shared = browser.page(&saved("Shared"), &a).await;
    assert!(
        shared.contains("&lt;standalone-visible&gt;") && !shared.contains("<standalone-visible>")
    );
    let denied = browser.get(&saved("Direct"), &a).await;
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    assert!(!denied.text().await.unwrap().contains("DIRECT_SECRET"));

    assert_eq!(
        browser
            .post("/logout", &a, &[("generation", generation(&a_page))])
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        browser.get(&detail, &a).await.status(),
        StatusCode::SEE_OTHER
    );
    assert!(
        browser
            .page(&detail, &another_a)
            .await
            .contains("Alice only database")
    );
    let a2 = browser.login("alice", ALICE).await;
    assert_eq!(
        browser
            .post("/logout", &a2, &[("generation", generation(&a_page))])
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        browser
            .page(&detail, &a2)
            .await
            .contains("Alice only database")
    );

    daemon.kill().await.unwrap();
    assert_eq!(
        browser.get("/health", "").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let mut daemon = installation.start(true).await;
    assert_eq!(
        browser.get(&detail, &a2).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        browser.get("/dashboard", &b).await.status(),
        StatusCode::SEE_OTHER
    );
    let new = browser.login("alice", ALICE).await;
    assert!(
        browser
            .page(&detail, &new)
            .await
            .contains("Alice only database")
    );
    gui.kill().await.unwrap();
    assert!(
        browser
            .client
            .get(format!("{}/health", browser.base))
            .send()
            .await
            .is_err()
    );
    installation.headless_write("Write after GUI exit").await;
    let (mut gui, browser) = start_gui(&installation.dashboard).await;
    assert_eq!(
        browser.get(&detail, &new).await.status(),
        StatusCode::SEE_OTHER
    );
    let latest = browser.login("alice", ALICE).await;
    assert!(
        browser
            .page("/dashboard", &latest)
            .await
            .contains("Write after GUI exit")
    );
    gui.kill().await.unwrap();
    daemon.kill().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standalone_logout_does_not_wait_for_its_in_flight_ticket() {
    use eidetica::{
        backend::database::InMemory,
        service::ServiceServer,
        sync::{Address, DatabaseTicket, transports::http::HttpTransport},
    };
    let (instance, owner) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::with_password("alice", ALICE),
    )
    .await
    .unwrap();
    instance.enable_sync().await.unwrap();
    instance
        .sync()
        .unwrap()
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();
    owner
        .admin()
        .await
        .unwrap()
        .create_account(NewAccount::with_password("bob", BOB))
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("dashboard.sock");
    let service = ServiceServer::bind_dashboard(instance.clone(), &path)
        .await
        .unwrap();
    let (stop, rx) = tokio::sync::watch::channel(());
    let server = tokio::spawn(service.run(rx));
    let (mut gui, browser) = start_gui(&path).await;
    let (alice, bob) = tokio::join!(browser.login("alice", ALICE), browser.login("bob", BOB));
    let a_page = browser.page("/dashboard", &alice).await;
    // A disposable peer accepts the route probe but withholds its reply.
    // The accept below is a positive control that the daemon received the request.
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = ID::from_bytes("untransferred database");
    let ticket = DatabaseTicket::with_addresses(
        root.clone(),
        vec![Address::http(gate.local_addr().unwrap().to_string())],
    )
    .to_string();
    let fields = [
        ("generation", generation(&a_page)),
        ("ticket", &ticket),
        ("permission", "read"),
    ];
    let pending = browser.post("/dashboard/track", &alice, &fields);
    tokio::pin!(pending);
    let waiting_peer = tokio::select! {
        peer = tokio::time::timeout(Duration::from_secs(10), gate.accept()) => peer.unwrap().unwrap().0,
        _ = &mut pending => panic!("ticket did not reach the waiting peer"),
    };
    let b_page = browser.page("/dashboard", &bob).await;
    assert!(b_page.contains("<span class=\"value\">bob</span>"));
    let logged_out = tokio::time::timeout(
        Duration::from_secs(5),
        browser.post("/logout", &alice, &[("generation", generation(&a_page))]),
    )
    .await
    .expect("logout waited for ticket");
    assert_eq!(logged_out.status(), StatusCode::SEE_OTHER);
    // Even while the peer has not replied, retirement interrupts the GUI's
    // abandoned response waiter; late writes are not reported as rejected.
    let result = tokio::time::timeout(Duration::from_secs(5), &mut pending)
        .await
        .expect("retirement did not wake waiter");
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(result.text().await.unwrap().contains("outcome unconfirmed"));
    assert_eq!(
        browser.get("/dashboard", &alice).await.status(),
        StatusCode::SEE_OTHER
    );
    let replacement = browser.login("alice", ALICE).await;
    assert!(
        !browser
            .page("/dashboard", &replacement)
            .await
            .contains(&root.to_string())
    );
    assert_eq!(
        browser.get("/dashboard", &bob).await.status(),
        StatusCode::OK
    );
    drop(waiting_peer);
    drop(stop);
    server.await.unwrap().unwrap();
    assert!(owner.tracked_database(&root).await.unwrap().is_none());
    gui.kill().await.unwrap();
}
