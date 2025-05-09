use eideticadb::data::KVOverWrite;
use eideticadb::data::CRDT;
use eideticadb::data::{KVNested, NestedValue};
use eideticadb::entry::Entry;
use std::collections::HashMap;

#[test]
fn test_kvoverwrite_basic_operations() {
    let mut kv = KVOverWrite::new();

    // Test set and get
    let key = "test_key";
    let value = "test_value";
    kv.set(key.to_string(), value.to_string());

    assert_eq!(kv.get(key), Some(&value.to_string()));
    assert_eq!(kv.get("non_existent_key"), None);

    // Test overwrite
    let new_value = "new_value";
    kv.set(key.to_string(), new_value.to_string());
    assert_eq!(kv.get(key), Some(&new_value.to_string()));
}

#[test]
fn test_kvoverwrite_merge() {
    let mut kv1 = KVOverWrite::new();
    kv1.set("key1".to_string(), "value1".to_string());
    kv1.set("key2".to_string(), "value2".to_string());

    let mut kv2 = KVOverWrite::new();
    kv2.set("key2".to_string(), "value2_updated".to_string());
    kv2.set("key3".to_string(), "value3".to_string());

    // Merge kv2 into kv1
    let merged = kv1.merge(&kv2).expect("Merge failed");

    // Verify merged result
    assert_eq!(merged.get("key1"), Some(&"value1".to_string()));
    assert_eq!(merged.get("key2"), Some(&"value2_updated".to_string())); // overwritten
    assert_eq!(merged.get("key3"), Some(&"value3".to_string())); // added from kv2
}

#[test]
fn test_kvoverwrite_serialization() {
    let mut kv = KVOverWrite::new();
    kv.set("key1".to_string(), "value1".to_string());
    kv.set("key2".to_string(), "value2".to_string());

    // Serialize to string
    let serialized = serde_json::to_string(&kv).expect("Serialization failed");
    assert!(!serialized.is_empty());

    // Deserialize back
    let deserialized: KVOverWrite =
        serde_json::from_str(&serialized).expect("Deserialization failed");
    assert_eq!(deserialized.get("key1"), Some(&"value1".to_string()));
    assert_eq!(deserialized.get("key2"), Some(&"value2".to_string()));
}

#[test]
fn test_kvoverwrite_from_entry() {
    // Create an entry with KVOverWrite data
    let mut kv = KVOverWrite::new();
    kv.set("key1".to_string(), "value1".to_string());
    kv.set("key2".to_string(), "value2".to_string());

    let serialized = serde_json::to_string(&kv).expect("Serialization failed");
    let entry = Entry::root_builder(serialized).build();

    // Extract KVOverWrite from entry
    let data = entry.get_settings().expect("Failed to get settings");
    let deserialized: KVOverWrite = serde_json::from_str(&data).expect("Deserialization failed");

    assert_eq!(deserialized.get("key1"), Some(&"value1".to_string()));
    assert_eq!(deserialized.get("key2"), Some(&"value2".to_string()));
}

#[test]
fn test_kvoverwrite_to_raw_data() {
    let mut kv = KVOverWrite::new();
    kv.set("key1".to_string(), "value1".to_string());

    let raw_data = serde_json::to_string(&kv).expect("Serialization failed");
    assert!(!raw_data.is_empty());

    // Should be valid JSON
    let json_result = serde_json::from_str::<serde_json::Value>(&raw_data);
    assert!(json_result.is_ok());
}

