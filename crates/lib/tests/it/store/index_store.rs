//! Tests for the IndexStore and _index subtree functionality

use eidetica::{
    Database, Instance, Store,
    auth::crypto::generate_keypair,
    backend::database::InMemory,
    crdt::Doc,
    store::{DocStore, Table},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TestRecord {
    id: u32,
    name: String,
}

// ============================================================================
// Basic IndexStore Functionality
// ============================================================================

#[test]
fn test_index_store_register_subtree() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create a subtree - this will auto-register with default config
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").unwrap();
    store.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Update the registration to custom values
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").unwrap();
    store.set("key2", "value2").unwrap();

    // Now update the index with custom type and config
    index
        .set_subtree_info("my_subtree", "custom:v1", r#"{"custom":"config"}"#)
        .unwrap();

    tx.commit().unwrap();

    // Verify updated registration
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let info = index.get_subtree_info("my_subtree").unwrap();
    assert_eq!(info.type_id, "custom:v1");
    assert_eq!(info.config, r#"{"custom":"config"}"#);
}

#[test]
fn test_index_store_get_subtree_info() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create and register multiple subtrees
    let tx = database.new_transaction().unwrap();
    let store1 = tx.get_store::<DocStore>("subtree1").unwrap();
    store1.set("key", "value").unwrap();

    let store2 = tx.get_store::<DocStore>("subtree2").unwrap();
    store2.set("key", "value").unwrap();

    tx.commit().unwrap();

    // Retrieve and verify
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let info1 = index.get_subtree_info("subtree1").unwrap();
    assert_eq!(info1.type_id, DocStore::type_id());

    let info2 = index.get_subtree_info("subtree2").unwrap();
    assert_eq!(info2.type_id, DocStore::type_id());
}

#[test]
fn test_index_store_contains_subtree() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create a subtree
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("test_subtree").unwrap();
    store.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Check existence
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    assert!(index.contains_subtree("test_subtree"));
    assert!(!index.contains_subtree("nonexistent"));
}

#[test]
fn test_index_store_list_subtrees() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create multiple subtrees
    let tx = database.new_transaction().unwrap();
    let alpha = tx.get_store::<DocStore>("alpha").unwrap();
    alpha.set("key", "value").unwrap();
    let beta = tx.get_store::<DocStore>("beta").unwrap();
    beta.set("key", "value").unwrap();
    let gamma = tx.get_store::<DocStore>("gamma").unwrap();
    gamma.set("key", "value").unwrap();
    tx.commit().unwrap();

    // List and verify
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let subtrees = index.list_subtrees().unwrap();
    assert!(subtrees.contains(&"alpha".to_string()));
    assert!(subtrees.contains(&"beta".to_string()));
    assert!(subtrees.contains(&"gamma".to_string()));
    assert_eq!(subtrees.len(), 3);
}

#[test]
fn test_index_store_update_existing() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtree with default config
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").unwrap();
    store.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Update config
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").unwrap();
    store.set("key2", "value2").unwrap();

    index
        .set_subtree_info("my_subtree", "docstore:v1", r#"{"updated":"config"}"#)
        .unwrap();
    tx.commit().unwrap();

    // Verify update
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let info = index.get_subtree_info("my_subtree").unwrap();
    assert_eq!(info.config, r#"{"updated":"config"}"#);
}

// ============================================================================
// Auto-Registration Behavior
// ============================================================================

#[test]
fn test_auto_register_on_first_access_docstore() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // First access to a new subtree
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("my_data").unwrap();
    store.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Verify _index contains the registration
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let info = index.get_subtree_info("my_data").unwrap();
    assert_eq!(info.type_id, DocStore::type_id());
    assert_eq!(info.config, "{}");
}

#[test]
fn test_no_auto_register_for_system_subtrees() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Access system subtrees and user subtree
    let tx = database.new_transaction().unwrap();
    let _settings = tx.get_settings().unwrap();
    let user_store = tx.get_store::<DocStore>("user_data").unwrap();
    user_store.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Verify only user subtree is in index, not system subtrees
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    assert!(index.contains_subtree("user_data"));
    // System subtrees should NOT be auto-registered
    assert!(!index.contains_subtree("_settings"));
    assert!(!index.contains_subtree("_index"));
    assert!(!index.contains_subtree("_root"));
}

#[test]
fn test_second_access_uses_existing_registration() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // First access - auto-registers
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("my_data").unwrap();
    store.set("key1", "value1").unwrap();
    tx.commit().unwrap();

    // Second access - should use existing registration
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("my_data").unwrap();
    store.set("key2", "value2").unwrap();
    tx.commit().unwrap();

    // Verify still only one entry in index
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let info = index.get_subtree_info("my_data").unwrap();
    assert_eq!(info.type_id, DocStore::type_id());

    // No duplicates in list
    let subtrees = index.list_subtrees().unwrap();
    let count = subtrees.iter().filter(|s| *s == "my_data").count();
    assert_eq!(count, 1);
}

