use eidetica::{
    Database, Error, Result,
    account::{AccountError, IdentityRegistration, IdentityStatus, SyncSettings},
    auth::{
        AuthKey, KeyStatus, Permission, PermissionBounds, SigKey,
        crypto::{PrivateKey, generate_keypair},
    },
    crdt::Doc,
    database::DatabaseKey,
    store::DocStore,
    sync::DatabaseTicket,
};

use super::helpers::{login_account, setup_instance_with_user, setup_local_instance_with_user};

async fn add_delegation(
    target: &Database,
    admin_key: PrivateKey,
    identity: &eidetica::account::Identity,
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
    let (instance, username) = setup_instance_with_user("identity_lifecycle", "").await;
    let mut user = login_account(&instance, &username, "").await;
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
        Err(Error::Account(error)) if matches!(*error, AccountError::IdentityAlreadyExists { .. })
    ));
    let before_getters = user.account().database().snapshot().await?;
    assert!(user.get_identity("unknown").await?.is_none());
    assert!(user.tracked_identity("unknown").await?.is_none());
    assert!(user.get_identity("personal").await?.is_some());
    assert_eq!(user.account().database().snapshot().await?, before_getters);

    let root = identity.root_id().clone();
    let preferences = SyncSettings::on_commit().with_interval(123);
    user.track_database(root.clone(), &default_key, preferences.clone())
        .await?;
    user.forget_identity("personal").await?;
    assert!(user.get_identity("personal").await?.is_none());
    assert!(user.tracked_identity("personal").await?.is_none());
    assert_eq!(user.identity_id("personal").await?, Some(root.clone()));
    assert_eq!(
        serde_json::to_value(user.database(&root).await?.sync_settings)?,
        serde_json::to_value(preferences)?
    );
    assert!(
        identity
            .keys()
            .await?
            .get_key_by_pubkey(&default_key)?
            .is_active()
    );
    let tx = identity.database().new_transaction().await?;
    tx.get_settings()?
        .set_name("still exists after forgetting")
        .await?;
    tx.commit().await?;
    user.open_database(&root).await?;
    assert!(matches!(
        user.forget_identity("personal").await,
        Err(Error::Account(error)) if matches!(*error, AccountError::IdentityNotFound { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn selected_key_changes_identity_and_target_signatures() -> Result<()> {
    let (instance, username) = setup_instance_with_user("identity_rotation", "").await;
    let mut user = login_account(&instance, &username, "").await;
    let old_key = user.get_default_key()?;
    let identity = user.create_identity("personal", &old_key).await?;

    let new_key = user.add_private_key(Some("rotated")).await?;
    identity
        .add_key(
            &new_key,
            AuthKey::active(Some("rotated"), Permission::Admin(0)),
        )
        .await?;

    let identity_before = identity.database().snapshot().await?;
    let original = identity;
    let identity = user.select_identity_key("personal", &new_key).await?;
    assert_eq!(original.key_id(), &old_key);
    let tx = original.database().new_transaction().await?;
    tx.get_settings()?
        .set_name("original handle still bound")
        .await?;
    let original_entry = tx.commit().await?;
    assert!(matches!(
        original.database().get_entry(&original_entry).await?.auth().key,
        SigKey::Direct { ref hint } if hint.pubkey.as_ref() == Some(&old_key)
    ));
    let tx = identity.database().new_transaction().await?;
    tx.get_settings()?.set_name("rotated identity").await?;
    let identity_entry_id = tx.commit().await?;
    assert!(matches!(
        identity.database().get_entry(&identity_entry_id).await?.auth().key,
        SigKey::Direct { ref hint } if hint.pubkey.as_ref() == Some(&new_key)
    ));
    assert_ne!(identity.database().snapshot().await?, identity_before);

    identity.revoke_key(&old_key).await?;
    let tx = identity.database().new_transaction().await?;
    tx.get_settings()?
        .set_name("rotated identity again")
        .await?;
    let post_revocation_entry_id = tx.commit().await?;
    assert!(matches!(
        identity.database().get_entry(&post_revocation_entry_id).await?.auth().key,
        SigKey::Direct { ref hint } if hint.pubkey.as_ref() == Some(&new_key)
    ));

    let root = identity.root_id().clone();
    let before_retry = identity.database().snapshot().await?;
    // No peer addresses: succeeding must not depend on replaying bootstrap.
    let ticket = DatabaseTicket::new(root.clone());
    let preferences = SyncSettings::disabled().with_interval(456);
    let IdentityRegistration::Active(retried) = Box::pin(user.register_identity(
        "personal",
        &ticket,
        &old_key,
        Permission::Admin(0),
        preferences.clone(),
    ))
    .await?
    else {
        panic!("an active Identity must not return to pending after rotation");
    };
    assert_eq!(retried.root_id(), &root);
    assert_eq!(retried.key_id(), &new_key);
    assert_eq!(retried.database().snapshot().await?, before_retry);
    assert_eq!(original.key_id(), &old_key);

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
    let delegated = retried.open_database(target.root_id()).await?;
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
    // Dropping every old handle before login is also safe on the connected path.
    drop(reopened);
    drop(retried);
    drop(original);
    drop(identity);
    user.logout()?;
    let user = login_account(&instance, &username, "").await;
    let tracked = user.tracked_identity("personal").await?.unwrap();
    assert_eq!(tracked.status, IdentityStatus::Active);
    assert_eq!(tracked.requested_key_id, old_key);
    assert_eq!(tracked.sync_settings, preferences);
    let database = user.database(&root).await?;
    assert_eq!(database.key_id, new_key);
    assert_eq!(database.sync_settings, preferences);
    let reopened = user.get_identity("personal").await?.unwrap();
    assert_eq!(reopened.key_id(), &new_key);
    let tx = reopened.database().new_transaction().await?;
    tx.get_settings()?
        .set_name("selected key after reload")
        .await?;
    let entry = tx.commit().await?;
    assert!(matches!(
        reopened.database().get_entry(&entry).await?.auth().key,
        SigKey::Direct { ref hint } if hint.pubkey.as_ref() == Some(&new_key)
    ));
    Ok(())
}

#[tokio::test]
async fn selected_key_rejects_missing_unowned_global_only_and_revoked_keys() -> Result<()> {
    let (instance, username) = setup_instance_with_user("identity_key_validation", "").await;
    let mut user = login_account(&instance, &username, "").await;
    let default_key = user.get_default_key()?;
    let identity = user.create_identity("personal", &default_key).await?;
    let tracked = user.tracked_identity("personal").await?.unwrap();
    let outsider = user.add_private_key(Some("outsider")).await?;
    assert!(
        user.select_identity_key("personal", &outsider)
            .await
            .is_err()
    );
    let private = PrivateKey::generate();
    let unowned = private.public_key();
    identity
        .add_key(&unowned, AuthKey::active(None, Permission::Admin(0)))
        .await?;
    assert!(matches!(
        user.select_identity_key("personal", &unowned).await,
        Err(Error::Account(error)) if matches!(*error, AccountError::KeyNotFound { .. })
    ));
    let imported = user
        .import_private_key(private.clone(), Some("imported"))
        .await?;
    assert_eq!(imported, unowned);
    assert_eq!(
        user.tracked_identity("personal").await?.unwrap(),
        tracked,
        "import alone must not select an identity key"
    );
    assert!(matches!(
        user.import_private_key(private, None).await,
        Err(Error::Account(error)) if matches!(*error, AccountError::KeyAlreadyExists { .. })
    ));
    let selected = user.select_identity_key("personal", &imported).await?;
    assert_eq!(selected.key_id(), &imported);
    user.select_identity_key("personal", &default_key).await?;
    identity.revoke_key(&imported).await?;
    assert!(
        user.select_identity_key("personal", &imported)
            .await
            .is_err()
    );
    assert!(matches!(
        user.select_identity_key("unknown", &default_key).await,
        Err(Error::Account(error)) if matches!(*error, AccountError::IdentityNotFound { .. })
    ));
    assert_eq!(user.tracked_identity("personal").await?.unwrap(), tracked);
    assert_eq!(identity.key_id(), &default_key);
    Ok(())
}

#[tokio::test]
async fn open_database_is_restricted_to_the_selected_identity_root() -> Result<()> {
    let (instance, username) = setup_instance_with_user("identity_paths", "").await;
    let mut user = login_account(&instance, &username, "").await;
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
    let (instance, username) = setup_local_instance_with_user("identity_public", "").await;
    let mut user = login_account(&instance, &username, "").await;
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
