//! The service integration derivation sets EIDETICA_EXTERNAL_SOCKET to exercise
//! the *external* daemon binary; ordinary unit runs have no external daemon.
#[tokio::test]
async fn socket_write_visible_to_dashboard() {
    let Ok(socket) = std::env::var("EIDETICA_EXTERNAL_SOCKET") else {
        return;
    };
    let instance = eidetica::Instance::connect(format!("unix://{socket}"))
        .await
        .expect("connect to real daemon");
    let mut admin = instance.login_user("admin", None).await.expect("login");
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "socket_written_database");
    let key = admin.get_default_key().expect("default key");
    let db = admin
        .create_database(settings, &key)
        .await
        .expect("write over socket");
    std::fs::write(
        std::env::var("EIDETICA_EXTERNAL_DB_ID_FILE").expect("result path"),
        db.root_id().to_string(),
    )
    .expect("record ID for dashboard assertion");
}
