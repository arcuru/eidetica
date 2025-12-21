//! Subtree integration tests
//!
//! This module contains tests for complex integration scenarios including
//! concurrent modifications, merging, authentication, and cross-subtree operations.

use eidetica::store::{DocStore, Table};

use super::helpers::*;
use crate::helpers::*;

#[tokio::test]
async fn test_table_complex_data_merging() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to test concurrent modifications
    let (key1, merged_record) =
        test_table_concurrent_modifications(ctx.database(), "merge_test").await;

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
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<TestRecord>>("merge_test")
        .await
        .expect("Failed to get Table viewer");

    let final_record = viewer
        .get(&key1)
        .await
        .expect("Failed to get final merged record");
    assert_eq!(
        final_record, merged_record,
        "Final state should match merged state"
    );
}

#[tokio::test]
async fn test_mixed_subtree_operations() {
    let ctx = TestContext::new().with_database().await;

    // Create operations that use multiple subtree types in one operation
    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");

    let table_key = {
        // Use Table subtree
        let table = op
            .get_store::<Table<TestRecord>>("records")
            .await
            .expect("Failed to get Table");

        let record = TestRecord {
            name: "Mixed Operation User".to_string(),
            age: 30,
            email: "mixed@test.com".to_string(),
        };

        let key = table.insert(record).await.expect("Failed to insert record");

        // Use Doc subtree in same operation
        let dict = op
            .get_store::<DocStore>("config")
            .await
            .expect("Failed to get Doc");
        dict.set("mode", "mixed")
            .await
            .expect("Failed to set config");
        dict.set("version", "1.0")
            .await
            .expect("Failed to set version");

        key
    };

    op.commit().await.expect("Failed to commit mixed operation");

    // Verify both subtrees persist correctly
    let table_viewer = ctx
        .database()
        .get_store_viewer::<Table<TestRecord>>("records")
        .await
        .expect("Failed to get Table viewer");
    let dict_viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("config")
        .await
        .expect("Failed to get Doc viewer");

    // Check Table data
    let record = table_viewer
        .get(&table_key)
        .await
        .expect("Failed to get record");
    assert_eq!(record.name, "Mixed Operation User");

    // Check Doc data
    assert_dict_value(&dict_viewer, "mode", "mixed").await;
    assert_dict_value(&dict_viewer, "version", "1.0").await;
}

#[tokio::test]
async fn test_subtree_persistence_across_operations() {
    let ctx = TestContext::new().with_database().await;

    // Operation 1: Create data in multiple subtrees
    let op1 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Op1: Failed to start");
    let table_key = {
        let table = op1
            .get_store::<Table<SimpleRecord>>("data")
            .await
            .expect("Op1: Failed to get Table");
        let dict = op1
            .get_store::<DocStore>("metadata")
            .await
            .expect("Op1: Failed to get Doc");

        let record = SimpleRecord { value: 100 };
        let key = table.insert(record).await.expect("Op1: Failed to insert");

        dict.set("created_by", "test")
            .await
            .expect("Op1: Failed to set");
        dict.set("timestamp", "2023-01-01")
            .await
            .expect("Op1: Failed to set");

        key
    };
    op1.commit().await.expect("Op1: Failed to commit");

    // Operation 2: Update existing data and add new data
    let op2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Op2: Failed to start");
    {
        let table = op2
            .get_store::<Table<SimpleRecord>>("data")
            .await
            .expect("Op2: Failed to get Table");
        let dict = op2
            .get_store::<DocStore>("metadata")
            .await
            .expect("Op2: Failed to get Doc");

        // Update existing record
        let updated_record = SimpleRecord { value: 200 };
        table
            .set(&table_key, updated_record)
            .await
            .expect("Op2: Failed to update");

        // Add new metadata
        dict.set("updated_by", "test2")
            .await
            .expect("Op2: Failed to set");
        dict.set("version", "2").await.expect("Op2: Failed to set");

        // Verify we can read the original metadata within this operation
        assert_dict_value(&dict, "created_by", "test").await;
        assert_dict_value(&dict, "timestamp", "2023-01-01").await;
    }
    op2.commit().await.expect("Op2: Failed to commit");

    // Verify final state
    let table_viewer = ctx
        .database()
        .get_store_viewer::<Table<SimpleRecord>>("data")
        .await
        .expect("Failed to get Table viewer");
    let dict_viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("metadata")
        .await
        .expect("Failed to get Doc viewer");

    // Check updated record
    let final_record = table_viewer
        .get(&table_key)
        .await
        .expect("Failed to get final record");
    assert_eq!(final_record.value, 200);

    // Check all metadata is preserved
    assert_dict_value(&dict_viewer, "created_by", "test").await;
    assert_dict_value(&dict_viewer, "timestamp", "2023-01-01").await;
    assert_dict_value(&dict_viewer, "updated_by", "test2").await;
    assert_dict_value(&dict_viewer, "version", "2").await;
}

