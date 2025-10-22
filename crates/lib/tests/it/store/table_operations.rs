//! Table subtree operation tests
//!
//! This module contains tests for Table subtree functionality including
//! CRUD operations, search functionality, UUID generation, and multiple operations.

use eidetica::store::Table;

use super::helpers::*;
use crate::helpers::*;

#[test]
fn test_table_basic_crud_operations() {
    let (_instance, tree) = setup_tree();

    // Use helper to create initial record
    let initial_record = TestRecord {
        name: "John Doe".to_string(),
        age: 30,
        email: "john@example.com".to_string(),
    };
    let keys = create_table_operation(&tree, "test_records", std::slice::from_ref(&initial_record));
    let primary_key = &keys[0];

    // Test CRUD operations within an operation
    let op = tree.new_transaction().expect("Failed to start operation");
    let table = op
        .get_store::<Table<TestRecord>>("test_records")
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
    let (_instance, tree) = setup_tree();

    // Use helper to create multiple records
    let values = &[10, 20, 30, 40, 50];
    let inserted_keys = create_simple_table_operation(&tree, "simple_records", values);

    // Verify all records persist after commit
    let viewer = tree
        .get_store_viewer::<Table<SimpleRecord>>("simple_records")
        .expect("Failed to get Table viewer");

    for (i, key) in inserted_keys.iter().enumerate() {
        let record = viewer.get(key).expect("Failed to get record after commit");
        assert_eq!(record.value, values[i]);
    }
}

#[test]
fn test_table_search_functionality() {
    let (_instance, tree) = setup_tree();

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
        .get_store_viewer::<Table<TestRecord>>("search_records")
        .expect("Failed to get Table viewer");

    let age_30_results = viewer
        .search(|record| record.age == 30)
        .expect("Failed to search after commit");
    assert_eq!(age_30_results.len(), 1);
    assert_eq!(age_30_results[0].1.name, "Bob Smith");
}

#[test]
fn test_table_uuid_generation() {
    let (_instance, tree) = setup_tree();

    // Generate 100 records to test UUID uniqueness
    let values: Vec<i32> = (1..=100).collect();
    let generated_keys = create_simple_table_operation(&tree, "uuid_test", &values);

    // Use helper to verify UUID format and uniqueness
    assert_valid_uuids(&generated_keys);

    // Verify all records are retrievable with their unique keys
    let viewer = tree
        .get_store_viewer::<Table<SimpleRecord>>("uuid_test")
        .expect("Failed to get Table viewer");

    for key in &generated_keys {
        let record = viewer.get(key).expect("Failed to get record by UUID");
        assert!(record.value >= 1 && record.value <= 100);
    }
}

