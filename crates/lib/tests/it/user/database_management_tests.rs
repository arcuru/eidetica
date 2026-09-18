use std::time::Duration;

use eidetica::{
    Instance, Result,
    auth::{AuthKey, Permission},
    sync::{DatabaseTicket, transports::http::HttpTransport},
    user::{PreferenceWriteOutcome, SyncSettings},
};

use super::helpers::{create_user_database, setup_instance_with_user};

async fn setup_full() -> Result<(Instance, eidetica::user::User, eidetica::Database)> {
    #[cfg(all(unix, feature = "service"))]
    if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
        use eidetica::{NewUser, backend::database::InMemory, service::ServiceServer};
        use tokio::sync::watch;

        let dir = Box::leak(Box::new(tempfile::tempdir()?));
        let socket = dir.path().join("management.sock");
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

async fn setup() -> Result<(Instance, eidetica::user::User, eidetica::entry::ID)> {
    let (instance, user, database) = setup_full().await?;
    let database_id = database.root_id().clone();
    Ok((instance, user, database_id))
}

#[tokio::test]
async fn snapshot_pins_the_settings_read_to_the_user_database() -> Result<()> {
    let (_instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;

    let before = management.snapshot().await?;
    assert_eq!(before.source, user.user_database().snapshot().await?);
    assert!(!before.settings.sync_enabled);

    management.share().await?.into_result()?;
    let after = management.snapshot().await?;
    assert_eq!(after.source, user.user_database().snapshot().await?);
    assert_ne!(after.source, before.source);
    assert!(after.settings.sync_enabled);
    Ok(())
}

#[tokio::test]
async fn preference_write_is_acknowledged_before_locator_query() -> Result<()> {
    let (_instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;

    assert!(matches!(
        management.share().await?,
        PreferenceWriteOutcome::Written(_)
    ));
    assert!(management.snapshot().await?.settings.sync_enabled);
    let ticket = management.ticket().await?;
    assert_eq!(ticket.database_id(), &database_id);
    assert!(ticket.addresses().is_empty());
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
    let management = alice.manage_database(&database_id).await?;
    assert!(management.ticket().await.is_err());

    let sync = instance.sync().unwrap();
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    sync.accept_connections().await?;

    // Another user enables the owner's combined state, but that does not make
    // this caller eligible for a locator.
    let mut bob = instance.login_user("bob", None).await?;
    let bob_key = bob.get_default_key()?;
    bob.track_database(database_id.clone(), &bob_key, SyncSettings::enabled())
        .await?;
    assert!(management.ticket().await.is_err());

    management.share().await?.into_result()?;
    assert!(!management.ticket().await?.addresses().is_empty());
    sync.stop_server().await?;
    Ok(())
}

#[tokio::test]
async fn live_address_makes_ticket_ready_and_stop_preserves_other_settings() -> Result<()> {
    let (instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;
    management.share().await?.into_result()?;

    let sync = instance.sync().unwrap();
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    sync.accept_connections().await?;

    let ticket = management.ticket().await?;
    let encoded = ticket.to_string();
    let decoded: DatabaseTicket = encoded.parse()?;
    assert_eq!(decoded.database_id(), &database_id);
    assert!(!decoded.addresses().is_empty());

    management.stop_sharing().await?.into_result()?;
    let stopped = management.snapshot().await?;
    assert!(!stopped.settings.sync_enabled);
    sync.stop_server().await?;
    Ok(())
}

#[tokio::test]
async fn runtime_address_changes_do_not_advance_settings_watch() -> Result<()> {
    let (instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;
    management.share().await?.into_result()?;
    let mut watch = management.watch().await?;
    let sync = instance.sync().unwrap();
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;

    sync.accept_connections().await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), watch.changed())
            .await
            .is_err()
    );
    assert!(!management.ticket().await?.addresses().is_empty());

    sync.stop_server().await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), watch.changed())
            .await
            .is_err()
    );
    assert!(management.ticket().await?.addresses().is_empty());
    Ok(())
}

