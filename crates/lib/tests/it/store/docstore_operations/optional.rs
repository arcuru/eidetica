//! Fallible optional reads distinguish absence from unreadable history.

use eidetica::{crdt::doc::path, store::DocStore};

use crate::{helpers::*, store::helpers::create_dict_operation};

#[tokio::test]
async fn test_docstore_optional_absence_and_deletion() {
    let ctx = TestContext::new().with_database().await;
    create_dict_operation(ctx.database(), "data", &[("user.name", "old")]).await;
    let tx = ctx.database().new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    assert_eq!(
        store.try_get("user.name").await.unwrap(),
        Some("old".into())
    );
    assert_eq!(store.try_get("missing").await.unwrap(), None);
    assert_eq!(
        store.try_get_path(path!("user.missing")).await.unwrap(),
        None
    );
    store.set("user.name", "new").await.unwrap();
    assert_eq!(
        store.try_get_path(path!("user.name")).await.unwrap(),
        Some("new".into())
    );
    assert!(store.delete("user.name").await.unwrap());
    assert_eq!(store.try_get("user.name").await.unwrap(), None);
    assert_eq!(store.try_get_path(path!("user.name")).await.unwrap(), None);
    // Staging does not change an independent historical view.
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("data")
        .await
        .unwrap();
    assert_eq!(
        viewer.try_get("user.name").await.unwrap(),
        Some("old".into())
    );
    tx.commit().await.unwrap();
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("data")
        .await
        .unwrap();
    assert_eq!(viewer.try_get("user.name").await.unwrap(), None);
    assert_eq!(viewer.try_get_path(path!("user.name")).await.unwrap(), None);
    assert_eq!(viewer.try_get("missing").await.unwrap(), None);
    // Deleting/replacing an ancestor must not resurrect its historical children.
    let tx = ctx.database().new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store.set("user.name", "restored").await.unwrap();
    tx.commit().await.unwrap();
    let tx = ctx.database().new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    assert!(store.delete("user").await.unwrap());
    assert_eq!(store.try_get("user.name").await.unwrap(), None);
    assert_eq!(store.try_get_path(path!("user.name")).await.unwrap(), None);
    store.set("user", "scalar replacement").await.unwrap();
    assert_eq!(store.try_get_path(path!("user.name")).await.unwrap(), None);
    // An empty/unwritten Store is genuine absence too.
    let empty = ctx
        .database()
        .get_store_viewer::<DocStore>("empty")
        .await
        .unwrap();
    assert_eq!(empty.try_get("missing").await.unwrap(), None);
}
