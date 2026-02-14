//! Tests for the Registry and _index subtree functionality

use eidetica::{
    Database, Registered,
    auth::crypto::generate_keypair,
    crdt::{Doc, doc::Value},
    store::{DocStore, Table},
};

/// Helper to create a Doc config with key-value pairs for testing
fn doc_config(pairs: &[(&str, &str)]) -> Doc {
    let mut doc = Doc::new();
    for (k, v) in pairs {
        doc.set(*k, *v);
    }
    doc
}

use serde::{Deserialize, Serialize};

use crate::helpers::test_instance;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TestRecord {
    id: u32,
    name: String,
}

// ============================================================================
// Basic Registry Functionality
// ============================================================================

#[tokio::test]
async fn test_index_store_register_subtree() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create a subtree - this will auto-register with default config
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
    store.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Update the registration to custom values
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
    store.set("key2", "value2").await.unwrap();

    // Now update the index with custom type and config
    index
        .set_entry(
            "my_subtree",
            "custom:v1",
            doc_config(&[("custom", "config")]),
        )
        .await
        .unwrap();

    tx.commit().await.unwrap();

    // Verify updated registration
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let info = index.get_entry("my_subtree").await.unwrap();
    assert_eq!(info.type_id, "custom:v1");
    assert_eq!(info.config, doc_config(&[("custom", "config")]));
}

#[tokio::test]
async fn test_index_store_get_subtree_info() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create and register multiple subtrees
    let tx = database.new_transaction().await.unwrap();
    let store1 = tx.get_store::<DocStore>("subtree1").await.unwrap();
    store1.set("key", "value").await.unwrap();

    let store2 = tx.get_store::<DocStore>("subtree2").await.unwrap();
    store2.set("key", "value").await.unwrap();

    tx.commit().await.unwrap();

    // Retrieve and verify
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let info1 = index.get_entry("subtree1").await.unwrap();
    assert_eq!(info1.type_id, DocStore::type_id());

    let info2 = index.get_entry("subtree2").await.unwrap();
    assert_eq!(info2.type_id, DocStore::type_id());
}

#[tokio::test]
async fn test_index_store_contains_subtree() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create a subtree
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("test_subtree").await.unwrap();
    store.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Check existence
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    assert!(index.contains("test_subtree").await);
    assert!(!index.contains("nonexistent").await);
}

#[tokio::test]
async fn test_index_store_list_subtrees() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create multiple subtrees
    let tx = database.new_transaction().await.unwrap();
    let alpha = tx.get_store::<DocStore>("alpha").await.unwrap();
    alpha.set("key", "value").await.unwrap();
    let beta = tx.get_store::<DocStore>("beta").await.unwrap();
    beta.set("key", "value").await.unwrap();
    let gamma = tx.get_store::<DocStore>("gamma").await.unwrap();
    gamma.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // List and verify
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let subtrees = index.list().await.unwrap();
    assert!(subtrees.contains(&"alpha".to_string()));
    assert!(subtrees.contains(&"beta".to_string()));
    assert!(subtrees.contains(&"gamma".to_string()));
    assert_eq!(subtrees.len(), 3);
}

#[tokio::test]
async fn test_index_store_update_existing() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtree with default config
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
    store.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Update config
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
    store.set("key2", "value2").await.unwrap();

    index
        .set_entry(
            "my_subtree",
            DocStore::type_id(),
            doc_config(&[("updated", "config")]),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Verify update
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let info = index.get_entry("my_subtree").await.unwrap();
    assert_eq!(info.config, doc_config(&[("updated", "config")]));
}

// ============================================================================
// Auto-Registration Behavior
// ============================================================================

#[tokio::test]
async fn test_auto_register_on_first_access_docstore() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // First access to a new subtree
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("my_data").await.unwrap();
    store.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Verify _index contains the registration
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let info = index.get_entry("my_data").await.unwrap();
    assert_eq!(info.type_id, DocStore::type_id());
    assert!(info.config.is_empty());
}

