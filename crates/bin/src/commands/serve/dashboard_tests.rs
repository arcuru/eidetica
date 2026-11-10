//! Account labels, read failures and registration through the served dashboard.
use super::test_helpers::{Web, instance};
use super::*;
use crate::templates::{DatabaseInfo, dashboard_page, database_detail_page};
use eidetica::{NewAccount, account::TrackedDatabase, crdt::Doc, service::ServiceServer};

#[tokio::test]
async fn dashboard_identifies_account_login_and_public_keys() {
    let (_, mut alice) = instance("alice<local>").await;
    let default = alice.get_default_key().unwrap();
    let named = alice
        .add_private_key(Some("<script>key alias</script>"))
        .await
        .unwrap();
    let html = dashboard_page(&alice, Vec::new(), None);
    let login_id = alice.login_id();
    let account_id = alice.account().id().to_string();
    assert_ne!(login_id, account_id);
    for (label, value) in [("Login ID:", login_id), ("Account Root ID:", &account_id)] {
        let row = html
            .split(&format!("<span class=\"label\">{label}</span>"))
            .nth(1)
            .unwrap()
            .split("</div>")
            .next()
            .unwrap();
        assert!(row.contains(value));
    }
    assert!(html.contains("alice&lt;local&gt;"));
    let default_row = html
        .split(&format!("{default}</span>"))
        .nth(1)
        .unwrap()
        .split("</div>")
        .next()
        .unwrap();
    assert!(default_row.contains("<strong>(default)</strong>"));
    assert!(html.contains(&named.to_string()));
    assert!(html.contains("&lt;script&gt;key alias&lt;/script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("An Identity is a separate concept"));
    assert!(html.contains("need not"));
    assert!(html.contains("correspond one-to-one"));
}

#[tokio::test]
async fn dashboard_escapes_row_errors_and_distinguishes_empty_from_failed() {
    let (_, alice) = instance("alice").await;
    let empty = dashboard_page(&alice, Vec::new(), None);
    assert!(empty.contains("No databases tracked yet."));
    assert!(!empty.contains("Failed to read the tracked-database list"));

    let failed = dashboard_page(&alice, Vec::new(), Some("store failure <boom>"));
    assert!(failed.contains("Failed to read the tracked-database list"));
    assert!(failed.contains("store failure &lt;boom&gt;"));
    assert!(!failed.contains("No databases tracked yet."));

    let row = DatabaseInfo {
        root_id: "<database>".into(),
        name: Err("open failure <boom>".into()),
        entry_count: Err("read failure <boom>".into()),
        sync_settings: SyncSettings::disabled(),
        key_id: "<key>".into(),
    };
    let html = dashboard_page(&alice, vec![row], None);
    assert!(html.contains("unavailable: open failure &lt;boom&gt;"));
    assert!(html.contains("unknown (read failure &lt;boom&gt;)"));
    assert!(html.contains("id=&lt;database&gt;"));
    assert!(!html.contains("<td>0</td>"));
    assert!(html.contains("Your Sync Preference"));
    assert!(html.contains("does not by itself indicate"));
    assert!(!html.contains("Sync Status"));
}

#[tokio::test]
async fn detail_distinguishes_empty_entries_from_read_failure() {
    let (_, alice) = instance("alice").await;
    let mut info = DatabaseInfo {
        root_id: "<database>".into(),
        name: Ok("<script>name</script>".into()),
        entry_count: Ok(0),
        sync_settings: SyncSettings::enabled(),
        key_id: "<key>".into(),
    };
    let empty = database_detail_page(&alice, info.clone(), Ok(Vec::new()));
    assert!(empty.contains("No entries in this database yet."));
    assert!(empty.contains("&lt;script&gt;name&lt;/script&gt;"));
    assert!(empty.contains("&lt;key&gt;"));
    assert!(!empty.contains("<script>"));
    assert!(empty.contains("Your Sync Preference:"));
    assert!(!empty.contains("Background sync is active"));
    assert!(empty.contains("action=\"/dashboard/database/sync\""));

    info.name = Err("name failure <boom>".into());
    info.entry_count = Err("entry failure <boom>".into());
    let failed = database_detail_page(&alice, info, Err("entry failure <boom>".into()));
    assert!(failed.contains("Failed to read entries"));
    assert!(failed.contains("unavailable: name failure &lt;boom&gt;"));
    assert!(failed.contains("entry failure &lt;boom&gt;"));
    assert!(!failed.contains("No entries in this database yet."));
    assert!(!failed.contains("<span class=\"value\">0</span>"));
}

#[tokio::test]
async fn dashboard_requires_session_and_logout_invalidates_it() {
    let (instance, alice) = instance("alice").await;
    let web = Web::start(&instance).await;
    for cookie in [None, Some("eidetica_session=forged")] {
        for uri in ["/", "/dashboard", "/dashboard/database?id=not-an-id"] {
            let response = web.get(uri, cookie).await;
            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            assert_eq!(response.headers()["location"], "/login");
        }
    }
    let cookie = web.login("alice").await;
    let response = web.get("/dashboard", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response.text().await.unwrap();
    assert!(html.contains(&alice.account().id().to_string()));
    assert!(html.contains(alice.login_id()));
    assert!(html.contains("No databases tracked yet."));
    assert!(html.contains("href=\"/dashboard/requests\""));

    let logout = web.post("/logout", Some(&cookie), &[]).await;
    assert_eq!(logout.status(), StatusCode::SEE_OTHER);
    assert_eq!(logout.headers()["location"], "/login");
    let after = web.get("/dashboard", Some(&cookie)).await;
    assert_eq!(after.status(), StatusCode::SEE_OTHER);
    assert_eq!(after.headers()["location"], "/login");
}

#[tokio::test]
async fn registration_is_only_a_notice_and_preserves_origin_protection() {
    let (instance, _) = instance("alice").await;
    let web = Web::start(&instance).await;
    let login = web.get("/login", None).await.text().await.unwrap();
    assert!(!login.contains("href=\"/register\""));
    for response in [
        web.get("/register", None).await,
        web.post(
            "/register",
            None,
            &[("username", "attacker"), ("password", "secret")],
        )
        .await,
        web.post("/register", None, &[]).await,
    ] {
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("set-cookie").is_none());
        let html = response.text().await.unwrap();
        assert!(html.contains("Self-service registration is not available"));
        assert!(html.contains("eidetica daemon init"));
        assert!(html.contains("administrator"));
        assert!(!html.contains("action=\"/register\""));
    }
    assert!(instance.login_account("attacker", "secret").await.is_err());
    assert_eq!(web.state.sessions.session_count().await, 0);
    for origin in [None, Some("http://elsewhere")] {
        let mut request = web.client.post(format!("{}/register", web.url));
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn dashboard_and_detail_show_only_the_logged_in_accounts_databases() {
    let (instance, mut alice) = instance("alice").await;
    alice
        .admin()
        .await
        .unwrap()
        .create_account(NewAccount::passwordless("bob"))
        .await
        .unwrap();
    let mut bob = instance.login_account("bob", "").await.unwrap();
    let mut databases = Vec::new();
    for (user, name) in [(&mut alice, "alice<db>"), (&mut bob, "bob<db>")] {
        let key = user.get_default_key().unwrap();
        let db = user.create_database(Doc::new(), &key).await.unwrap();
        db.with_transaction(|tx| async move { tx.get_settings()?.set_name(name).await })
            .await
            .unwrap();
        databases.push(db);
    }
    let web = Web::start(&instance).await;
    for (user, own, other) in [
        ("alice", &databases[0], &databases[1]),
        ("bob", &databases[1], &databases[0]),
    ] {
        let cookie = web.login(user).await;
        let html = web
            .get("/dashboard", Some(&cookie))
            .await
            .text()
            .await
            .unwrap();
        assert!(html.contains(&own.root_id().to_string()));
        assert!(html.contains(&format!("{user}&lt;db&gt;")));
        assert!(!html.contains(&other.root_id().to_string()));
        assert!(!html.contains("No databases tracked yet."));
        let response = web
            .get(
                &format!("/dashboard/database?id={}", own.root_id()),
                Some(&cookie),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let detail = response.text().await.unwrap();
        assert!(detail.contains(&format!("{user}&lt;db&gt;")));
        for entry in own.get_all_entries().await.unwrap() {
            assert!(detail.contains(&entry.id().to_string()));
        }
        let denied = web
            .get(
                &format!("/dashboard/database?id={}", other.root_id()),
                Some(&cookie),
            )
            .await;
        assert!(denied.status().is_server_error());
        let denied = denied.text().await.unwrap();
        assert!(!denied.contains("&lt;db&gt;"));
        let bad = web
            .get("/dashboard/database?id=not-an-id", Some(&cookie))
            .await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn dashboard_list_read_failure_does_not_look_empty() {
    let (instance, alice) = instance("alice").await;
    let web = Web::start(&instance).await;
    let cookie = web.login("alice").await;
    // Fault only this fixture's native tracking record: typed reads must fail,
    // not turn a persistence/decode error into an empty dashboard.
    let native = instance.backend().local_engine().unwrap();
    let name = format!("account-state:{}", alice.login_id());
    let bytes = native.local_account_record(&name).await.unwrap().unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    state["databases"]["broken"] = serde_json::json!("not a tracked row");
    assert!(
        native
            .compare_exchange_local_account_record(
                &name,
                Some(&bytes),
                &serde_json::to_vec(&state).unwrap()
            )
            .await
            .unwrap()
    );
    assert!(alice.databases().await.is_err());
    let response = web.get("/dashboard", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response.text().await.unwrap();
    assert!(html.contains("Failed to read the tracked-database list"));
    assert!(!html.contains("No databases tracked yet."));
}

#[tokio::test]
async fn dashboard_retains_name_and_open_failures_per_row() {
    let (instance, mut alice) = instance("alice").await;
    let key = alice.get_default_key().unwrap();
    let db = alice.create_database(Doc::new(), &key).await.unwrap();
    let tracked = alice.database(db.root_id()).await.unwrap();
    let healthy = DatabaseInfo::from_tracked(&tracked, Ok(&db)).await;
    assert_eq!(healthy.name, Ok(String::new()));
    assert_eq!(
        healthy.entry_count,
        Ok(db.get_all_entries().await.unwrap().len())
    );
    db.with_transaction(|tx| async move { tx.get_settings()?.as_doc_store().delete("name").await })
        .await
        .unwrap();
    assert_eq!(
        DatabaseInfo::from_tracked(&tracked, Ok(&db)).await.name,
        Ok(String::new())
    );
    db.with_transaction(
        |tx| async move { tx.get_settings()?.as_doc_store().set("name", 123).await },
    )
    .await
    .unwrap();
    assert!(db.get_name().await.is_err());
    let web = Web::start(&instance).await;
    let cookie = web.login("alice").await;
    let html = web
        .get("/dashboard", Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains(&db.root_id().to_string()));
    assert!(html.contains("unavailable:"));
    assert!(!html.contains("<td>0</td>"));

    // An intact tracked row can outlive local access to its target.
    let missing_id = ID::from_bytes("unavailable target");
    let orphan = TrackedDatabase {
        database_id: missing_id.clone(),
        ..tracked
    };
    let native = instance.backend().local_engine().unwrap();
    let name = format!("account-state:{}", alice.login_id());
    let bytes = native.local_account_record(&name).await.unwrap().unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    state["databases"][orphan.database_id.to_string()] = serde_json::to_value(&orphan).unwrap();
    assert!(
        native
            .compare_exchange_local_account_record(
                &name,
                Some(&bytes),
                &serde_json::to_vec(&state).unwrap()
            )
            .await
            .unwrap()
    );
    assert!(alice.open_database(&missing_id).await.is_err());
    let cookie = web.login("alice").await;
    let html = web
        .get("/dashboard", Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains(&db.root_id().to_string()));
    assert!(html.contains("unavailable:"));
    assert!(html.contains(&missing_id.to_string()));
    assert!(html.contains("unknown ("));
    assert!(!html.contains("<td>0</td>"));
}

#[tokio::test]
async fn dashboard_unavailable_entry_reads_are_not_zero_or_empty() {
    let (instance, mut alice) = instance("alice").await;
    let db = alice
        .create_database(Doc::new(), &alice.get_default_key().unwrap())
        .await
        .unwrap();
    let web = Web::start(&instance).await;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("dashboard.sock");
    let service = ServiceServer::bind(instance.clone(), &socket)
        .await
        .unwrap();
    let (_shutdown, rx) = tokio::sync::watch::channel(());
    let service_task = tokio::spawn(service.run(rx));
    let remote = Instance::connect(format!("unix://{}", socket.display()))
        .await
        .unwrap();
    let remote_alice = remote.login_account("alice", "").await.unwrap();
    let remote_db = remote_alice.open_database(db.root_id()).await.unwrap();
    // Raw Entry enumeration is local-only; exercise that genuine unavailable read.
    assert!(remote_db.get_all_entries().await.is_err());
    let token = web.state.sessions.create_session(remote_alice).await;
    let cookie = format!("eidetica_session={token}");
    let html = web
        .get("/dashboard", Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains(&db.root_id().to_string()));
    assert!(html.contains("unknown ("));
    assert!(!html.contains("<td>0</td>"));
    let response = web
        .get(
            &format!("/dashboard/database?id={}", db.root_id()),
            Some(&cookie),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response.text().await.unwrap();
    assert!(html.contains("Failed to read entries"));
    assert!(!html.contains("No entries in this database yet."));
    assert!(!html.contains("<span class=\"value\">0</span>"));
    service_task.abort();
}

#[tokio::test]
async fn blank_password_form_logs_in_and_opens_dashboard() {
    let (instance, _) = instance("alice").await;
    let web = Web::start(&instance).await;
    let response = web
        .post("/login", None, &[("username", "alice"), ("password", "")])
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/dashboard");
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let dashboard = web.get("/dashboard", Some(cookie)).await;
    assert_eq!(dashboard.status(), StatusCode::OK);
    assert!(dashboard.text().await.unwrap().contains("alice"));
}
