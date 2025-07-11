#[cfg(test)]
mod test_map {
    use crate::crdt::map::array::Position;
    use crate::crdt::map::{Array, Value};
    use crate::crdt::{CRDT, CRDTError, Map};

    // Value tests
    #[test]
    fn test_map_value_basic_types() {
        let null_val = Value::Null;
        let bool_val = Value::Bool(true);
        let int_val = Value::Int(42);
        let text_val = Value::Text("hello".to_string());
        let deleted_val = Value::Deleted;

        assert!(null_val.is_leaf());
        assert!(bool_val.is_leaf());
        assert!(int_val.is_leaf());
        assert!(text_val.is_leaf());
        assert!(deleted_val.is_leaf());

        assert!(!null_val.is_branch());
        assert!(!bool_val.is_branch());
        assert!(!int_val.is_branch());
        assert!(!text_val.is_branch());
        assert!(!deleted_val.is_branch());

        assert!(null_val.is_null());
        assert!(!bool_val.is_null());
        assert!(!int_val.is_null());
        assert!(!text_val.is_null());
        assert!(!deleted_val.is_null());

        assert!(!null_val.is_deleted());
        assert!(!bool_val.is_deleted());
        assert!(!int_val.is_deleted());
        assert!(!text_val.is_deleted());
        assert!(deleted_val.is_deleted());
    }

    #[test]
    fn test_map_value_branch_types() {
        let map_val = Value::Map(Map::new());
        let list_val = Value::Array(Array::new());

        assert!(!map_val.is_leaf());
        assert!(!list_val.is_leaf());

        assert!(map_val.is_branch());
        assert!(list_val.is_branch());

        assert!(!map_val.is_null());
        assert!(!list_val.is_null());

        assert!(!map_val.is_deleted());
        assert!(!list_val.is_deleted());
    }

