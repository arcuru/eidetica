//! Application references are stored and opened through the AccountSession,
//! not through a mandatory account catalog or raw Account stores.
use eidetica::{
    AccountSession, Database, Error, ID, Instance, NewAccount, Store,
    account::AccountError,
    auth::SigKey,
    backend::database::InMemory,
    crdt::{Doc, doc::Value},
    store::{DocStore, StoreError, Table},
};

use super::helpers::setup_instance_with_user;

const APP: &str = "app";

async fn create_target(session: &mut AccountSession, name: &str) -> Database {
    let key = session.get_default_key().unwrap();
    let mut settings = Doc::new();
    settings.set("name", name);
    session.create_database(settings, &key).await.unwrap()
}

#[tokio::test]
async fn test_account_root_reference_roundtrip_and_rename() {
    // This helper uses a genuine connected client under TEST_BACKEND=service.
    let (instance, _) = setup_instance_with_user("application", None).await;
    let mut session = instance.login_account("application", None).await.unwrap();
    let root = session.account().id().clone();
    let login = session.login_id().to_string();
    assert_eq!(&root, &session.account_info().account_id);
    assert_ne!(login, root.to_string());
    let target = create_target(&mut session, "display-name").await;
    session
        .set_reference(APP, "group", target.root_id())
        .await
        .unwrap();
    let tx = target.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_name("renamed")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    session.logout().unwrap();
    let session = instance.login_account("application", None).await.unwrap();
    assert_eq!(session.account().id(), &root);
    assert_eq!(session.login_id(), login);
    let reopened = session.open_reference(APP, "group").await.unwrap().unwrap();
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
    session
        .set_reference(APP, "target", target.root_id())
        .await
        .unwrap();
    let tracked_before = session.databases().await.unwrap().len();
    // Absent names and namespaces are absent references, not errors.
    assert!(
        session
            .open_reference(APP, "missing")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        session
            .reference("other", "target")
            .await
            .unwrap()
            .is_none()
    );

    // Corrupt reference data is a decoding error, not absence. Writing it needs
    // the test-only raw Account handle; applications have no such path.
    let tx = session
        .account()
        .database()
        .new_transaction()
        .await
        .unwrap();
    tx.get_store::<Table<String>>("refs:app")
        .await
        .unwrap()
        .set("corrupt", "not-an-id".into())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        matches!(session.open_reference(APP, "corrupt").await, Err(Error::Store(e)) if matches!(e.as_ref(), StoreError::DeserializationFailed { .. }))
    );

    // A known reference without a usable key does not authorize replacement.
    session
        .set_reference(APP, "unavailable", &ID::from_bytes("not-present"))
        .await
        .unwrap();
    assert!(
        matches!(session.open_reference(APP, "unavailable").await, Err(Error::Account(e)) if matches!(e.as_ref(), AccountError::NoKeyForDatabase { .. }))
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
    assert!(matches!(session.open_reference(APP, "unavailable").await,
        Err(Error::Account(e)) if matches!(e.as_ref(), AccountError::DatabaseAccessPending { .. })));
    assert_eq!(session.databases().await.unwrap().len(), tracked_before);
}

#[tokio::test]
async fn test_account_root_reference_rejects_path_names_without_mutation() {
    let (instance, _) = setup_instance_with_user("application", None).await;
    let session = instance.login_account("application", None).await.unwrap();
    let original = ID::from_bytes("original");
    let replacement = ID::from_bytes("replacement");
    session
        .set_reference(APP, "group", &original)
        .await
        .unwrap();
    let before = session.account().database().snapshot().await.unwrap();

    for name in [
        "",
        ".group",
        "group.",
        "group.archive",
        "group..archive",
        "...",
    ] {
        for result in [
            session.set_reference(APP, name, &replacement).await,
            session.reference(APP, name).await.map(|_| ()),
            session.open_reference(APP, name).await.map(|_| ()),
        ] {
            assert!(
                matches!(result, Err(Error::Store(e)) if matches!(e.as_ref(), StoreError::InvalidOperation { .. })),
                "invalid reference name {name:?} must be rejected"
            );
        }
        assert_eq!(
            session.reference(APP, "group").await.unwrap(),
            Some(original.clone())
        );
        assert_eq!(
            session.account().database().snapshot().await.unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn test_account_root_reference_rejects_nontext_metadata() {
    let (instance, _) = setup_instance_with_user("application", None).await;
    let session = instance.login_account("application", None).await.unwrap();
    let target = ID::from_bytes("target");
    for value in [Value::Int(42), Value::Bool(true), Value::Doc(Doc::new())] {
        // Warm the typed projection first: malformed data must not leave a stale ID.
        session.set_reference(APP, "root", &target).await.unwrap();
        assert_eq!(
            session.reference(APP, "root").await.unwrap(),
            Some(target.clone())
        );
        let tx = session
            .account()
            .database()
            .new_transaction()
            .await
            .unwrap();
        DocStore::load(&tx, "refs:app".into())
            .await
            .unwrap()
            .set("root", value)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert!(
            matches!(session.reference(APP, "root").await, Err(Error::Store(e)) if matches!(e.as_ref(), StoreError::TypeMismatch { .. }))
        );
        assert!(
            matches!(session.open_reference(APP, "root").await, Err(Error::Store(e)) if matches!(e.as_ref(), StoreError::TypeMismatch { .. }))
        );
    }
    let tx = session
        .account()
        .database()
        .new_transaction()
        .await
        .unwrap();
    DocStore::load(&tx, "refs:app".into())
        .await
        .unwrap()
        .delete("root")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(session.reference(APP, "root").await.unwrap(), None);
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
    reader
        .set_reference(APP, "foreign", &target_id)
        .await
        .unwrap();
    assert!(
        matches!(reader.open_reference(APP, "foreign").await, Err(Error::Account(e)) if matches!(e.as_ref(), AccountError::NoKeyForDatabase { .. }))
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
    session
        .set_reference(APP, "group", target.root_id())
        .await
        .unwrap();
    let engine = instance.backend().local_engine().unwrap();
    let memory = engine.as_any().downcast_ref::<InMemory>().unwrap();
    let mut baseline = None;
    for total in [1, 8, 32] {
        while session.databases().await.unwrap().len() < total {
            create_target(&mut session, "unrelated").await;
        }
        session.open_reference(APP, "group").await.unwrap();
        let before = memory.store_state_read_counts();
        assert_eq!(
            session
                .open_reference(APP, "group")
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

#[tokio::test]
async fn test_account_reference_namespaces_are_separate_from_account_stores() {
    let (instance, _) = setup_instance_with_user("application", None).await;
    let mut session = instance.login_account("application", None).await.unwrap();
    let first = create_target(&mut session, "first").await;
    let second = create_target(&mut session, "second").await;
    let tracked = session.databases().await.unwrap();

    // Namespaces named like the Account's own stores cannot reach them.
    for namespace in ["keys", "databases", "_settings"] {
        session
            .set_reference(namespace, "root", first.root_id())
            .await
            .unwrap();
    }
    session
        .set_reference(APP, "root", second.root_id())
        .await
        .unwrap();
    session.logout().unwrap();

    let session = instance.login_account("application", None).await.unwrap();
    assert_eq!(session.databases().await.unwrap(), tracked);
    session.get_default_key().unwrap();
    assert_eq!(
        session.reference("keys", "root").await.unwrap().as_ref(),
        Some(first.root_id())
    );
    assert_eq!(
        session.reference(APP, "root").await.unwrap().as_ref(),
        Some(second.root_id())
    );
}