#[tokio::test]
async fn test_no_auto_register_for_system_subtrees() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Access system subtrees and user subtree
    let tx = database.new_transaction().await.unwrap();
    let _settings = tx.get_settings().unwrap();
    let user_store = tx.get_store::<DocStore>("user_data").await.unwrap();
    user_store.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Verify only user subtree is in index, not system subtrees
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    assert!(index.contains("user_data").await);
    // System subtrees should NOT be auto-registered
    assert!(!index.contains("_settings").await);
    assert!(!index.contains("_index").await);
    assert!(!index.contains("_root").await);
}

#[tokio::test]
async fn test_second_access_uses_existing_registration() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // First access - auto-registers
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("my_data").await.unwrap();
    store.set("key1", "value1").await.unwrap();
    tx.commit().await.unwrap();

    // Second access - should use existing registration
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("my_data").await.unwrap();
    store.set("key2", "value2").await.unwrap();
    tx.commit().await.unwrap();

    // Verify still only one entry in index
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let info = index.get_entry("my_data").await.unwrap();
    assert_eq!(info.type_id, DocStore::type_id());

    // No duplicates in list
    let subtrees = index.list().await.unwrap();
    let count = subtrees.iter().filter(|s| *s == "my_data").count();
    assert_eq!(count, 1);
}

// ============================================================================
// Architectural Constraint Enforcement
// ============================================================================

#[tokio::test]
async fn test_index_update_includes_target_subtree() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtree first
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
    store.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Update index for this subtree
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
    store.set("key2", "value2").await.unwrap();

    index
        .set_entry(
            "my_subtree",
            DocStore::type_id(),
            doc_config(&[("new", "config")]),
        )
        .await
        .unwrap();

    let entry_id = tx.commit().await.unwrap();

    // Verify Entry contains both _index and my_subtree SubTreeNodes
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();
    let subtrees = entry.subtrees();

    assert!(subtrees.contains(&"_index".to_string()));
    assert!(subtrees.contains(&"my_subtree".to_string()));
}

#[tokio::test]
async fn test_auto_dummy_write_on_index_update() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtree
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("target").await.unwrap();
    store.set("original", "data").await.unwrap();
    tx.commit().await.unwrap();

    // Update index without explicitly modifying target subtree
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    // This should automatically add a dummy write to "target"
    index
        .set_entry(
            "target",
            DocStore::type_id(),
            doc_config(&[("modified", "config")]),
        )
        .await
        .unwrap();

    let entry_id = tx.commit().await.unwrap();

    // Verify target subtree is in the Entry (due to automatic dummy write)
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();
    assert!(entry.in_subtree("target"));
}

#[tokio::test]
async fn test_entry_has_both_index_and_subtree_nodes() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtree with auto-registration
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("test_subtree").await.unwrap();
    store.set("key", "value").await.unwrap();
    let entry_id = tx.commit().await.unwrap();

    // Verify Entry structure
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();
    let subtrees = entry.subtrees();

    // Should have both _index and test_subtree
    assert!(subtrees.contains(&"_index".to_string()));
    assert!(subtrees.contains(&"test_subtree".to_string()));

    // Verify we can read from both
    let index_data = entry.data("_index").unwrap();
    assert!(!index_data.is_empty());

    let subtree_data = entry.data("test_subtree").unwrap();
    assert!(!subtree_data.is_empty());
}

#[tokio::test]
async fn test_manual_index_update_with_subtree_modification() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtree initially
    let tx = database.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("my_subtree").await.unwrap();
    tx.commit().await.unwrap();

    // Manually update both index and subtree in same transaction
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();

    // Modify subtree
    store.set("new_key", "new_value").await.unwrap();

    // Update index metadata
    index
        .set_entry("my_subtree", "docstore:v2", doc_config(&[("version", "2")]))
        .await
        .unwrap();

    let entry_id = tx.commit().await.unwrap();

    // Verify both are in Entry
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();
    assert!(entry.in_subtree("_index"));
    assert!(entry.in_subtree("my_subtree"));

    // Verify updated metadata
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();
    let info = index.get_entry("my_subtree").await.unwrap();
    assert_eq!(info.type_id, "docstore:v2");
}

