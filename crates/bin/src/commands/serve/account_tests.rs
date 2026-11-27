//! Linked Account browsing through the real HTTP router and Account APIs.
use super::{
    test_helpers::{Web, instance},
    *,
};
use eidetica::{
    Database, NewAccount,
    auth::{AuthKey, AuthSettings, types::PermissionBounds},
    crdt::Doc,
    store::Table,
};
use serde_json::{Value, json};

async fn follow(web: &Web, page: &str, text: &str, cookie: &str, status: StatusCode) -> String {
    let href = href(page, text);
    let response = web.get(&href, Some(cookie)).await;
    assert_eq!(response.status(), status, "{text}");
    response.text().await.unwrap()
}

fn href(page: &str, text: &str) -> String {
    let (before, _) = page.split_once(&format!(">{text}</a>")).expect(text);
    let (_, quoted) = before.rsplit_once("href=\"").expect("link attribute");
    quoted.strip_suffix('"').unwrap().replace("&amp;", "&")
}

fn select(page: &str, name: &str) -> String {
    let (before, _) = page.split_once(&format!(">{name} (")).expect(name);
    let (_, quoted) = before
        .rsplit_once("<option value=\"")
        .expect("Identity choice");
    let selection = quoted.strip_suffix('"').unwrap().replace("&quot;", "\"");
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("identity", &selection)
        .finish();
    format!("/dashboard/accounts?{query}")
}

