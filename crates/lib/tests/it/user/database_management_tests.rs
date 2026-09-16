use std::time::Duration;

use eidetica::{
    Instance, Result,
    auth::{AuthKey, Permission},
    sync::{DatabaseTicket, transports::http::HttpTransport},
    user::{AppliedState, PreferenceWriteOutcome, SyncSettings, TicketNotReady, TicketStatus},
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
async fn preference_write_is_acknowledged_before_ticket_readiness() -> Result<()> {
    let (_instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;

    assert!(matches!(
        management.share().await?,
        PreferenceWriteOutcome::Written(_)
    ));
    assert!(matches!(
        management.ticket().await?,
        TicketStatus::NotReady(TicketNotReady::NoLiveAddress)
            | TicketStatus::NotReady(TicketNotReady::DesiredNotApplied)
    ));
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

    let snapshot = management
        .wait_for(Duration::from_secs(2), |snapshot| {
            snapshot.applied == AppliedState::Current
                && !snapshot.observed.listen_addresses.is_empty()
        })
        .await?
        .expect("owner did not apply sharing with a live address");
    assert!(snapshot.effective.unwrap().sync_enabled);
    match management.ticket().await? {
        TicketStatus::Ready(ticket) => {
            let encoded = ticket.to_string();
            let decoded: DatabaseTicket = encoded.parse()?;
            assert_eq!(decoded.database_id(), &database_id);
            assert!(!decoded.addresses().is_empty());
        }
        other => panic!("expected ready ticket, got {other:?}"),
    }

    management.stop_sharing().await?.into_result()?;
    let stopped = management
        .wait_for(Duration::from_secs(2), |snapshot| {
            snapshot.applied == AppliedState::Current && !snapshot.desired.sync_enabled
        })
        .await?
        .expect("owner did not apply stopped sharing");
    assert!(!stopped.desired.sync_enabled);
    sync.stop_server().await?;
    Ok(())
}

#[tokio::test]
async fn runtime_address_changes_invalidate_watch_without_database_writes() -> Result<()> {
    let (instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;
    management.share().await?.into_result()?;
    let sync = instance.sync().unwrap();
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;

    let mut watch = management.watch().await?;
    sync.accept_connections().await?;
    let started = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = watch.changed().await?;
            if !snapshot.observed.listen_addresses.is_empty() {
                return Ok::<_, eidetica::Error>(snapshot);
            }
        }
    })
    .await
    .expect("address start did not trigger watch")?;
    assert!(!started.observed.listen_addresses.is_empty());

    sync.stop_server().await?;
    let stopped = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = watch.changed().await?;
            if snapshot.observed.listen_addresses.is_empty() {
                return Ok::<_, eidetica::Error>(snapshot);
            }
        }
    })
    .await
    .expect("address stop did not trigger watch")?;
    assert!(stopped.observed.listen_addresses.is_empty());
    Ok(())
}

#[tokio::test]
async fn wait_timeout_does_not_change_preference() -> Result<()> {
    let (_instance, user, database_id) = setup().await?;
    let management = user.manage_database(&database_id).await?;
    assert!(
        management
            .wait_for(Duration::from_millis(20), |snapshot| {
                !snapshot.observed.listen_addresses.is_empty()
            })
            .await?
            .is_none()
    );
    assert!(!management.snapshot().await?.desired.sync_enabled);
    Ok(())
}

#[tokio::test]
async fn stop_sharing_preserves_settings_and_another_users_effective_enablement() -> Result<()> {
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
    bob.track_database(database_id.clone(), &bob_key, SyncSettings::enabled())
        .await?;

    let management = alice.manage_database(&database_id).await?;
    management.stop_sharing().await?.into_result()?;
    let snapshot = management.snapshot().await?;
    assert!(!snapshot.desired.sync_enabled);
    assert!(snapshot.desired.sync_on_commit);
    assert_eq!(snapshot.desired.interval_seconds, Some(17));
    assert!(
        snapshot
            .effective
            .is_some_and(|settings| settings.sync_enabled)
    );
    Ok(())
}