// ============================================================================
// Architectural Constraint Enforcement
// ============================================================================

#[test]
fn test_index_update_includes_target_subtree() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtree first
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").unwrap();
    store.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Update index for this subtree
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").unwrap();
    store.set("key2", "value2").unwrap();

    index
        .set_subtree_info("my_subtree", "docstore:v1", r#"{"new":"config"}"#)
        .unwrap();

    let entry_id = tx.commit().unwrap();

    // Verify Entry contains both _index and my_subtree SubTreeNodes
    let entry = database.backend().unwrap().get(&entry_id).unwrap();
    let subtrees = entry.subtrees();

    assert!(subtrees.contains(&"_index".to_string()));
    assert!(subtrees.contains(&"my_subtree".to_string()));
}

#[test]
fn test_auto_dummy_write_on_index_update() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtree
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("target").unwrap();
    store.set("original", "data").unwrap();
    tx.commit().unwrap();

    // Update index without explicitly modifying target subtree
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    // This should automatically add a dummy write to "target"
    index
        .set_subtree_info("target", "docstore:v1", r#"{"modified":"config"}"#)
        .unwrap();

    let entry_id = tx.commit().unwrap();

    // Verify target subtree is in the Entry (due to automatic dummy write)
    let entry = database.backend().unwrap().get(&entry_id).unwrap();
    assert!(entry.in_subtree("target"));
}

#[test]
fn test_entry_has_both_index_and_subtree_nodes() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtree with auto-registration
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("test_subtree").unwrap();
    store.set("key", "value").unwrap();
    let entry_id = tx.commit().unwrap();

    // Verify Entry structure
    let entry = database.backend().unwrap().get(&entry_id).unwrap();
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

#[test]
fn test_manual_index_update_with_subtree_modification() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtree initially
    let tx = database.new_transaction().unwrap();
    tx.get_store::<DocStore>("my_subtree").unwrap();
    tx.commit().unwrap();

    // Manually update both index and subtree in same transaction
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").unwrap();

    // Modify subtree
    store.set("new_key", "new_value").unwrap();

    // Update index metadata
    index
        .set_subtree_info("my_subtree", "docstore:v2", r#"{"version":"2"}"#)
        .unwrap();

    let entry_id = tx.commit().unwrap();

    // Verify both are in Entry
    let entry = database.backend().unwrap().get(&entry_id).unwrap();
    assert!(entry.in_subtree("_index"));
    assert!(entry.in_subtree("my_subtree"));

    // Verify updated metadata
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();
    let info = index.get_subtree_info("my_subtree").unwrap();
    assert_eq!(info.type_id, "docstore:v2");
}

// ============================================================================
// Integration Tests
// ============================================================================

#[test]
fn test_multi_store_database_index_complete() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create database with multiple store types
    let tx = database.new_transaction().unwrap();

    let doc_store = tx.get_store::<DocStore>("documents").unwrap();
    doc_store.set("doc1", "content").unwrap();

    let table_store = tx.get_store::<Table<TestRecord>>("records").unwrap();
    table_store
        .insert(TestRecord {
            id: 1,
            name: "test".to_string(),
        })
        .unwrap();

    let doc_store2 = tx.get_store::<DocStore>("metadata").unwrap();
    doc_store2.set("key", "value").unwrap();

    tx.commit().unwrap();

    // Verify all are registered with correct types
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let doc_info = index.get_subtree_info("documents").unwrap();
    assert_eq!(doc_info.type_id, "docstore:v1");

    let table_info = index.get_subtree_info("records").unwrap();
    assert_eq!(table_info.type_id, "table:v1");

    let meta_info = index.get_subtree_info("metadata").unwrap();
    assert_eq!(meta_info.type_id, "docstore:v1");

    // Verify list is complete
    let subtrees = index.list_subtrees().unwrap();
    assert_eq!(subtrees.len(), 3);
}

#[test]
fn test_index_persists_across_transactions() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Transaction 1: Create subtrees
    let tx = database.new_transaction().unwrap();
    let store1 = tx.get_store::<DocStore>("subtree1").unwrap();
    store1.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Transaction 2: Create more subtrees
    let tx = database.new_transaction().unwrap();
    let store2 = tx.get_store::<DocStore>("subtree2").unwrap();
    store2.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Transaction 3: Verify both are registered
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    assert!(index.contains_subtree("subtree1"));
    assert!(index.contains_subtree("subtree2"));

    let subtrees = index.list_subtrees().unwrap();
    assert_eq!(subtrees.len(), 2);
}