#[tokio::test]
async fn wait_timeout_does_not_change_preference() -> Result<()> {
    let (_instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;
    assert!(
        management
            .wait_for(Duration::from_millis(20), |snapshot| {
                snapshot.settings.sync_on_commit
            })
            .await?
            .is_none()
    );
    assert!(!management.snapshot().await?.settings.sync_enabled);
    Ok(())
}

#[tokio::test]
async fn watch_observes_actual_preference_write_through_native_callback() -> Result<()> {
    let (_instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;
    let mut watch = management.watch().await?;
    let before = watch.current().source;

    management.share().await?.into_result()?;
    let changed = tokio::time::timeout(Duration::from_secs(2), watch.changed())
        .await
        .expect("preference write did not reach native callback")?;
    assert!(changed.settings.sync_enabled);
    assert_ne!(changed.source, before);
    Ok(())
}

#[tokio::test]
async fn other_users_preferences_are_not_exposed_or_watched() -> Result<()> {
    let instance = crate::helpers::test_local_instance().await;
    crate::helpers::create_user(&instance, "alice", None).await?;
    crate::helpers::create_user(&instance, "bob", None).await?;
    instance.enable_sync().await?;

    let mut alice = instance.login_user("alice", None).await?;
    let alice_key = alice.get_default_key()?;
    let database = create_user_database(&mut alice).await;
    let database_id = database.root_id().clone();
    let tx = database.new_transaction().await?;
    tx.get_settings()?
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await?;
    tx.commit().await?;

    alice
        .track_database(
            database_id.clone(),
            &alice_key,
            SyncSettings::on_commit().with_interval(17),
        )
        .await?;
    let mut bob = instance.login_user("bob", None).await?;
    let bob_key = bob.get_default_key()?;
    bob.track_database(database_id.clone(), &bob_key, SyncSettings::disabled())
        .await?;

    let management = alice.manage_database(&database_id).await?;
    management.stop_sharing().await?.into_result()?;
    let mut watch = management.watch().await?;
    let alice_before = watch.current();
    bob.manage_database(&database_id)
        .await?
        .share()
        .await?
        .into_result()?;

    assert!(
        tokio::time::timeout(Duration::from_millis(50), watch.changed())
            .await
            .is_err()
    );
    let alice_after = management.snapshot().await?;
    assert_eq!(alice_after.source, alice_before.source);
    assert!(!alice_after.settings.sync_enabled);
    assert!(alice_after.settings.sync_on_commit);
    assert_eq!(alice_after.settings.interval_seconds, Some(17));
    Ok(())
}

#[tokio::test]
async fn repeated_preference_write_is_idempotent() -> Result<()> {
    let (_instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;

    let first = management.share().await?.into_result()?;
    let tips = user.user_database().snapshot().await?;
    let second = management.share().await?.into_result()?;

    assert!(first.entry_id.is_some());
    assert!(second.entry_id.is_none());
    assert_eq!(user.user_database().snapshot().await?, tips);
    Ok(())
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn service_watch_stops_after_read_authority_is_revoked() -> Result<()> {
    use eidetica::auth::types::KeyStatus;

    if std::env::var("TEST_BACKEND").as_deref() != Ok("service") {
        return Ok(());
    }
    let (_owner, user, owner_database) = setup_full().await?;
    let database_id = owner_database.root_id().clone();
    let user_key = user.get_default_key()?;
    let mut watch = user.manage_database(&database_id).await?.watch().await?;

    let tx = owner_database.new_transaction().await?;
    tx.get_settings()?
        .set_auth_key(
            &user_key,
            AuthKey::new(Some("revoked"), Permission::Read, KeyStatus::Revoked),
        )
        .await?;
    tx.commit().await?;

    assert!(user.manage_database(&database_id).await.is_err());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), watch.changed())
            .await
            .expect("target authorization change did not reach native callback")
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn dropped_watch_unregisters_native_callbacks() -> Result<()> {
    let (_instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;
    let watch = management.watch().await?;
    drop(watch);

    management.share().await?.into_result()?;
    assert!(management.snapshot().await?.settings.sync_enabled);
    Ok(())
}
