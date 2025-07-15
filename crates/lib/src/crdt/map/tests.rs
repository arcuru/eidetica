#[cfg(test)]
mod test_map {
    use crate::crdt::map::list::Position;
    use crate::crdt::map::{List, Value};
    use crate::crdt::{CRDTError, Map};

    // Minimal unit tests for internal implementation details not accessible from integration tests
    // Most functionality is now comprehensively tested in integration tests under tests/it/crdt/

    #[test]
    fn test_map_as_hashmap_internal_access() {
        let mut map = Map::new();
        map.set("key1", "value1");
        map.set("key2", "value2");
        map.remove("key1");

        // Test internal hashmap access - this exposes tombstones which the public API hides
        let hashmap = map.as_hashmap();
        assert_eq!(hashmap.len(), 2);
        assert_eq!(hashmap.get("key1"), Some(&Value::Deleted)); // Tombstone visible
        assert_eq!(
            hashmap.get("key2"),
            Some(&Value::Text("value2".to_string()))
        );

        // Public API should hide the tombstone
        assert_eq!(map.get("key1"), None);
        assert_eq!(map.get("key2"), Some(&Value::Text("value2".to_string())));
    }

    #[test]
    fn test_position_ordering_and_creation() {
        let pos1 = Position::beginning();
        let pos2 = Position::end();
        let pos_between = Position::between(&pos1, &pos2);

        // Test that positions maintain proper ordering
        assert!(pos1 < pos_between);
        assert!(pos_between < pos2);
        assert!(pos1 < pos2);

        // Test position creation with explicit values
        let pos_quarter = Position::new(1, 4); // 0.25
        let pos_half = Position::new(1, 2); // 0.5
        let pos_three_quarters = Position::new(3, 4); // 0.75

        assert!(pos_quarter < pos_half);
        assert!(pos_half < pos_three_quarters);
    }

    #[test]
    fn test_crdt_error_types() {
        // Test CRDT error type construction and pattern matching
        let error = CRDTError::ListIndexOutOfBounds { index: 5, len: 3 };

        match error {
            CRDTError::ListIndexOutOfBounds { index, len } => {
                assert_eq!(index, 5);
                assert_eq!(len, 3);
            }
            _ => panic!("Expected ListIndexOutOfBounds error"),
        }

        // Test error display formatting
        let error_str = format!("{error}");
        assert!(error_str.contains("5"));
        assert!(error_str.contains("3"));
    }

    #[test]
    fn test_value_type_checking_methods() {
        // Test Value enum type checking methods (internal categorization)
        let leaf_values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(42),
            Value::Text("test".to_string()),
            Value::Deleted,
        ];

        for value in &leaf_values {
            assert!(value.is_leaf(), "Value should be leaf: {value:?}");
            assert!(!value.is_branch(), "Value should not be branch: {value:?}");
        }

        let branch_values = vec![Value::Map(Map::new()), Value::List(List::new())];

        for value in &branch_values {
            assert!(value.is_branch(), "Value should be branch: {value:?}");
            assert!(!value.is_leaf(), "Value should not be leaf: {value:?}");
        }
    }

    #[test]
    fn test_value_type_names() {
        // Test internal type name reporting
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "bool");
        assert_eq!(Value::Int(42).type_name(), "int");
        assert_eq!(Value::Text("test".to_string()).type_name(), "text");
        assert_eq!(Value::Map(Map::new()).type_name(), "node");
        assert_eq!(Value::List(List::new()).type_name(), "list");
        assert_eq!(Value::Deleted.type_name(), "deleted");
    }
}