#[test]
fn test_kvoverwrite_multiple_merge_operations() {
    // Start with an initial KVOverWrite
    let mut base = KVOverWrite::new();
    base.set("key1".to_string(), "initial1".to_string());
    base.set("key2".to_string(), "initial2".to_string());
    base.set("common".to_string(), "base".to_string());

    // Create two diverging updates
    let mut branch1 = KVOverWrite::new();
    branch1.set("key1".to_string(), "branch1_value".to_string());
    branch1.set("branch1_key".to_string(), "branch1_only".to_string());
    branch1.set("common".to_string(), "branch1".to_string());

    let mut branch2 = KVOverWrite::new();
    branch2.set("key2".to_string(), "branch2_value".to_string());
    branch2.set("branch2_key".to_string(), "branch2_only".to_string());
    branch2.set("common".to_string(), "branch2".to_string());

    // Merge in different orders to compare last-write-wins behavior

    // Order: base -> branch1 -> branch2
    let merged1 = base.merge(&branch1).expect("First merge failed");
    let merged1_2 = merged1.merge(&branch2).expect("Second merge failed");

    // Order: base -> branch2 -> branch1
    let merged2 = base.merge(&branch2).expect("First merge failed");
    let merged2_1 = merged2.merge(&branch1).expect("Second merge failed");

    // Since branch1 and branch2 modify different keys (except for "common"),
    // merged1_2 and merged2_1 should be mostly identical

    assert_eq!(merged1_2.get("key1"), Some(&"branch1_value".to_string()));
    assert_eq!(merged1_2.get("key2"), Some(&"branch2_value".to_string()));
    assert_eq!(
        merged1_2.get("branch1_key"),
        Some(&"branch1_only".to_string())
    );
    assert_eq!(
        merged1_2.get("branch2_key"),
        Some(&"branch2_only".to_string())
    );

    assert_eq!(merged2_1.get("key1"), Some(&"branch1_value".to_string()));
    assert_eq!(merged2_1.get("key2"), Some(&"branch2_value".to_string()));
    assert_eq!(
        merged2_1.get("branch1_key"),
        Some(&"branch1_only".to_string())
    );
    assert_eq!(
        merged2_1.get("branch2_key"),
        Some(&"branch2_only".to_string())
    );

    // But for the "common" key, the order matters
    assert_eq!(merged1_2.get("common"), Some(&"branch2".to_string())); // Last write wins
    assert_eq!(merged2_1.get("common"), Some(&"branch1".to_string())); // Last write wins
}

#[test]
fn test_kvoverwrite_serialization_roundtrip_with_merge() {
    // Create and serialize original data
    let mut original = KVOverWrite::new();
    original.set("key1".to_string(), "value1".to_string());
    original.set("key2".to_string(), "value2".to_string());

    let serialized = serde_json::to_string(&original).expect("Serialization failed");

    // Deserialize to a new instance
    let deserialized: KVOverWrite =
        serde_json::from_str(&serialized).expect("Deserialization failed");

    // Create a second KVOverWrite with different data
    let mut update = KVOverWrite::new();
    update.set("key2".to_string(), "updated2".to_string());
    update.set("key3".to_string(), "value3".to_string());

    // Merge update into the deserialized data
    let merged = deserialized.merge(&update).expect("Merge failed");

    // Serialize the merged result
    let merged_serialized =
        serde_json::to_string(&merged).expect("Serialization of merged data failed");

    // Deserialize again
    let final_data: KVOverWrite =
        serde_json::from_str(&merged_serialized).expect("Deserialization of merged data failed");

    // Verify final state
    assert_eq!(final_data.get("key1"), Some(&"value1".to_string())); // Unchanged
    assert_eq!(final_data.get("key2"), Some(&"updated2".to_string())); // Updated
    assert_eq!(final_data.get("key3"), Some(&"value3".to_string())); // Added

    // Test merging with an empty CRDT
    let empty = KVOverWrite::new();
    let merged_with_empty = final_data.merge(&empty).expect("Merge with empty failed");

    // Merging with empty should not change anything
    assert_eq!(merged_with_empty.get("key1"), Some(&"value1".to_string()));
    assert_eq!(merged_with_empty.get("key2"), Some(&"updated2".to_string()));
    assert_eq!(merged_with_empty.get("key3"), Some(&"value3".to_string()));
}

#[test]
fn test_kvoverwrite_new() {
    // Test creation of a new KVOverWrite
    let kv = KVOverWrite::new();
    assert_eq!(kv.as_hashmap().len(), 0);
}

#[test]
fn test_kvoverwrite_from_hashmap() {
    // Test creation from an existing HashMap
    let mut data = HashMap::new();
    data.insert("key1".to_string(), "value1".to_string());
    data.insert("key2".to_string(), "value2".to_string());

    let kv = KVOverWrite::from_hashmap(data.clone());
    assert_eq!(kv.as_hashmap().len(), 2);
    assert_eq!(kv.get("key1"), Some(&"value1".to_string()));
    assert_eq!(kv.get("key2"), Some(&"value2".to_string()));
}

#[test]
fn test_kvoverwrite_remove() {
    // Test removing values
    let mut kv = KVOverWrite::new();

    // Add a value then remove it
    kv.set("key1".to_string(), "value1".to_string());
    assert_eq!(kv.get("key1"), Some(&"value1".to_string()));

    let removed = kv.remove("key1");
    assert_eq!(removed, Some("value1".to_string()));
    assert_eq!(kv.get("key1"), None);
    // Assert that key1 is now a tombstone
    assert_eq!(kv.as_hashmap().get("key1"), Some(&None));

    // Try removing a non-existent key
    let removed = kv.remove("nonexistent");
    assert_eq!(removed, None);
    // Nonexistent key should also result in a tombstone, checked by test_kvoverwrite_delete_nonexistent
}

