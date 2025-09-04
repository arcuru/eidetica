//! YDoc subtree operation tests
//!
//! This module contains tests for Y-CRDT subtree functionality including
//! text operations, map operations, incremental updates, and external updates.

#[cfg(feature = "y-crdt")]
use super::helpers::*;
#[cfg(feature = "y-crdt")]
use crate::helpers::*;
#[cfg(feature = "y-crdt")]
use eidetica::store::YDoc;
#[cfg(feature = "y-crdt")]
use yrs::{GetString, Map as YrsMapTrait, Text, Transact};

#[cfg(feature = "y-crdt")]
#[test]
fn test_ydoc_basic_text_operations() {
    let tree = setup_tree();

    // Use helper to create YDoc with text
    create_ydoc_text_operation(&tree, "yrs_text", "Hello, World!");

    // Verify using helper
    assert_ydoc_text_content(&tree, "yrs_text", "Hello, World!");
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_ydoc_incremental_updates_save_diffs_only() {
    let tree = setup_tree();

    // Use helper to test incremental updates
    let (first_diff_size, second_diff_size) = test_ydoc_incremental_updates(&tree, "yrs_diff_test");

    // Print the actual diff sizes for verification
    println!("First diff size: {first_diff_size}, Second diff size: {second_diff_size}");

    // Assert that the second diff is significantly smaller than the first
    // The first diff contains ~10KB of content, the second should be just a few bytes
    assert!(
        second_diff_size < first_diff_size / 10,
        "Second diff size ({second_diff_size}) should be much smaller than first diff size ({first_diff_size})"
    );

    // The second diff should be smaller than 200 bytes for such a small change
    assert!(
        second_diff_size < 200,
        "Second diff size ({second_diff_size}) should be much smaller for just adding a few characters"
    );

    // Verify final content contains the change
    let viewer = tree
        .get_store_viewer::<YDoc>("yrs_diff_test")
        .expect("Failed to get YDoc viewer");

    viewer
        .with_doc(|doc| {
            let text = doc.get_or_insert_text("document");
            let txn = doc.transact();
            let content = text.get_string(&txn);

            // Verify the small change was inserted at the correct position
            assert!(
                content.contains(" SMALL_CHANGE"),
                "Content should contain the inserted text"
            );

            // Verify the content is still large (confirming we didn't lose the original)
            assert!(
                content.len() > 10000,
                "Content should still be large after the small change"
            );

            Ok(())
        })
        .expect("Failed to verify final text content");
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_ydoc_map_operations() {
    let tree = setup_tree();

    // Use helper to create YDoc with map data
    let map_data = &[("key1", "value1"), ("key2", "42"), ("key3", "true")];
    create_ydoc_map_operation(&tree, "yrs_map", map_data);

    // Verify using helper
    assert_ydoc_map_content(&tree, "yrs_map", map_data);
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_ydoc_multiple_operations_with_diffs() {
    let tree = setup_tree();

    // Operation 1: Create initial state
    let op1 = tree.new_operation().expect("Op1: Failed to start");
    {
        let ydoc = op1
            .get_store::<YDoc>("yrs_multi")
            .expect("Op1: Failed to get YDoc");

        ydoc.with_doc_mut(|doc| {
            let map = doc.get_or_insert_map("data");
            let text = doc.get_or_insert_text("notes");

            let mut txn = doc.transact_mut();
            map.insert(&mut txn, "version", 1);
            text.insert(&mut txn, 0, "Version 1 notes");
            Ok(())
        })
        .expect("Op1: Failed to perform operations");
    }
    op1.commit().expect("Op1: Failed to commit");

    // Operation 2: Update existing data
    let op2 = tree.new_operation().expect("Op2: Failed to start");
    {
        let ydoc = op2
            .get_store::<YDoc>("yrs_multi")
            .expect("Op2: Failed to get YDoc");

        ydoc.with_doc_mut(|doc| {
            let map = doc.get_or_insert_map("data");
            let text = doc.get_or_insert_text("notes");

            let mut txn = doc.transact_mut();
            map.insert(&mut txn, "version", 2);
            map.insert(&mut txn, "author", "test_user");
            let text_len = text.len(&txn);
            text.insert(&mut txn, text_len, " - Updated in v2");
            Ok(())
        })
        .expect("Op2: Failed to perform operations");
    }
    op2.commit().expect("Op2: Failed to commit");

    // Operation 3: Add more data
    let op3 = tree.new_operation().expect("Op3: Failed to start");
    {
        let ydoc = op3
            .get_store::<YDoc>("yrs_multi")
            .expect("Op3: Failed to get YDoc");

        ydoc.with_doc_mut(|doc| {
            let map = doc.get_or_insert_map("data");

            let mut txn = doc.transact_mut();
            map.insert(&mut txn, "features", vec!["diff_saving", "crdt_support"]);
            Ok(())
        })
        .expect("Op3: Failed to perform operations");
    }
    op3.commit().expect("Op3: Failed to commit");

    // Verify final state
    let viewer = tree
        .get_store_viewer::<YDoc>("yrs_multi")
        .expect("Failed to get YDoc viewer");

    viewer
        .with_doc(|doc| {
            let map = doc.get_or_insert_map("data");
            let text = doc.get_or_insert_text("notes");
            let txn = doc.transact();

            // Check map values
            let version = map.get(&txn, "version").expect("version should exist");
            assert_eq!(version.to_string(&txn), "2");

            let author = map.get(&txn, "author").expect("author should exist");
            assert_eq!(author.to_string(&txn), "test_user");

            // Check text content
            let notes_content = text.get_string(&txn);
            assert_eq!(notes_content, "Version 1 notes - Updated in v2");

            // Check features
            let features = map.get(&txn, "features").expect("features should exist");
            let features_string = features.to_string(&txn);
            assert!(features_string.contains("diff_saving"));
            assert!(features_string.contains("crdt_support"));

            Ok(())
        })
        .expect("Failed to verify final state");
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_ydoc_apply_external_update() {
    let tree = setup_tree();

    // Create external update using helper
    let external_update = create_external_ydoc_update("External change");

    // Apply the external update to our YDoc
    let op = tree.new_operation().expect("Failed to start operation");
    {
        let ydoc = op
            .get_store::<YDoc>("yrs_external")
            .expect("Failed to get YDoc");

        ydoc.apply_update(&external_update)
            .expect("Failed to apply external update");
    }
    op.commit().expect("Failed to commit operation");

    // Verify the external update was applied
    let viewer = tree
        .get_store_viewer::<YDoc>("yrs_external")
        .expect("Failed to get YDoc viewer");

    viewer
        .with_doc(|doc| {
            let text = doc.get_or_insert_text("shared_doc");
            let txn = doc.transact();
            let content = text.get_string(&txn);
            assert_eq!(content, "External change");
            Ok(())
        })
        .expect("Failed to verify external update");
}
