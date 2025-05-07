use eideticadb::data::KVOverWrite;
use eideticadb::data::CRDT;
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

    // Try removing a non-existent key
    let removed = kv.remove("nonexistent");
    assert_eq!(removed, None);
}

#[test]
fn test_kvoverwrite_as_hashmap_mut() {
    // Test mutable access to the underlying HashMap
    let mut kv = KVOverWrite::new();

    // Modify through the KVOverWrite methods
    kv.set("key1".to_string(), "value1".to_string());

    // Modify through the mutable HashMap reference
    kv.as_hashmap_mut()
        .insert("key2".to_string(), "value2".to_string());

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
