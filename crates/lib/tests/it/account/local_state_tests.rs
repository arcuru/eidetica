use eidetica::{Database, crdt::Doc, store::Table};

use super::helpers::{login_account, setup_instance_with_user};

#[tokio::test]
async fn account_local_keys_and_activation_never_write_account_entries() {
    let (instance, name) = setup_instance_with_user("local-state", Some("password")).await;
    let mut user = login_account(&instance, &name, Some("password")).await;
    let root_key = user.get_default_key().unwrap();
    let account = Database::open(&instance, user.account().id())
        .await.unwrap().with_key(user.get_signing_key(&root_key).unwrap());
    let initial = account.snapshot().await.unwrap();
    let key = user.add_private_key(Some("installation-only")).await.unwrap();
    let database = user.create_database(Doc::new(), &key).await.unwrap();
    database.share().await.unwrap();
    assert!(database.is_shared().await.unwrap());
    user.set_reference("app", "root", database.root_id()).await.unwrap();
    let reference_snapshot = account.snapshot().await.unwrap();
    assert_ne!(initial, reference_snapshot, "portable references still replicate");
    database.stop_sharing().await.unwrap();
    assert!(!database.is_shared().await.unwrap());
    assert_eq!(reference_snapshot, account.snapshot().await.unwrap(), "local choices must not author Account entries");
    // Inspect full history, not only current fields: no keys/unlock or local
    // preference store was ever authored, including during first login.
    for entry in user.backend().store_at(account.root_id(), "keys", &account.snapshot().await.unwrap()).await.unwrap() {
        panic!("private-key store replicated in Account history: {}", entry.id());
    }
    assert!(account.get_store_viewer::<Table<eidetica::account::TrackedDatabase>>("databases").await.unwrap().search(|_| true).await.unwrap().is_empty());
    let id = database.root_id().clone();
    let actual = user.get_signing_key(&key).unwrap().public_key();
    user.logout().unwrap();
    let user = login_account(&instance, &name, Some("password")).await;
    assert_eq!(user.get_signing_key(&key).unwrap().public_key(), actual);
    assert!(!user.database(&id).await.unwrap().sync_settings.sync_enabled);
    assert_eq!(user.reference("app", "root").await.unwrap(), Some(id));
}