// ============================================================================
// Integration Tests
// ============================================================================

#[tokio::test]
async fn test_multi_store_database_index_complete() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create database with multiple store types
    let tx = database.new_transaction().await.unwrap();

    let doc_store = tx.get_store::<DocStore>("documents").await.unwrap();
    doc_store.set("doc1", "content").await.unwrap();

    let table_store = tx.get_store::<Table<TestRecord>>("records").await.unwrap();
    table_store
        .insert(TestRecord {
            id: 1,
            name: "test".to_string(),
        })
        .await
        .unwrap();

    let doc_store2 = tx.get_store::<DocStore>("metadata").await.unwrap();
    doc_store2.set("key", "value").await.unwrap();

    tx.commit().await.unwrap();

    // Verify all are registered with correct types
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let doc_info = index.get_entry("documents").await.unwrap();
    assert_eq!(doc_info.type_id, DocStore::type_id());

    let table_info = index.get_entry("records").await.unwrap();
    assert_eq!(table_info.type_id, Table::<()>::type_id());

    let meta_info = index.get_entry("metadata").await.unwrap();
    assert_eq!(meta_info.type_id, DocStore::type_id());

    // Verify list is complete
    let subtrees = index.list().await.unwrap();
    assert_eq!(subtrees.len(), 3);
}

#[tokio::test]
async fn test_index_persists_across_transactions() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Transaction 1: Create subtrees
    let tx = database.new_transaction().await.unwrap();
    let store1 = tx.get_store::<DocStore>("subtree1").await.unwrap();
    store1.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Transaction 2: Create more subtrees
    let tx = database.new_transaction().await.unwrap();
    let store2 = tx.get_store::<DocStore>("subtree2").await.unwrap();
    store2.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Transaction 3: Verify both are registered
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    assert!(index.contains("subtree1").await);
    assert!(index.contains("subtree2").await);

    let subtrees = index.list().await.unwrap();
    assert_eq!(subtrees.len(), 2);
}

#[tokio::test]
async fn test_read_index_from_viewer() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(
        Doc::new(),
        &instance,
        private_key.clone(),
        "test_key".to_string(),
    )
    .await
    .unwrap();

    // Create some subtrees
    let tx = database.new_transaction().await.unwrap();
    let store1 = tx.get_store::<DocStore>("data1").await.unwrap();
    store1.set("key", "value").await.unwrap();
    let store2 = tx.get_store::<DocStore>("data2").await.unwrap();
    store2.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Read _index using viewer (read-only access)
    let viewer = database
        .get_store_viewer::<DocStore>("_index")
        .await
        .unwrap();

    // Verify we can read the index data
    let data1_info_value = viewer.get("data1").await.unwrap();
    assert!(matches!(data1_info_value, Value::Doc(_)));

    let data2_info_value = viewer.get("data2").await.unwrap();
    assert!(matches!(data2_info_value, Value::Doc(_)));
}

#[tokio::test]
async fn test_index_survives_database_reload() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    // Create database and add subtrees
    let database = Database::create(
        Doc::new(),
        &instance,
        private_key.clone(),
        "test_key".to_string(),
    )
    .await
    .unwrap();

    let root_id = database.root_id().clone();

    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("persistent_data").await.unwrap();
    store.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Drop database
    drop(database);

    // Reload database from same instance to test persistence
    // Reopen with the same key that was used to create
    let database = Database::open(
        instance.clone(),
        &root_id,
        private_key,
        "test_key".to_string(),
    )
    .await
    .unwrap();

    // Verify index is intact using viewer (read-only)
    let viewer = database
        .get_store_viewer::<DocStore>("_index")
        .await
        .unwrap();

    // Check that persistent_data is registered
    let data_info = viewer.get("persistent_data").await;
    assert!(data_info.is_ok(), "persistent_data should be in _index");
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[tokio::test]
async fn test_get_nonexistent_subtree_info() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    // Try to get info for non-existent subtree
    let result = index.get_entry("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_index_with_empty_database() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Query empty index
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let subtrees = index.list().await.unwrap();
    assert!(subtrees.is_empty());
}

