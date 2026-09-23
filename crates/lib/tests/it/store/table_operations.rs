//! Table subtree operation tests
//!
//! This module contains tests for Table subtree functionality including
//! CRUD operations, search functionality, UUID generation, and multiple operations.

use eidetica::store::Table;
use eidetica::{
    Snapshot, Store,
    crdt::{CanonicalJson, LwwMap},
};

use super::helpers::*;
use crate::helpers::*;

#[tokio::test]
async fn test_table_entry_delta_has_inline_canonical_json_and_tombstone() {
    let ctx = TestContext::new().with_database().await;
    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<serde_json::Value>>("wire_rows")
        .await
        .unwrap();
    table
        .set("a.b", serde_json::json!({"z": 2, "a": 1}))
        .await
        .unwrap();
    table.set("", serde_json::json!([true])).await.unwrap();
    let id = tx.commit().await.unwrap();
    let entry = ctx.database().backend().unwrap().get(&id).await.unwrap();
    let bytes = entry.data("wire_rows").unwrap();
    assert_eq!(
        std::str::from_utf8(bytes).unwrap(),
        r#"[["",{"set":[true]}],["a.b",{"set":{"a":1,"z":2}}]]"#
    );
    let delta: LwwMap<String, CanonicalJson> = serde_json::from_slice(bytes).unwrap();
    assert!(
        serde_json::from_slice::<LwwMap<String, CanonicalJson>>(br#"{"a.b":"old Doc row"}"#)
            .is_err()
    );
    assert_eq!(
        delta.get(&"a.b".to_string()).unwrap().as_bytes(),
        br#"{"a":1,"z":2}"#
    );
    assert_eq!(
        Table::<serde_json::Value>::state_model().descriptor().name,
        "eidetica/table/rows/canonical-json:v0"
    );

    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<serde_json::Value>>("wire_rows")
        .await
        .unwrap();
    assert!(table.delete("a.b").await.unwrap());
    let id = tx.commit().await.unwrap();
    let entry = ctx.database().backend().unwrap().get(&id).await.unwrap();
    assert_eq!(
        std::str::from_utf8(entry.data("wire_rows").unwrap()).unwrap(),
        r#"[["a.b","delete"]]"#
    );
}

#[tokio::test]
async fn test_table_delete_does_not_swallow_typed_decode_failure() {
    let ctx = TestContext::new().with_database().await;
    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<serde_json::Value>>("typed_delete")
        .await
        .unwrap();
    table
        .set("row", serde_json::json!({"not_a_value": true}))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<SimpleRecord>>("typed_delete")
        .await
        .unwrap();
    assert!(matches!(table.delete("row").await,
        Err(eidetica::Error::Store(error)) if matches!(*error, eidetica::store::StoreError::DeserializationFailed { .. })));
    assert!(table.scan_page(None, 5).await.is_err());
}

#[tokio::test]
async fn test_table_basic_crud_operations() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create initial record
    let initial_record = TestRecord {
        name: "John Doe".to_string(),
        age: 30,
        email: "john@example.com".to_string(),
    };
    let keys = create_table_operation(
        ctx.database(),
        "test_records",
        std::slice::from_ref(&initial_record),
    )
    .await;
    let primary_key = &keys[0];

    // Test CRUD operations within a transaction
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let table = txn
        .get_store::<Table<TestRecord>>("test_records")
        .await
        .expect("Failed to get Table");

    // Test get (should see existing record)
    let retrieved = table
        .get(primary_key)
        .await
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
        .await
        .expect("Failed to update record");

    // Verify update within same operation
    let retrieved_updated = table
        .get(primary_key)
        .await
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
        .await
        .expect("Failed to insert new record");
    assert!(!new_pk.is_empty(), "New primary key should not be empty");

    // Verify new record retrieval
    let retrieved_new = table.get(&new_pk).await.expect("Failed to get new record");
    assert_eq!(retrieved_new, new_record);

    txn.commit().await.expect("Failed to commit transaction");

    // Verify persistence using helper
    assert_table_record(ctx.database(), "test_records", primary_key, &updated_record).await;
    assert_table_record(ctx.database(), "test_records", &new_pk, &new_record).await;
}

