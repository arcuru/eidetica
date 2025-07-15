//! Table subtree operation tests
//!
//! This module contains tests for Table subtree functionality including
//! CRUD operations, search functionality, UUID generation, and multiple operations.

use super::helpers::*;
use crate::helpers::*;
use eidetica::subtree::Table;

#[test]
fn test_table_basic_crud_operations() {
    let tree = setup_tree();

    // Use helper to create initial record
    let initial_record = TestRecord {
        name: "John Doe".to_string(),
        age: 30,
        email: "john@example.com".to_string(),
    };
    let keys = create_table_operation(&tree, "test_records", &[initial_record.clone()]);
    let primary_key = &keys[0];

    // Test CRUD operations within an operation
    let op = tree.new_operation().expect("Failed to start operation");
    let table = op
        .get_subtree::<Table<TestRecord>>("test_records")
        .expect("Failed to get Table");

    // Test get (should see existing record)
    let retrieved = table
        .get(primary_key)
        .expect("Failed to get existing record");
    assert_eq!(retrieved, initial_record);

    // Test update/set
    let updated_record = TestRecord {
        name: "John Smith".to_string(),
        age: 31,
        email: "john.smith@example.com".to_string(),
    };
    table
        .set(primary_key, updated_record.clone())
        .expect("Failed to update record");

    // Verify update within same operation
    let retrieved_updated = table
        .get(primary_key)
        .expect("Failed to get updated record");
    assert_eq!(retrieved_updated, updated_record);

    // Test insert of new record
    let new_record = TestRecord {
        name: "Jane Doe".to_string(),
        age: 25,
        email: "jane@example.com".to_string(),
    };
    let new_pk = table
        .insert(new_record.clone())
        .expect("Failed to insert new record");
    assert!(!new_pk.is_empty(), "New primary key should not be empty");

    // Verify new record retrieval
    let retrieved_new = table.get(&new_pk).expect("Failed to get new record");
    assert_eq!(retrieved_new, new_record);

    op.commit().expect("Failed to commit operation");

    // Verify persistence using helper
    assert_table_record(&tree, "test_records", primary_key, &updated_record);
    assert_table_record(&tree, "test_records", &new_pk, &new_record);
}

#[test]
fn test_table_multiple_records() {
    let tree = setup_tree();

    // Use helper to create multiple records
    let values = &[10, 20, 30, 40, 50];
    let inserted_keys = create_simple_table_operation(&tree, "simple_records", values);

    // Verify all records persist after commit
    let viewer = tree
        .get_subtree_viewer::<Table<SimpleRecord>>("simple_records")
        .expect("Failed to get Table viewer");

    for (i, key) in inserted_keys.iter().enumerate() {
        let record = viewer.get(key).expect("Failed to get record after commit");
        assert_eq!(record.value, values[i]);
    }
}

#[test]
fn test_table_search_functionality() {
    let tree = setup_tree();

    // Use helper to create test records
    let records = create_test_records();
    create_table_operation(&tree, "search_records", &records);

    // Test search by age using helper
    assert_table_search_count(&tree, "search_records", |record| record.age == 25, 2);

    // Test search by email domain using helper
    assert_table_search_count(
        &tree,
        "search_records",
        |record| record.email.contains("example.com"),
        2,
    );

    // Test search by name prefix using helper
    assert_table_search_count(
        &tree,
        "search_records",
        |record| record.name.starts_with('B'),
        1,
    );

    // Test search with no matches using helper
    assert_table_search_count(&tree, "search_records", |record| record.age > 100, 0);

    // Test search after commit with detailed verification
    let viewer = tree
        .get_subtree_viewer::<Table<TestRecord>>("search_records")
        .expect("Failed to get Table viewer");

    let age_30_results = viewer
        .search(|record| record.age == 30)
        .expect("Failed to search after commit");
    assert_eq!(age_30_results.len(), 1);
    assert_eq!(age_30_results[0].1.name, "Bob Smith");
}

