//! Subtree integration tests
//!
//! This module contains tests for complex integration scenarios including
//! concurrent modifications, merging, authentication, and cross-subtree operations.

use eidetica::store::{DocStore, Table};

use super::helpers::*;
use crate::helpers::*;

#[test]
fn test_table_complex_data_merging() {
    let (_instance, tree) = setup_tree();

    // Use helper to test concurrent modifications
    let (key1, merged_record) = test_table_concurrent_modifications(&tree, "merge_test");

    // With KVOverWrite semantics, one of the concurrent updates should win
    // The exact result depends on the deterministic merge order of the underlying CRDT
    assert!(
        merged_record.name == "Updated by Branch A" || merged_record.name == "Updated by Branch B",
        "Merged record should contain updates from either branch A or B, got: {}",
        merged_record.name
    );

    // Verify the age was also updated according to whichever branch won
    if merged_record.name == "Updated by Branch A" {
        assert_eq!(merged_record.age, 26);
        assert_eq!(merged_record.email, "updated_a@test.com");
    } else {
        assert_eq!(merged_record.age, 27);
        assert_eq!(merged_record.email, "updated_b@test.com");
    }

    // Verify the merged state persists after commit
    let viewer = tree
        .get_store_viewer::<Table<TestRecord>>("merge_test")
        .expect("Failed to get Table viewer");

    let final_record = viewer
        .get(&key1)
        .expect("Failed to get final merged record");
    assert_eq!(
        final_record, merged_record,
        "Final state should match merged state"
    );
}

#[test]
fn test_mixed_subtree_operations() {
    let (_instance, tree) = setup_tree();

    // Create operations that use multiple subtree types in one operation
    let op = tree.new_transaction().expect("Failed to start operation");

    let table_key = {
        // Use Table subtree
        let table = op
            .get_store::<Table<TestRecord>>("records")
            .expect("Failed to get Table");

        let record = TestRecord {
            name: "Mixed Operation User".to_string(),
            age: 30,
            email: "mixed@test.com".to_string(),
        };

        let key = table.insert(record).expect("Failed to insert record");

        // Use Doc subtree in same operation
        let dict = op
            .get_store::<DocStore>("config")
            .expect("Failed to get Doc");
        dict.set("mode", "mixed").expect("Failed to set config");
        dict.set("version", "1.0").expect("Failed to set version");

        key
    };

    op.commit().expect("Failed to commit mixed operation");

    // Verify both subtrees persist correctly
    let table_viewer = tree
        .get_store_viewer::<Table<TestRecord>>("records")
        .expect("Failed to get Table viewer");
    let dict_viewer = tree
        .get_store_viewer::<DocStore>("config")
        .expect("Failed to get Doc viewer");

    // Check Table data
    let record = table_viewer.get(&table_key).expect("Failed to get record");
    assert_eq!(record.name, "Mixed Operation User");

    // Check Doc data
    assert_dict_value(&dict_viewer, "mode", "mixed");
    assert_dict_value(&dict_viewer, "version", "1.0");
}

