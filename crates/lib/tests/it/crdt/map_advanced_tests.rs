//! Advanced CRDT Map operation tests
//!
//! This module tests advanced CRDT Map operations including merge semantics,
//! tombstone handling, recursive merging, and complex conflict resolution.

use super::helpers::*;
use eidetica::crdt::CRDT;
use eidetica::crdt::doc::Value;

#[test]
fn test_map_basic_operations() {
    // Create Map with string values
    let map = create_map_with_values(&[("key1", "value1"), ("key2", "value2")]);

    // Test get values
    assert_text_value(map.get("key1").unwrap(), "value1");
    assert_text_value(map.get("key2").unwrap(), "value2");
    assert_eq!(map.get("non_existent"), None);

    // Create a nested map
    let nested = create_nested_map(&[(
        "outer",
        &[("inner1", "nested_value1"), ("inner2", "nested_value2")],
    )]);

    // Test nested access
    assert_nested_value(&nested, &["outer", "inner1"], "nested_value1");
    assert_nested_value(&nested, &["outer", "inner2"], "nested_value2");

    // Test basic merge
    let (map1, map2) = create_merge_test_maps(
        &[("a", "value_a"), ("b", "value_b")],
        &[("b", "updated_b"), ("c", "value_c")],
    );

    let merged = test_merge_result(
        &map1,
        &map2,
        &[("a", "value_a"), ("b", "updated_b"), ("c", "value_c")],
    )
    .expect("Merge should succeed");

    // Verify specific merge behavior
    assert_text_value(merged.get("a").unwrap(), "value_a");
    assert_text_value(merged.get("b").unwrap(), "updated_b"); // Should be updated
    assert_text_value(merged.get("c").unwrap(), "value_c");
}

#[test]
fn test_map_tombstone_handling() {
    // Create Map with initial values
    let mut map = create_map_with_values(&[("str_key", "str_value")]);

    // Add a nested map
    let mut nested = eidetica::crdt::Doc::new();
    nested.set_string("inner_key", "inner_value");
    map.set_map("map_key", nested.into());

    // Remove a string value
    let removed = map.remove("str_key");
    assert_text_value(&removed.unwrap(), "str_value");

    // Verify it's gone from regular access
    assert_eq!(map.get("str_key"), None);

    // Verify the tombstone using the helper
    assert_path_deleted(&map, &["str_key"]);

    // Test merging with tombstones
    let map2 = create_map_with_values(&[("str_key", "revived_value")]); // Try to resurrect

    let merged = map.merge(&map2).expect("Merge failed");

    // The string should be revived
    assert_text_value(merged.get("str_key").unwrap(), "revived_value");

    // Now go the other way - delete in map3 and merge
    let mut map3 = eidetica::crdt::Doc::new();
    map3.remove("map_key"); // Delete the map

    let final_merged = merged.merge(map3.as_node()).expect("Second merge failed");

    // The map should be gone - verify using the path helper
    assert_path_deleted(&final_merged, &["map_key"]);

    // But the revived string should remain
    assert_text_value(final_merged.get("str_key").unwrap(), "revived_value");
}

#[test]
fn test_map_recursive_merge() {
    // Create two nested structures using the helper
    let map1 = create_complex_nested_structure();

    // Create a second structure with overlapping keys but different values
    let mut map2 = eidetica::crdt::Doc::new();

    // Setup a different level 2
    let mut level2_alt = eidetica::crdt::Doc::new();
    level2_alt.set_string("level2_key2", "level2_value2");
    level2_alt.set_string("shared_key", "map2_value"); // Same key, different value

    // Setup a different level 3
    let mut level3_alt = eidetica::crdt::Doc::new();
    level3_alt.set_string("level3_key2", "level3_value2");

    // Link them
    level2_alt.set_map("level3", level3_alt);
    map2.set_map("level2", level2_alt);

    // Add a top-level key that will conflict
    map2.set_string("top_key", "updated_top_value");

    // Merge them
    let merged = map1.merge(map2.as_node()).expect("Merge failed");

    // Check merged result - top level
    assert_text_value(merged.get("top_key").unwrap(), "updated_top_value"); // map2 overwrites

    // Level 2 - should contain keys from both sources
    match merged.get("level2").unwrap() {
        Value::Node(level2_merged) => {
            // Both unique keys should be present
            assert_text_value(level2_merged.get("level2_key1").unwrap(), "level2_value1");
            assert_text_value(level2_merged.get("level2_key2").unwrap(), "level2_value2");

            // Shared key should have map2's value (last write wins)
            assert_text_value(level2_merged.get("shared_key").unwrap(), "map2_value");

            // Level 3 - should contain keys from both sources
            match level2_merged.get("level3").unwrap() {
                Value::Node(level3_merged) => {
                    assert_text_value(level3_merged.get("level3_key1").unwrap(), "level3_value1");
                    assert_text_value(level3_merged.get("level3_key2").unwrap(), "level3_value2");
                }
                _ => panic!("Expected merged level3 map"),
            }
        }
        _ => panic!("Expected merged level2 map"),
    }
}

