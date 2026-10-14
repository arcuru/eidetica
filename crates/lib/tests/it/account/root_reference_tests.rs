//! App-owned references use ordinary stores, not a mandatory account catalog.
use eidetica::{
    AccountSession, Database, Error, ID, Instance, NewAccount, Result,
    account::AccountError,
    auth::SigKey,
    backend::database::InMemory,
    crdt::Doc,
    store::{StoreError, Table},
};

use super::helpers::setup_instance_with_user;

// Only a missing row in the application's own root store means absent metadata.
// In particular, an unavailable target is NOT an absent reference.
async fn open_reference(session: &AccountSession, name: &str) -> Result<Option<Database>> {
    let roots = session
        .account()
        .get_store_viewer::<Table<ID>>("app_roots")
        .await?;
    let id = match roots.get(name).await {
        Ok(id) => id,
        Err(Error::Store(e)) => {
            if let StoreError::KeyNotFound { store, key } = e.as_ref()
                && store == "app_roots"
                && key == name
            {
                return Ok(None);
            }
            return Err(Error::Store(e));
        }
        Err(e) => return Err(e),
    };
    session.open_database(&id).await.map(Some)
}

async fn create_target(session: &mut AccountSession, name: &str) -> Database {
    let key = session.get_default_key().unwrap();
    let mut settings = Doc::new();
    settings.set("name", name);
    session.create_database(settings, &key).await.unwrap()
}

async fn save_reference(session: &AccountSession, name: &str, id: &ID) {
    let tx = session.account().new_transaction().await.unwrap();
    tx.get_store::<Table<ID>>("app_roots")
        .await
        .unwrap()
        .set(name, id.clone())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn test_account_root_reference_roundtrip_and_rename() {
    // This helper uses a genuine connected client under TEST_BACKEND=service.
    let (instance, _) = setup_instance_with_user("application", None).await;
    let mut session = instance.login_account("application", None).await.unwrap();
    let root = session.account_id().clone();
    let login = session.login_id().to_string();
    assert_eq!(&root, &session.account_info().account_id);
    assert_ne!(login, root.to_string());
    let target = create_target(&mut session, "display-name").await;
    save_reference(&session, "group", target.root_id()).await;
    let tx = target.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_name("renamed")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    session.logout().unwrap();
    let session = instance.login_account("application", None).await.unwrap();
    assert_eq!(session.account_id(), &root);
    assert_eq!(session.login_id(), login);
    let reopened = open_reference(&session, "group").await.unwrap().unwrap();
    assert_eq!(reopened.root_id(), target.root_id());
    assert_eq!(reopened.get_name().await.unwrap(), "renamed");
    let tx = reopened.new_transaction().await.unwrap();
    tx.get_store::<Table<String>>("data")
        .await
        .unwrap()
        .set("proof", "authorized write".into())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        target
            .get_store_viewer::<Table<String>>("data")
            .await
            .unwrap()
            .get("proof")
            .await
            .unwrap(),
        "authorized write"
    );
}

#[tokio::test]
async fn test_account_root_reference_absence_is_not_failure_or_creation() {
    let (instance, _) = setup_instance_with_user("application", None).await;
    let mut session = instance.login_account("application", None).await.unwrap();
    let target = create_target(&mut session, "target").await;
    save_reference(&session, "target", target.root_id()).await;
    let tracked_before = session.databases().await.unwrap().len();
    assert!(open_reference(&session, "missing").await.unwrap().is_none());

    // Corrupt metadata is a decoding error, not absence.
    let tx = session.account().new_transaction().await.unwrap();
    tx.get_store::<Table<String>>("app_roots")
        .await
        .unwrap()
        .set("corrupt", "not-an-id".into())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        matches!(open_reference(&session, "corrupt").await, Err(Error::Store(e)) if matches!(e.as_ref(), StoreError::DeserializationFailed { .. }))
    );

    // A known reference without a usable key does not authorize replacement.
    save_reference(&session, "unavailable", &ID::from_bytes("not-present")).await;
    assert!(
        matches!(open_reference(&session, "unavailable").await, Err(Error::Account(e)) if matches!(e.as_ref(), AccountError::NoKeyForDatabase { .. }))
    );
    // Even a held mapping does not make absent target data an absent reference.
    let key = session.get_default_key().unwrap();
    session
        .map_key(
            &key,
            &ID::from_bytes("not-present"),
            SigKey::from_pubkey(&key),
        )
        .await
        .unwrap();
    assert!(matches!(open_reference(&session, "unavailable").await,
        Err(Error::Account(e)) if matches!(e.as_ref(), AccountError::DatabaseAccessPending { .. })));
    assert_eq!(session.databases().await.unwrap().len(), tracked_before);
}

#[tokio::test]
async fn test_account_root_reference_does_not_grant_authority() {
    let instance = crate::helpers::test_instance().await;
    crate::helpers::create_account(&instance, "owner", None)
        .await
        .unwrap();
    crate::helpers::create_account(&instance, "reader", None)
        .await
        .unwrap();
    let mut owner = instance.login_account("owner", None).await.unwrap();
    let target = create_target(&mut owner, "private").await;
    let target_id = target.root_id().clone();
    owner.logout().unwrap();
    let reader = instance.login_account("reader", None).await.unwrap();
    save_reference(&reader, "foreign", &target_id).await;
    assert!(
        matches!(open_reference(&reader, "foreign").await, Err(Error::Account(e)) if matches!(e.as_ref(), AccountError::NoKeyForDatabase { .. }))
    );
}

#[tokio::test]
async fn test_account_root_reference_lookup_reads_are_constant() {
    // Local instrumentation proves bounded Store reads independently of RPC timing.
    let (instance, mut session) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("app"))
            .await
            .unwrap();
    let target = create_target(&mut session, "target").await;
    save_reference(&session, "group", target.root_id()).await;
    let engine = instance.backend().local_engine().unwrap();
    let memory = engine.as_any().downcast_ref::<InMemory>().unwrap();
    let mut baseline = None;
    for total in [1, 8, 32] {
        while session.databases().await.unwrap().len() < total {
            create_target(&mut session, "unrelated").await;
        }
        open_reference(&session, "group").await.unwrap();
        let before = memory.store_state_read_counts();
        assert_eq!(
            open_reference(&session, "group")
                .await
                .unwrap()
                .unwrap()
                .root_id(),
            target.root_id()
        );
        let after = memory.store_state_read_counts();
        let reads = (after.0 - before.0, after.1 - before.1);
        println!(
            "direct reference: tracked={total}, point_reads={}, scan_pages={}",
            reads.0, reads.1
        );
        assert!(
            reads.0 > 0,
            "positive control: the root reference was actually read"
        );
        assert_eq!(*baseline.get_or_insert(reads), reads);
    }
}