#[tokio::test]
async fn test_table_load_is_lazy_and_projection_is_row_addressable() {
    if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
        return;
    }
    let ctx = TestContext::new().with_database().await;
    let engine = ctx.database().backend().unwrap().local_engine().unwrap();
    let memory = engine
        .as_any()
        .downcast_ref::<eidetica::backend::database::InMemory>()
        .unwrap();
    let txn = ctx.database().new_transaction().await.unwrap();
    let before_load = memory.store_state_read_counts();
    let table = txn
        .get_store::<Table<SimpleRecord>>("lazy_rows")
        .await
        .unwrap();
    assert_eq!(memory.store_state_read_counts(), before_load);
    assert_eq!(
        memory.store_state_record_count(ctx.database().root_id(), "lazy_rows"),
        0
    );

    table.set("b", SimpleRecord { value: 2 }).await.unwrap();
    table.set("a", SimpleRecord { value: 1 }).await.unwrap();
    assert_eq!(table.get("a").await.unwrap().value, 1);
    assert_eq!(
        table
            .scan_page(None, 1)
            .await
            .unwrap()
            .rows
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
    txn.commit().await.unwrap();

    let before_load = memory.store_state_read_counts();
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<SimpleRecord>>("lazy_rows")
        .await
        .unwrap();
    assert_eq!(memory.store_state_read_counts(), before_load);
    assert_eq!(
        memory.store_state_record_count(ctx.database().root_id(), "lazy_rows"),
        0
    );
    let first = viewer.scan_page(None, 1).await.unwrap();
    assert_eq!(first.rows[0].0, "a");
    let second = viewer.scan_page(first.next.as_ref(), 1).await.unwrap();
    assert_eq!(second.rows[0].0, "b");
    assert!(second.next.is_none());
    assert_eq!(
        memory.store_state_record_count(ctx.database().root_id(), "lazy_rows"),
        2
    );
    let after_scan = memory.store_state_read_counts();
    assert_eq!(after_scan.0, before_load.0);
    assert!(after_scan.1 > before_load.1);
    assert!(after_scan.1 <= before_load.1 + 4);
    assert_eq!(viewer.get("a").await.unwrap().value, 1);
    let after_get = memory.store_state_read_counts();
    assert_eq!(after_get, (after_scan.0 + 1, after_scan.1));

    ctx.database()
        .backend()
        .unwrap()
        .clear_derived_store_state()
        .await
        .unwrap();
    assert_eq!(viewer.get("b").await.unwrap().value, 2);
}