#[test]
fn test_kvoverwrite_as_hashmap_mut() {
    // Test mutable access to the underlying HashMap
    let mut kv = KVOverWrite::new();

    // Modify through the KVOverWrite methods
    kv.set("key1".to_string(), "value1".to_string());

    // Modify through the mutable HashMap reference
    kv.as_hashmap_mut()
        .insert("key2".to_string(), Some("value2".to_string()));

    // Verify both modifications worked
    assert_eq!(kv.get("key1"), Some(&"value1".to_string()));
    assert_eq!(kv.get("key2"), Some(&"value2".to_string()));
}

#[test]
fn test_kvowrite_to_entry() {
    let mut kvstore = KVOverWrite::default();
    kvstore.set("key1".to_string(), "value1".to_string());
    kvstore.set("key2".to_string(), "value2".to_string());

    // Serialize the KVOverwrite to a string
    let serialized = serde_json::to_string(&kvstore).unwrap();

    // Create an entry with this data
    let entry = Entry::root_builder(serialized).build();

    // Ensure the entry data matches the serialized KVOverwrite
    let entry_data = entry.get_settings().unwrap();
    let deserialized: KVOverWrite = serde_json::from_str(&entry_data).unwrap();

    // Verify the deserialized data matches the original KVOverwrite
    assert_eq!(deserialized.get("key1").unwrap(), "value1");
    assert_eq!(deserialized.get("key2").unwrap(), "value2");
}

#[test]
fn test_kvoverwrite_tombstones() {
    // Test tombstone functionality
    let mut kv = KVOverWrite::new();

    // Add and then remove some values
    kv.set("key1".to_string(), "value1".to_string());
    kv.set("key2".to_string(), "value2".to_string());

    assert_eq!(kv.get("key1"), Some(&"value1".to_string()));

    // Remove key1, should return the value and create a tombstone
    let removed = kv.remove("key1");
    assert_eq!(removed, Some("value1".to_string()));

    // get() should now return None for the removed key
    assert_eq!(kv.get("key1"), None);

    // But in the underlying HashMap, it should be a None tombstone
    assert!(kv.as_hashmap().contains_key("key1"));
    assert_eq!(kv.as_hashmap().get("key1"), Some(&None));

    // Test merging with tombstones
    let mut kv2 = KVOverWrite::new();
    kv2.set("key1".to_string(), "new_value1".to_string()); // Try to resurrect the deleted key
    kv2.set("key3".to_string(), "value3".to_string());

    // Should overwrite the tombstone
    let merged = kv.merge(&kv2).expect("Merge failed");
    assert_eq!(merged.get("key1"), Some(&"new_value1".to_string())); // Resurrected
    assert_eq!(merged.get("key2"), Some(&"value2".to_string())); // Unchanged
    assert_eq!(merged.get("key3"), Some(&"value3".to_string())); // Added

    // Now test deleting in the other direction
    let mut kv3 = KVOverWrite::new();
    kv3.remove("key2"); // Delete key2 in kv3

    // Merge kv3 into merged (kv1+kv2)
    let final_merge = merged.merge(&kv3).expect("Second merge failed");

    // key2 should now be deleted
    assert_eq!(final_merge.get("key2"), None);
    assert_eq!(final_merge.get("key1"), Some(&"new_value1".to_string())); // Still present
    assert_eq!(final_merge.get("key3"), Some(&"value3".to_string())); // Still present
}

#[test]
fn test_kvoverwrite_tombstone_serialization() {
    // Test serialization with tombstones
    let mut kv = KVOverWrite::new();
    kv.set("key1".to_string(), "value1".to_string());
    kv.set("key2".to_string(), "value2".to_string());

    // Create tombstone
    kv.remove("key2");

    // Verify tombstone exists
    assert!(kv.as_hashmap().contains_key("key2"));
    assert_eq!(kv.as_hashmap().get("key2"), Some(&None));

    // Serialize with tombstone
    let serialized = serde_json::to_string(&kv).expect("Serialization failed");

    // Deserialize
    let deserialized: KVOverWrite =
        serde_json::from_str(&serialized).expect("Deserialization failed");

    // Verify structure is maintained
    assert_eq!(deserialized.get("key1"), Some(&"value1".to_string()));
    assert_eq!(deserialized.get("key2"), None);

    // Verify tombstone survived serialization
    assert!(deserialized.as_hashmap().contains_key("key2"));
    assert_eq!(deserialized.as_hashmap().get("key2"), Some(&None));
}

