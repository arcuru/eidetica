use eidetica::{
    Database, Error, Result,
    auth::{
        AuthKey, KeyStatus, Permission, PermissionBounds, SigKey,
        crypto::{PrivateKey, generate_keypair},
    },
    crdt::Doc,
    database::DatabaseKey,
    store::DocStore,
    sync::{DatabaseTicket, transports::http::HttpTransport},
    user::{IdentityStatus, UserError},
};

use super::helpers::{
    login_user, setup_instance, setup_instance_with_user, setup_local_instance_with_user,
};
use crate::sync::helpers::{setup_instance_with_initialized, start_sync_server};

async fn add_delegation(
    target: &Database,
    admin_key: PrivateKey,
    identity: &eidetica::user::Identity,
    bounds: PermissionBounds,
) -> Result<Database> {
    let target = target.clone().with_key(DatabaseKey::new(admin_key));
    let tx = target.new_transaction().await?;
    tx.get_settings()?
        .add_delegated_tree(identity.as_delegation(bounds).await?)
        .await?;
    tx.commit().await?;
    Ok(target)
}

#[tokio::test]
async fn lifecycle_and_key_management() -> Result<()> {
    let (instance, username) = setup_instance_with_user("identity_lifecycle", None).await;
    let mut user = login_user(&instance, &username, None).await;
    let default_key = user.get_default_key()?;

    let identity = user.create_identity("personal", &default_key).await?;
    assert_eq!(identity.name(), "personal");
    assert_eq!(identity.key_id(), &default_key);
    assert_eq!(
        user.identity_id("personal").await?,
        Some(identity.root_id().clone())
    );
    assert_eq!(user.identity_key("personal").await?, default_key);

    let tracked = user.identities().await?;
    assert_eq!(tracked.len(), 1);
    assert_eq!(tracked[0].0, "personal");
    assert_eq!(tracked[0].1.status, IdentityStatus::Active);

    let auth = identity.keys().await?;
    assert_eq!(auth.get_global_key()?.permissions(), &Permission::Read);
    assert_eq!(
        auth.get_key_by_pubkey(&default_key)?.permissions(),
        &Permission::Admin(0)
    );

    let (_, phone) = generate_keypair();
    identity
        .add_key(
            &phone,
            AuthKey::active(Some("phone"), Permission::Write(10)),
        )
        .await?;
    assert_eq!(
        identity.keys().await?.get_key_by_pubkey(&phone)?.status(),
        &KeyStatus::Active
    );
    identity.revoke_key(&phone).await?;
    assert_eq!(
        identity.keys().await?.get_key_by_pubkey(&phone)?.status(),
        &KeyStatus::Revoked
    );

    let duplicate = user.create_identity("personal", &default_key).await;
    assert!(matches!(
        duplicate,
        Err(Error::User(error)) if matches!(*error, UserError::IdentityAlreadyExists { .. })
    ));
    assert!(user.get_identity("unknown").await?.is_none());

    let root = identity.root_id().clone();
    user.remove_identity("personal").await?;
    assert!(user.get_identity("personal").await?.is_none());
    Database::open(&instance, &root).await?;
    assert!(matches!(
        user.remove_identity("personal").await,
        Err(Error::User(error)) if matches!(*error, UserError::IdentityNotFound { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn selected_key_changes_identity_and_target_signatures() -> Result<()> {
    let (instance, username) = setup_instance_with_user("identity_rotation", None).await;
    let mut user = login_user(&instance, &username, None).await;
    let old_key = user.get_default_key()?;
    let mut identity = user.create_identity("personal", &old_key).await?;

    let new_key = user.add_private_key(Some("rotated")).await?;
    let new_signing_key = user.get_signing_key(&new_key)?;
    identity
        .add_key(
            &new_key,
            AuthKey::active(Some("rotated"), Permission::Admin(0)),
        )
        .await?;

    let identity_before = identity.snapshot().await?;
    identity.set_key(new_key.clone(), new_signing_key).await?;
    let tx = identity.new_transaction().await?;
    tx.get_settings()?.set_name("rotated identity").await?;
    let identity_entry_id = tx.commit().await?;
    assert!(matches!(
        identity.get_entry(&identity_entry_id).await?.auth().key,
        SigKey::Direct { ref hint } if hint.pubkey.as_ref() == Some(&new_key)
    ));
    assert_ne!(identity.snapshot().await?, identity_before);

    let admin_key = PrivateKey::generate();
    let target = Database::create(&instance, admin_key.clone(), Doc::new()).await?;
    let target = add_delegation(
        &target,
        admin_key,
        &identity,
        PermissionBounds {
            max: Permission::Write(10),
            min: None,
        },
    )
    .await?;
    let delegated = identity.open_database(target.root_id()).await?;
    let tx = delegated.new_transaction().await?;
    tx.get_store::<DocStore>("data")
        .await?
        .set("signed", "rotated")
        .await?;
    let target_entry_id = tx.commit().await?;
    assert!(matches!(
        target.get_entry(&target_entry_id).await?.auth().key,
        SigKey::Delegation { ref path, ref hint }
            if path.first().is_some_and(|step| step.tree == *identity.root_id())
                && hint.pubkey.as_ref() == Some(&new_key)
    ));

    let reopened = user.get_identity("personal").await?.unwrap();
    assert_eq!(reopened.key_id(), &new_key);
    Ok(())
}

#[tokio::test]
async fn selected_key_rejects_global_only_and_mismatched_keys() -> Result<()> {
    let (instance, username) = setup_instance_with_user("identity_key_validation", None).await;
    let mut user = login_user(&instance, &username, None).await;
    let default_key = user.get_default_key()?;
    let mut identity = user.create_identity("personal", &default_key).await?;
    let outsider = user.add_private_key(Some("outsider")).await?;
    let outsider_private = user.get_signing_key(&outsider)?;

    assert!(
        identity
            .set_key(outsider.clone(), outsider_private.clone())
            .await
            .is_err()
    );
    assert!(matches!(
        identity
            .set_key(outsider, user.get_signing_key(&default_key)?)
            .await,
        Err(Error::User(error)) if matches!(*error, UserError::IdentityKeyMismatch { .. })
    ));
    assert_eq!(identity.key_id(), &default_key);
    Ok(())
}

#[tokio::test]
async fn open_database_is_restricted_to_the_selected_identity_root() -> Result<()> {
    let (instance, username) = setup_instance_with_user("identity_paths", None).await;
    let mut user = login_user(&instance, &username, None).await;
    let key = user.get_default_key()?;
    let identity_a = user.create_identity("a", &key).await?;
    let identity_b = user.create_identity("b", &key).await?;

    let admin_key = PrivateKey::generate();
    let target = Database::create(&instance, admin_key.clone(), Doc::new()).await?;
    let target = add_delegation(
        &target,
        admin_key,
        &identity_b,
        PermissionBounds {
            max: Permission::Write(10),
            min: None,
        },
    )
    .await?;

    assert!(identity_a.open_database(target.root_id()).await.is_err());
    let via_b = identity_b.open_database(target.root_id()).await?;
    assert!(matches!(
        via_b.auth_identity(),
        Some(SigKey::Delegation { path, .. })
            if path.first().is_some_and(|step| step.tree == *identity_b.root_id())
    ));
    Ok(())
}

#[tokio::test]
async fn public_identity_metadata_does_not_grant_target_membership() -> Result<()> {
    // This regression calls the static discovery helper directly. Connected
    // clients cannot inspect a private target without first choosing an
    // identity, while `Identity::open_database` covers that service path.
    let (instance, username) = setup_local_instance_with_user("identity_public", None).await;
    let mut user = login_user(&instance, &username, None).await;
    let member_key = user.get_default_key()?;
    let identity = user.create_identity("personal", &member_key).await?;

    let admin_key = PrivateKey::generate();
    let target = Database::create(&instance, admin_key.clone(), Doc::new()).await?;
    add_delegation(
        &target,
        admin_key,
        &identity,
        PermissionBounds {
            max: Permission::Write(10),
            min: None,
        },
    )
    .await?;

    let (_, outsider) = generate_keypair();
    assert!(
        Database::find_sigkeys(&instance, target.root_id(), &outsider)
            .await?
            .is_empty()
    );
    assert!(
        !Database::can_access(&instance, target.root_id(), &outsider, &Permission::Read,).await?
    );
    Ok(())
}

#[tokio::test]
async fn failed_registration_removes_provisional_tracking() -> Result<()> {
    let instance = setup_instance().await;
    instance.enable_sync().await?;
    crate::helpers::create_user(&instance, "identity_pending", None).await?;
    let mut user = login_user(&instance, "identity_pending", None).await;
    let key = user.get_default_key()?;
    let ticket = DatabaseTicket::with_addresses(
        eidetica::ID::from_bytes(b"pending identity"),
        vec![eidetica::sync::Address::http("127.0.0.1:1")],
    );

    assert!(
        user.register_identity(
            "pending",
            &ticket,
            &key,
            AuthKey::active(None, Permission::Admin(0)),
        )
        .await
        .is_err()
    );
    assert!(user.identity_id("pending").await?.is_none());
    Ok(())
}

#[tokio::test]
async fn register_identity_recovers_from_pending_bootstrap() -> Result<()> {
    let server = setup_instance_with_initialized().await;
    crate::helpers::create_user(&server, "identity_server", None).await?;
    let mut server_user = server.login_user("identity_server", None).await?;
    let server_key = server_user.get_default_key()?;
    let identity = server_user.create_identity("personal", &server_key).await?;
    let root = identity.root_id().clone();
    let server_sync = server.sync().unwrap();
    server_user
        .track_database(
            root.clone(),
            &server_key,
            eidetica::user::SyncSettings::enabled(),
        )
        .await?;
    server_sync
        .sync_user(
            server_user.user_uuid(),
            server_user.user_database().root_id(),
        )
        .await?;
    let address = start_sync_server(&server_sync).await;
    let ticket = DatabaseTicket::with_addresses(root.clone(), vec![address.clone()]);

    let client = setup_instance_with_initialized().await;
    crate::helpers::create_user(&client, "identity_client", None).await?;
    let mut client_user = client.login_user("identity_client", None).await?;
    let client_key = client_user.get_default_key()?;
    let client_sync = client.sync().unwrap();
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await?;

    client_user
        .register_identity(
            "personal",
            &ticket,
            &client_key,
            AuthKey::active(Some("client"), Permission::Admin(0)),
        )
        .await?;
    assert_eq!(
        client_user.identities().await?[0].1.status,
        IdentityStatus::Pending
    );
    assert!(client_user.get_identity("personal").await?.is_none());

    let pending = server_sync.pending_bootstrap_requests().await?;
    let request_id = pending
        .iter()
        .find(|(_, request)| request.tree_id == root)
        .map(|(id, _)| id)
        .expect("identity bootstrap request must reach the owner");
    server_user
        .approve_bootstrap_request(&server_sync, request_id, &server_key)
        .await?;

    client_sync.sync_with_ticket(&ticket).await?;
    let activated = client_user.activate_identity("personal").await?;
    assert_eq!(activated.root_id(), &root);
    assert_eq!(activated.key_id(), &client_key);
    assert_eq!(
        client_user.identities().await?[0].1.status,
        IdentityStatus::Active
    );

    server_sync.stop_server().await?;
    Ok(())
}
