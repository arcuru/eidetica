use super::*;
use eidetica::{
    Database, ID, Result,
    account::SyncSettings,
    auth::{AuthKey, PublicKey},
    service::RemoteConnection,
    sync::{Address, HostSyncRegistration, SyncPeerInfo},
};

async fn create_db(instance: &Instance, login: &str, name: &str) -> Result<Database> {
    let mut account = instance.login_account(login, "").await?;
    let key = account.get_default_key()?;
    let mut settings = Doc::new();
    settings.set("name", name);
    account.create_database(settings, &key).await
}

async fn grant(db: &Database, key: &PublicKey) -> Result<()> {
    let txn = db.new_transaction().await?;
    txn.get_settings()?
        .set_auth_key(key, AuthKey::active(None, Permission::Read))
        .await?;
    txn.commit().await?;
    Ok(())
}

fn mapping(tree_id: &ID, peer_pubkey: PublicKey) -> SyncPeerInfo {
    SyncPeerInfo {
        peer_pubkey,
        tree_id: tree_id.clone(),
        addresses: vec![Address::http("127.0.0.1:12345")],
        auth: None,
        display_name: Some("not returned".into()),
    }
}

#[tokio::test]
async fn host_sync_observation_separates_account_preference_registration_and_peers() -> Result<()> {
    let (socket, _shutdown, daemon, _dir) = start_test_server().await;
    daemon.enable_sync().await?;
    create_user_via_admin(&daemon, "participant").await;
    let bank = create_db(&daemon, "admin", "bank").await?;
    let other = create_db(&daemon, "admin", "other").await?;
    let root = bank.root_id();
    let sync = daemon.sync().unwrap();

    let server_url = format!("unix://{}", socket.display());
    let client = Instance::connect(&server_url).await?;
    let admin = client.login_account("admin", "").await?;
    let operator = remote_conn(&client);
    let identity = SigKey::from_pubkey(&admin.get_default_key()?);
    let unregistered = operator.observe_host_sync(root, identity.clone()).await?;
    assert_eq!(
        unregistered.registration,
        HostSyncRegistration::Unregistered
    );
    assert_eq!(unregistered.mapped_peers, Some(vec![]));

    let participant_client = Instance::connect(&server_url).await?;
    let mut participant = participant_client.login_account("participant", "").await?;
    let participant_key = participant.get_default_key()?;
    grant(&bank, &participant_key).await?;
    let mut settings = SyncSettings::on_commit().with_interval(60);
    settings
        .properties
        .insert("application".into(), "not returned".into());
    participant
        .track_database(root, &participant_key, settings)
        .await?;

    let other_peer = generate_keypair().1;
    sync.register_sync_peer(mapping(other.root_id(), other_peer))
        .await?;
    let before = sync.sync_tree().snapshot().await?;
    let no_peers = operator.observe_host_sync(root, identity.clone()).await?;
    assert!(!admin.is_sync_enabled(root).await?);
    assert_eq!(no_peers.mapped_peers, Some(vec![]));
    assert_eq!(
        sync.sync_tree().snapshot().await?,
        before,
        "read must not commit to _sync"
    );

    let peer = generate_keypair().1;
    sync.register_sync_peer(mapping(root, peer.clone())).await?;
    let with_peer = operator.observe_host_sync(root, identity).await?;
    assert_eq!(with_peer.registration, no_peers.registration);
    assert!(with_peer.observed_at_ms > 0);
    assert_eq!(
        serde_json::to_value(&with_peer)?,
        serde_json::json!({
            "root_id": root,
            "daemon_id": daemon.id(),
            "observed_at_ms": with_peer.observed_at_ms,
            "registration": {"Registered": {
                "sync_enabled": true, "sync_on_commit": true, "interval_seconds": 60,
            }},
            "mapped_peers": [peer],
        })
    );
    Ok(())
}

