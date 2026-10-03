//! Tests for the user_session module.

use super::*;
use crate::{NewUser, backend::database::InMemory};

async fn create_test_user_session() -> (Instance, User) {
    Instance::create_backend(
        Box::new(InMemory::new()),
        NewUser::with_password("test_user", "test_password"),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_creation() {
    let (_instance, user) = create_test_user_session().await;
    assert_eq!(user.username(), "test_user");
    assert!(!user.user_uuid().is_empty());
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_getters() {
    let (_instance, user) = create_test_user_session().await;

    assert_eq!(user.username(), "test_user");
    assert!(!user.user_uuid().is_empty());
    assert_eq!(user.user_info().username, "test_user");
    assert!(!user.user_database().root_id().to_string().is_empty());
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_logout() {
    let (_instance, user) = create_test_user_session().await;
    let username = user.username().to_string();

    // Logout consumes the user
    user.logout().unwrap();

    // User is dropped, keys should be cleared
    assert_eq!(username, "test_user");
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
async fn test_user_drop() {
    {
        let (_instance, _user) = create_test_user_session().await;
        // User will be dropped when it goes out of scope
    }
    // Keys should be cleared automatically
}

#[tokio::test]
async fn join_mapping_staging_and_future_drop_do_not_publish_cache() -> Result<()> {
    let (instance, mut user) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("client"))
            .await?;
    let key = user.get_default_key()?;
    let tree = ID::from_bytes("staged-only");
    let before = user.user_database.snapshot().await?;
    let tx = user.user_database.new_transaction().await?;
    user.map_key_in_txn(&tx, &key, &tree, SigKey::from_pubkey(&key))
        .await?;
    assert_eq!(
        user.key_mapping(&key, &tree)?,
        None,
        "mapping staging published cache"
    );
    drop(tx); // Failure/cancellation before staging preferences or putting User Entry.
    assert_eq!(before, user.user_database.snapshot().await?);

    let (owner, _) = crate::auth::crypto::generate_keypair();
    let db = Database::create(&instance, owner, Doc::new()).await?;
    db.with_transaction(|tx| async move {
        tx.get_settings()?
            .set_global_auth_key(crate::auth::AuthKey::active(None, Permission::Read))
            .await
    })
    .await?;
    let reached = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let callback = user
        .user_database
        .on_write({
            let reached = reached.clone();
            let resume = resume.clone();
            move |_, _| {
                let reached = reached.clone();
                let resume = resume.clone();
                async move {
                    reached.notify_one();
                    resume.notified().await;
                    Ok(())
                }
            }
        })
        .await?;
    let mut tracking = Box::pin(user.track_database(db.root_id(), &key, SyncSettings::disabled()));
    tokio::select! {
        _ = reached.notified() => {},
        result = &mut tracking => panic!("commit did not pause before acknowledgement: {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => panic!("User commit did not reach callback"),
    }
    drop(tracking);
    assert_eq!(
        user.key_mapping(&key, db.root_id())?,
        None,
        "dropped future speculatively published staged mapping"
    );
    resume.notify_one();
    drop(callback);
    let persisted = user.user_database.snapshot().await?;
    // Retry reads the actual committed state without registry repair or duplicate history.
    user.track_database(db.root_id(), &key, SyncSettings::disabled())
        .await?;
    assert_eq!(persisted, user.user_database.snapshot().await?);
    assert_eq!(
        user.key_mapping(&key, db.root_id())?,
        Some(SigKey::global(&key))
    );
    Ok(())
}

#[tokio::test]
async fn join_bookkeeping_preserves_settings_changed_during_network_wait() -> Result<()> {
    let (instance, mut user) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("client"))
            .await?;
    let key = user.get_default_key()?;
    let db = user.create_database(Doc::new(), &key).await?;
    let original = user.tracked_database_before_join(db.root_id()).await?;
    let mut another = instance.login_user("client", None).await?;
    another
        .track_database(
            db.root_id(),
            &key,
            SyncSettings::on_commit().with_interval(47),
        )
        .await?;
    user.record_database_access_with_expected(
        db.root_id(),
        &key,
        SyncSettings::disabled(),
        Ok(()),
        Some(original),
    )
    .await?;
    assert_eq!(
        user.database(db.root_id())
            .await?
            .sync_settings
            .interval_seconds,
        Some(47)
    );
    let original = user.tracked_database_before_join(db.root_id()).await?;
    another.untrack_database(db.root_id()).await?;
    user.record_database_access_with_expected(
        db.root_id(),
        &key,
        SyncSettings::on_commit(),
        Ok(()),
        Some(original),
    )
    .await?;
    assert!(
        user.database(db.root_id()).await.is_err(),
        "old attempt restored later untracked preferences"
    );
    Ok(())
}

#[tokio::test]
async fn join_final_open_checks_requested_permission_and_retains_committed_state() -> Result<()> {
    let (instance, mut user) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("client"))
            .await?;
    let key = user.get_default_key()?;
    let db = Database::create(
        &instance,
        crate::auth::crypto::generate_keypair().0,
        Doc::new(),
    )
    .await?;
    db.with_transaction(|tx| async move {
        tx.get_settings()?
            .set_global_auth_key(crate::auth::AuthKey::active(None, Permission::Write(5)))
            .await
    })
    .await?;
    user.track_database(db.root_id(), &key, SyncSettings::disabled())
        .await?;
    let before = user.user_database.snapshot().await?;
    for rejected in [false, true] {
        let error = if rejected {
            SyncError::BootstrapRejected {
                request_id: "legacy".into(),
                message: "rejected".into(),
            }
        } else {
            SyncError::BootstrapPending {
                request_id: "legacy".into(),
                message: "pending".into(),
            }
        };
        assert!(
            user.record_database_access(
                db.root_id(),
                &key,
                SyncSettings::on_commit(),
                Err(error.into())
            )
            .await
            .is_err()
        );
        assert_eq!(before, user.user_database.snapshot().await?);
        assert_eq!(
            user.key_mapping(&key, db.root_id())?,
            Some(SigKey::global(&key))
        );
    }
    db.with_transaction(|tx| async move {
        tx.get_settings()?
            .set_global_auth_key(crate::auth::AuthKey::active(None, Permission::Read))
            .await
    })
    .await?;
    assert!(
        user.open_joined_database(db.root_id(), &key, Permission::Write(5))
            .await
            .is_err(),
        "final open accepted less than requested permission"
    );
    assert_eq!(
        before,
        user.user_database.snapshot().await?,
        "open failure rewrote User state"
    );
    db.with_transaction(|tx| async move {
        tx.get_settings()?
            .set_global_auth_key(crate::auth::AuthKey::active(None, Permission::Write(5)))
            .await
    })
    .await?;
    user.track_database(db.root_id(), &key, SyncSettings::disabled())
        .await?;
    assert!(
        user.open_joined_database(db.root_id(), &key, Permission::Write(5))
            .await
            .is_ok()
    );
    assert_eq!(before, user.user_database.snapshot().await?);
    Ok(())
}

#[tokio::test]
async fn join_concurrent_local_bookkeeping_writes_one_equivalent_entry() -> Result<()> {
    let (instance, mut first) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("client"))
            .await?;
    let key = first.get_default_key()?;
    let mut second = instance.login_user("client", None).await?;
    let db = Database::create(
        &instance,
        crate::auth::crypto::generate_keypair().0,
        Doc::new(),
    )
    .await?;
    db.with_transaction(|tx| async move {
        tx.get_settings()?
            .set_global_auth_key(crate::auth::AuthKey::active(None, Permission::Read))
            .await
    })
    .await?;
    let count = instance
        .require_local_engine()?
        .get_tree(first.user_database.root_id())
        .await?
        .len();
    let (a, b) = tokio::join!(
        first.track_database(db.root_id(), &key, SyncSettings::disabled()),
        second.track_database(db.root_id(), &key, SyncSettings::disabled())
    );
    a?;
    b?;
    assert_eq!(
        count + 1,
        instance
            .require_local_engine()?
            .get_tree(first.user_database.root_id())
            .await?
            .len()
    );
    assert_eq!(
        first.key_mapping(&key, db.root_id())?,
        second.key_mapping(&key, db.root_id())?
    );
    Ok(())
}
