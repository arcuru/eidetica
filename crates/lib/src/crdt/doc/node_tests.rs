#[cfg(test)]
mod test_node {
    use crate::crdt::{
        CRDTError, Doc,
        doc::{List, Value, list::Position},
        traits::CRDT,
    };

    // Minimal unit tests for internal implementation details not accessible from integration tests
    // Most functionality is now comprehensively tested in integration tests under tests/it/crdt/

    #[test]
    fn test_doc_tombstone_handling() {
        let mut doc = Doc::new();
        doc.set("key1", "value1");
        doc.set("key2", "value2");
        doc.remove("key1");

        // Test that tombstones are tracked internally but hidden from public API
        assert!(doc.is_tombstone("key1")); // Tombstone detectable via is_tombstone
        assert!(!doc.is_tombstone("key2")); // Not a tombstone

        // Public API should hide the tombstone
        assert_eq!(doc.get("key1"), None);
        assert_eq!(doc.get("key2"), Some(&Value::Text("value2".to_string())));

        // len() and iter() should only count non-tombstoned values
        assert_eq!(doc.len(), 1);
        assert_eq!(doc.iter().count(), 1);
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

        let branch_values = vec![Value::Doc(Doc::new()), Value::List(List::new())];

        for value in &branch_values {
            assert!(value.is_branch(), "Value should be branch: {value:?}");
            assert!(!value.is_leaf(), "Value should not be leaf: {value:?}");
        }
    }

    #[test]
    fn test_atomic_doc_merge_is_lww() {
        let mut doc1 = Doc::atomic();
        doc1.set("x", 1);
        doc1.set("y", 2);

        let mut doc2 = Doc::atomic();
        doc2.set("x", 10);
        doc2.set("z", 30);

        let merged = doc1.merge(&doc2).unwrap();
        // LWW: other replaces entirely
        assert_eq!(merged.get_as::<i64>("x"), Some(10));
        assert_eq!(merged.get_as::<i64>("z"), Some(30));
        assert_eq!(merged.get_as::<i64>("y"), None); // not carried from doc1
        // Merged result preserves other's flag (atomic), since it's a clone of other
        assert!(merged.is_atomic());
    }

    #[test]
    fn test_atomic_is_contagious() {
        // left.atomic + non-atomic right → structural merge, result stays atomic
        let mut atomic = Doc::atomic();
        atomic.set("x", 1);

        let mut plain = Doc::new();
        plain.set("x", 99);
        plain.set("y", 2);

        // atomic.merge(plain) — structural merge, result stays atomic
        let merged = atomic.merge(&plain).unwrap();
        assert!(merged.is_atomic()); // contagious
        assert_eq!(merged.get_as::<i64>("x"), Some(99)); // per-field LWW, other wins
        assert_eq!(merged.get_as::<i64>("y"), Some(2)); // added from other

        // plain.merge(atomic) — other is atomic → LWW, clone of other
        let merged = plain.merge(&atomic).unwrap();
        assert!(merged.is_atomic());
        assert_eq!(merged.get_as::<i64>("x"), Some(1)); // LWW from other
        assert_eq!(merged.get_as::<i64>("y"), None); // not carried from plain
    }

    #[test]
    fn test_atomic_contagious_nested() {
        // Nested: parent1 has atomic child, parent2 has non-atomic child
        // atomic child as self → structural merge of fields, result stays atomic
        let mut parent1 = Doc::new();
        let mut child1 = Doc::atomic();
        child1.set("a", 1);
        parent1.set("config", Value::Doc(child1));

        let mut parent2 = Doc::new();
        let mut child2 = Doc::new(); // non-atomic
        child2.set("a", 99);
        child2.set("b", 2);
        parent2.set("config", Value::Doc(child2));

        // parent1.merge(parent2): child1(atomic).merge(child2) → structural, atomic
        let merged = parent1.merge(&parent2).unwrap();
        let config = merged.get("config").unwrap().as_doc().unwrap();
        assert!(config.is_atomic()); // contagious from child1
        assert_eq!(config.get_as::<i64>("a"), Some(99)); // per-field LWW
        assert_eq!(config.get_as::<i64>("b"), Some(2)); // added from child2

        // parent2.merge(parent1): child2.merge(child1(atomic)) → other atomic, LWW
        let merged = parent2.merge(&parent1).unwrap();
        let config = merged.get("config").unwrap().as_doc().unwrap();
        assert!(config.is_atomic());
        assert_eq!(config.get_as::<i64>("a"), Some(1)); // LWW from atomic other
        assert_eq!(config.get_as::<i64>("b"), None); // not carried
    }

