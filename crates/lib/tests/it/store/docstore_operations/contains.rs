//! DocStore key/path existence and consistency tests.

use eidetica::{crdt::doc::path, store::DocStore};

use crate::helpers::TestContext;

#[tokio::test]
async fn test_docstore_contains_key() {
    let ctx = TestContext::new().with_database().await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("contains_key_test")
        .await
        .expect("Failed to get DocStore");

    // Test empty DocStore
    assert!(!dict.contains_key("nonexistent").await);
    assert!(!dict.contains_key("missing").await);

    // Test with staged data (not yet committed)
    dict.set("name", "Alice").await.expect("Failed to set name");
    dict.set("age", 30).await.expect("Failed to set age");
    assert!(dict.contains_key("name").await);
    assert!(dict.contains_key("age").await);
    assert!(!dict.contains_key("missing").await);

    // Test with nested values
    dict.set_path(path!("user.profile.email"), "alice@example.com")
        .await
        .expect("Failed to set nested value");
    assert!(dict.contains_key("user").await); // Top-level key exists
    assert!(!dict.contains_key("profile").await); // Nested key doesn't exist at top level
    assert!(!dict.contains_key("email").await); // Deep nested key doesn't exist at top level

    // Test deletion (tombstones)
    dict.delete("name").await.expect("Failed to delete name");
    assert!(!dict.contains_key("name").await); // Deleted key should not exist
    assert!(dict.contains_key("age").await); // Other keys should still exist

    // Commit and test persistence
    txn.commit().await.expect("Failed to commit transaction");

    // Test with committed data
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("contains_key_test")
        .await
        .expect("Failed to get viewer");

    assert!(!viewer.contains_key("name").await); // Deleted key not in committed data
    assert!(viewer.contains_key("age").await); // Existing key in committed data
    assert!(viewer.contains_key("user").await); // Nested structure in committed data

    // Test with new operation on committed data
    let txn2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start second operation");
    let dict2 = txn2
        .get_store::<DocStore>("contains_key_test")
        .await
        .expect("Failed to get DocStore");

    // Should see committed data
    assert!(!dict2.contains_key("name").await); // Still deleted
    assert!(dict2.contains_key("age").await); // Still exists
    assert!(dict2.contains_key("user").await); // Still exists

    // Stage new changes
    dict2
        .set("name", "Bob")
        .await
        .expect("Failed to re-set name");
    dict2.set("city", "NYC").await.expect("Failed to set city");

    // Should see both committed and staged data
    assert!(dict2.contains_key("name").await); // Now exists in staging
    assert!(dict2.contains_key("age").await); // Exists in committed data
    assert!(dict2.contains_key("user").await); // Exists in committed data
    assert!(dict2.contains_key("city").await); // Exists in staging

    // Viewer should only see committed data
    assert!(!viewer.contains_key("name").await); // Not in committed data
    assert!(!viewer.contains_key("city").await); // Not in committed data
}

