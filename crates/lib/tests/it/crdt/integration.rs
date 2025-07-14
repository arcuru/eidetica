use super::helpers::*;
use crate::helpers::*;
use eidetica::crdt::Map;
use eidetica::crdt::map::Value;

#[test]
fn test_crdt_map_basic_operations() {
    let mut map = Map::new();
    
    // Test set and get
    map.set_string("key1".to_string(), "value1".to_string());
    match map.get("key1") {
        Some(Value::Text(value)) => assert_eq!(value, "value1"),
        other => panic!("Expected text value, got: {:?}", other),
    }
    
    // Test update
    map.set_string("key1".to_string(), "updated_value".to_string());
    match map.get("key1") {
        Some(Value::Text(value)) => assert_eq!(value, "updated_value"),
        other => panic!("Expected updated text value, got: {:?}", other),
    }
}

#[test]
fn test_crdt_map_merge_semantics() {
    let (map1, map2) = setup_concurrent_maps();
    
    // Merge map2 into map1
    let merged = map1.merge(&map2);
    
    // Should contain data from both maps
    assert_map_contains(&merged, &[
        ("key1", "value1"),
        ("key2", "value2"),
        ("unique1", "from_map1"),
        ("unique2", "from_map2"),
    ]);
    
    // Should contain one of the conflicting values (deterministic)
    assert!(merged.get("branch").is_some());
}

#[test]
fn test_crdt_commutativity() {
    let (map1, map2) = setup_concurrent_maps();
    
    // Test that A ⊕ B = B ⊕ A
    let merge_1_2 = map1.merge(&map2);
    let merge_2_1 = map2.merge(&map1);
    
    // Results should be identical
    assert_eq!(merge_1_2.as_hashmap(), merge_2_1.as_hashmap());
}

#[test]
fn test_crdt_associativity() {
    let base = setup_test_map();
    let mut map_a = base.clone();
    let mut map_b = base.clone();
    let mut map_c = base.clone();
    
    map_a.set_string("source".to_string(), "A".to_string());
    map_b.set_string("source".to_string(), "B".to_string());
    map_c.set_string("source".to_string(), "C".to_string());
    
    // Test that (A ⊕ B) ⊕ C = A ⊕ (B ⊕ C)
    let left_assoc = map_a.merge(&map_b).merge(&map_c);
    let right_assoc = map_a.merge(&map_b.merge(&map_c));
    
    assert_eq!(left_assoc.as_hashmap(), right_assoc.as_hashmap());
}

#[test]
fn test_crdt_idempotency() {
    let map = setup_test_map();
    
    // Test that A ⊕ A = A
    let merged = map.merge(&map);
    
    assert_eq!(map.as_hashmap(), merged.as_hashmap());
}