#[test]
fn test_table_uuid_generation() {
    let tree = setup_tree();

    // Generate 100 records to test UUID uniqueness
    let values: Vec<i32> = (1..=100).collect();
    let generated_keys = create_simple_table_operation(&tree, "uuid_test", &values);

    // Use helper to verify UUID format and uniqueness
    assert_valid_uuids(&generated_keys);

    // Verify all records are retrievable with their unique keys
    let viewer = tree
        .get_subtree_viewer::<Table<SimpleRecord>>("uuid_test")
        .expect("Failed to get Table viewer");

    for key in &generated_keys {
        let record = viewer.get(key).expect("Failed to get record by UUID");
        assert!(record.value >= 1 && record.value <= 100);
    }
}

#[test]
fn test_table_multiple_operations() {
    let tree = setup_tree();

    // Use helper to test multi-operation workflow
    let (key1, key2, key3) = test_table_multi_operations(&tree, "multi_op_test");

    // Verify final state
    let viewer = tree
        .get_subtree_viewer::<Table<TestRecord>>("multi_op_test")
        .expect("Failed to get Table viewer");

    // Check updated record
    let final_record1 = viewer.get(&key1).expect("Failed to get final record1");
    assert_eq!(final_record1.name, "Updated User 1");
    assert_eq!(final_record1.age, 21);
    assert_eq!(final_record1.email, "user1@updated.com");

    // Check unchanged record
    let final_record2 = viewer.get(&key2).expect("Failed to get final record2");
    assert_eq!(final_record2.name, "Initial User 2");
    assert_eq!(final_record2.age, 25);
    assert_eq!(final_record2.email, "user2@initial.com");

    // Check new record
    let final_record3 = viewer.get(&key3).expect("Failed to get final record3");
    assert_eq!(final_record3.name, "New User 3");
    assert_eq!(final_record3.age, 30);
    assert_eq!(final_record3.email, "user3@new.com");

    // Verify search across all records
    let all_records = viewer
        .search(|_| true)
        .expect("Failed to search all records");
    assert_eq!(all_records.len(), 3);
}

#[test]
fn test_table_empty_search() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let table = op
            .get_subtree::<Table<SimpleRecord>>("empty_search_test")
            .expect("Failed to get Table");

        // Search in empty store
        let results = table
            .search(|_| true)
            .expect("Failed to search empty store");
        assert_eq!(results.len(), 0);
    }

    op.commit().expect("Failed to commit operation");

    // Search in empty store after commit
    let viewer = tree
        .get_subtree_viewer::<Table<SimpleRecord>>("empty_search_test")
        .expect("Failed to get Table viewer");

    let results = viewer
        .search(|_| true)
        .expect("Failed to search empty store after commit");
    assert_eq!(results.len(), 0);
}

#[test]
fn test_empty_table_behavior() {
    let tree = setup_tree();

    // Test empty Table behavior
    let table_viewer = tree
        .get_subtree_viewer::<Table<TestRecord>>("empty_table")
        .expect("Failed to get empty Table viewer");

    let empty_search = table_viewer
        .search(|_| true)
        .expect("Failed to search empty table");
    assert_eq!(empty_search.len(), 0);
}

#[test]
fn test_table_with_authenticated_tree() {
    let db = setup_db_with_key("table_auth_key");
    let tree = db
        .new_tree_default("table_auth_key")
        .expect("Failed to create authenticated tree");

    let op = tree.new_operation().expect("Failed to start operation");

    let primary_key = {
        let table = op
            .get_subtree::<Table<TestRecord>>("auth_records")
            .expect("Failed to get Table");

        let record = TestRecord {
            name: "Authenticated User".to_string(),
            age: 28,
            email: "auth@secure.com".to_string(),
        };

        // Insert record in authenticated tree
        let pk = table
            .insert(record.clone())
            .expect("Failed to insert authenticated record");

        // Verify retrieval within same operation
        let retrieved = table.get(&pk).expect("Failed to get authenticated record");
        assert_eq!(retrieved, record);

        pk
    };

    op.commit()
        .expect("Failed to commit authenticated operation");

    // Verify persistence in authenticated tree
    let expected = TestRecord {
        name: "Authenticated User".to_string(),
        age: 28,
        email: "auth@secure.com".to_string(),
    };
    assert_table_record(&tree, "auth_records", &primary_key, &expected);
}