    #[test]
    fn test_atomic_merge_is_associative() {
        // Chain: E1 ⊕ E2 ⊕ E3(atomic) ⊕ E4
        // E3 is an atomic config replacement, E4 edits a subfield on top.
        // Associativity: the left-fold and grouped forms must agree.
        //
        // This matters because 3⊕4 must stay atomic (contagious) so that
        // when merged with (1⊕2), the atomic flag triggers LWW and
        // everything before E3 is correctly overwritten.

        let mut e1 = Doc::new();
        e1.set("old_key", "old_value");
        e1.set("x", 1);

        let mut e2 = Doc::new();
        e2.set("y", 2);

        let mut e3 = Doc::atomic();
        e3.set("algorithm", "aes-256");
        e3.set("key_id", "abc123");

        let mut e4 = Doc::new();
        e4.set("key_id", "def456"); // edit subfield of E3's data

        // Left fold: ((E1 ⊕ E2) ⊕ E3) ⊕ E4
        let left_fold = e1
            .merge(&e2)
            .unwrap()
            .merge(&e3)
            .unwrap()
            .merge(&e4)
            .unwrap();

        // Grouped: (E1 ⊕ E2) ⊕ (E3 ⊕ E4)
        let left = e1.merge(&e2).unwrap();
        let right = e3.merge(&e4).unwrap();

        // E3 ⊕ E4: self=E3(atomic), other=E4(non-atomic) → structural, atomic
        assert!(right.is_atomic()); // contagious: E3's atomic flag survives
        assert_eq!(right.get_as::<&str>("algorithm"), Some("aes-256"));
        assert_eq!(right.get_as::<&str>("key_id"), Some("def456")); // E4's edit applied

        let grouped = left.merge(&right).unwrap();

        // Both forms produce the same result
        assert_eq!(left_fold, grouped);

        // Result is E3's data with E4's edits, atomic, old data gone
        assert!(grouped.is_atomic());
        assert_eq!(grouped.get_as::<&str>("algorithm"), Some("aes-256"));
        assert_eq!(grouped.get_as::<&str>("key_id"), Some("def456"));
        assert_eq!(grouped.get_as::<&str>("old_key"), None); // overwritten by atomic
        assert_eq!(grouped.get_as::<i64>("x"), None);
        assert_eq!(grouped.get_as::<i64>("y"), None);

        // Also verify right-associated: E1 ⊕ (E2 ⊕ (E3 ⊕ E4))
        let right_fold = e1.merge(&e2.merge(&right).unwrap()).unwrap();
        assert_eq!(left_fold, right_fold);
    }

    #[test]
    fn test_atomic_nested_in_structural() {
        let mut parent1 = Doc::new();
        let mut child1 = Doc::atomic();
        child1.set("a", 1);
        child1.set("b", 2);
        parent1.set("config", Value::Doc(child1));
        parent1.set("name", "alice");

        let mut parent2 = Doc::new();
        let mut child2 = Doc::atomic();
        child2.set("a", 10);
        child2.set("c", 30);
        parent2.set("config", Value::Doc(child2));
        parent2.set("age", 25);

        let merged = parent1.merge(&parent2).unwrap();
        // Parent merges structurally
        assert_eq!(merged.get_as::<&str>("name"), Some("alice"));
        assert_eq!(merged.get_as::<i64>("age"), Some(25));
        // Child merges atomically (LWW)
        let config = merged.get("config").unwrap().as_doc().unwrap();
        assert_eq!(config.get_as::<i64>("a"), Some(10));
        assert_eq!(config.get_as::<i64>("c"), Some(30));
        assert_eq!(config.get_as::<i64>("b"), None); // not carried from child1
    }

    #[test]
    fn test_atomic_serialization_roundtrip() {
        let mut atomic = Doc::atomic();
        atomic.set("key", "value");

        let json = serde_json::to_string(&atomic).unwrap();
        assert!(json.contains("\"_a\":true"));

        let deserialized: Doc = serde_json::from_str(&json).unwrap();
        assert!(deserialized.is_atomic());
        assert_eq!(deserialized.get_as::<&str>("key"), Some("value"));

        // Non-atomic doc should not emit _a
        let mut non_atomic = Doc::new();
        non_atomic.set("key", "value");
        let json = serde_json::to_string(&non_atomic).unwrap();
        assert!(!json.contains("\"_a\""));
    }

    #[test]
    fn test_value_type_names() {
        // Test internal type name reporting
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "bool");
        assert_eq!(Value::Int(42).type_name(), "int");
        assert_eq!(Value::Text("test".to_string()).type_name(), "text");
        assert_eq!(Value::Doc(Doc::new()).type_name(), "doc");
        assert_eq!(Value::List(List::new()).type_name(), "list");
        assert_eq!(Value::Deleted.type_name(), "deleted");
    }
}