#[test]
fn test_table_multiple_operations() {
    let (_instance, tree) = setup_tree();

    // Use helper to test multi-operation workflow
    let (key1, key2, key3) = test_table_multi_operations(&tree, "multi_op_test");

    // Verify final state
    let viewer = tree
        .get_store_viewer::<Table<TestRecord>>("multi_op_test")
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
    let (_instance, tree) = setup_tree();
    let op = tree.new_transaction().expect("Failed to start operation");

    {
        let table = op
            .get_store::<Table<SimpleRecord>>("empty_search_test")
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
        .get_store_viewer::<Table<SimpleRecord>>("empty_search_test")
        .expect("Failed to get Table viewer");

    let results = viewer
        .search(|_| true)
        .expect("Failed to search empty store after commit");
    assert_eq!(results.len(), 0);
}

#[test]
fn test_empty_table_behavior() {
    let (_instance, tree) = setup_tree();

    // Test empty Table behavior
    let table_viewer = tree
        .get_store_viewer::<Table<TestRecord>>("empty_table")
        .expect("Failed to get empty Table viewer");

    let empty_search = table_viewer
        .search(|_| true)
        .expect("Failed to search empty table");
    assert_eq!(empty_search.len(), 0);
}

#[test]
fn test_table_delete_basic() {
    let (_instance, tree) = setup_tree();

    // Create initial records using helper
    let initial_records = vec![
        TestRecord {
            name: "User 1".to_string(),
            age: 25,
            email: "user1@test.com".to_string(),
        },
        TestRecord {
            name: "User 2".to_string(),
            age: 30,
            email: "user2@test.com".to_string(),
        },
        TestRecord {
            name: "User 3".to_string(),
            age: 35,
            email: "user3@test.com".to_string(),
        },
    ];
    let keys = create_table_operation(&tree, "delete_test", &initial_records);

    // Delete one record within an operation
    let op = tree.new_transaction().expect("Failed to start operation");
    {
        let table = op
            .get_store::<Table<TestRecord>>("delete_test")
            .expect("Failed to get Table");

        // Delete existing record
        let deleted = table
            .delete(&keys[1])
            .expect("Failed to delete existing record");
        assert!(deleted, "Should return true when deleting existing record");

        // Verify deletion within same operation
        assert!(
            table.get(&keys[1]).is_err(),
            "Deleted record should not be retrievable"
        );

        // Verify other records still exist
        let record1 = table.get(&keys[0]).expect("Record 1 should still exist");
        assert_eq!(record1.name, "User 1");

        let record3 = table.get(&keys[2]).expect("Record 3 should still exist");
        assert_eq!(record3.name, "User 3");
    }
    op.commit().expect("Failed to commit operation");

    // Verify deletion persisted using helper
    assert_table_record_deleted(&tree, "delete_test", &keys[1]);

    // Verify other records still exist
    assert_table_record(&tree, "delete_test", &keys[0], &initial_records[0]);
    assert_table_record(&tree, "delete_test", &keys[2], &initial_records[2]);
}

#[test]
fn test_table_delete_nonexistent() {
    let (_instance, tree) = setup_tree();

    // Create one record
    let record = TestRecord {
        name: "Existing User".to_string(),
        age: 30,
        email: "existing@test.com".to_string(),
    };
    let keys = create_table_operation(&tree, "delete_nonexistent", std::slice::from_ref(&record));

    let op = tree.new_transaction().expect("Failed to start operation");
    {
        let table = op
            .get_store::<Table<TestRecord>>("delete_nonexistent")
            .expect("Failed to get Table");

        // Try to delete non-existent key
        let deleted = table
            .delete("non-existent-uuid")
            .expect("Delete should not error on non-existent key");
        assert!(
            !deleted,
            "Should return false when deleting non-existent record"
        );

        // Verify existing record is still there
        let existing = table.get(&keys[0]).expect("Existing record should remain");
        assert_eq!(existing.name, "Existing User");
    }
    op.commit().expect("Failed to commit operation");

    // Verify existing record persisted
    assert_table_record(&tree, "delete_nonexistent", &keys[0], &record);
}

#[test]
fn test_table_delete_and_reinsert() {
    let (_instance, tree) = setup_tree();

    // Create initial record
    let initial_record = TestRecord {
        name: "Original User".to_string(),
        age: 25,
        email: "original@test.com".to_string(),
    };
    let keys = create_table_operation(
        &tree,
        "delete_reinsert",
        std::slice::from_ref(&initial_record),
    );
    let original_key = &keys[0];

    // Delete the record
    let op1 = tree.new_transaction().expect("Failed to start operation");
    {
        let table = op1
            .get_store::<Table<TestRecord>>("delete_reinsert")
            .expect("Failed to get Table");

        table.delete(original_key).expect("Failed to delete record");
    }
    op1.commit().expect("Failed to commit deletion");

    // Verify deletion
    assert_table_record_deleted(&tree, "delete_reinsert", original_key);

    // Re-insert with the same key
    let op2 = tree.new_transaction().expect("Failed to start operation");
    {
        let table = op2
            .get_store::<Table<TestRecord>>("delete_reinsert")
            .expect("Failed to get Table");

        let new_record = TestRecord {
            name: "New User".to_string(),
            age: 30,
            email: "new@test.com".to_string(),
        };

        table
            .set(original_key, new_record.clone())
            .expect("Failed to re-insert record");

        // Verify re-inserted record is retrievable
        let retrieved = table
            .get(original_key)
            .expect("Re-inserted record should be retrievable");
        assert_eq!(retrieved, new_record);
    }
    op2.commit().expect("Failed to commit re-insertion");

    // Verify new record persisted with same key
    let new_record = TestRecord {
        name: "New User".to_string(),
        age: 30,
        email: "new@test.com".to_string(),
    };
    assert_table_record(&tree, "delete_reinsert", original_key, &new_record);
}

#[test]
fn test_table_search_after_delete() {
    let (_instance, tree) = setup_tree();

    // Create test records using helper
    let records = create_test_records();
    let keys = create_table_operation(&tree, "search_after_delete", &records);

    // Verify initial search count
    assert_table_search_count(&tree, "search_after_delete", |record| record.age == 25, 2);

    // Delete one of the age=25 records
    let op = tree.new_transaction().expect("Failed to start operation");
    {
        let table = op
            .get_store::<Table<TestRecord>>("search_after_delete")
            .expect("Failed to get Table");

        table.delete(&keys[0]).expect("Failed to delete record");
    }
    op.commit().expect("Failed to commit deletion");

    // Verify search count decreased
    assert_table_search_count(&tree, "search_after_delete", |record| record.age == 25, 1);

    // Verify the remaining age=25 record is the correct one
    let viewer = tree
        .get_store_viewer::<Table<TestRecord>>("search_after_delete")
        .expect("Failed to get Table viewer");

    let age_25_results = viewer
        .search(|record| record.age == 25)
        .expect("Failed to search after delete");
    assert_eq!(age_25_results.len(), 1);
    assert_eq!(age_25_results[0].1.name, "Charlie Brown");
}

#[test]
fn test_table_delete_multiple() {
    let (_instance, tree) = setup_tree();

    // Create multiple records
    let values = &[10, 20, 30, 40, 50];
    let keys = create_simple_table_operation(&tree, "delete_multiple", values);

    // Delete multiple records in one operation
    let op = tree.new_transaction().expect("Failed to start operation");
    {
        let table = op
            .get_store::<Table<SimpleRecord>>("delete_multiple")
            .expect("Failed to get Table");

        // Delete records at indices 1 and 3
        let deleted1 = table.delete(&keys[1]).expect("Failed to delete record 1");
        let deleted3 = table.delete(&keys[3]).expect("Failed to delete record 3");

        assert!(deleted1);
        assert!(deleted3);

        // Verify deletions
        assert!(table.get(&keys[1]).is_err());
        assert!(table.get(&keys[3]).is_err());

        // Verify remaining records
        assert_eq!(table.get(&keys[0]).expect("Record 0 exists").value, 10);
        assert_eq!(table.get(&keys[2]).expect("Record 2 exists").value, 30);
        assert_eq!(table.get(&keys[4]).expect("Record 4 exists").value, 50);
    }
    op.commit().expect("Failed to commit deletions");

    // Verify search returns only non-deleted records
    let viewer = tree
        .get_store_viewer::<Table<SimpleRecord>>("delete_multiple")
        .expect("Failed to get Table viewer");

    let all_records = viewer
        .search(|_| true)
        .expect("Failed to search all records");
    assert_eq!(all_records.len(), 3);

    // Verify correct records remain
    let values: Vec<i32> = all_records.iter().map(|(_, r)| r.value).collect();
    assert!(values.contains(&10));
    assert!(values.contains(&30));
    assert!(values.contains(&50));
}

#[test]
fn test_table_delete_concurrent_modifications() {
    let (_instance, tree) = setup_tree();

    // Create base record
    let op_base = tree.new_transaction().expect("Failed to start operation");
    let key1 = {
        let table = op_base
            .get_store::<Table<TestRecord>>("concurrent_delete")
            .expect("Failed to get Table");
        let record = TestRecord {
            name: "Base User".to_string(),
            age: 25,
            email: "base@test.com".to_string(),
        };
        table.insert(record).expect("Failed to insert base record")
    };
    let base_entry_id = op_base.commit().expect("Failed to commit base");

    // Branch A: Delete the record
    let op_branch_a = tree
        .new_transaction_with_tips([base_entry_id.clone()])
        .expect("Failed to start branch A");
    {
        let table = op_branch_a
            .get_store::<Table<TestRecord>>("concurrent_delete")
            .expect("Failed to get Table");

        table.delete(&key1).expect("Failed to delete in branch A");
    }
    op_branch_a
        .commit()
        .expect("Failed to commit branch A deletion");

    // Branch B: Update the same record
    let op_branch_b = tree
        .new_transaction_with_tips([base_entry_id])
        .expect("Failed to start branch B");
    {
        let table = op_branch_b
            .get_store::<Table<TestRecord>>("concurrent_delete")
            .expect("Failed to get Table");

        let updated_record = TestRecord {
            name: "Updated User".to_string(),
            age: 30,
            email: "updated@test.com".to_string(),
        };
        table
            .set(&key1, updated_record)
            .expect("Failed to update in branch B");
    }
    op_branch_b
        .commit()
        .expect("Failed to commit branch B update");

    // Get merged result - CRDT last-write-wins should apply
    // The result depends on CRDT merge semantics
    let viewer = tree
        .get_store_viewer::<Table<TestRecord>>("concurrent_delete")
        .expect("Failed to get Table viewer");

    // After CRDT merge, one operation will win
    // We just verify the system doesn't crash and produces a deterministic result
    let result = viewer.get(&key1);

    // Either the record exists (update won) or doesn't exist (delete won)
    // Both are valid CRDT outcomes depending on timestamp/ID ordering
    match result {
        Ok(record) => {
            // Update won - verify it's the updated record
            assert_eq!(record.name, "Updated User");
        }
        Err(_) => {
            // Delete won - record doesn't exist
            // This is also valid
        }
    }
}
