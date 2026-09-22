use eidetica::{
    Error, Instance, Result,
    auth::{AuthKey, Permission},
    sync::{SyncError, transports::http::HttpTransport},
    user::{SyncSettings, UserError},
};

use super::helpers::{create_user_database, setup_instance_with_user};

async fn setup_full() -> Result<(Instance, eidetica::user::User, eidetica::Database)> {
    #[cfg(all(unix, feature = "service"))]
    if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
        use eidetica::{NewUser, backend::database::InMemory, service::ServiceServer};
        use tokio::sync::watch;

        let dir = Box::leak(Box::new(tempfile::tempdir()?));
        let socket = dir.path().join("database-sharing.sock");
        let (owner, _) =
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("manager"))
                .await?;
        owner.enable_sync().await?;
        let server = ServiceServer::bind(owner.clone(), &socket).await?;
        let (shutdown, rx) = watch::channel(());
        Box::leak(Box::new(shutdown));
        tokio::spawn(server.run(rx));

        let client = Instance::connect(format!("unix://{}", socket.display())).await?;
        let mut user = client.login_user("manager", None).await?;
        let database = create_user_database(&mut user).await;
        return Ok((owner, user, database));
    }

    let (instance, _) = setup_instance_with_user("manager", None).await;
    instance.enable_sync().await?;
    let mut user = instance.login_user("manager", None).await?;
    let database = create_user_database(&mut user).await;
    Ok((instance, user, database))
}

#[tokio::test]
async fn database_handle_reads_and_writes_its_owners_preference() -> Result<()> {
    let (_instance, user, database) = setup_full().await?;

    assert!(!database.is_shared().await?);
    assert!(!database.sync_settings().await?.sync_enabled);

    database.share().await?;
    assert!(database.is_shared().await?);
    assert!(database.sync_settings().await?.sync_enabled);
    assert!(
        user.database(database.root_id())
            .await?
            .sync_settings
            .sync_enabled
    );

    database.stop_sharing().await?;
    assert!(!database.is_shared().await?);
    Ok(())
}

#[tokio::test]
async fn preference_write_is_acknowledged_before_locator_query() -> Result<()> {
    let (_instance, _user, database) = setup_full().await?;

    database.share().await?;
    let ticket = database.ticket().await?;
    assert_eq!(ticket.database_id(), database.root_id());
    assert!(ticket.addresses().is_empty());
    Ok(())
}

#[tokio::test]
async fn sharing_intent_does_not_require_an_attached_sync_engine() -> Result<()> {
    let instance = crate::helpers::test_local_instance().await;
    crate::helpers::create_user(&instance, "manager", None).await?;
    let mut user = instance.login_user("manager", None).await?;
    let database = create_user_database(&mut user).await;

    database.share().await?;
    assert!(database.is_shared().await?);
    assert!(matches!(
        database.ticket().await,
        Err(Error::Sync(error)) if matches!(*error, SyncError::SyncNotEnabled)
    ));
    Ok(())
}

#[tokio::test]
async fn sharing_rejects_a_handle_after_its_database_is_untracked() -> Result<()> {
    let (_instance, mut user, database) = setup_full().await?;
    user.untrack_database(database.root_id()).await?;

    assert!(matches!(
        database.share().await,
        Err(Error::User(error)) if matches!(*error, UserError::DatabaseNotTracked { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn ticket_requires_this_users_sharing_setting() -> Result<()> {
    let instance = crate::helpers::test_local_instance().await;
    crate::helpers::create_user(&instance, "alice", None).await?;
    crate::helpers::create_user(&instance, "bob", None).await?;
    instance.enable_sync().await?;
    let mut alice = instance.login_user("alice", None).await?;
    let database = create_user_database(&mut alice).await;
    let database_id = database.root_id().clone();
    let tx = database.new_transaction().await?;
    tx.get_settings()?
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await?;
    tx.commit().await?;
    assert!(database.ticket().await.is_err());

    let sync = instance.sync().unwrap();
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    sync.accept_connections().await?;

    let mut bob = instance.login_user("bob", None).await?;
    let bob_key = bob.get_default_key()?;
    bob.track_database(database_id.clone(), &bob_key, SyncSettings::enabled())
        .await?;
    let bob_database = bob.open_database(&database_id).await?;
    bob_database.share().await?;
    assert!(database.ticket().await.is_err());

    database.share().await?;
    assert!(!database.ticket().await?.addresses().is_empty());
    sync.stop_server().await?;
    Ok(())
}

#[tokio::test]
async fn stop_sharing_preserves_other_sync_settings() -> Result<()> {
    let (_instance, mut user, database) = setup_full().await?;
    let key = user.get_default_key()?;
    user.track_database(
        database.root_id().clone(),
        &key,
        SyncSettings::on_commit().with_interval(17),
    )
    .await?;
    let database = user.open_database(database.root_id()).await?;

    database.stop_sharing().await?;
    let settings = database.sync_settings().await?;
    assert!(!settings.sync_enabled);
    assert!(settings.sync_on_commit);
    assert_eq!(settings.interval_seconds, Some(17));
    Ok(())
}

#[tokio::test]
async fn repeated_preference_write_is_idempotent() -> Result<()> {
    let (_instance, user, database) = setup_full().await?;

    database.share().await?;
    let tips = user.user_database().snapshot().await?;
    database.share().await?;

    assert_eq!(user.user_database().snapshot().await?, tips);
    Ok(())
}

#[tokio::test]
async fn plain_database_handle_lacks_user_sharing_capability() -> Result<()> {
    let (instance, _user, database) = setup_full().await?;
    let plain = eidetica::Database::open(&instance, database.root_id()).await?;

    let error = plain
        .share()
        .await
        .expect_err("plain handle must not share");
    assert!(error.to_string().contains("lacks the user capability"));
    Ok(())
}

#[tokio::test]
async fn sharing_methods_reject_a_revoked_database_key() -> Result<()> {
    use eidetica::auth::types::KeyStatus;

    let (_instance, user, database) = setup_full().await?;
    let key = user.get_default_key()?;
    let tx = database.new_transaction().await?;
    tx.get_settings()?
        .set_auth_key(
            &key,
            AuthKey::new(Some("revoked"), Permission::Read, KeyStatus::Revoked),
        )
        .await?;
    tx.commit().await?;

    assert!(database.is_shared().await.is_err());
    assert!(database.share().await.is_err());
    Ok(())
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn service_database_handle_uses_its_registered_identity() -> Result<()> {
    if std::env::var("TEST_BACKEND").as_deref() != Ok("service") {
        return Ok(());
    }
    let (_owner, _user, database) = setup_full().await?;
    database.share().await?;
    assert_eq!(database.ticket().await?.database_id(), database.root_id());
    Ok(())
}