#[tokio::test]
async fn host_sync_observation_requires_both_live_grants_and_the_exact_root() -> Result<()> {
    let (socket, _shutdown, daemon, _dir) = start_test_server().await;
    daemon.enable_sync().await?;
    create_user_via_admin(&daemon, "owner").await;
    create_user_via_admin(&daemon, "reader").await;
    let bank = create_db(&daemon, "owner", "bank").await?;
    let alias = create_db(&daemon, "owner", "bank").await?;
    assert_ne!(bank.root_id(), alias.root_id());
    let root = bank.root_id();

    let reader = login_client(&socket, "reader").await;
    let reader = remote_conn(&reader);
    let reader_id = reader.session_identity().unwrap();
    grant(&bank, reader_id.hint().pubkey.as_ref().unwrap()).await?;
    let owner = login_client(&socket, "owner").await;
    let owner = remote_conn(&owner);
    let owner_id = owner.session_identity().unwrap();
    let admin = login_client(&socket, "admin").await;
    let admin = remote_conn(&admin);
    let admin_id = admin.session_identity().unwrap();
    let admin_key = admin_id.hint().pubkey.as_ref().unwrap();
    grant(&bank, admin_key).await?;

    let allowed = admin.observe_host_sync(root, admin_id.clone()).await?;
    assert_eq!(&allowed.root_id, root);
    assert_eq!(allowed.registration, HostSyncRegistration::Unregistered);
    // Only the admin with target Read may observe; a request cannot name
    // an unproven actor even when that actor is a member of the target.
    assert!(reader.observe_host_sync(root, reader_id).await.is_err());
    assert!(
        owner
            .observe_host_sync(root, owner_id.clone())
            .await
            .is_err()
    );
    assert!(admin.observe_host_sync(root, owner_id).await.is_err());
    let socket_only = RemoteConnection::connect(&socket).await?;
    assert!(
        socket_only
            .observe_host_sync(root, admin_id.clone())
            .await
            .is_err()
    );
    assert!(
        admin
            .observe_host_sync(alias.root_id(), admin_id.clone())
            .await
            .is_err()
    );
    assert!(
        admin
            .observe_host_sync(&ID::from_bytes("absent root"), admin_id.clone())
            .await
            .is_err()
    );

    let txn = bank.new_transaction().await?;
    txn.get_settings()?.revoke_auth_key(admin_key).await?;
    txn.commit().await?;
    assert!(admin.observe_host_sync(root, admin_id).await.is_err());
    Ok(())
}

#[tokio::test]
async fn host_sync_observation_does_not_substitute_empty_for_missing_or_stale_state() -> Result<()>
{
    let (socket, _shutdown, daemon, _dir) = start_test_server().await;
    let bank = create_db(&daemon, "admin", "bank").await?;
    let root = bank.root_id();
    let client = login_client(&socket, "admin").await;
    let conn = remote_conn(&client);
    let identity = conn.session_identity().unwrap();
    let missing = conn.observe_host_sync(root, identity.clone()).await?;
    assert_eq!(missing.registration, HostSyncRegistration::Unknown);
    assert_eq!(missing.mapped_peers, None);

    daemon.enable_sync().await?;
    let mut admin = client.login_account("admin", "").await?;
    let key = admin.get_default_key()?;
    admin
        .track_database(root, &key, SyncSettings::enabled())
        .await?;
    let sync = daemon.sync().unwrap();
    let peer = generate_keypair().1;
    sync.register_sync_peer(mapping(root, peer.clone())).await?;
    let current = conn.observe_host_sync(root, identity.clone()).await?;
    assert!(matches!(
        current.registration,
        HostSyncRegistration::Registered { .. }
    ));
    assert_eq!(current.mapped_peers, Some(vec![peer.clone()]));

    // A missing combined value does not make a known mapping unknown.
    let txn = sync.sync_tree().new_transaction().await?;
    txn.get_store::<DocStore>("database_users")
        .await?
        .delete(format!("{root}.combined_settings"))
        .await?;
    let registration_tip = txn.commit().await?;
    let incomplete = conn.observe_host_sync(root, identity.clone()).await?;
    assert_eq!(incomplete.registration, HostSyncRegistration::Unknown);
    assert_eq!(incomplete.mapped_peers, current.mapped_peers);

    // Simulate a newer state pending verification. The previous accepted
    // frontier is healthy but is not a current observation.
    let engine = daemon.backend().local_engine().unwrap();
    engine
        .update_verification_status(&registration_tip, VerificationStatus::Unverified)
        .await?;
    let stale = conn.observe_host_sync(root, identity.clone()).await?;
    assert_eq!(stale.registration, HostSyncRegistration::Unknown);
    assert_eq!(stale.mapped_peers, None);
    assert_eq!(
        engine.get_verification_status(&registration_tip).await?,
        VerificationStatus::Unverified
    );

    // Mark the incomplete branch failed, then simulate a dangling mapping
    // on the accepted lineage. The read must not return a partial list.
    engine
        .update_verification_status(&registration_tip, VerificationStatus::Failed)
        .await?;
    let txn = sync.sync_tree().new_transaction().await?;
    txn.get_store::<DocStore>("peers")
        .await?
        .delete(peer.to_string())
        .await?;
    txn.commit().await?;
    let dangling = conn.observe_host_sync(root, identity).await?;
    assert_eq!(dangling.registration, current.registration);
    assert_eq!(dangling.mapped_peers, None);
    Ok(())
}