#[test]
fn test_kvoverwrite_delete_nonexistent() {
    // Test creating a tombstone for non-existent key
    let mut kv = KVOverWrite::new();

    // Remove a key that doesn't exist
    let result = kv.remove("nonexistent");
    assert_eq!(result, None);

    // Verify a tombstone was still created
    assert!(kv.as_hashmap().contains_key("nonexistent"));
    assert_eq!(kv.as_hashmap().get("nonexistent"), Some(&None));

    // Ensure get still returns None
    assert_eq!(kv.get("nonexistent"), None);
}

#[test]
fn test_kvoverwrite_merge_with_dual_tombstones() {
    // Test merging when both sources have tombstones
    let mut kv1 = KVOverWrite::new();
    kv1.set("key1".to_string(), "value1".to_string());
    kv1.set("key2".to_string(), "value2".to_string());
    kv1.remove("key1"); // Create tombstone in kv1

    let mut kv2 = KVOverWrite::new();
    kv2.set("key2".to_string(), "updated2".to_string());
    kv2.set("key3".to_string(), "value3".to_string());
    kv2.remove("key3"); // Create tombstone in kv2

    // Merge kv2 into kv1
    let merged = kv1.merge(&kv2).expect("Merge failed");

    // Verify results:
    // key1: tombstone from kv1 (still tombstone)
    // key2: value from kv2 overwrites kv1
    // key3: tombstone from kv2

    assert_eq!(merged.get("key1"), None);
    assert_eq!(merged.get("key2"), Some(&"updated2".to_string()));
    assert_eq!(merged.get("key3"), None);

    // Verify tombstones are present
    assert!(merged.as_hashmap().contains_key("key1"));
    assert!(merged.as_hashmap().contains_key("key3"));
    assert_eq!(merged.as_hashmap().get("key1"), Some(&None));
    assert_eq!(merged.as_hashmap().get("key3"), Some(&None));
}

#[test]
fn test_kvnested_basic() {
    let mut kv = KVNested::new();

    // Test adding string values
    kv.set_string("str_key".to_string(), "str_value".to_string());

    // Test retrieving values
    match kv.get("str_key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "str_value"),
        _ => panic!("Expected string value"),
    }

    // Test adding nested maps
    let mut nested = KVNested::new();
    nested.set_string("inner_key".to_string(), "inner_value".to_string());

    kv.set_map("map_key".to_string(), nested);

    // Test retrieving nested values
    match kv.get("map_key") {
        Some(NestedValue::Map(inner_map)) => match inner_map.get("inner_key") {
            Some(NestedValue::String(s)) => assert_eq!(s, "inner_value"),
            _ => panic!("Expected string in inner map"),
        },
        _ => panic!("Expected map value"),
    }

    // Test using the NestedValue enum directly
    kv.set(
        "direct_key".to_string(),
        NestedValue::String("direct_value".to_string()),
    );

    match kv.get("direct_key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "direct_value"),
        _ => panic!("Expected string value for direct_key"),
    }
}

#[test]
fn test_kvnested_tombstones() {
    let mut kv = KVNested::new();

    // Add some values
    kv.set_string("str_key".to_string(), "str_value".to_string());

    let mut nested = KVNested::new();
    nested.set_string("inner_key".to_string(), "inner_value".to_string());
    kv.set_map("map_key".to_string(), nested);

    // Remove a string value
    let removed = kv.remove("str_key");
    match removed {
        Some(NestedValue::String(s)) => assert_eq!(s, "str_value"),
        _ => panic!("Expected to remove a string value"),
    }

    // Verify it's gone from regular access
    assert_eq!(kv.get("str_key"), None);

    // But there should be a tombstone in the underlying HashMap
    match kv.as_hashmap().get("str_key") {
        Some(NestedValue::Deleted) => (), // This is correct
        _ => panic!("Expected a tombstone"),
    }

    // Test merging with tombstones
    let mut kv2 = KVNested::new();
    kv2.set_string("str_key".to_string(), "revived_value".to_string()); // Try to resurrect

    let merged = kv.merge(&kv2).expect("Merge failed");

    // The string should be revived
    match merged.get("str_key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "revived_value"),
        _ => panic!("Expected revived string value"),
    }

    // Now go the other way - delete in kv2 and merge
    let mut kv3 = KVNested::new();
    kv3.remove("map_key"); // Delete the map

    let final_merged = merged.merge(&kv3).expect("Second merge failed");

    // The map should be gone
    assert_eq!(final_merged.get("map_key"), None);

    // But the revived string should remain
    match final_merged.get("str_key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "revived_value"),
        _ => panic!("Expected string value to remain"),
    }
}