#[tokio::test]
async fn test_subtree_concurrent_access_patterns() {
    let ctx = TestContext::new().with_database().await;

    // Create base entry with both Doc and Table data
    let op_base = ctx
        .database()
        .new_transaction()
        .await
        .expect("Base: Failed to start");
    let base_table_key = {
        let table = op_base
            .get_store::<Table<TestRecord>>("shared_data")
            .await
            .expect("Base: Failed to get Table");
        let dict = op_base
            .get_store::<DocStore>("shared_config")
            .await
            .expect("Base: Failed to get Doc");

        let record = TestRecord {
            name: "Base User".to_string(),
            age: 25,
            email: "base@test.com".to_string(),
        };
        let key = table.insert(record).await.expect("Base: Failed to insert");

        dict.set("status", "active")
            .await
            .expect("Base: Failed to set");
        dict.set("priority", "normal")
            .await
            .expect("Base: Failed to set");

        key
    };
    let base_entry_id = op_base.commit().await.expect("Base: Failed to commit");

    // Branch A: Modify Table data
    let op_branch_a = ctx
        .database()
        .new_transaction_with_tips([base_entry_id.clone()])
        .await
        .expect("Branch A: Failed to start");
    {
        let table = op_branch_a
            .get_store::<Table<TestRecord>>("shared_data")
            .await
            .expect("Branch A: Failed to get Table");

        let updated_record = TestRecord {
            name: "Branch A User".to_string(),
            age: 26,
            email: "branch_a@test.com".to_string(),
        };
        table
            .set(&base_table_key, updated_record)
            .await
            .expect("Branch A: Failed to update");
    }
    op_branch_a
        .commit()
        .await
        .expect("Branch A: Failed to commit");

    // Branch B: Modify Doc data (parallel to Branch A)
    let op_branch_b = ctx
        .database()
        .new_transaction_with_tips([base_entry_id])
        .await
        .expect("Branch B: Failed to start");
    {
        let dict = op_branch_b
            .get_store::<DocStore>("shared_config")
            .await
            .expect("Branch B: Failed to get Doc");

        dict.set("status", "modified")
            .await
            .expect("Branch B: Failed to set");
        dict.set("modified_by", "branch_b")
            .await
            .expect("Branch B: Failed to set");
    }
    op_branch_b
        .commit()
        .await
        .expect("Branch B: Failed to commit");

    // Merge operation: Read the merged state
    let op_merge = ctx
        .database()
        .new_transaction()
        .await
        .expect("Merge: Failed to start");
    let (merged_record, merged_status) = {
        let table = op_merge
            .get_store::<Table<TestRecord>>("shared_data")
            .await
            .expect("Merge: Failed to get Table");
        let dict = op_merge
            .get_store::<DocStore>("shared_config")
            .await
            .expect("Merge: Failed to get Doc");

        let record = table
            .get(&base_table_key)
            .await
            .expect("Merge: Failed to get record");
        let status = dict
            .get_string("status")
            .await
            .expect("Merge: Failed to get status");

        (record, status)
    };
    op_merge.commit().await.expect("Merge: Failed to commit");

    // Verify the merge results
    // Table should have Branch A's changes
    assert_eq!(merged_record.name, "Branch A User");
    assert_eq!(merged_record.age, 26);

    // Doc should have Branch B's changes
    assert_eq!(merged_status, "modified");

    // Verify final persistence
    let table_viewer = ctx
        .database()
        .get_store_viewer::<Table<TestRecord>>("shared_data")
        .await
        .expect("Failed to get Table viewer");
    let dict_viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("shared_config")
        .await
        .expect("Failed to get Doc viewer");

    let final_record = table_viewer
        .get(&base_table_key)
        .await
        .expect("Failed to get final record");
    assert_eq!(final_record, merged_record);

    assert_dict_value(&dict_viewer, "status", "modified").await;
    assert_dict_value(&dict_viewer, "modified_by", "branch_b").await;
    assert_dict_value(&dict_viewer, "priority", "normal").await; // Should persist from base
}