#[test]
fn test_subtree_persistence_across_operations() {
    let (_instance, tree) = setup_tree();

    // Operation 1: Create data in multiple subtrees
    let op1 = tree.new_transaction().expect("Op1: Failed to start");
    let table_key = {
        let table = op1
            .get_store::<Table<SimpleRecord>>("data")
            .expect("Op1: Failed to get Table");
        let dict = op1
            .get_store::<DocStore>("metadata")
            .expect("Op1: Failed to get Doc");

        let record = SimpleRecord { value: 100 };
        let key = table.insert(record).expect("Op1: Failed to insert");

        dict.set("created_by", "test").expect("Op1: Failed to set");
        dict.set("timestamp", "2023-01-01")
            .expect("Op1: Failed to set");

        key
    };
    op1.commit().expect("Op1: Failed to commit");

    // Operation 2: Update existing data and add new data
    let op2 = tree.new_transaction().expect("Op2: Failed to start");
    {
        let table = op2
            .get_store::<Table<SimpleRecord>>("data")
            .expect("Op2: Failed to get Table");
        let dict = op2
            .get_store::<DocStore>("metadata")
            .expect("Op2: Failed to get Doc");

        // Update existing record
        let updated_record = SimpleRecord { value: 200 };
        table
            .set(&table_key, updated_record)
            .expect("Op2: Failed to update");

        // Add new metadata
        dict.set("updated_by", "test2").expect("Op2: Failed to set");
        dict.set("version", "2").expect("Op2: Failed to set");

        // Verify we can read the original metadata within this operation
        assert_dict_value(&dict, "created_by", "test");
        assert_dict_value(&dict, "timestamp", "2023-01-01");
    }
    op2.commit().expect("Op2: Failed to commit");

    // Verify final state
    let table_viewer = tree
        .get_store_viewer::<Table<SimpleRecord>>("data")
        .expect("Failed to get Table viewer");
    let dict_viewer = tree
        .get_store_viewer::<DocStore>("metadata")
        .expect("Failed to get Doc viewer");

    // Check updated record
    let final_record = table_viewer
        .get(&table_key)
        .expect("Failed to get final record");
    assert_eq!(final_record.value, 200);

    // Check all metadata is preserved
    assert_dict_value(&dict_viewer, "created_by", "test");
    assert_dict_value(&dict_viewer, "timestamp", "2023-01-01");
    assert_dict_value(&dict_viewer, "updated_by", "test2");
    assert_dict_value(&dict_viewer, "version", "2");
}

#[test]
fn test_subtree_concurrent_access_patterns() {
    let (_instance, tree) = setup_tree();

    // Create base entry with both Doc and Table data
    let op_base = tree.new_transaction().expect("Base: Failed to start");
    let base_table_key = {
        let table = op_base
            .get_store::<Table<TestRecord>>("shared_data")
            .expect("Base: Failed to get Table");
        let dict = op_base
            .get_store::<DocStore>("shared_config")
            .expect("Base: Failed to get Doc");

        let record = TestRecord {
            name: "Base User".to_string(),
            age: 25,
            email: "base@test.com".to_string(),
        };
        let key = table.insert(record).expect("Base: Failed to insert");

        dict.set("status", "active").expect("Base: Failed to set");
        dict.set("priority", "normal").expect("Base: Failed to set");

        key
    };
    let base_entry_id = op_base.commit().expect("Base: Failed to commit");

    // Branch A: Modify Table data
    let op_branch_a = tree
        .new_transaction_with_tips([base_entry_id.clone()])
        .expect("Branch A: Failed to start");
    {
        let table = op_branch_a
            .get_store::<Table<TestRecord>>("shared_data")
            .expect("Branch A: Failed to get Table");

        let updated_record = TestRecord {
            name: "Branch A User".to_string(),
            age: 26,
            email: "branch_a@test.com".to_string(),
        };
        table
            .set(&base_table_key, updated_record)
            .expect("Branch A: Failed to update");
    }
    op_branch_a.commit().expect("Branch A: Failed to commit");

    // Branch B: Modify Doc data (parallel to Branch A)
    let op_branch_b = tree
        .new_transaction_with_tips([base_entry_id])
        .expect("Branch B: Failed to start");
    {
        let dict = op_branch_b
            .get_store::<DocStore>("shared_config")
            .expect("Branch B: Failed to get Doc");

        dict.set("status", "modified")
            .expect("Branch B: Failed to set");
        dict.set("modified_by", "branch_b")
            .expect("Branch B: Failed to set");
    }
    op_branch_b.commit().expect("Branch B: Failed to commit");

    // Merge operation: Read the merged state
    let op_merge = tree.new_transaction().expect("Merge: Failed to start");
    let (merged_record, merged_status) = {
        let table = op_merge
            .get_store::<Table<TestRecord>>("shared_data")
            .expect("Merge: Failed to get Table");
        let dict = op_merge
            .get_store::<DocStore>("shared_config")
            .expect("Merge: Failed to get Doc");

        let record = table
            .get(&base_table_key)
            .expect("Merge: Failed to get record");
        let status = dict
            .get_string("status")
            .expect("Merge: Failed to get status");

        (record, status)
    };
    op_merge.commit().expect("Merge: Failed to commit");

    // Verify the merge results
    // Table should have Branch A's changes
    assert_eq!(merged_record.name, "Branch A User");
    assert_eq!(merged_record.age, 26);

    // Doc should have Branch B's changes
    assert_eq!(merged_status, "modified");

    // Verify final persistence
    let table_viewer = tree
        .get_store_viewer::<Table<TestRecord>>("shared_data")
        .expect("Failed to get Table viewer");
    let dict_viewer = tree
        .get_store_viewer::<DocStore>("shared_config")
        .expect("Failed to get Doc viewer");

    let final_record = table_viewer
        .get(&base_table_key)
        .expect("Failed to get final record");
    assert_eq!(final_record, merged_record);

    assert_dict_value(&dict_viewer, "status", "modified");
    assert_dict_value(&dict_viewer, "modified_by", "branch_b");
    assert_dict_value(&dict_viewer, "priority", "normal"); // Should persist from base
}