#[test]
fn test_kvnested_recursive_merge() {
    // Create two nested structures
    let mut kv1 = KVNested::new();

    // Setup level 1
    kv1.set_string("key1".to_string(), "value1".to_string());

    // Setup level 2
    let mut level2 = KVNested::new();
    level2.set_string("level2_key1".to_string(), "level2_value1".to_string());
    level2.set_string("shared_key".to_string(), "kv1_value".to_string());

    // Setup level 3
    let mut level3 = KVNested::new();
    level3.set_string("level3_key1".to_string(), "level3_value1".to_string());

    // Link them together
    level2.set_map("level3".to_string(), level3);
    kv1.set_map("level2".to_string(), level2);

    // Create a second structure with overlapping keys but different values
    let mut kv2 = KVNested::new();

    // Setup a different level 2
    let mut level2_alt = KVNested::new();
    level2_alt.set_string("level2_key2".to_string(), "level2_value2".to_string());
    level2_alt.set_string("shared_key".to_string(), "kv2_value".to_string()); // Same key, different value

    // Setup a different level 3
    let mut level3_alt = KVNested::new();
    level3_alt.set_string("level3_key2".to_string(), "level3_value2".to_string());

    // Link them
    level2_alt.set_map("level3".to_string(), level3_alt);
    kv2.set_map("level2".to_string(), level2_alt);

    // Add a top-level key that will conflict
    kv2.set_string("key1".to_string(), "value1_updated".to_string());

    // Merge them
    let merged = kv1.merge(&kv2).expect("Merge failed");

    // Check merged result - top level
    match merged.get("key1") {
        Some(NestedValue::String(s)) => assert_eq!(s, "value1_updated"), // kv2 overwrites
        _ => panic!("Expected updated string at top level"),
    }

    // Level 2 - should contain keys from both sources
    match merged.get("level2") {
        Some(NestedValue::Map(level2_merged)) => {
            // Both unique keys should be present
            match level2_merged.get("level2_key1") {
                Some(NestedValue::String(s)) => assert_eq!(s, "level2_value1"),
                _ => panic!("Expected level2_key1 preserved"),
            }

            match level2_merged.get("level2_key2") {
                Some(NestedValue::String(s)) => assert_eq!(s, "level2_value2"),
                _ => panic!("Expected level2_key2 added"),
            }

            // Shared key should have kv2's value (last write wins)
            match level2_merged.get("shared_key") {
                Some(NestedValue::String(s)) => assert_eq!(s, "kv2_value"),
                _ => panic!("Expected shared_key with kv2's value"),
            }

            // Level 3 - should contain keys from both sources
            match level2_merged.get("level3") {
                Some(NestedValue::Map(level3_merged)) => {
                    match level3_merged.get("level3_key1") {
                        Some(NestedValue::String(s)) => assert_eq!(s, "level3_value1"),
                        _ => panic!("Expected level3_key1 preserved"),
                    }

                    match level3_merged.get("level3_key2") {
                        Some(NestedValue::String(s)) => assert_eq!(s, "level3_value2"),
                        _ => panic!("Expected level3_key2 added"),
                    }
                }
                _ => panic!("Expected merged level3 map"),
            }
        }
        _ => panic!("Expected merged level2 map"),
    }
}

#[test]
fn test_kvnested_serialization() {
    // Test serialization and deserialization of KVNested
    let mut kv = KVNested::new();

    // Add various value types
    kv.set_string("string_key".to_string(), "string_value".to_string());

    let mut nested = KVNested::new();
    nested.set_string("inner".to_string(), "inner_value".to_string());
    kv.set_map("map_key".to_string(), nested);

    // Create a tombstone
    kv.remove("deleted_key");

    // Serialize
    let serialized = serde_json::to_string(&kv).expect("Serialization failed");

    // Deserialize
    let deserialized: KVNested = serde_json::from_str(&serialized).expect("Deserialization failed");

    // Verify string survived
    match deserialized.get("string_key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "string_value"),
        _ => panic!("Expected string value"),
    }

    // Verify nested map survived
    match deserialized.get("map_key") {
        Some(NestedValue::Map(m)) => match m.get("inner") {
            Some(NestedValue::String(s)) => assert_eq!(s, "inner_value"),
            _ => panic!("Expected inner string value"),
        },
        _ => panic!("Expected map value"),
    }

    // Verify tombstone survived
    assert!(deserialized.as_hashmap().contains_key("deleted_key"));
    match deserialized.as_hashmap().get("deleted_key") {
        Some(NestedValue::Deleted) => (),
        _ => panic!("Expected tombstone"),
    }
}