#[test]
fn test_read_index_from_viewer() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database = Database::create(
        Doc::new(),
        &instance,
        private_key.clone(),
        "test_key".to_string(),
    )
    .unwrap();

    // Create some subtrees
    let tx = database.new_transaction().unwrap();
    let store1 = tx.get_store::<DocStore>("data1").unwrap();
    store1.set("key", "value").unwrap();
    let store2 = tx.get_store::<DocStore>("data2").unwrap();
    store2.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Read _index using viewer (read-only access)
    let viewer = database.get_store_viewer::<DocStore>("_index").unwrap();

    // Verify we can read the index data
    let data1_info_value = viewer.get("data1").unwrap();
    assert!(matches!(
        data1_info_value,
        eidetica::crdt::doc::Value::Doc(_)
    ));

    let data2_info_value = viewer.get("data2").unwrap();
    assert!(matches!(
        data2_info_value,
        eidetica::crdt::doc::Value::Doc(_)
    ));
}

#[test]
fn test_index_survives_database_reload() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    // Create database and add subtrees
    let database = Database::create(
        Doc::new(),
        &instance,
        private_key.clone(),
        "test_key".to_string(),
    )
    .unwrap();

    let root_id = database.root_id().clone();

    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("persistent_data").unwrap();
    store.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Drop database
    drop(database);

    // Reload database from same instance using open (which takes Instance by value, so clone it)
    // Since we need the instance for later use, we'll use open_readonly instead to test persistence
    let database = Database::open_readonly(root_id, &instance).unwrap();

    // Verify index is intact using viewer (read-only)
    let viewer = database.get_store_viewer::<DocStore>("_index").unwrap();

    // Check that persistent_data is registered
    let data_info = viewer.get("persistent_data");
    assert!(data_info.is_ok(), "persistent_data should be in _index");
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[test]
fn test_get_nonexistent_subtree_info() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    // Try to get info for non-existent subtree
    let result = index.get_subtree_info("nonexistent");
    assert!(result.is_err());
}

#[test]
fn test_index_with_empty_database() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Query empty index
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let subtrees = index.list_subtrees().unwrap();
    assert!(subtrees.is_empty());
}

#[test]
fn test_concurrent_index_updates() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Register multiple subtrees in single transaction
    let tx = database.new_transaction().unwrap();

    let store1 = tx.get_store::<DocStore>("subtree1").unwrap();
    store1.set("key", "value").unwrap();
    let store2 = tx.get_store::<DocStore>("subtree2").unwrap();
    store2.set("key", "value").unwrap();
    let store3 = tx.get_store::<Table<TestRecord>>("subtree3").unwrap();
    store3
        .insert(TestRecord {
            id: 1,
            name: "test".to_string(),
        })
        .unwrap();
    let store4 = tx.get_store::<DocStore>("subtree4").unwrap();
    store4.set("key", "value").unwrap();

    tx.commit().unwrap();

    // Verify all are registered
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    assert!(index.contains_subtree("subtree1"));
    assert!(index.contains_subtree("subtree2"));
    assert!(index.contains_subtree("subtree3"));
    assert!(index.contains_subtree("subtree4"));

    let subtrees = index.list_subtrees().unwrap();
    assert_eq!(subtrees.len(), 4);
}

#[test]
fn test_empty_config_is_valid() {
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtree with default empty config
    let tx = database.new_transaction().unwrap();
    let _store = tx.get_store::<DocStore>("test").unwrap();
    tx.commit().unwrap();

    // Verify empty config is stored and retrieved correctly
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let info = index.get_subtree_info("test").unwrap();
    assert_eq!(info.config, "{}");
}

// ============================================================================
// Architectural Constraint Tests
// ============================================================================

