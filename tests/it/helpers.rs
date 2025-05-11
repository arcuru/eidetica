use eideticadb::backend::InMemoryBackend;
use eideticadb::basedb::BaseDB;
use eideticadb::data::{KVNested, NestedValue};
use eideticadb::subtree::KVStore;

/// Creates a basic tree using an InMemoryBackend
pub fn setup_tree() -> eideticadb::Tree {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);
    db.new_tree_default()
        .expect("Failed to create tree for testing")
}

/// Creates a tree with initial settings using KVNested
pub fn setup_tree_with_settings(settings: &[(&str, &str)]) -> eideticadb::Tree {
    let mut kv_nested = KVNested::new();

    for (key, value) in settings {
        kv_nested.set_string(*key, *value);
    }

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);
    db.new_tree(kv_nested)
        .expect("Failed to create tree with settings")
}

/// Creates a KVNested with the specified key-value pairs
pub fn create_kvnested(values: &[(&str, &str)]) -> KVNested {
    let mut kv = KVNested::new();

    for (key, value) in values {
        kv.set_string(*key, *value);
    }

    kv
}

/// Creates a nested KVNested structure
pub fn create_nested_kvnested(structure: &[(&str, &[(&str, &str)])]) -> KVNested {
    let mut root = KVNested::new();

    for (outer_key, inner_values) in structure {
        let mut inner = KVNested::new();

        for (inner_key, inner_value) in *inner_values {
            inner.set_string(*inner_key, *inner_value);
        }

        root.set_map(*outer_key, inner);
    }

    root
}

/// Helper for common assertions around KVStore value retrieval
pub fn assert_kvstore_value(store: &KVStore, key: &str, expected: &str) {
    match store
        .get(key)
        .unwrap_or_else(|_| panic!("Failed to get key {}", key))
    {
        NestedValue::String(value) => assert_eq!(value, expected),
        _ => panic!("Expected string value for key {}", key),
    }
}

/// Helper for checking NotFound errors
pub fn assert_key_not_found(result: Result<NestedValue, eideticadb::Error>) {
    match result {
        Err(eideticadb::Error::NotFound) => (), // Expected
        other => panic!("Expected NotFound error, got {:?}", other),
    }
}

/// Helper to create a KVOverWrite with initial data
pub fn create_kvoverwrite(values: &[(&str, &str)]) -> eideticadb::data::KVOverWrite {
    let mut kv = eideticadb::data::KVOverWrite::new();

    for (key, value) in values {
        kv.set(*key, *value);
    }

    kv
}

/// Helper to check deep nested values inside a KVNested structure
pub fn assert_nested_value(kv: &KVNested, path: &[&str], expected: &str) {
    let mut current = kv;
    let last_idx = path.len() - 1;

    // Navigate through the nested maps
    for key in path.iter().take(last_idx) {
        match current.get(key) {
            Some(NestedValue::Map(map)) => current = map,
            Some(other) => panic!("Expected map at path element '{}', got {:?}", key, other),
            None => panic!("Path element '{}' not found in nested structure", key),
        }
    }

    // Check final value
    let final_key = path[last_idx];
    match current.get(final_key) {
        Some(NestedValue::String(value)) => assert_eq!(value, expected),
        Some(other) => panic!(
            "Expected string at path end '{}', got {:?}",
            final_key, other
        ),
        None => panic!(
            "Final path element '{}' not found in nested structure",
            final_key
        ),
    }
}

/// Helper to validate that a path is deleted (has tombstone or is missing)
pub fn assert_path_deleted(kv: &KVNested, path: &[&str]) {
    if path.is_empty() {
        panic!("Empty path provided to assert_path_deleted");
    }

    let mut current = kv;
    let last_idx = path.len() - 1;

    // If early path doesn't exist, that's fine - the path is deleted
    for key in path.iter().take(last_idx) {
        match current.get(key) {
            Some(NestedValue::Map(map)) => current = map,
            Some(NestedValue::Deleted) => return, // Found tombstone
            Some(other) => panic!(
                "Unexpected value at path element '{}', got {:?}",
                key, other
            ),
            None => return, // Path doesn't exist, which is valid for a deleted path
        }
    }

    // Check final key
    let final_key = path[last_idx];
    match current.get(final_key) {
        Some(NestedValue::Deleted) => (), // Tombstone, as expected
        None => (),                       // Key doesn't exist, which is valid
        Some(other) => panic!(
            "Expected tombstone at path end '{}', got {:?}",
            final_key, other
        ),
    }
}

/// Creates a tree with multiple KVStore subtrees and preset values
pub fn setup_tree_with_multiple_kvstores(
    subtree_values: &[(&str, &[(&str, &str)])],
) -> eideticadb::Tree {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    for (subtree_name, values) in subtree_values {
        let kv_store = op
            .get_subtree::<KVStore>(subtree_name)
            .unwrap_or_else(|_| panic!("Failed to get KVStore '{}'", subtree_name));

        for (key, value) in *values {
            kv_store
                .set(*key, *value)
                .unwrap_or_else(|_| panic!("Failed to set value for '{}.{}'", subtree_name, key));
        }
    }

    op.commit().expect("Failed to commit operation");
    tree
}