#[test]
fn test_kvnested_cascading_delete() {
    // Test deleting a nested structure
    let mut kv = KVNested::new();

    // Create a deeply nested structure
    let mut level1 = KVNested::new();
    let mut level2 = KVNested::new();
    let mut level3 = KVNested::new();

    level3.set_string("deepest".to_string(), "treasure".to_string());
    level2.set_map("level3".to_string(), level3);
    level1.set_map("level2".to_string(), level2);
    kv.set_map("level1".to_string(), level1);

    // Delete the entire structure by removing level1
    kv.remove("level1");

    // Verify it's gone from get
    assert_eq!(kv.get("level1"), None);

    // Verify tombstone exists
    match kv.as_hashmap().get("level1") {
        Some(NestedValue::Deleted) => (),
        _ => panic!("Expected tombstone for level1"),
    }

    // Add a new level1 with different content and verify it works
    let mut new_level1 = KVNested::new();
    new_level1.set_string("new_value".to_string(), "resurrected".to_string());
    kv.set_map("level1".to_string(), new_level1);

    // Verify level1 is accessible again
    match kv.get("level1") {
        Some(NestedValue::Map(m)) => match m.get("new_value") {
            Some(NestedValue::String(s)) => assert_eq!(s, "resurrected"),
            _ => panic!("Expected string in new level1"),
        },
        _ => panic!("Expected map for level1"),
    }
}

#[test]
fn test_kvnested_type_conflicts() {
    // Test merging when same key has different types in different CRDTs
    let mut kv1 = KVNested::new();
    let mut kv2 = KVNested::new();

    // In kv1, key is a string
    kv1.set_string("conflict_key".to_string(), "string_value".to_string());

    // In kv2, same key is a map
    let mut nested = KVNested::new();
    nested.set_string("inner".to_string(), "inner_value".to_string());
    kv2.set_map("conflict_key".to_string(), nested);

    // Test merge in both directions

    // Direction 1: kv1 -> kv2 (map should win)
    let merged1 = kv1.merge(&kv2).expect("Merge 1 failed");
    match merged1.get("conflict_key") {
        Some(NestedValue::Map(m)) => match m.get("inner") {
            Some(NestedValue::String(s)) => assert_eq!(s, "inner_value"),
            _ => panic!("Expected inner string in map"),
        },
        _ => panic!("Expected map to win in merge 1"),
    }

    // Direction 2: kv2 -> kv1 (map should win)
    let merged2 = kv2.merge(&kv1).expect("Merge 2 failed");
    match merged2.get("conflict_key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "string_value"),
        _ => panic!("Expected string to win in merge 2"),
    }
}