    #[test]
    fn test_map_value_type_names() {
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "bool");
        assert_eq!(Value::Int(42).type_name(), "int");
        assert_eq!(Value::Text("hello".to_string()).type_name(), "text");
        assert_eq!(Value::Map(Map::new()).type_name(), "node");
        assert_eq!(Value::Array(Array::new()).type_name(), "list");
        assert_eq!(Value::Deleted.type_name(), "deleted");
    }

    #[test]
    fn test_map_value_accessors() {
        let bool_val = Value::Bool(true);
        let int_val = Value::Int(42);
        let text_val = Value::Text("hello".to_string());
        let map_val = Value::Map(Map::new());
        let list_val = Value::Array(Array::new());

        // Test as_bool
        assert_eq!(bool_val.as_bool(), Some(true));
        assert_eq!(int_val.as_bool(), None);

        // Test as_int
        assert_eq!(int_val.as_int(), Some(42));
        assert_eq!(bool_val.as_int(), None);

        // Test as_text
        assert_eq!(text_val.as_text(), Some("hello"));
        assert_eq!(bool_val.as_text(), None);

        // Test direct comparisons
        assert!(bool_val == true);
        assert!(int_val == 42);
        assert!(text_val == "hello");

        // Test as_node
        assert!(map_val.as_node().is_some());
        assert!(bool_val.as_node().is_none());

        // Test as_list
        assert!(list_val.as_list().is_some());
        assert!(bool_val.as_list().is_none());
    }

    #[test]
    fn test_map_value_from_impls() {
        let from_bool: Value = true.into();
        let from_i64: Value = 42i64.into();
        let from_string: Value = "hello".into();
        let from_node: Value = Map::new().into();
        let from_list: Value = Array::new().into();

        assert_eq!(from_bool.as_bool(), Some(true));
        assert_eq!(from_i64.as_int(), Some(42));
        assert_eq!(from_string.as_text(), Some("hello"));
        assert!(from_node.as_node().is_some());
        assert!(from_list.as_list().is_some());
    }

    #[test]
    fn test_map_value_merge_leafs() {
        let mut val1 = Value::Int(42);
        let val2 = Value::Int(100);

        val1.merge(&val2);
        assert_eq!(val1.as_int(), Some(100)); // Last write wins

        let mut val3 = Value::Text("hello".to_string());
        let val4 = Value::Text("world".to_string());

        val3.merge(&val4);
        assert_eq!(val3.as_text(), Some("world")); // Last write wins
    }

    #[test]
    fn test_map_value_merge_with_deleted() {
        let mut val1 = Value::Int(42);
        let val2 = Value::Deleted;

        val1.merge(&val2);
        assert!(val1.is_deleted()); // Deletion wins

        let mut val3 = Value::Deleted;
        let val4 = Value::Int(100);

        val3.merge(&val4);
        assert_eq!(val3.as_int(), Some(100)); // Resurrection
    }

    // Array tests
    #[test]
    fn test_node_list_basic_operations() {
        let mut list = Array::new();

        assert!(list.is_empty());
        assert_eq!(list.len(), 0);

        // Test push with flexible input
        let idx1 = list.push("hello");
        let idx2 = list.push(42);
        let idx3 = list.push(true);

        assert!(!list.is_empty());
        assert_eq!(list.len(), 3);

        // Test get
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("hello"));
        assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(42));
        assert_eq!(list.get(2).and_then(|v| v.as_bool()), Some(true));
        assert!(list.get(3).is_none());

        // Test indexes returned by push
        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 2);
    }

    #[test]
    fn test_node_list_set_operations() {
        let mut list = Array::new();

        list.push("original");
        list.push(100);

        // Test set with flexible input
        let old_val = list.set(0, "modified");
        assert_eq!(old_val.as_ref().and_then(|v| v.as_text()), Some("original"));
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("modified"));

        let old_val2 = list.set(1, 200);
        assert_eq!(old_val2.as_ref().and_then(|v| v.as_int()), Some(100));
        assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(200));

        // Test set on non-existent index
        let result = list.set(10, "nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_node_list_remove_operations() {
        let mut list = Array::new();

        list.push("first");
        list.push("second");
        list.push("third");

        // Test remove
        let removed = list.remove(1);
        assert_eq!(removed.as_ref().and_then(|v| v.as_text()), Some("second"));
        assert_eq!(list.len(), 2);

        // Verify remaining elements
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("first"));
        assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("third"));

        // Test remove on non-existent index
        let result = list.remove(10);
        assert!(result.is_none());
    }

    #[test]
    fn test_node_list_insert_at_position() {
        let mut list = Array::new();

        let pos1 = Position::new(10, 1);
        let pos2 = Position::new(20, 1);
        let pos3 = Position::new(15, 1); // Between pos1 and pos2

        list.insert_at_position(pos1, "first");
        list.insert_at_position(pos2, "third");
        list.insert_at_position(pos3, "second");

        // Should be ordered by position
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("first"));
        assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("second"));
        assert_eq!(list.get(2).and_then(|v| v.as_text()), Some("third"));
    }

    #[test]
    fn test_node_list_iterators() {
        let mut list = Array::new();

        list.push("a");
        list.push("b");
        list.push("c");

        // Test iter
        let values: Vec<_> = list.iter().collect();
        assert_eq!(values.len(), 3);

        // Test iter_with_positions
        let pairs: Vec<_> = list.iter_with_positions().collect();
        assert_eq!(pairs.len(), 3);

        // Test iter_mut
        for value in list.iter_mut() {
            if let Value::Text(s) = value {
                s.push_str("_modified");
            }
        }

        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("a_modified"));
        assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("b_modified"));
        assert_eq!(list.get(2).and_then(|v| v.as_text()), Some("c_modified"));
    }

    #[test]
    fn test_node_list_merge() {
        let mut list1 = Array::new();
        let mut list2 = Array::new();

        let pos1 = Position::new(10, 1);
        let pos2 = Position::new(20, 1);
        let pos3 = Position::new(15, 1);

        list1.insert_at_position(pos1.clone(), "first");
        list1.insert_at_position(pos2.clone(), "second");

        list2.insert_at_position(pos2.clone(), "second_modified"); // Conflict
        list2.insert_at_position(pos3.clone(), "middle");

        list1.merge(&list2);

        // Should have merged
        assert_eq!(list1.len(), 3);
        assert_eq!(
            list1.get_by_position(&pos1).and_then(|v| v.as_text()),
            Some("first")
        );
        assert_eq!(
            list1.get_by_position(&pos2).and_then(|v| v.as_text()),
            Some("second_modified")
        );
        assert_eq!(
            list1.get_by_position(&pos3).and_then(|v| v.as_text()),
            Some("middle")
        );
    }

    #[test]
    fn test_node_list_from_iterator() {
        let values = vec![
            Value::Text("a".to_string()),
            Value::Int(42),
            Value::Bool(true),
        ];

        let list: Array = values.into_iter().collect();
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("a"));
        assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(42));
        assert_eq!(list.get(2).and_then(|v| v.as_bool()), Some(true));
    }

    // Map tests
    #[test]
    fn test_map_basic_operations() {
        let mut map = Map::new();

        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        // Test set with flexible input
        let old_val = map.set("name", "Alice");
        assert!(old_val.is_none());
        assert!(!map.is_empty());
        assert_eq!(map.len(), 1);

        let old_val2 = map.set("age", 30);
        assert!(old_val2.is_none());
        assert_eq!(map.len(), 2);

        // Test contains_key with flexible input
        assert!(map.contains_key("name"));
        assert!(map.contains_key("age"));
        assert!(!map.contains_key("nonexistent"));

        // Test get with flexible input
        assert_eq!(map.get_text("name"), Some("Alice"));
        assert_eq!(map.get_int("age"), Some(30));
        assert!(map.get("nonexistent").is_none());
    }

    #[test]
    fn test_map_overwrite_values() {
        let mut map = Map::new();

        map.set("key", "original");
        let old_val = map.set("key", "modified");

        assert_eq!(old_val.as_ref().and_then(|v| v.as_text()), Some("original"));
        assert_eq!(map.get_text("key"), Some("modified"));
        assert_eq!(map.len(), 1); // Should still be 1
    }

    #[test]
    fn test_map_remove_operations() {
        let mut map = Map::new();

        map.set("name", "Alice");
        map.set("age", 30);
        map.set("active", true);

        // Test remove with flexible input
        let removed = map.remove("age");
        assert_eq!(removed.as_ref().and_then(|v| v.as_int()), Some(30));
        assert!(!map.contains_key("age")); // Key no longer exists (tombstone hidden)
        assert!(map.get("age").is_none()); // get returns None
        assert_eq!(map.len(), 2); // Tombstones excluded from len

        // Test remove on non-existent key
        let result = map.remove("nonexistent");
        assert!(result.is_none());
        assert!(!map.contains_key("nonexistent")); // Tombstone hidden from API
        assert_eq!(map.len(), 2); // Tombstones excluded from len
    }

    #[test]
    fn test_map_delete_operations() {
        let mut map = Map::new();

        map.set("name", "Alice");
        map.set("age", 30);

        // Test delete with flexible input
        let result = map.delete("age");
        assert!(result);
        assert!(!map.contains_key("age")); // Key no longer exists (tombstone hidden)
        assert!(map.get("age").is_none()); // Returns None (filtered out)

        // Test delete on non-existent key
        let result2 = map.delete("nonexistent");
        assert!(!result2);
    }

    #[test]
    fn test_map_get_mut() {
        let mut map = Map::new();

        map.set("name", "Alice");
        map.set("age", 30);

        // Test get_mut with flexible input
        if let Some(Value::Text(name)) = map.get_mut("name") {
            name.push_str(" Smith");
        }

        assert_eq!(map.get_text("name"), Some("Alice Smith"));

        // Test get_mut on non-existent key
        assert!(map.get_mut("nonexistent").is_none());
    }

    #[test]
    fn test_map_path_operations() {
        let mut map = Map::new();

        // Test set_path creating intermediate nodes
        let result = map.set_path("user.profile.name", "Alice");
        assert!(result.is_ok());

        let result2 = map.set_path("user.profile.age", 30);
        assert!(result2.is_ok());

        let result3 = map.set_path("user.settings.theme", "dark");
        assert!(result3.is_ok());

        // Test get_path
        assert_eq!(map.get_text_at_path("user.profile.name"), Some("Alice"));
        assert_eq!(map.get_int_at_path("user.profile.age"), Some(30));
        assert_eq!(map.get_text_at_path("user.settings.theme"), Some("dark"));
        assert!(map.get_path("nonexistent.path").is_none());

        // Test get_path_mut
        if let Some(Value::Text(name)) = map.get_path_mut("user.profile.name") {
            name.push_str(" Smith");
        }

        assert_eq!(
            map.get_text_at_path("user.profile.name"),
            Some("Alice Smith")
        );
    }

    #[test]
    fn test_map_path_with_lists() {
        let mut map = Map::new();

        // Create a node with a list
        let mut list = Array::new();
        list.push("item1");
        list.push("item2");
        map.set("items", list);

        // Test path access with list indices
        assert_eq!(map.get_text_at_path("items.0"), Some("item1"));
        assert_eq!(map.get_text_at_path("items.1"), Some("item2"));
        assert!(map.get_path("items.2").is_none());
        assert!(map.get_path("items.invalid").is_none());
    }

    #[test]
    fn test_map_path_errors() {
        let mut map = Map::new();

        map.set("scalar", "value");

        // Test setting path through scalar value
        let result = map.set_path("scalar.nested", "should_fail");
        assert!(result.is_err());

        // Test empty path - this actually works, it just sets at root level
        let result2 = map.set_path("", "value");
        assert!(result2.is_ok()); // Empty path is treated as root level

        // Test path with single component
        let result3 = map.set_path("single", "value");
        assert!(result3.is_ok());
        assert_eq!(map.get_text("single"), Some("value"));
    }

    #[test]
    fn test_map_iterators() {
        let mut map = Map::new();

        map.set("name", "Alice");
        map.set("age", 30);
        map.set("active", true);

        // Test iter
        let pairs: Vec<_> = map.iter().collect();
        assert_eq!(pairs.len(), 3);

        // Test keys
        let keys: Vec<_> = map.keys().collect();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&&"name".to_string()));
        assert!(keys.contains(&&"age".to_string()));
        assert!(keys.contains(&&"active".to_string()));

        // Test values
        let values: Vec<_> = map.values().collect();
        assert_eq!(values.len(), 3);

        // Test iter_mut
        for (key, value) in map.iter_mut() {
            if key == "name"
                && let Value::Text(s) = value
            {
                s.push_str(" Smith");
            }
        }

        assert_eq!(map.get_text("name"), Some("Alice Smith"));
    }

    #[test]
    fn test_map_builder_pattern() {
        let map = Map::new()
            .with_text("name", "Alice")
            .with_int("age", 30)
            .with_bool("active", true)
            .with_node("profile", Map::new().with_text("bio", "Developer"))
            .with_list("tags", Array::new());

        assert_eq!(map.get_text("name"), Some("Alice"));
        assert_eq!(map.get_int("age"), Some(30));
        assert_eq!(map.get_bool("active"), Some(true));
        assert!(map.get_node("profile").is_some());
        assert!(map.get_list("tags").is_some());

        // Test nested access
        assert_eq!(map.get_text_at_path("profile.bio"), Some("Developer"));
    }

    #[test]
    fn test_map_clear() {
        let mut map = Map::new();

        map.set("name", "Alice");
        map.set("age", 30);

        assert_eq!(map.len(), 2);

        map.clear();

        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn test_map_crdt_merge() {
        let mut map1 = Map::new();
        let mut map2 = Map::new();

        map1.set("name", "Alice");
        map1.set("age", 30);

        map2.set("name", "Bob"); // Conflict
        map2.set("city", "NYC");

        let merged = map1.merge(&map2).unwrap();

        assert_eq!(merged.get_text("name"), Some("Bob")); // Last write wins
        assert_eq!(merged.get_int("age"), Some(30));
        assert_eq!(merged.get_text("city"), Some("NYC"));
    }

    #[test]
    fn test_map_from_iterator() {
        let pairs = vec![
            ("name".to_string(), Value::Text("Alice".to_string())),
            ("age".to_string(), Value::Int(30)),
            ("active".to_string(), Value::Bool(true)),
        ];

        let map: Map = pairs.into_iter().collect();

        assert_eq!(map.get_text("name"), Some("Alice"));
        assert_eq!(map.get_int("age"), Some(30));
        assert_eq!(map.get_bool("active"), Some(true));
    }

    #[test]
    fn test_list_position_ordering() {
        let pos1 = Position::new(1, 2); // 0.5
        let pos2 = Position::new(3, 4); // 0.75
        let pos3 = Position::new(1, 1); // 1.0

        assert!(pos1 < pos2);
        assert!(pos2 < pos3);
        assert!(pos1 < pos3);

        // Test between
        let between = Position::between(&pos1, &pos3);
        assert!(pos1 < between);
        assert!(between < pos3);
    }

    #[test]
    fn test_list_position_beginning_end() {
        let beginning = Position::beginning();
        let end = Position::end();
        let middle = Position::new(100, 1);

        assert!(beginning < middle);
        assert!(middle < end);
        assert!(beginning < end);
    }

    // Legacy compatibility test (just one to verify the conversion works)
    #[test]
    fn test_cleaner_api_examples() {
        let mut map = Map::new();

        // Set some values
        map.set("name", "Alice");
        map.set("age", 30);
        map.set("active", true);

        // Old verbose way (still works)
        assert_eq!(map.get("name").and_then(|v| v.as_text()), Some("Alice"));
        assert_eq!(map.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(map.get("active").and_then(|v| v.as_bool()), Some(true));

        // New clean way with typed getters
        assert_eq!(map.get_text("name"), Some("Alice"));
        assert_eq!(map.get_int("age"), Some(30));
        assert_eq!(map.get_bool("active"), Some(true));

        // Even cleaner with direct comparisons on Value!
        assert!(*map.get("name").unwrap() == "Alice");
        assert!(*map.get("age").unwrap() == 30);
        assert!(*map.get("active").unwrap() == true);

        // Path-based access
        map.set_path("user.profile.bio", "Developer").unwrap();

        // Old verbose way (still works)
        assert_eq!(
            map.get_path("user.profile.bio").and_then(|v| v.as_text()),
            Some("Developer")
        );

        // New clean way with typed getters
        assert_eq!(map.get_text_at_path("user.profile.bio"), Some("Developer"));

        // Even cleaner with direct comparisons on Value!
        assert!(*map.get_path("user.profile.bio").unwrap() == "Developer");

        // Convenience methods for Value
        let value = Value::Text("hello".to_string());
        assert_eq!(value.as_text_or_empty(), "hello");

        let value = Value::Int(42);
        assert_eq!(value.as_int_or_zero(), 42);
        assert!(!value.as_bool_or_false()); // not a bool, returns false
    }

    #[test]
    fn test_partial_eq_nodevalue() {
        let text_val = Value::Text("hello".to_string());
        let int_val = Value::Int(42);
        let bool_val = Value::Bool(true);

        // Test Value comparisons with primitive types
        assert!(text_val == "hello");
        assert!(text_val == "hello");
        assert!(int_val == 42i64);
        assert!(int_val == 42i32);
        assert!(int_val == 42u32);
        assert!(bool_val == true);

        // Test reverse comparisons
        assert!("hello" == text_val);
        assert!("hello" == text_val);
        assert!(42i64 == int_val);
        assert!(42i32 == int_val);
        assert!(42u32 == int_val);
        assert!(true == bool_val);

        // Test non-matching types
        assert!(!(text_val == 42));
        assert!(!(int_val == "hello"));
        assert!(!(bool_val == "hello"));
    }

    #[test]
    fn test_partial_eq_with_unwrap() {
        let mut map = Map::new();
        map.set("name", "Alice");
        map.set("age", 30);
        map.set("active", true);

        // Test Value comparisons through unwrap
        assert!(*map.get("name").unwrap() == "Alice");
        assert!(*map.get("age").unwrap() == 30i64);
        assert!(*map.get("age").unwrap() == 30i32);
        assert!(*map.get("age").unwrap() == 30u32);
        assert!(*map.get("active").unwrap() == true);

        // Test reverse comparisons
        assert!("Alice" == *map.get("name").unwrap());
        assert!(30i64 == *map.get("age").unwrap());
        assert!(30i32 == *map.get("age").unwrap());
        assert!(30u32 == *map.get("age").unwrap());
        assert!(true == *map.get("active").unwrap());

        // Test non-matching types
        assert!(!(*map.get("name").unwrap() == 42));
        assert!(!(*map.get("age").unwrap() == "Alice"));
        assert!(!(*map.get("active").unwrap() == "Alice"));

        // Test with matches! macro for cleaner pattern
        assert!(matches!(map.get("name"), Some(v) if *v == "Alice"));
        assert!(matches!(map.get("age"), Some(v) if *v == 30));
        assert!(matches!(map.get("active"), Some(v) if *v == true));
        assert!(map.get("nonexistent").is_none());
    }

    #[test]
    fn test_map_array_serialization() {
        let mut map = Map::new();

        // Add an array element
        let result = map.array_add("fruits", Value::Text("apple".to_string()));
        assert!(result.is_ok());

        // Check array length before serialization
        let length_before = map.array_len("fruits");
        assert_eq!(length_before, 1);

        // Serialize and deserialize
        let serialized = serde_json::to_string(&map).unwrap();
        let deserialized: Map = serde_json::from_str(&serialized).unwrap();

        // Check array length after deserialization
        let length_after = deserialized.array_len("fruits");
        assert_eq!(length_after, 1);

        // Check if they're equal
        assert_eq!(length_before, length_after);
    }

    // Additional tests for new API methods
    #[test]
    fn test_node_list_push_returns_index() {
        let mut list = Array::new();

        // Test push returns correct sequential indices
        let idx1 = list.push("first");
        let idx2 = list.push("second");
        let idx3 = list.push("third");

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 2);
        assert_eq!(list.len(), 3);

        // Verify values are accessible by returned indices
        assert_eq!(list.get(idx1).unwrap().as_text(), Some("first"));
        assert_eq!(list.get(idx2).unwrap().as_text(), Some("second"));
        assert_eq!(list.get(idx3).unwrap().as_text(), Some("third"));
    }

    #[test]
    fn test_node_list_push_different_types() {
        let mut list = Array::new();

        let idx1 = list.push("hello");
        let idx2 = list.push(42);
        let idx3 = list.push(true);
        let idx4 = list.push(3.13); // Use non-pi value to avoid clippy warning

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 2);
        assert_eq!(idx4, 3);

        assert_eq!(list.get(0).unwrap().as_text(), Some("hello"));
        assert_eq!(list.get(1).unwrap().as_int(), Some(42));
        assert_eq!(list.get(2).unwrap().as_bool(), Some(true));
        assert_eq!(list.get(3).unwrap().as_int(), Some(3)); // float converted to int
    }

    #[test]
    fn test_node_list_insert_at_valid_indices() {
        let mut list = Array::new();

        // Insert at beginning of empty list
        assert!(list.insert(0, "first").is_ok());
        assert_eq!(list.len(), 1);
        assert_eq!(list.get(0).unwrap().as_text(), Some("first"));

        // Insert at end
        assert!(list.insert(1, "last").is_ok());
        assert_eq!(list.len(), 2);
        assert_eq!(list.get(1).unwrap().as_text(), Some("last"));

        // Insert in middle
        assert!(list.insert(1, "middle").is_ok());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
        assert_eq!(list.get(1).unwrap().as_text(), Some("middle"));
        assert_eq!(list.get(2).unwrap().as_text(), Some("last"));
    }

    #[test]
    fn test_node_list_insert_at_beginning() {
        let mut list = Array::new();

        list.push("second");
        list.push("third");

        // Insert at beginning
        assert!(list.insert(0, "first").is_ok());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
        assert_eq!(list.get(1).unwrap().as_text(), Some("second"));
        assert_eq!(list.get(2).unwrap().as_text(), Some("third"));
    }

    #[test]
    fn test_node_list_insert_at_end() {
        let mut list = Array::new();

        list.push("first");
        list.push("second");

        // Insert at end (equivalent to push)
        assert!(list.insert(2, "third").is_ok());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(2).unwrap().as_text(), Some("third"));
    }

    #[test]
    fn test_node_list_insert_index_out_of_bounds() {
        let mut list = Array::new();

        // Insert beyond bounds in empty list
        let result = list.insert(1, "invalid");
        assert!(result.is_err());
        match result.unwrap_err() {
            CRDTError::ListIndexOutOfBounds { index, len } => {
                assert_eq!(index, 1);
                assert_eq!(len, 0);
            }
            _ => panic!("Expected ListIndexOutOfBounds error"),
        }

        // Add some items
        list.push("first");
        list.push("second");

        // Insert way beyond bounds
        let result = list.insert(10, "invalid");
        assert!(result.is_err());
        match result.unwrap_err() {
            CRDTError::ListIndexOutOfBounds { index, len } => {
                assert_eq!(index, 10);
                assert_eq!(len, 2);
            }
            _ => panic!("Expected ListIndexOutOfBounds error"),
        }
    }

    #[test]
    fn test_node_list_insert_mixed_with_push() {
        let mut list = Array::new();

        // Mix insert and push operations
        let idx1 = list.push("a");
        assert!(list.insert(1, "c").is_ok());
        assert!(list.insert(1, "b").is_ok());
        let idx4 = list.push("d");

        assert_eq!(idx1, 0);
        assert_eq!(idx4, 3);
        assert_eq!(list.len(), 4);

        // Verify order
        assert_eq!(list.get(0).unwrap().as_text(), Some("a"));
        assert_eq!(list.get(1).unwrap().as_text(), Some("b"));
        assert_eq!(list.get(2).unwrap().as_text(), Some("c"));
        assert_eq!(list.get(3).unwrap().as_text(), Some("d"));
    }

    #[test]
    fn test_node_list_insert_maintains_stable_ordering() {
        let mut list = Array::new();

        // Add initial items
        list.push("first");
        list.push("third");

        // Insert in middle
        assert!(list.insert(1, "second").is_ok());

        // Create another list with same operations
        let mut list2 = Array::new();
        list2.push("first");
        list2.push("third");
        assert!(list2.insert(1, "second").is_ok());

        // Both lists should have same order
        assert_eq!(list.len(), list2.len());
        for i in 0..list.len() {
            assert_eq!(
                list.get(i).unwrap().as_text(),
                list2.get(i).unwrap().as_text()
            );
        }
    }

    #[test]
    fn test_node_list_insert_with_nested_values() {
        let mut list = Array::new();

        // Insert nested structures
        let mut nested_node = Map::new();
        nested_node.set("name", "Alice");
        nested_node.set("age", 30);

        let mut nested_list = Array::new();
        nested_list.push(1);
        nested_list.push(2);
        nested_list.push(3);

        assert!(list.insert(0, nested_node).is_ok());
        assert!(list.insert(1, nested_list).is_ok());

        assert_eq!(list.len(), 2);
        assert!(list.get(0).unwrap().as_node().is_some());
        assert!(list.get(1).unwrap().as_list().is_some());

        // Verify nested content
        let node = list.get(0).unwrap().as_node().unwrap();
        assert_eq!(node.get_text("name"), Some("Alice"));
        assert_eq!(node.get_int("age"), Some(30));

        let inner_list = list.get(1).unwrap().as_list().unwrap();
        assert_eq!(inner_list.len(), 3);
        assert_eq!(inner_list.get(0).unwrap().as_int(), Some(1));
    }

    #[test]
    fn test_node_list_error_integration() {
        let mut list = Array::new();

        let result = list.insert(5, "test");
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.is_list_error());
        assert!(!error.is_merge_error());
        assert!(!error.is_serialization_error());
        assert!(!error.is_type_error());
        assert!(!error.is_array_error());
        assert!(!error.is_map_error());
        assert!(!error.is_nested_error());
        assert!(!error.is_not_found_error());
    }

    #[test]
    fn test_node_list_push_after_removals() {
        let mut list = Array::new();

        // Add items
        list.push("a");
        list.push("b");
        list.push("c");

        // Remove middle item
        list.remove(1);
        assert_eq!(list.len(), 2);

        // Push should still return correct index
        let idx = list.push("d");
        assert_eq!(idx, 2);
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(2).unwrap().as_text(), Some("d"));
    }

    #[test]
    fn test_node_list_insert_after_removals() {
        let mut list = Array::new();

        // Add items
        list.push("a");
        list.push("b");
        list.push("c");

        // Remove middle item
        list.remove(1);
        assert_eq!(list.len(), 2);

        // Insert should work correctly
        assert!(list.insert(1, "new").is_ok());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(1).unwrap().as_text(), Some("new"));
    }
}