#[tokio::test]
async fn database_generations_do_not_reveal_other_database_activity() -> Result<()> {
    let (instance, mut user, first_id) = setup().await?;
    let second = create_user_database(&mut user).await;
    let second_id = second.root_id().clone();
    let first = user.manage_database(&first_id).await?;
    let second = user.manage_database(&second_id).await?;

    let first_before = first.snapshot().await?.observed.generation;
    second.share().await?.into_result()?;
    let first_after = first.snapshot().await?.observed.generation;
    let second_after = second.snapshot().await?.observed.generation;

    assert_eq!(first_after, first_before);
    assert!(second_after > first_after);
    drop(instance);
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

#[tokio::test]
async fn owner_run_changes_across_instance_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let snapshot = dir.path().join("owner.json");
    let url = format!("memory://{}", snapshot.display());
    let (owner, user) =
        Instance::connect_or_create(&url, eidetica::NewUser::passwordless("manager")).await?;
    let mut user = user.expect("new snapshot-backed instance returns its bootstrap user");
    owner.enable_sync().await?;
    let database = create_user_database(&mut user).await;
    let database_id = database.root_id().clone();
    let before = user
        .manage_database(&database_id)
        .await?
        .snapshot()
        .await?
        .observed
        .owner_run;
    owner.flush()?;
    drop(database);
    drop(user);
    drop(owner);

    let restarted = Instance::connect(&url).await?;
    restarted.enable_sync().await?;
    let user = restarted.login_user("manager", None).await?;
    let after = user
        .manage_database(&database_id)
        .await?
        .snapshot()
        .await?
        .observed
        .owner_run;

    assert_ne!(after, before);
    Ok(())
}

#[tokio::test]
async fn successful_peer_exchange_updates_only_the_database_observation() -> Result<()> {
    use eidetica::{crdt::Doc, testing::Cluster};

    let mut cluster = Cluster::builder().peers(2).build().await?;
    let key = cluster.peer(0).key_id().clone();
    let mut settings = Doc::new();
    settings.set("name", "managed");
    let managed = cluster
        .peer_mut(0)
        .user_mut()
        .create_database(settings, &key)
        .await?;
    let managed_id = managed.root_id().clone();
    let key = cluster.peer(0).key_id().clone();
    let mut settings = Doc::new();
    settings.set("name", "unrelated");
    let unrelated = cluster
        .peer_mut(0)
        .user_mut()
        .create_database(settings, &key)
        .await?;
    let unrelated_id = unrelated.root_id().clone();
    let tx = managed.new_transaction().await?;
    tx.get_settings()?
        .set_global_auth_key(AuthKey::active(None, Permission::Write(10)))
        .await?;
    tx.commit().await?;

    cluster.peer_mut(0).serve(&managed_id).await?;
    cluster
        .bootstrap(0, 1, &managed_id, Permission::Write(10))
        .await?;
    cluster.peer_mut(1).serve(&managed_id).await?;
    let management = cluster.peer(0).user().manage_database(&managed_id).await?;
    let unrelated = cluster
        .peer(0)
        .user()
        .manage_database(&unrelated_id)
        .await?;
    let mut watch = management.watch().await?;
    let unrelated_generation = unrelated.snapshot().await?.observed.generation;

    cluster.exchange(0, 1, &managed_id).await?;
    let observed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = watch.changed().await?;
            if snapshot
                .observed
                .peers
                .iter()
                .any(|peer| peer.last_success_ms.is_some())
            {
                return Ok::<_, eidetica::Error>(snapshot);
            }
        }
    })
    .await
    .expect("successful peer exchange did not invalidate management watch")?;

    assert!(
        observed
            .observed
            .peers
            .iter()
            .all(|peer| peer.observed_at_ms > 0)
    );
    assert_eq!(
        unrelated.snapshot().await?.observed.generation,
        unrelated_generation
    );
    Ok(())
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn service_watch_stops_after_read_authority_is_revoked() -> Result<()> {
    use eidetica::auth::types::KeyStatus;

    if std::env::var("TEST_BACKEND").as_deref() != Ok("service") {
        return Ok(());
    }
    let (owner, user, owner_database) = setup_full().await?;
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

    loop {
        if watch.changed().await.is_err() {
            break;
        }
    }
    assert!(user.manage_database(&database_id).await.is_err());
    drop(owner);
    Ok(())
}