#[tokio::test]
async fn browse_linked_accounts_requires_the_selected_identity_on_every_target() {
    let (instance, mut person) = instance("person").await;
    let owner_key = person.add_private_key(None).await.unwrap();
    let owner = person.create_identity("owner", &owner_key).await.unwrap();
    let other_key = person.add_private_key(None).await.unwrap();
    person.create_identity("other", &other_key).await.unwrap();
    let admin = person.admin().await.unwrap();
    let app_id = admin
        .provision_owned_app(
            "Notes",
            &owner,
            NewAccount::with_password("app-login", "disposable app password"),
        )
        .await
        .unwrap();
    let nested_id = admin
        .provision_owned_app(
            "Nested",
            &owner,
            NewAccount::with_password("nested-login", "disposable nested password"),
        )
        .await
        .unwrap();
    admin
        .create_account(NewAccount::with_password(
            "hidden-login",
            "disposable hidden password",
        ))
        .await
        .unwrap();
    let hidden = instance
        .login_account("hidden-login", "disposable hidden password")
        .await
        .unwrap();
    person
        .set_reference("accounts", "Other account", hidden.account().id())
        .await
        .unwrap();
    let app = instance
        .login_account("app-login", "disposable app password")
        .await
        .unwrap();
    assert_eq!(app.account().id(), &app_id);
    assert!(person.remove_reference("accounts", "Nested").await.unwrap());
    app.set_reference("accounts", "Nested", &nested_id)
        .await
        .unwrap();

    let app_key = app
        .get_signing_key(&app.get_default_key().unwrap())
        .unwrap();
    for (alias, name, payload) in [
        ("Readable", "Visible notes", "<script>visible note</script>"),
        ("Direct only", "DIRECT_DB_NAME", "DIRECT_DB_PAYLOAD"),
        ("Private", "PRIVATE_DB_NAME", "PRIVATE_DB_PAYLOAD"),
        ("Legacy", "LEGACY_ACCOUNT_NAME", "LEGACY_ACCOUNT_PAYLOAD"),
    ] {
        let readable = alias == "Readable";
        let mut auth = AuthSettings::new();
        auth.add_key(
            &app_key.public_key(),
            AuthKey::active(None, Permission::Admin(0)),
        )
        .unwrap();
        if readable || alias == "Legacy" {
            auth.add_delegated_tree(
                owner
                    .as_delegation(PermissionBounds::default())
                    .await
                    .unwrap(),
            )
            .unwrap();
        } else if alias == "Direct only" {
            // Even the same public key's direct Read is not the selected Identity path.
            auth.add_key(&owner_key, AuthKey::active(None, Permission::Read))
                .unwrap();
        }
        let mut settings = Doc::new();
        settings.set("name", name);
        settings.set("auth", auth.as_doc().clone());
        if alias == "Legacy" {
            settings.set("type", "user");
        }
        let db = Database::create(&instance, app_key.clone(), settings)
            .await
            .unwrap();
        let tx = db.new_transaction().await.unwrap();
        let table = tx.get_store::<Table<Value>>("notes").await.unwrap();
        table.set("00", json!({"note": payload})).await.unwrap();
        if readable {
            table
                .set("01", json!("OVERSIZE_MARKER".repeat(300)))
                .await
                .unwrap();
            for row in 2..=10 {
                table.set(format!("{row:02}"), json!(row)).await.unwrap();
            }
            tx.get_store::<Table<Value>>("_internal")
                .await
                .unwrap()
                .set("secret", json!("INTERNAL_MARKER"))
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        if alias == "Direct only" {
            let direct = person
                .acting_as(&owner_key, None)
                .unwrap()
                .open_database(&instance, db.root_id())
                .await
                .unwrap();
            assert_eq!(direct.current_permission().await.unwrap(), Permission::Read);
            assert_eq!(direct.get_name().await.unwrap(), name);
        } else if alias == "Legacy" {
            assert_eq!(
                owner
                    .open_database(db.root_id())
                    .await
                    .unwrap()
                    .get_name()
                    .await
                    .unwrap(),
                name
            );
        }
        app.set_reference("notes", alias, db.root_id())
            .await
            .unwrap();
    }
    app.set_reference("notes", "Missing", &ID::from_bytes(b"missing target"))
        .await
        .unwrap();

    let web = Web::start(&instance).await;
    let anonymous = web.get("/dashboard/accounts", None).await;
    assert_eq!(anonymous.status(), StatusCode::SEE_OTHER);
    assert_eq!(anonymous.headers()["location"], "/login");
    let cookie = web.login("person").await;
    let dashboard = web
        .get("/dashboard", Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    let landing = follow(
        &web,
        &dashboard,
        "Browse linked Accounts",
        &cookie,
        StatusCode::OK,
    )
    .await;
    assert!(landing.contains("Notes") && landing.contains("none selected"));
    assert!(!landing.contains(">Notes</a>"));
    assert!(!landing.contains("Nested"));

    let wrong = web
        .get(&select(&landing, "other"), Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    let wrong_denial = follow(&web, &wrong, "Notes", &cookie, StatusCode::NOT_FOUND).await;
    assert!(wrong_denial.contains("Target inaccessible or unavailable."));
    assert!(!wrong_denial.contains("Nested"));

    let selected = web
        .get(&select(&landing, "owner"), Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    let denied_account = follow(
        &web,
        &selected,
        "Other account",
        &cookie,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert!(!denied_account.contains("hidden-login"));
    let open = href(&selected, "Notes");
    let account = follow(&web, &selected, "Notes", &cookie, StatusCode::OK).await;
    for context in [
        "Signed in as:</strong> person",
        "Acting Identity:</strong> owner",
        "Viewed Account:</strong> Notes",
    ] {
        assert!(account.contains(context), "{context}");
    }
    assert!(!account.contains("app-login"));
    assert!(!account.contains("disposable app password"));
    let nested = follow(&web, &account, "Nested", &cookie, StatusCode::OK).await;
    assert!(nested.contains("Viewed Account:</strong> Nested"));
    let back = follow(&web, &nested, "Back", &cookie, StatusCode::OK).await;
    assert!(back.contains("Viewed Account:</strong> Notes"));

    let visible = follow(&web, &account, "Readable", &cookie, StatusCode::OK).await;
    assert!(visible.contains("Visible notes"));
    assert!(visible.contains("&lt;script&gt;visible note&lt;/script&gt;"));
    assert!(!visible.contains("<script>"));
    assert_eq!(visible.matches("<tr>").count(), 11); // header + 10 data rows
    assert!(visible.contains("Showing the first 10 rows only."));
    assert!(visible.contains("Value too large to preview"));
    assert!(!visible.contains("OVERSIZE_MARKER"));
    assert!(!visible.contains("INTERNAL_MARKER") && !visible.contains("_internal"));

    let missing = follow(&web, &account, "Missing", &cookie, StatusCode::NOT_FOUND).await;
    for (alias, forbidden) in [
        ("Direct only", "DIRECT_DB_"),
        ("Private", "PRIVATE_DB_"),
        ("Legacy", "LEGACY_ACCOUNT_"),
    ] {
        let denial = follow(&web, &account, alias, &cookie, StatusCode::NOT_FOUND).await;
        assert_eq!(denial, missing);
        assert!(!denial.contains(forbidden), "{forbidden}");
    }
    // After an alias is rebound to another accessible Account, an old URL must not retarget.
    person
        .set_reference("accounts", "Notes", &nested_id)
        .await
        .unwrap();
    assert_eq!(
        web.get(&open, Some(&cookie)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        web.post("/logout", Some(&cookie), &[]).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        web.get(&open, Some(&cookie)).await.status(),
        StatusCode::SEE_OTHER
    );
}