#[test]
fn test_map_type_conflicts() {
    // Test merging when same key has different types in different CRDTs
    let mut map1 = eidetica::crdt::Doc::new();
    let mut map2 = eidetica::crdt::Doc::new();

    // In map1, key is a string
    map1.set_string("conflict_key", "string_value");

    // In map2, same key is a map
    let mut nested = eidetica::crdt::Doc::new();
    nested.set_string("inner", "inner_value");
    map2.set_map("conflict_key", nested);

    // Test merge in both directions

    // Direction 1: map1 -> map2 (map should win)
    let merged1 = map1.merge(&map2).expect("Merge 1 failed");
    match merged1.get("conflict_key").unwrap() {
        Value::Node(m) => assert_text_value(m.get("inner").unwrap(), "inner_value"),
        _ => panic!("Expected map to win in merge 1"),
    }

    // Direction 2: map2 -> map1 (string should win)
    let merged2 = map2.merge(&map1).expect("Merge 2 failed");
    assert_text_value(merged2.get("conflict_key").unwrap(), "string_value");
}

#[test]
fn test_map_complex_merge_with_tombstones() {
    // Test complex merge scenario with multiple levels containing tombstones
    let (map1, map2) = build_complex_merge_data();

    // Merge
    let merged = map1.merge(&map2).expect("Complex merge failed");

    // Verify top level
    assert_eq!(merged.get("top_level_key"), None); // Should be tombstone
    assert_text_value(merged.get("new_top_key").unwrap(), "new_top_value");

    // Verify level1
    match merged.get("level1").unwrap() {
        Value::Node(level1_merged) => {
            // Verify level1.key1 (only in map1, should be preserved)
            assert_text_value(level1_merged.get("key1").unwrap(), "value1");

            // Verify level1.key2 (only in map2, should be added)
            assert_text_value(level1_merged.get("key2").unwrap(), "value2");

            // Verify level1.to_delete (deleted in map2, should be gone)
            assert_eq!(level1_merged.get("to_delete"), None);
            // Verify it's a tombstone
            assert_path_deleted(level1_merged, &["to_delete"]);

            // Verify level1.to_update (updated in map2, should have new value)
            assert_text_value(level1_merged.get("to_update").unwrap(), "updated_value");
        }
        _ => panic!("Expected level1 map"),
    }
}

#[test]
fn test_map_multi_generation_updates() {
    // Test a sequence of updates and merges to verify LWW semantics
    let _generation_data = build_generation_test_data();

    // Initialize base state
    let mut base = eidetica::crdt::Doc::new();
    base.set_string("key", "original");

    // Generation 1: Update in branch1
    let mut branch1 = eidetica::crdt::Doc::new();
    branch1.set_string("key", "branch1_value");
    let gen1 = base.merge(&branch1).expect("Gen1 merge failed");

    // Verify gen1
    assert_text_value(gen1.get("key").unwrap(), "branch1_value");

    // Generation 2: Delete in branch2
    let mut branch2 = eidetica::crdt::Doc::new();
    branch2.remove("key");
    let gen2 = gen1.merge(&branch2).expect("Gen2 merge failed");

    // Verify gen2
    assert_eq!(gen2.get("key"), None);
    assert_path_deleted(gen2.as_node(), &["key"]);

    // Generation 3: Resurrect in branch3
    let mut branch3 = eidetica::crdt::Doc::new();
    branch3.set_string("key", "resurrected");
    let gen3 = gen2.merge(&branch3).expect("Gen3 merge failed");

    // Verify gen3
    assert_text_value(gen3.get("key").unwrap(), "resurrected");

    // Generation 4: Replace with map in branch4
    let mut branch4 = eidetica::crdt::Doc::new();
    let mut nested = eidetica::crdt::Doc::new();
    nested.set_string("inner", "inner_value");
    branch4.set_map("key", nested);
    let gen4 = gen3.merge(&branch4).expect("Gen4 merge failed");

    // Verify gen4
    match gen4.get("key").unwrap() {
        Value::Node(m) => assert_text_value(m.get("inner").unwrap(), "inner_value"),
        _ => panic!("Expected map in gen4"),
    }
}