#[test]
fn test_subtree_integration_with_helpers() {
    let (_instance, tree) = setup_tree();

    // Use helpers to set up complex scenario
    let records = create_test_records();
    let table_keys = create_table_operation(&tree, "integration_records", &records);

    let dict_data = &[("config_a", "value_a"), ("config_b", "value_b")];
    create_dict_operation(&tree, "integration_config", dict_data);

    // Verify integration using helpers
    assert_table_search_count(&tree, "integration_records", |r| r.age >= 30, 2);
    assert_dict_viewer_data(&tree, "integration_config", dict_data);
    assert_dict_viewer_count(&tree, "integration_config", 2);

    // Test UUID generation for all table records
    assert_valid_uuids(&table_keys);
}

#[test]
fn test_subtree_helper_functions_integration() {
    let (_instance, tree) = setup_tree();

    // Test Doc helper functions
    let dict_data = &[("key1", "value1"), ("key2", "value2")];
    create_dict_operation(&tree, "helper_dict", dict_data);
    assert_dict_viewer_data(&tree, "helper_dict", dict_data);
    assert_dict_viewer_count(&tree, "helper_dict", 2);

    // Test Table helper functions
    let table_records = &[TestRecord {
        name: "Helper Test User".to_string(),
        age: 25,
        email: "helper@test.com".to_string(),
    }];
    let table_keys = create_table_operation(&tree, "helper_table", table_records);
    assert_eq!(table_keys.len(), 1);
    assert_table_record(&tree, "helper_table", &table_keys[0], &table_records[0]);
    assert_valid_uuids(&table_keys);

    // Test simple Table helper functions
    let simple_values = &[100, 200, 300];
    let simple_keys = create_simple_table_operation(&tree, "helper_simple_table", simple_values);
    assert_eq!(simple_keys.len(), 3);
    assert_valid_uuids(&simple_keys);

    #[cfg(feature = "y-crdt")]
    {
        // Test YDoc helper functions when feature is enabled
        let text_id = create_ydoc_text_operation(&tree, "helper_text", "Helper test content");
        assert!(!text_id.to_string().is_empty());
        assert_ydoc_text_content(&tree, "helper_text", "Helper test content");

        let map_data = &[("helper_name", "test"), ("helper_value", "123")];
        let map_id = create_ydoc_map_operation(&tree, "helper_map", map_data);
        assert!(!map_id.to_string().is_empty());
        assert_ydoc_map_content(&tree, "helper_map", map_data);
    }
}