#[tokio::test]
async fn test_table_scan_merges_changes_across_page_boundaries() {
    let ctx = TestContext::new().with_database().await;
    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<SimpleRecord>>("paged_rows")
        .await
        .unwrap();
    for (key, value) in [("a", 1), ("c", 3), ("e", 5)] {
        table.set(key, SimpleRecord { value }).await.unwrap();
    }
    tx.commit().await.unwrap();

    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<SimpleRecord>>("paged_rows")
        .await
        .unwrap();
    table.set("b", SimpleRecord { value: 2 }).await.unwrap();
    table.set("d", SimpleRecord { value: 4 }).await.unwrap();
    assert!(table.delete("c").await.unwrap());

    let empty = table.scan_page(None, 0).await.unwrap();
    assert!(empty.rows.is_empty());
    assert!(empty.next.is_none());

    let first = table.scan_page(None, 2).await.unwrap();
    assert_eq!(
        first
            .rows
            .iter()
            .map(|row| row.0.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    let second = table.scan_page(first.next.as_ref(), 2).await.unwrap();
    assert_eq!(
        second
            .rows
            .iter()
            .map(|row| row.0.as_str())
            .collect::<Vec<_>>(),
        ["d", "e"]
    );
    assert!(second.next.is_none());
}

#[tokio::test]
async fn test_table_dotted_primary_keys_survive_commits() {
    let ctx = TestContext::new().with_database().await;
    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<SimpleRecord>>("dotted_primary_keys")
        .await
        .unwrap();
    table.set("a.b", SimpleRecord { value: 1 }).await.unwrap();
    table.set("a.c", SimpleRecord { value: 2 }).await.unwrap();
    table.set("z", SimpleRecord { value: 3 }).await.unwrap();
    tx.commit().await.unwrap();

    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<SimpleRecord>>("dotted_primary_keys")
        .await
        .unwrap();
    assert_eq!(table.get("a.b").await.unwrap().value, 1);
    let first = table.scan_page(None, 2).await.unwrap();
    assert_eq!(
        first.rows,
        [
            ("a.b".to_string(), SimpleRecord { value: 1 }),
            ("a.c".to_string(), SimpleRecord { value: 2 }),
        ]
    );
    let second = table.scan_page(first.next.as_ref(), 2).await.unwrap();
    assert_eq!(second.rows, [("z".to_string(), SimpleRecord { value: 3 })]);
    assert!(second.next.is_none());

    assert!(table.delete("a.b").await.unwrap());
    tx.commit().await.unwrap();

    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<SimpleRecord>>("dotted_primary_keys")
        .await
        .unwrap();
    assert!(table.get("a.b").await.is_err());
    table.set("a.b", SimpleRecord { value: 4 }).await.unwrap();
    tx.commit().await.unwrap();

    let viewer = ctx
        .database()
        .get_store_viewer::<Table<SimpleRecord>>("dotted_primary_keys")
        .await
        .unwrap();
    assert_eq!(viewer.get("a.b").await.unwrap().value, 4);
    assert_eq!(viewer.get("a.c").await.unwrap().value, 2);
}

#[tokio::test]
async fn test_table_exact_keys_multi_operation_and_cold_warm_reads() {
    let ctx = TestContext::new().with_database().await;
    let keys = [
        "", ".", "...", "a", "a.b", "a..b", "a/b", "é", "e\u{301}", "🚀",
    ];
    let tx = ctx.database().new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<SimpleRecord>>("exact_keys")
        .await
        .unwrap();
    for (index, key) in keys.iter().enumerate() {
        table
            .set(
                key,
                SimpleRecord {
                    value: index as i32,
                },
            )
            .await
            .unwrap();
    }
    table.set("a", SimpleRecord { value: 42 }).await.unwrap();
    assert!(table.delete("a.b").await.unwrap());
    table.set("a.b", SimpleRecord { value: 99 }).await.unwrap();
    assert!(table.delete("...").await.unwrap());
    assert!(!table.delete("missing").await.unwrap());
    assert_eq!(table.get("a").await.unwrap().value, 42);
    let id = tx.commit().await.unwrap();
    let entry = ctx.database().backend().unwrap().get(&id).await.unwrap();
    let delta: LwwMap<String, CanonicalJson> =
        serde_json::from_slice(entry.data("exact_keys").unwrap()).unwrap();
    assert_eq!(delta.operations().count(), keys.len());
    assert_eq!(
        delta.get(&"a".to_string()).unwrap().as_bytes(),
        br#"{"value":42}"#
    );
    assert_eq!(
        delta.get(&"a.b".to_string()).unwrap().as_bytes(),
        br#"{"value":99}"#
    );
    assert!(matches!(
        delta.operation(&"...".to_string()),
        Some(eidetica::crdt::Lww::Delete)
    ));

    for cold in [true, false] {
        if cold {
            ctx.database()
                .backend()
                .unwrap()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        let viewer = ctx
            .database()
            .get_store_viewer::<Table<SimpleRecord>>("exact_keys")
            .await
            .unwrap();
        for (index, key) in keys.iter().enumerate() {
            if *key == "..." {
                assert!(viewer.get(key).await.is_err());
            } else {
                let expected = if *key == "a" {
                    42
                } else if *key == "a.b" {
                    99
                } else {
                    index as i32
                };
                assert_eq!(viewer.get(key).await.unwrap().value, expected, "{key:?}");
            }
        }
        let mut rows = Vec::new();
        let mut cursor = None;
        loop {
            let page = viewer.scan_page(cursor.as_ref(), 2).await.unwrap();
            rows.extend(page.rows.into_iter().map(|(key, row)| (key, row.value)));
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        let mut expected = keys
            .iter()
            .enumerate()
            .filter(|(_, key)| **key != "...")
            .map(|(index, key)| {
                (
                    (*key).to_string(),
                    if *key == "a" {
                        42
                    } else if *key == "a.b" {
                        99
                    } else {
                        index as i32
                    },
                )
            })
            .collect::<Vec<_>>();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(rows, expected);
        assert_eq!(viewer.search(|row| row.value == 99).await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn test_table_zero_limit_scan_is_empty_and_terminal() {
    let ctx = TestContext::new().with_database().await;
    let txn = ctx.database().new_transaction().await.unwrap();
    let table = txn
        .get_store::<Table<SimpleRecord>>("zero_limit")
        .await
        .unwrap();
    table.set("row", SimpleRecord { value: 1 }).await.unwrap();

    let page = table.scan_page(None, 0).await.unwrap();
    assert!(page.rows.is_empty());
    assert!(page.next.is_none());
}

#[tokio::test]
async fn test_table_multiple_records() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create multiple records
    let values = &[10, 20, 30, 40, 50];
    let inserted_keys =
        create_simple_table_operation(ctx.database(), "simple_records", values).await;

    // Verify all records persist after commit
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<SimpleRecord>>("simple_records")
        .await
        .expect("Failed to get Table viewer");

    for (i, key) in inserted_keys.iter().enumerate() {
        let record = viewer
            .get(key)
            .await
            .expect("Failed to get record after commit");
        assert_eq!(record.value, values[i]);
    }
}

#[tokio::test]
async fn test_table_search_functionality() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create test records
    let records = create_test_records();
    create_table_operation(ctx.database(), "search_records", &records).await;

    // Test search by age using helper
    assert_table_search_count(
        ctx.database(),
        "search_records",
        |record| record.age == 25,
        2,
    )
    .await;

    // Test search by email domain using helper
    assert_table_search_count(
        ctx.database(),
        "search_records",
        |record| record.email.contains("example.com"),
        2,
    )
    .await;

    // Test search by name prefix using helper
    assert_table_search_count(
        ctx.database(),
        "search_records",
        |record| record.name.starts_with('B'),
        1,
    )
    .await;

    // Test search with no matches using helper
    assert_table_search_count(
        ctx.database(),
        "search_records",
        |record| record.age > 100,
        0,
    )
    .await;

    // Test search after commit with detailed verification
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<TestRecord>>("search_records")
        .await
        .expect("Failed to get Table viewer");

    let age_30_results = viewer
        .search(|record| record.age == 30)
        .await
        .expect("Failed to search after commit");
    assert_eq!(age_30_results.len(), 1);
    assert_eq!(age_30_results[0].1.name, "Bob Smith");
}

#[tokio::test]
async fn test_table_uuid_generation() {
    let ctx = TestContext::new().with_database().await;

    // Generate 100 records to test UUID uniqueness
    let values: Vec<i32> = (1..=100).collect();
    let generated_keys = create_simple_table_operation(ctx.database(), "uuid_test", &values).await;

    // Use helper to verify UUID format and uniqueness
    assert_valid_uuids(&generated_keys);

    // Verify all records are retrievable with their unique keys
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<SimpleRecord>>("uuid_test")
        .await
        .expect("Failed to get Table viewer");

    for key in &generated_keys {
        let record = viewer.get(key).await.expect("Failed to get record by UUID");
        assert!(record.value >= 1 && record.value <= 100);
    }
}

#[tokio::test]
async fn test_table_multiple_operations() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to test multi-operation workflow
    let (key1, key2, key3) = test_table_multi_operations(ctx.database(), "multi_op_test").await;

    // Verify final state
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<TestRecord>>("multi_op_test")
        .await
        .expect("Failed to get Table viewer");

    // Check updated record
    let final_record1 = viewer
        .get(&key1)
        .await
        .expect("Failed to get final record1");
    assert_eq!(final_record1.name, "Updated User 1");
    assert_eq!(final_record1.age, 21);
    assert_eq!(final_record1.email, "user1@updated.com");

    // Check unchanged record
    let final_record2 = viewer
        .get(&key2)
        .await
        .expect("Failed to get final record2");
    assert_eq!(final_record2.name, "Initial User 2");
    assert_eq!(final_record2.age, 25);
    assert_eq!(final_record2.email, "user2@initial.com");

    // Check new record
    let final_record3 = viewer
        .get(&key3)
        .await
        .expect("Failed to get final record3");
    assert_eq!(final_record3.name, "New User 3");
    assert_eq!(final_record3.age, 30);
    assert_eq!(final_record3.email, "user3@new.com");

    // Verify search across all records
    let all_records = viewer
        .search(|_| true)
        .await
        .expect("Failed to search all records");
    assert_eq!(all_records.len(), 3);
}

#[tokio::test]
async fn test_table_empty_search() {
    let ctx = TestContext::new().with_database().await;
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");

    {
        let table = txn
            .get_store::<Table<SimpleRecord>>("empty_search_test")
            .await
            .expect("Failed to get Table");

        // Search in empty store
        let results = table
            .search(|_| true)
            .await
            .expect("Failed to search empty store");
        assert_eq!(results.len(), 0);
    }

    txn.commit().await.expect("Failed to commit transaction");

    // Search in empty store after commit
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<SimpleRecord>>("empty_search_test")
        .await
        .expect("Failed to get Table viewer");

    let results = viewer
        .search(|_| true)
        .await
        .expect("Failed to search empty store after commit");
    assert_eq!(results.len(), 0);
}

#[tokio::test]
async fn test_empty_table_behavior() {
    let ctx = TestContext::new().with_database().await;

    // Test empty Table behavior
    let table_viewer = ctx
        .database()
        .get_store_viewer::<Table<TestRecord>>("empty_table")
        .await
        .expect("Failed to get empty Table viewer");

    let empty_search = table_viewer
        .search(|_| true)
        .await
        .expect("Failed to search empty table");
    assert_eq!(empty_search.len(), 0);
}

#[tokio::test]
async fn test_table_delete_basic() {
    let ctx = TestContext::new().with_database().await;

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
    let keys = create_table_operation(ctx.database(), "delete_test", &initial_records).await;

    // Delete one record within a transaction
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    {
        let table = txn
            .get_store::<Table<TestRecord>>("delete_test")
            .await
            .expect("Failed to get Table");

        // Delete existing record
        let deleted = table
            .delete(&keys[1])
            .await
            .expect("Failed to delete existing record");
        assert!(deleted, "Should return true when deleting existing record");

        // Verify deletion within same operation
        assert!(
            table.get(&keys[1]).await.is_err(),
            "Deleted record should not be retrievable"
        );

        // Verify other records still exist
        let record1 = table
            .get(&keys[0])
            .await
            .expect("Record 1 should still exist");
        assert_eq!(record1.name, "User 1");

        let record3 = table
            .get(&keys[2])
            .await
            .expect("Record 3 should still exist");
        assert_eq!(record3.name, "User 3");
    }
    txn.commit().await.expect("Failed to commit transaction");

    // Verify deletion persisted using helper
    assert_table_record_deleted(ctx.database(), "delete_test", &keys[1]).await;

    // Verify other records still exist
    assert_table_record(ctx.database(), "delete_test", &keys[0], &initial_records[0]).await;
    assert_table_record(ctx.database(), "delete_test", &keys[2], &initial_records[2]).await;
}

#[tokio::test]
async fn test_table_delete_nonexistent() {
    let ctx = TestContext::new().with_database().await;

    // Create one record
    let record = TestRecord {
        name: "Existing User".to_string(),
        age: 30,
        email: "existing@test.com".to_string(),
    };
    let keys = create_table_operation(
        ctx.database(),
        "delete_nonexistent",
        std::slice::from_ref(&record),
    )
    .await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    {
        let table = txn
            .get_store::<Table<TestRecord>>("delete_nonexistent")
            .await
            .expect("Failed to get Table");

        // Try to delete non-existent key
        let deleted = table
            .delete("non-existent-uuid")
            .await
            .expect("Delete should not error on non-existent key");
        assert!(
            !deleted,
            "Should return false when deleting non-existent record"
        );

        // Verify existing record is still there
        let existing = table
            .get(&keys[0])
            .await
            .expect("Existing record should remain");
        assert_eq!(existing.name, "Existing User");
    }
    txn.commit().await.expect("Failed to commit transaction");

    // Verify existing record persisted
    assert_table_record(ctx.database(), "delete_nonexistent", &keys[0], &record).await;
}

#[tokio::test]
async fn test_table_delete_and_reinsert() {
    let ctx = TestContext::new().with_database().await;

    // Create initial record
    let initial_record = TestRecord {
        name: "Original User".to_string(),
        age: 25,
        email: "original@test.com".to_string(),
    };
    let keys = create_table_operation(
        ctx.database(),
        "delete_reinsert",
        std::slice::from_ref(&initial_record),
    )
    .await;
    let original_key = &keys[0];

    // Delete the record
    let txn1 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    {
        let table = txn1
            .get_store::<Table<TestRecord>>("delete_reinsert")
            .await
            .expect("Failed to get Table");

        table
            .delete(original_key)
            .await
            .expect("Failed to delete record");
    }
    txn1.commit().await.expect("Failed to commit deletion");

    // Verify deletion
    assert_table_record_deleted(ctx.database(), "delete_reinsert", original_key).await;

    // Re-insert with the same key
    let txn2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    {
        let table = txn2
            .get_store::<Table<TestRecord>>("delete_reinsert")
            .await
            .expect("Failed to get Table");

        let new_record = TestRecord {
            name: "New User".to_string(),
            age: 30,
            email: "new@test.com".to_string(),
        };

        table
            .set(original_key, new_record.clone())
            .await
            .expect("Failed to re-insert record");

        // Verify re-inserted record is retrievable
        let retrieved = table
            .get(original_key)
            .await
            .expect("Re-inserted record should be retrievable");
        assert_eq!(retrieved, new_record);
    }
    txn2.commit().await.expect("Failed to commit re-insertion");

    // Verify new record persisted with same key
    let new_record = TestRecord {
        name: "New User".to_string(),
        age: 30,
        email: "new@test.com".to_string(),
    };
    assert_table_record(ctx.database(), "delete_reinsert", original_key, &new_record).await;
}

#[tokio::test]
async fn test_table_search_after_delete() {
    let ctx = TestContext::new().with_database().await;

    // Create test records using helper
    let records = create_test_records();
    let keys = create_table_operation(ctx.database(), "search_after_delete", &records).await;

    // Verify initial search count
    assert_table_search_count(
        ctx.database(),
        "search_after_delete",
        |record| record.age == 25,
        2,
    )
    .await;

    // Delete one of the age=25 records
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    {
        let table = txn
            .get_store::<Table<TestRecord>>("search_after_delete")
            .await
            .expect("Failed to get Table");

        table
            .delete(&keys[0])
            .await
            .expect("Failed to delete record");
    }
    txn.commit().await.expect("Failed to commit deletion");

    // Verify search count decreased
    assert_table_search_count(
        ctx.database(),
        "search_after_delete",
        |record| record.age == 25,
        1,
    )
    .await;

    // Verify the remaining age=25 record is the correct one
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<TestRecord>>("search_after_delete")
        .await
        .expect("Failed to get Table viewer");

    let age_25_results = viewer
        .search(|record| record.age == 25)
        .await
        .expect("Failed to search after delete");
    assert_eq!(age_25_results.len(), 1);
    assert_eq!(age_25_results[0].1.name, "Charlie Brown");
}

#[tokio::test]
async fn test_table_delete_multiple() {
    let ctx = TestContext::new().with_database().await;

    // Create multiple records
    let values = &[10, 20, 30, 40, 50];
    let keys = create_simple_table_operation(ctx.database(), "delete_multiple", values).await;

    // Delete multiple records in one transaction
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    {
        let table = txn
            .get_store::<Table<SimpleRecord>>("delete_multiple")
            .await
            .expect("Failed to get Table");

        // Delete records at indices 1 and 3
        let deleted1 = table
            .delete(&keys[1])
            .await
            .expect("Failed to delete record 1");
        let deleted3 = table
            .delete(&keys[3])
            .await
            .expect("Failed to delete record 3");

        assert!(deleted1);
        assert!(deleted3);

        // Verify deletions
        assert!(table.get(&keys[1]).await.is_err());
        assert!(table.get(&keys[3]).await.is_err());

        // Verify remaining records
        assert_eq!(
            table.get(&keys[0]).await.expect("Record 0 exists").value,
            10
        );
        assert_eq!(
            table.get(&keys[2]).await.expect("Record 2 exists").value,
            30
        );
        assert_eq!(
            table.get(&keys[4]).await.expect("Record 4 exists").value,
            50
        );
    }
    txn.commit().await.expect("Failed to commit deletions");

    // Verify search returns only non-deleted records
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<SimpleRecord>>("delete_multiple")
        .await
        .expect("Failed to get Table viewer");

    let all_records = viewer
        .search(|_| true)
        .await
        .expect("Failed to search all records");
    assert_eq!(all_records.len(), 3);

    // Verify correct records remain
    let values: Vec<i32> = all_records.iter().map(|(_, r)| r.value).collect();
    assert!(values.contains(&10));
    assert!(values.contains(&30));
    assert!(values.contains(&50));
}

#[tokio::test]
async fn test_table_delete_concurrent_modifications() {
    let ctx = TestContext::new().with_database().await;

    // Create base record
    let txn_base = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let key1 = {
        let table = txn_base
            .get_store::<Table<TestRecord>>("concurrent_delete")
            .await
            .expect("Failed to get Table");
        let record = TestRecord {
            name: "Base User".to_string(),
            age: 25,
            email: "base@test.com".to_string(),
        };
        table
            .insert(record)
            .await
            .expect("Failed to insert base record")
    };
    let base_entry_id = txn_base.commit().await.expect("Failed to commit base");

    // Branch A: Delete the record
    let op_branch_a = ctx
        .database()
        .new_transaction_at(&Snapshot::from([base_entry_id.clone()]))
        .await
        .expect("Failed to start branch A");
    {
        let table = op_branch_a
            .get_store::<Table<TestRecord>>("concurrent_delete")
            .await
            .expect("Failed to get Table");

        table
            .delete(&key1)
            .await
            .expect("Failed to delete in branch A");
    }
    op_branch_a
        .commit()
        .await
        .expect("Failed to commit branch A deletion");

    // Branch B: Update the same record
    let op_branch_b = ctx
        .database()
        .new_transaction_at(&Snapshot::from([base_entry_id]))
        .await
        .expect("Failed to start branch B");
    {
        let table = op_branch_b
            .get_store::<Table<TestRecord>>("concurrent_delete")
            .await
            .expect("Failed to get Table");

        let updated_record = TestRecord {
            name: "Updated User".to_string(),
            age: 30,
            email: "updated@test.com".to_string(),
        };
        table
            .set(&key1, updated_record)
            .await
            .expect("Failed to update in branch B");
    }
    op_branch_b
        .commit()
        .await
        .expect("Failed to commit branch B update");

    // Get merged result - CRDT last-write-wins should apply
    // The result depends on CRDT merge semantics
    let viewer = ctx
        .database()
        .get_store_viewer::<Table<TestRecord>>("concurrent_delete")
        .await
        .expect("Failed to get Table viewer");

    // After CRDT merge, one operation will win
    // We just verify the system doesn't crash and produces a deterministic result
    let result = viewer.get(&key1).await;

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