#[tokio::test]
async fn test_subtree_integration_with_helpers() {
    let ctx = TestContext::new().with_database().await;

    // Use helpers to set up complex scenario
    let records = create_test_records();
    let table_keys = create_table_operation(ctx.database(), "integration_records", &records).await;

    let dict_data = &[("config_a", "value_a"), ("config_b", "value_b")];
    create_dict_operation(ctx.database(), "integration_config", dict_data).await;

    // Verify integration using helpers
    assert_table_search_count(ctx.database(), "integration_records", |r| r.age >= 30, 2).await;
    assert_dict_viewer_data(ctx.database(), "integration_config", dict_data).await;
    assert_dict_viewer_count(ctx.database(), "integration_config", 2).await;

    // Test UUID generation for all table records
    assert_valid_uuids(&table_keys);
}

#[tokio::test]
async fn test_subtree_helper_functions_integration() {
    let ctx = TestContext::new().with_database().await;

    // Test Doc helper functions
    let dict_data = &[("key1", "value1"), ("key2", "value2")];
    create_dict_operation(ctx.database(), "helper_dict", dict_data).await;
    assert_dict_viewer_data(ctx.database(), "helper_dict", dict_data).await;
    assert_dict_viewer_count(ctx.database(), "helper_dict", 2).await;

    // Test Table helper functions
    let table_records = &[TestRecord {
        name: "Helper Test User".to_string(),
        age: 25,
        email: "helper@test.com".to_string(),
    }];
    let table_keys = create_table_operation(ctx.database(), "helper_table", table_records).await;
    assert_eq!(table_keys.len(), 1);
    assert_table_record(
        ctx.database(),
        "helper_table",
        &table_keys[0],
        &table_records[0],
    )
    .await;
    assert_valid_uuids(&table_keys);

    // Test simple Table helper functions
    let simple_values = &[100, 200, 300];
    let simple_keys =
        create_simple_table_operation(ctx.database(), "helper_simple_table", simple_values).await;
    assert_eq!(simple_keys.len(), 3);
    assert_valid_uuids(&simple_keys);

    #[cfg(feature = "y-crdt")]
    {
        // Test YDoc helper functions when feature is enabled
        let text_id =
            create_ydoc_text_operation(ctx.database(), "helper_text", "Helper test content").await;
        assert!(!text_id.to_string().is_empty());
        assert_ydoc_text_content(ctx.database(), "helper_text", "Helper test content").await;

        let map_data = &[("helper_name", "test"), ("helper_value", "123")];
        let map_id = create_ydoc_map_operation(ctx.database(), "helper_map", map_data).await;
        assert!(!map_id.to_string().is_empty());
        assert_ydoc_map_content(ctx.database(), "helper_map", map_data).await;
    }
}
