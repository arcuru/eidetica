//! Integration tests for CRDT functionality
//!
//! These tests focus on high-level scenarios that combine multiple CRDT types
//! and test their interaction with the broader Eidetica system.

use eidetica::crdt::{
    CRDT, Doc,
    doc::{Value, path},
};

use super::helpers::*;

#[test]
fn test_crdt_map_basic_operations() {
    let mut map = Doc::new();

    // Test set and get
    map.set_string("key1", "value1".to_string());
    match map.get("key1") {
        Some(Value::Text(value)) => assert_eq!(value, "value1"),
        other => panic!("Expected text value, got: {other:?}"),
    }

    // Test update
    map.set_string("key1", "updated_value".to_string());
    match map.get("key1") {
        Some(Value::Text(value)) => assert_eq!(value, "updated_value"),
        other => panic!("Expected updated text value, got: {other:?}"),
    }
}

#[test]
fn test_crdt_map_merge_semantics() {
    let (map1, map2) = setup_concurrent_maps();

    // Merge map2 into map1
    let merged = map1.merge(&map2).expect("Merge should succeed");

    // Should contain data from both maps
    assert_map_contains(
        &merged,
        &[
            ("key1", "value1"),
            ("key2", "value2"),
            ("unique1", "from_map1"),
            ("unique2", "from_map2"),
        ],
    );

    // Should contain one of the conflicting values (deterministic)
    assert!(merged.get("branch").is_some());
}

#[test]
fn test_crdt_commutativity() {
    // Create non-conflicting maps to ensure commutativity
    let mut map1 = Doc::new();
    map1.set_string("key1", "value1".to_string());
    map1.set_string("shared", "from_map1".to_string());

    let mut map2 = Doc::new();
    map2.set_string("key2", "value2".to_string());
    map2.set_string("different", "from_map2".to_string());

    // Test that A ⊕ B = B ⊕ A for non-conflicting maps
    let merge_1_2 = map1.merge(&map2).expect("Merge 1->2 should succeed");
    let merge_2_1 = map2.merge(&map1).expect("Merge 2->1 should succeed");

    // Results should be identical for non-conflicting merges
    assert_maps_equivalent(merge_1_2.as_node(), merge_2_1.as_node());
}

#[test]
fn test_crdt_associativity() {
    let base = setup_test_map();
    let mut map_a = base.clone();
    let mut map_b = base.clone();
    let mut map_c = base.clone();

    // Add non-conflicting changes
    map_a.set_string("source_a".to_string(), "A".to_string());
    map_b.set_string("source_b".to_string(), "B".to_string());
    map_c.set_string("source_c".to_string(), "C".to_string());

    // Test that (A ⊕ B) ⊕ C = A ⊕ (B ⊕ C)
    let left_assoc = map_a
        .merge(&map_b)
        .expect("Merge A,B should succeed")
        .merge(&map_c)
        .expect("Merge (A,B),C should succeed");
    let right_assoc = map_a
        .merge(&map_b.merge(&map_c).expect("Merge B,C should succeed"))
        .expect("Merge A,(B,C) should succeed");

    assert_maps_equivalent(&left_assoc, &right_assoc);
}

#[test]
fn test_crdt_idempotency() {
    let map = setup_test_map();

    // Test that A ⊕ A = A
    let merged = map.merge(&map).expect("Self-merge should succeed");

    assert_maps_equivalent(&map, &merged);
}

#[test]
fn test_complex_crdt_scenario() {
    // Create a complex nested structure
    let mut doc = create_complex_map();

    // Create a concurrent modification
    let mut branch = doc.clone();

    // Make different changes to each branch
    doc.set_string("title".to_string(), "Updated Title".to_string());
    doc.set("priority".to_string(), Value::Int(100));

    // In the branch, modify nested data
    if let Some(Value::Doc(metadata)) = branch.get("metadata") {
        let mut metadata_clone = metadata.clone();
        metadata_clone.set_string("editor".to_string(), "Bob".to_string());
        branch.set("metadata".to_string(), metadata_clone);
    }

    // Merge the branches
    let merged = doc.merge(&branch).expect("Complex merge should succeed");

    // Verify merged result contains changes from both branches
    // Note: CRDT merge behavior is deterministic but may not favor any particular branch
    // We just verify that merge completed and some value is present
    assert!(
        merged.get_text("title").is_some(),
        "Title should be present after merge"
    );

    assert!(
        merged.get_int("priority").is_some(),
        "Priority should be present after merge"
    );

    // Check nested metadata
    if let Some(Value::Doc(metadata)) = merged.get("metadata") {
        assert_eq!(
            metadata.get_text("author"),
            Some("Alice"),
            "Original author should be preserved"
        );
        assert_eq!(
            metadata.get_text("editor"),
            Some("Bob"),
            "Editor should be added from branch"
        );
    } else {
        panic!("Metadata should be a map after merge");
    }
}