#[tokio::test]
async fn test_concurrent_index_updates() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Register multiple subtrees in single transaction
    let tx = database.new_transaction().await.unwrap();

    let store1 = tx.get_store::<DocStore>("subtree1").await.unwrap();
    store1.set("key", "value").await.unwrap();
    let store2 = tx.get_store::<DocStore>("subtree2").await.unwrap();
    store2.set("key", "value").await.unwrap();
    let store3 = tx.get_store::<Table<TestRecord>>("subtree3").await.unwrap();
    store3
        .insert(TestRecord {
            id: 1,
            name: "test".to_string(),
        })
        .await
        .unwrap();
    let store4 = tx.get_store::<DocStore>("subtree4").await.unwrap();
    store4.set("key", "value").await.unwrap();

    tx.commit().await.unwrap();

    // Verify all are registered
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    assert!(index.contains("subtree1").await);
    assert!(index.contains("subtree2").await);
    assert!(index.contains("subtree3").await);
    assert!(index.contains("subtree4").await);

    let subtrees = index.list().await.unwrap();
    assert_eq!(subtrees.len(), 4);
}

#[tokio::test]
async fn test_empty_config_is_valid() {
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtree with default empty config
    let tx = database.new_transaction().await.unwrap();
    let _store = tx.get_store::<DocStore>("test").await.unwrap();
    tx.commit().await.unwrap();

    // Verify empty config is stored and retrieved correctly
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let info = index.get_entry("test").await.unwrap();
    assert!(info.config.is_empty());
}

// ============================================================================
// Architectural Constraint Tests
// ============================================================================

#[tokio::test]
async fn test_index_modification_forces_subtree_in_entry() {
    // Verify that when _index is modified for a subtree, that subtree appears in the Entry
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create a subtree first
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
    store.set("key", "value").await.unwrap();
    let _entry_id1 = tx.commit().await.unwrap();

    // Now update the index for that subtree
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();
    index
        .set_entry(
            "my_subtree",
            "custom:v1",
            doc_config(&[("custom", "config")]),
        )
        .await
        .unwrap();
    let entry_id2 = tx.commit().await.unwrap();

    // Load the entry and verify both _index and my_subtree are present
    let backend = database.backend().unwrap();
    let entry = backend.get(&entry_id2).await.unwrap();

    let subtrees = entry.subtrees();
    assert!(
        subtrees.contains(&"_index".to_string()),
        "_index should be in the entry"
    );
    assert!(
        subtrees.contains(&"my_subtree".to_string()),
        "my_subtree should be in the entry"
    );
}

#[tokio::test]
async fn test_auto_registration_includes_both_subtrees() {
    // Verify that auto-registration during commit includes both _index and the data subtree
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create a subtree - this triggers auto-registration
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("new_subtree").await.unwrap();
    store.set("key", "value").await.unwrap();
    let entry_id = tx.commit().await.unwrap();

    // Load the entry and verify both _index and new_subtree are present
    let backend = database.backend().unwrap();
    let entry = backend.get(&entry_id).await.unwrap();

    let subtrees = entry.subtrees();
    assert!(
        subtrees.contains(&"_index".to_string()),
        "_index should be in the entry from auto-registration"
    );
    assert!(
        subtrees.contains(&"new_subtree".to_string()),
        "new_subtree should be in the entry"
    );
}

#[tokio::test]
async fn test_accessing_store_registers_in_index() {
    // Verify that calling get_store() registers the subtree in _index even without writing data
    // This is the expected behavior: accessing a store initializes it
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Get a store handle but don't write any data
    let tx = database.new_transaction().await.unwrap();
    let _store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
    // No writes needed - accessing the store initializes it
    tx.commit().await.unwrap();

    // Verify the subtree IS registered in the index
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    assert!(index.contains("my_subtree").await);

    let subtrees = index.list().await.unwrap();
    assert!(subtrees.contains(&"my_subtree".to_string()));

    // Verify the type is correct
    let info = index.get_entry("my_subtree").await.unwrap();
    assert_eq!(info.type_id, DocStore::type_id());
}