#[test]
fn test_map_set_deleted_and_get() {
    let mut map = eidetica::crdt::Doc::new();

    // Set a key directly to Deleted
    map.set("deleted_key", Value::Deleted);

    // get() should return None
    assert_eq!(map.get("deleted_key"), None);

    // as_hashmap() should show the tombstone
    assert_eq!(map.as_hashmap().get("deleted_key"), Some(&Value::Deleted));

    // Set another key with a value, then set to Deleted
    map.set_string("another_key", "value");
    map.set("another_key", Value::Deleted);
    assert_eq!(map.get("another_key"), None);
    assert_eq!(map.as_hashmap().get("another_key"), Some(&Value::Deleted));
}

#[test]
fn test_map_remove_non_existent() {
    let mut map = eidetica::crdt::Doc::new();

    // Remove a key that doesn't exist
    let removed = map.remove("non_existent_key");
    assert!(
        removed.is_none(),
        "Removing non-existent key should return None"
    );

    // get() should return None
    assert_eq!(map.get("non_existent_key"), None);

    // as_hashmap() should show a tombstone was created
    assert_eq!(
        map.as_hashmap().get("non_existent_key"),
        Some(&Value::Deleted)
    );
}

#[test]
fn test_map_remove_existing_tombstone() {
    let mut map = eidetica::crdt::Doc::new();

    // Create a tombstone by removing a key
    map.set_string("key_to_tombstone", "some_value");
    let _ = map.remove("key_to_tombstone"); // This creates the first tombstone

    // Verify it's a tombstone
    assert_eq!(map.get("key_to_tombstone"), None);
    assert_path_deleted(map.as_node(), &["key_to_tombstone"]);

    // Try to remove the key again (which is now a tombstone)
    let removed_again = map.remove("key_to_tombstone");

    // Removing an existing tombstone should return None (as per Doc::remove logic for already deleted)
    assert!(
        removed_again.is_none(),
        "Removing an existing tombstone should return None"
    );

    // get() should still return None
    assert_eq!(map.get("key_to_tombstone"), None);

    // as_hashmap() should still show the tombstone
    assert_path_deleted(map.as_node(), &["key_to_tombstone"]);

    // Directly set a tombstone and then remove it
    map.set("direct_tombstone", Value::Deleted);
    let removed_direct = map.remove("direct_tombstone");
    assert!(removed_direct.is_none());
    assert_eq!(map.get("direct_tombstone"), None);
    assert_path_deleted(map.as_node(), &["direct_tombstone"]);
}

#[test]
fn test_map_merge_dual_tombstones() {
    let mut map1 = eidetica::crdt::Doc::new();
    map1.set_string("key1_map1", "value1_map1");
    map1.remove("key1_map1"); // Tombstone in map1

    map1.set_string("common_key", "value_common_map1");
    map1.remove("common_key"); // Tombstone for common_key in map1

    let mut map2 = eidetica::crdt::Doc::new();
    map2.set_string("key2_map2", "value2_map2");
    map2.remove("key2_map2"); // Tombstone in map2

    map2.set_string("common_key", "value_common_map2"); // Value in map2
    map2.remove("common_key"); // Tombstone for common_key in map2 (other's tombstone wins)

    // Merge map2 into map1
    let merged = map1
        .merge(&map2)
        .expect("Merge with dual tombstones failed");

    // Check key1_map1 (only in map1, tombstoned)
    assert_eq!(merged.get("key1_map1"), None);
    assert_path_deleted(merged.as_node(), &["key1_map1"]);

    // Check key2_map2 (only in map2, tombstoned)
    assert_eq!(merged.get("key2_map2"), None);
    assert_path_deleted(merged.as_node(), &["key2_map2"]);

    // Check common_key (tombstoned in both, map2's tombstone should prevail, resulting in a tombstone)
    assert_eq!(merged.get("common_key"), None);
    assert_path_deleted(merged.as_node(), &["common_key"]);

    // What if one has a value and the other a tombstone (map2's tombstone wins)
    let mut map3 = eidetica::crdt::Doc::new();
    map3.set_string("val_then_tomb", "i_existed");

    let mut map4 = eidetica::crdt::Doc::new();
    map4.remove("val_then_tomb");

    let merged2 = map3.merge(&map4).expect("Merge val then tomb failed");
    assert_eq!(merged2.get("val_then_tomb"), None);
    assert_path_deleted(merged2.as_node(), &["val_then_tomb"]);

    // What if one has a tombstone and the other a value (map3's value wins)
    let merged3 = map4.merge(&map3).expect("Merge tomb then val failed");
    assert_text_value(merged3.get("val_then_tomb").unwrap(), "i_existed");
}