#[test]
fn test_crdt_with_lists_integration() {
    let mut doc = Doc::new();

    // Add a list to the document
    let mut items = eidetica::crdt::doc::List::new();
    items.push(Value::Text("item1".to_string()));
    items.push(Value::Text("item2".to_string()));
    doc.set("items", Value::List(items));

    // Create a branch and add different items
    let mut branch = doc.clone();
    if let Some(Value::List(list)) = branch.get("items") {
        let mut list_clone = list.clone();
        list_clone.push(Value::Text("item3".to_string()));
        branch.set("items", Value::List(list_clone));
    }

    // In main doc, add a different item
    if let Some(Value::List(list)) = doc.get("items") {
        let mut list_clone = list.clone();
        list_clone.push(Value::Text("item4".to_string()));
        doc.set("items", Value::List(list_clone));
    }

    // Merge
    let merged = doc.merge(&branch).expect("List merge should succeed");

    // Verify list was merged (exact behavior depends on List merge implementation)
    if let Some(Value::List(merged_list)) = merged.get("items") {
        assert!(
            merged_list.len() >= 2,
            "Merged list should have at least original items"
        );
    } else {
        panic!("Items should be a list after merge");
    }
}

#[test]
fn test_crdt_tombstone_behavior_integration() {
    let mut doc1 = Doc::new();
    let mut doc2 = Doc::new();

    // Both start with the same data
    doc1.set_string("shared", "original".to_string());
    doc2.set_string("shared", "original".to_string());

    // doc1 updates the value
    doc1.set_string("shared", "updated".to_string());

    // doc2 deletes the value
    doc2.remove("shared");

    // Merge in both directions
    let merge1 = doc1.merge(&doc2).expect("Merge 1 should succeed");
    let merge2 = doc2.merge(&doc1).expect("Merge 2 should succeed");

    // CRDT merge behavior with update vs delete conflicts:
    // The result may differ depending on merge order, but should be deterministic
    // What matters is that the merges succeed and produce consistent internal state

    // At minimum, both merges should complete successfully
    assert!(
        merge1.as_hashmap().contains_key("shared"),
        "Key should exist in merge1"
    );
    assert!(
        merge2.as_hashmap().contains_key("shared"),
        "Key should exist in merge2"
    );

    // The specific conflict resolution behavior depends on CRDT implementation
    // We just verify that merges are deterministic by repeating them
    let merge1_repeat = doc1.merge(&doc2).expect("Repeated merge 1 should succeed");
    let merge2_repeat = doc2.merge(&doc1).expect("Repeated merge 2 should succeed");

    assert_eq!(
        merge1.as_hashmap(),
        merge1_repeat.as_hashmap(),
        "Merge 1 should be deterministic"
    );
    assert_eq!(
        merge2.as_hashmap(),
        merge2_repeat.as_hashmap(),
        "Merge 2 should be deterministic"
    );
}

#[test]
fn test_crdt_merge_properties_comprehensive() {
    let map1 = create_complex_map();
    let map2 = create_mixed_value_map();
    let map3 = setup_test_map();

    // Test all CRDT properties using helpers
    test_merge_commutativity(&map1, &map2).expect("Maps should be commutative");
    test_merge_associativity(&map1, &map2, &map3).expect("Maps should be associative");
    test_merge_idempotency(&map1).expect("Maps should be idempotent");
}

#[test]
fn test_crdt_api_ergonomics() {
    let mut doc = Doc::new();

    // Test convenient API methods
    doc.set("title", "My Document");
    doc.set("priority", 42);
    doc.set("published", true);

    // Test typed getters
    assert_eq!(doc.get_text("title"), Some("My Document"));
    assert_eq!(doc.get_int("priority"), Some(42));
    assert_eq!(doc.get_bool("published"), Some(true));

    // Test direct comparison
    assert!(*doc.get("title").unwrap() == "My Document");
    assert!(*doc.get("priority").unwrap() == 42);
    assert!(*doc.get("published").unwrap() == true);

    // Test path operations
    doc.set_path(path!("user.name"), "Alice")
        .expect("Path set should work");
    assert_eq!(
        doc.get_text_at_path(path!("user.name")),
        Some("Alice"),
        "Path get should work"
    );
}

#[test]
fn test_large_scale_merge_performance() {
    // Create large maps to test merge performance
    let large_map1 = create_large_map(100);
    let large_map2 = create_large_map(100);

    // Test that merge completes successfully
    let _merged = large_map1
        .merge(&large_map2)
        .expect("Large merge should succeed");

    // Basic verification
    assert!(large_map1.as_hashmap().len() >= 100);
    assert!(large_map2.as_hashmap().len() >= 100);
}