#[tokio::test]
async fn test_multiple_stores_registered_on_access() {
    // Verify that accessing multiple stores registers all of them in _index
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Get multiple store handles - each access initializes the store
    let tx = database.new_transaction().await.unwrap();
    let _store1 = tx.get_store::<DocStore>("store1").await.unwrap();
    let _store2 = tx.get_store::<Table<TestRecord>>("store2").await.unwrap();
    let _store3 = tx.get_store::<DocStore>("store3").await.unwrap();
    tx.commit().await.unwrap();

    // Verify all are registered with correct types
    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    assert!(index.contains("store1").await);
    assert!(index.contains("store2").await);
    assert!(index.contains("store3").await);

    let subtrees = index.list().await.unwrap();
    assert_eq!(subtrees.len(), 3);

    // Verify types are correct
    assert_eq!(
        index.get_entry("store1").await.unwrap().type_id,
        DocStore::type_id()
    );
    assert_eq!(
        index.get_entry("store2").await.unwrap().type_id,
        Table::<()>::type_id()
    );
    assert_eq!(
        index.get_entry("store3").await.unwrap().type_id,
        DocStore::type_id()
    );
}

// ============================================================================
// Type Mismatch Detection Tests
// ============================================================================

#[tokio::test]
async fn test_type_mismatch_docstore_as_table() {
    // Register a subtree as DocStore, then try to access as Table
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtree as DocStore
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("my_data").await.unwrap();
    store.set("key", "value").await.unwrap();
    tx.commit().await.unwrap();

    // Try to access as Table - should fail with TypeMismatch
    let tx = database.new_transaction().await.unwrap();
    let result = tx.get_store::<Table<TestRecord>>("my_data").await;

    assert!(
        result.is_err(),
        "Should fail when accessing DocStore as Table"
    );
    let err = result.err().unwrap();
    assert!(
        err.to_string().contains("Type mismatch"),
        "Error should mention type mismatch: {err}"
    );
    assert!(
        err.to_string().contains(DocStore::type_id()),
        "Error should mention actual type: {err}"
    );
    assert!(
        err.to_string().contains(Table::<()>::type_id()),
        "Error should mention expected type: {err}"
    );
}

#[tokio::test]
async fn test_type_mismatch_table_as_docstore() {
    // Register a subtree as Table, then try to access as DocStore
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtree as Table
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<Table<TestRecord>>("records").await.unwrap();
    store
        .insert(TestRecord {
            id: 1,
            name: "test".to_string(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Try to access as DocStore - should fail with TypeMismatch
    let tx = database.new_transaction().await.unwrap();
    let result = tx.get_store::<DocStore>("records").await;

    assert!(
        result.is_err(),
        "Should fail when accessing Table as DocStore"
    );
    let err = result.err().unwrap();
    assert!(
        err.to_string().contains("Type mismatch"),
        "Error should mention type mismatch: {err}"
    );
}

#[tokio::test]
async fn test_correct_type_access_succeeds() {
    // Verify that accessing with correct type still works
    let instance = test_instance().await;
    let (private_key, _) = generate_keypair();

    let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
        .await
        .unwrap();

    // Create subtrees
    let tx = database.new_transaction().await.unwrap();
    let doc_store = tx.get_store::<DocStore>("documents").await.unwrap();
    doc_store.set("key", "value").await.unwrap();
    let table_store = tx.get_store::<Table<TestRecord>>("records").await.unwrap();
    table_store
        .insert(TestRecord {
            id: 1,
            name: "test".to_string(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Access with correct types - should succeed
    let tx = database.new_transaction().await.unwrap();
    let doc_result = tx.get_store::<DocStore>("documents").await;
    assert!(doc_result.is_ok(), "DocStore access should succeed");

    let table_result = tx.get_store::<Table<TestRecord>>("records").await;
    assert!(table_result.is_ok(), "Table access should succeed");
}