#[test]
fn test_kvnested_complex_merge_with_tombstones() {
    // Test complex merge scenario with multiple levels containing tombstones

    // Structure 1
    let mut kv1 = KVNested::new();
    let mut level1a = KVNested::new();

    level1a.set_string("key1".to_string(), "value1".to_string());
    level1a.set_string("to_delete".to_string(), "will_be_deleted".to_string());
    level1a.set_string("to_update".to_string(), "initial_value".to_string());

    kv1.set_map("level1".to_string(), level1a);
    kv1.set_string("top_level_key".to_string(), "top_value".to_string());

    // Structure 2 (with changes and tombstones)
    let mut kv2 = KVNested::new();
    let mut level1b = KVNested::new();

    level1b.set_string("key2".to_string(), "value2".to_string()); // New key
    level1b.remove("to_delete"); // Create tombstone
    level1b.set_string("to_update".to_string(), "updated_value".to_string()); // Update

    kv2.set_map("level1".to_string(), level1b);
    kv2.remove("top_level_key"); // Create tombstone at top level
    kv2.set_string("new_top_key".to_string(), "new_top_value".to_string()); // New top level

    // Merge
    let merged = kv1.merge(&kv2).expect("Complex merge failed");

    // Verify top level
    assert_eq!(merged.get("top_level_key"), None); // Should be tombstone
    match merged.get("new_top_key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "new_top_value"),
        _ => panic!("Expected new_top_key"),
    }

    // Verify level1
    match merged.get("level1") {
        Some(NestedValue::Map(level1_merged)) => {
            // Verify level1.key1 (only in kv1, should be preserved)
            match level1_merged.get("key1") {
                Some(NestedValue::String(s)) => assert_eq!(s, "value1"),
                _ => panic!("Expected level1.key1 preserved"),
            }

            // Verify level1.key2 (only in kv2, should be added)
            match level1_merged.get("key2") {
                Some(NestedValue::String(s)) => assert_eq!(s, "value2"),
                _ => panic!("Expected level1.key2 added"),
            }

            // Verify level1.to_delete (deleted in kv2, should be gone)
            assert_eq!(level1_merged.get("to_delete"), None);
            // Verify it's a tombstone
            match level1_merged.as_hashmap().get("to_delete") {
                Some(NestedValue::Deleted) => (),
                _ => panic!("Expected tombstone for level1.to_delete"),
            }

            // Verify level1.to_update (updated in kv2, should have new value)
            match level1_merged.get("to_update") {
                Some(NestedValue::String(s)) => assert_eq!(s, "updated_value"),
                _ => panic!("Expected level1.to_update updated"),
            }
        }
        _ => panic!("Expected level1 map"),
    }
}

#[test]
fn test_kvnested_multi_generation_updates() {
    // Test a sequence of updates and merges to verify LWW semantics

    // Initialize base state
    let mut base = KVNested::new();
    base.set_string("key".to_string(), "original".to_string());

    // Generation 1: Update in branch1
    let mut branch1 = KVNested::new();
    branch1.set_string("key".to_string(), "branch1_value".to_string());
    let gen1 = base.merge(&branch1).expect("Gen1 merge failed");

    // Verify gen1
    match gen1.get("key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "branch1_value"),
        _ => panic!("Expected branch1 value in gen1"),
    }

    // Generation 2: Delete in branch2
    let mut branch2 = KVNested::new();
    branch2.remove("key");
    let gen2 = gen1.merge(&branch2).expect("Gen2 merge failed");

    // Verify gen2
    assert_eq!(gen2.get("key"), None);
    match gen2.as_hashmap().get("key") {
        Some(NestedValue::Deleted) => (),
        _ => panic!("Expected tombstone in gen2"),
    }

    // Generation 3: Resurrect in branch3
    let mut branch3 = KVNested::new();
    branch3.set_string("key".to_string(), "resurrected".to_string());
    let gen3 = gen2.merge(&branch3).expect("Gen3 merge failed");

    // Verify gen3
    match gen3.get("key") {
        Some(NestedValue::String(s)) => assert_eq!(s, "resurrected"),
        _ => panic!("Expected resurrected value in gen3"),
    }

    // Generation 4: Replace with map in branch4
    let mut branch4 = KVNested::new();
    let mut nested = KVNested::new();
    nested.set_string("inner".to_string(), "inner_value".to_string());
    branch4.set_map("key".to_string(), nested);
    let gen4 = gen3.merge(&branch4).expect("Gen4 merge failed");

    // Verify gen4
    match gen4.get("key") {
        Some(NestedValue::Map(m)) => match m.get("inner") {
            Some(NestedValue::String(s)) => assert_eq!(s, "inner_value"),
            _ => panic!("Expected inner string in gen4"),
        },
        _ => panic!("Expected map in gen4"),
    }
}

#[test]
fn test_kvnested_set_deleted_and_get() {
    let mut kv = KVNested::new();

    // Set a key directly to Deleted
    kv.set("deleted_key".to_string(), NestedValue::Deleted);

    // get() should return None
    assert_eq!(kv.get("deleted_key"), None);

    // as_hashmap() should show the tombstone
    assert_eq!(
        kv.as_hashmap().get("deleted_key"),
        Some(&NestedValue::Deleted)
    );

    // Set another key with a value, then set to Deleted
    kv.set_string("another_key".to_string(), "value".to_string());
    kv.set("another_key".to_string(), NestedValue::Deleted);
    assert_eq!(kv.get("another_key"), None);
    assert_eq!(
        kv.as_hashmap().get("another_key"),
        Some(&NestedValue::Deleted)
    );
}