#[test]
fn test_index_modification_forces_subtree_in_entry() {
    // Verify that when _index is modified for a subtree, that subtree appears in the Entry
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create a subtree first
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("my_subtree").unwrap();
    store.set("key", "value").unwrap();
    let _entry_id1 = tx.commit().unwrap();

    // Now update the index for that subtree
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();
    index
        .set_subtree_info("my_subtree", "custom:v1", r#"{"custom":"config"}"#)
        .unwrap();
    let entry_id2 = tx.commit().unwrap();

    // Load the entry and verify both _index and my_subtree are present
    let backend = database.backend().unwrap();
    let entry = backend.get(&entry_id2).unwrap();

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

#[test]
fn test_auto_registration_includes_both_subtrees() {
    // Verify that auto-registration during commit includes both _index and the data subtree
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create a subtree - this triggers auto-registration
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("new_subtree").unwrap();
    store.set("key", "value").unwrap();
    let entry_id = tx.commit().unwrap();

    // Load the entry and verify both _index and new_subtree are present
    let backend = database.backend().unwrap();
    let entry = backend.get(&entry_id).unwrap();

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

#[test]
fn test_accessing_store_registers_in_index() {
    // Verify that calling get_store() registers the subtree in _index even without writing data
    // This is the expected behavior: accessing a store initializes it
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Get a store handle but don't write any data
    let tx = database.new_transaction().unwrap();
    let _store = tx.get_store::<DocStore>("my_subtree").unwrap();
    // No writes needed - accessing the store initializes it
    tx.commit().unwrap();

    // Verify the subtree IS registered in the index
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    assert!(index.contains_subtree("my_subtree"));

    let subtrees = index.list_subtrees().unwrap();
    assert!(subtrees.contains(&"my_subtree".to_string()));

    // Verify the type is correct
    let info = index.get_subtree_info("my_subtree").unwrap();
    assert_eq!(info.type_id, "docstore:v1");
}

#[test]
fn test_multiple_stores_registered_on_access() {
    // Verify that accessing multiple stores registers all of them in _index
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Get multiple store handles - each access initializes the store
    let tx = database.new_transaction().unwrap();
    let _store1 = tx.get_store::<DocStore>("store1").unwrap();
    let _store2 = tx.get_store::<Table<TestRecord>>("store2").unwrap();
    let _store3 = tx.get_store::<DocStore>("store3").unwrap();
    tx.commit().unwrap();

    // Verify all are registered with correct types
    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    assert!(index.contains_subtree("store1"));
    assert!(index.contains_subtree("store2"));
    assert!(index.contains_subtree("store3"));

    let subtrees = index.list_subtrees().unwrap();
    assert_eq!(subtrees.len(), 3);

    // Verify types are correct
    assert_eq!(
        index.get_subtree_info("store1").unwrap().type_id,
        "docstore:v1"
    );
    assert_eq!(
        index.get_subtree_info("store2").unwrap().type_id,
        "table:v1"
    );
    assert_eq!(
        index.get_subtree_info("store3").unwrap().type_id,
        "docstore:v1"
    );
}

// ============================================================================
// Type Mismatch Detection Tests
// ============================================================================

#[test]
fn test_type_mismatch_docstore_as_table() {
    // Register a subtree as DocStore, then try to access as Table
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtree as DocStore
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<DocStore>("my_data").unwrap();
    store.set("key", "value").unwrap();
    tx.commit().unwrap();

    // Try to access as Table - should fail with TypeMismatch
    let tx = database.new_transaction().unwrap();
    let result: Result<Table<TestRecord>, _> = tx.get_store("my_data");

    assert!(
        result.is_err(),
        "Should fail when accessing DocStore as Table"
    );
    let err = result.err().unwrap();
    assert!(
        err.to_string().contains("Type mismatch"),
        "Error should mention type mismatch: {}",
        err
    );
    assert!(
        err.to_string().contains("docstore:v1"),
        "Error should mention actual type: {}",
        err
    );
    assert!(
        err.to_string().contains("table:v1"),
        "Error should mention expected type: {}",
        err
    );
}

#[test]
fn test_type_mismatch_table_as_docstore() {
    // Register a subtree as Table, then try to access as DocStore
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtree as Table
    let tx = database.new_transaction().unwrap();
    let store = tx.get_store::<Table<TestRecord>>("records").unwrap();
    store
        .insert(TestRecord {
            id: 1,
            name: "test".to_string(),
        })
        .unwrap();
    tx.commit().unwrap();

    // Try to access as DocStore - should fail with TypeMismatch
    let tx = database.new_transaction().unwrap();
    let result: Result<DocStore, _> = tx.get_store("records");

    assert!(
        result.is_err(),
        "Should fail when accessing Table as DocStore"
    );
    let err = result.err().unwrap();
    assert!(
        err.to_string().contains("Type mismatch"),
        "Error should mention type mismatch: {}",
        err
    );
}

#[test]
fn test_correct_type_access_succeeds() {
    // Verify that accessing with correct type still works
    let backend = InMemory::new();
    let instance = Instance::open(Box::new(backend)).unwrap();
    let (private_key, _) = generate_keypair();

    let database =
        Database::create(Doc::new(), &instance, private_key, "test_key".to_string()).unwrap();

    // Create subtrees
    let tx = database.new_transaction().unwrap();
    let doc_store = tx.get_store::<DocStore>("documents").unwrap();
    doc_store.set("key", "value").unwrap();
    let table_store = tx.get_store::<Table<TestRecord>>("records").unwrap();
    table_store
        .insert(TestRecord {
            id: 1,
            name: "test".to_string(),
        })
        .unwrap();
    tx.commit().unwrap();

    // Access with correct types - should succeed
    let tx = database.new_transaction().unwrap();
    let doc_result = tx.get_store::<DocStore>("documents");
    assert!(doc_result.is_ok(), "DocStore access should succeed");

    let table_result = tx.get_store::<Table<TestRecord>>("records");
    assert!(table_result.is_ok(), "Table access should succeed");
}
