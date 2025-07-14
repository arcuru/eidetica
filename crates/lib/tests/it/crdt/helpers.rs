use crate::helpers::*;
use eidetica::crdt::Map;
use eidetica::crdt::map::Value;

/// Create a test Map with some initial data
pub fn setup_test_map() -> Map {
    let mut map = Map::new();
    map.set_string("key1".to_string(), "value1".to_string());
    map.set_string("key2".to_string(), "value2".to_string());
    map
}

/// Create two concurrent Maps with different modifications
pub fn setup_concurrent_maps() -> (Map, Map) {
    let base = setup_test_map();
    
    let mut map1 = base.clone();
    map1.set_string("branch".to_string(), "left".to_string());
    map1.set_string("unique1".to_string(), "from_map1".to_string());
    
    let mut map2 = base.clone();
    map2.set_string("branch".to_string(), "right".to_string());
    map2.set_string("unique2".to_string(), "from_map2".to_string());
    
    (map1, map2)
}

/// Assert that a Map contains expected key-value pairs
pub fn assert_map_contains(map: &Map, expected: &[(&str, &str)]) {
    for (key, expected_value) in expected {
        match map.get(key) {
            Some(Value::Text(actual_value)) => {
                assert_eq!(actual_value, expected_value, "Value mismatch for key '{}'", key);
            }
            Some(other) => panic!("Expected text value for key '{}', got: {:?}", key, other),
            None => panic!("Key '{}' not found in map", key),
        }
    }
}