#[tokio::test]
async fn test_docstore_contains_path() {
    let ctx = TestContext::new().with_database().await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("contains_path_test")
        .await
        .expect("Failed to get DocStore");

    // Test empty DocStore
    assert!(!dict.contains_path(path!("nonexistent")).await);
    assert!(!dict.contains_path(path!("missing.path")).await);

    // Test with simple paths
    dict.set("name", "Alice").await.expect("Failed to set name");
    assert!(dict.contains_path(path!("name")).await); // Direct key exists
    assert!(!dict.contains_path(path!("name.invalid")).await); // Can't navigate through string

    // Test with nested structure
    dict.set_path(path!("user.profile.name"), "Alice")
        .await
        .expect("Failed to set nested path");
    dict.set_path(path!("user.profile.email"), "alice@example.com")
        .await
        .expect("Failed to set nested email");
    dict.set_path(path!("user.settings.theme"), "dark")
        .await
        .expect("Failed to set settings");

    // Test intermediate paths
    assert!(dict.contains_path(path!("user")).await); // Top level
    assert!(dict.contains_path(path!("user.profile")).await); // Intermediate level
    assert!(dict.contains_path(path!("user.settings")).await); // Another intermediate level

    // Test full paths
    assert!(dict.contains_path(path!("user.profile.name")).await); // Full nested path
    assert!(dict.contains_path(path!("user.profile.email")).await); // Another full path
    assert!(dict.contains_path(path!("user.settings.theme")).await); // Different branch

    // Test non-existent paths
    assert!(!dict.contains_path(path!("user.profile.age")).await); // Missing leaf
    assert!(!dict.contains_path(path!("user.profile.missing")).await); // Missing leaf
    assert!(!dict.contains_path(path!("user.missing")).await); // Missing intermediate
    assert!(!dict.contains_path(path!("missing.user.profile")).await); // Missing root

    // Test deep nesting
    dict.set_path(path!("app.config.database.host"), "localhost")
        .await
        .expect("Failed to set deep path");
    dict.set_path(path!("app.config.database.port"), 5432)
        .await
        .expect("Failed to set deep port");

    assert!(dict.contains_path(path!("app")).await);
    assert!(dict.contains_path(path!("app.config")).await);
    assert!(dict.contains_path(path!("app.config.database")).await);
    assert!(dict.contains_path(path!("app.config.database.host")).await);
    assert!(dict.contains_path(path!("app.config.database.port")).await);
    assert!(!dict.contains_path(path!("app.config.database.name")).await);

    // Test path deletion
    dict.set_path(path!("temp.value"), "test")
        .await
        .expect("Failed to set temp value");
    assert!(dict.contains_path(path!("temp")).await);
    assert!(dict.contains_path(path!("temp.value")).await);

    // Override with simple value (should make nested path invalid)
    dict.set("temp", "simple")
        .await
        .expect("Failed to override temp");
    assert!(dict.contains_path(path!("temp")).await); // Exists as simple value
    assert!(!dict.contains_path(path!("temp.value")).await); // No longer accessible

    // Commit and test persistence
    txn.commit().await.expect("Failed to commit transaction");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("contains_path_test")
        .await
        .expect("Failed to get viewer");

    // Test committed paths
    assert!(viewer.contains_path(path!("name")).await);
    assert!(viewer.contains_path(path!("user.profile.name")).await);
    assert!(viewer.contains_path(path!("user.profile.email")).await);
    assert!(viewer.contains_path(path!("user.settings.theme")).await);
    assert!(
        viewer
            .contains_path(path!("app.config.database.host"))
            .await
    );
    assert!(viewer.contains_path(path!("temp")).await); // Should be simple value
    assert!(!viewer.contains_path(path!("temp.value")).await); // Should not exist

    // Test with new operation adding more paths
    let txn2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start second operation");
    let dict2 = txn2
        .get_store::<DocStore>("contains_path_test")
        .await
        .expect("Failed to get DocStore");

    // Should see committed paths
    assert!(dict2.contains_path(path!("user.profile.name")).await);

    // Add staged paths
    dict2
        .set_path(path!("user.profile.age"), 30)
        .await
        .expect("Failed to set age");
    dict2
        .set_path(path!("new.staged.path"), "value")
        .await
        .expect("Failed to set staged path");

    // Should see both committed and staged paths
    assert!(dict2.contains_path(path!("user.profile.name")).await); // Committed
    assert!(dict2.contains_path(path!("user.profile.age")).await); // Staged
    assert!(dict2.contains_path(path!("new")).await); // Staged intermediate
    assert!(dict2.contains_path(path!("new.staged")).await); // Staged intermediate
    assert!(dict2.contains_path(path!("new.staged.path")).await); // Staged full

    // Viewer should only see committed paths
    assert!(viewer.contains_path(path!("user.profile.name")).await); // Committed
    assert!(!viewer.contains_path(path!("user.profile.age")).await); // Not committed
    assert!(!viewer.contains_path(path!("new")).await); // Not committed
}

#[tokio::test]
async fn test_docstore_contains_methods_consistency() {
    let ctx = TestContext::new().with_database().await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("consistency_test")
        .await
        .expect("Failed to get DocStore");

    // Test consistency between contains_key and contains_path for simple keys
    dict.set("simple", "value")
        .await
        .expect("Failed to set simple");
    dict.set("number", 42).await.expect("Failed to set number");

    assert_eq!(
        dict.contains_key("simple").await,
        dict.contains_path(path!("simple")).await
    );
    assert_eq!(
        dict.contains_key("number").await,
        dict.contains_path(path!("number")).await
    );
    assert_eq!(
        dict.contains_key("missing").await,
        dict.contains_path(path!("missing")).await
    );

    // Test nested structure
    dict.set_path(path!("nested.key"), "value")
        .await
        .expect("Failed to set nested");

    // contains_key should find top-level key
    assert!(dict.contains_key("nested").await);
    assert!(dict.contains_path(path!("nested")).await);

    // contains_path should find nested path, contains_key should not
    assert!(!dict.contains_key("key").await); // key is not at top level
    assert!(dict.contains_path(path!("nested.key")).await); // but path exists

    // Test get methods consistency with contains methods
    assert_eq!(
        dict.get("simple").await.is_ok(),
        dict.contains_key("simple").await
    );
    assert_eq!(
        dict.get("number").await.is_ok(),
        dict.contains_key("number").await
    );
    assert_eq!(
        dict.get("missing").await.is_err(),
        !dict.contains_key("missing").await
    );

    assert_eq!(
        dict.get_path(path!("simple")).await.is_ok(),
        dict.contains_path(path!("simple")).await
    );
    assert_eq!(
        dict.get_path(path!("nested.key")).await.is_ok(),
        dict.contains_path(path!("nested.key")).await
    );
    assert_eq!(
        dict.get_path(path!("missing.path")).await.is_err(),
        !dict.contains_path(path!("missing.path")).await
    );

    // Test with deletion
    dict.delete("simple")
        .await
        .expect("Failed to delete simple");
    assert_eq!(
        dict.get("simple").await.is_err(),
        !dict.contains_key("simple").await
    );
    assert_eq!(
        dict.get_path(path!("simple")).await.is_err(),
        !dict.contains_path(path!("simple")).await
    );

    // Test with type-safe getters
    let name_exists = dict.contains_key("number").await;
    let name_get_result = dict.get_as::<i64>("number").await.is_ok();
    assert_eq!(name_exists, name_get_result);

    let path_exists = dict.contains_path(path!("nested.key")).await;
    let path_get_result = dict
        .get_path_as::<String>(path!("nested.key"))
        .await
        .is_ok();
    assert_eq!(path_exists, path_get_result);
}