#[test]
fn test_kvnested_remove_non_existent() {
    let mut kv = KVNested::new();

    // Remove a key that doesn't exist
    let removed = kv.remove("non_existent_key");
    assert!(
        removed.is_none(),
        "Removing non-existent key should return None"
    );

    // get() should return None
    assert_eq!(kv.get("non_existent_key"), None);

    // as_hashmap() should show a tombstone was created
    assert_eq!(
        kv.as_hashmap().get("non_existent_key"),
        Some(&NestedValue::Deleted)
    );
}

#[test]
fn test_kvnested_remove_existing_tombstone() {
    let mut kv = KVNested::new();

    // Create a tombstone by removing a key
    kv.set_string("key_to_tombstone".to_string(), "some_value".to_string());
    let _ = kv.remove("key_to_tombstone"); // This creates the first tombstone

    // Verify it's a tombstone
    assert_eq!(kv.get("key_to_tombstone"), None);
    assert_eq!(
        kv.as_hashmap().get("key_to_tombstone"),
        Some(&NestedValue::Deleted)
    );

    // Try to remove the key again (which is now a tombstone)
    let removed_again = kv.remove("key_to_tombstone");

    // Removing an existing tombstone should return None (as per KVNested::remove logic for already deleted)
    assert!(
        removed_again.is_none(),
        "Removing an existing tombstone should return None"
    );

    // get() should still return None
    assert_eq!(kv.get("key_to_tombstone"), None);

    // as_hashmap() should still show the tombstone
    assert_eq!(
        kv.as_hashmap().get("key_to_tombstone"),
        Some(&NestedValue::Deleted)
    );

    // Directly set a tombstone and then remove it
    kv.set("direct_tombstone".to_string(), NestedValue::Deleted);
    let removed_direct = kv.remove("direct_tombstone");
    assert!(removed_direct.is_none());
    assert_eq!(kv.get("direct_tombstone"), None);
    assert_eq!(
        kv.as_hashmap().get("direct_tombstone"),
        Some(&NestedValue::Deleted)
    );
}

#[test]
fn test_kvnested_merge_dual_tombstones() {
    let mut kv1 = KVNested::new();
    kv1.set_string("key1_kv1".to_string(), "value1_kv1".to_string());
    kv1.remove("key1_kv1"); // Tombstone in kv1

    kv1.set_string("common_key".to_string(), "value_common_kv1".to_string());
    kv1.remove("common_key"); // Tombstone for common_key in kv1

    let mut kv2 = KVNested::new();
    kv2.set_string("key2_kv2".to_string(), "value2_kv2".to_string());
    kv2.remove("key2_kv2"); // Tombstone in kv2

    kv2.set_string("common_key".to_string(), "value_common_kv2".to_string()); // Value in kv2
    kv2.remove("common_key"); // Tombstone for common_key in kv2 (other's tombstone wins)

    // Merge kv2 into kv1
    let merged = kv1.merge(&kv2).expect("Merge with dual tombstones failed");

    // Check key1_kv1 (only in kv1, tombstoned)
    assert_eq!(merged.get("key1_kv1"), None);
    assert_eq!(
        merged.as_hashmap().get("key1_kv1"),
        Some(&NestedValue::Deleted)
    );

    // Check key2_kv2 (only in kv2, tombstoned)
    assert_eq!(merged.get("key2_kv2"), None);
    assert_eq!(
        merged.as_hashmap().get("key2_kv2"),
        Some(&NestedValue::Deleted)
    );

    // Check common_key (tombstoned in both, kv2's tombstone should prevail, resulting in a tombstone)
    assert_eq!(merged.get("common_key"), None);
    assert_eq!(
        merged.as_hashmap().get("common_key"),
        Some(&NestedValue::Deleted)
    );

    // What if one has a value and the other a tombstone (kv2's tombstone wins)
    let mut kv3 = KVNested::new();
    kv3.set_string("val_then_tomb".to_string(), "i_existed".to_string());

    let mut kv4 = KVNested::new();
    kv4.remove("val_then_tomb");

    let merged2 = kv3.merge(&kv4).expect("Merge val then tomb failed");
    assert_eq!(merged2.get("val_then_tomb"), None);
    assert_eq!(
        merged2.as_hashmap().get("val_then_tomb"),
        Some(&NestedValue::Deleted)
    );

    // What if one has a tombstone and the other a value (kv4's value wins)
    let merged3 = kv4.merge(&kv3).expect("Merge tomb then val failed");
    match merged3.get("val_then_tomb") {
        Some(NestedValue::String(s)) => assert_eq!(s, "i_existed"),
        _ => panic!("Expected value to overwrite tombstone"),
    }
}
