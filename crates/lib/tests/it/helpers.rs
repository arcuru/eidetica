use eidetica::backend::database::InMemory;
use eidetica::crdt::Map;
use eidetica::crdt::map::Value;
use eidetica::subtree::KVStore;

const DEFAULT_TEST_KEY_ID: &str = "test_key";

/// Creates a basic authenticated database with the default test key
pub fn setup_db() -> eidetica::basedb::BaseDB {
    let backend = Box::new(InMemory::new());
    let db = eidetica::basedb::BaseDB::new(backend);
    db.add_private_key(DEFAULT_TEST_KEY_ID)
        .expect("Failed to add default test key");
    db
}

/// Creates a database without any default keys (for tests that manage keys manually)
pub fn setup_empty_db() -> eidetica::basedb::BaseDB {
    let backend = Box::new(InMemory::new());
    eidetica::basedb::BaseDB::new(backend)
}

/// Creates an authenticated database with a specific key
pub fn setup_db_with_key(key_id: &str) -> eidetica::basedb::BaseDB {
    let backend = Box::new(InMemory::new());
    let db = eidetica::basedb::BaseDB::new(backend);
    db.add_private_key(key_id).expect("Failed to add test key");
    db
}

/// Creates an authenticated database with multiple keys
pub fn setup_db_with_keys(key_ids: &[&str]) -> eidetica::basedb::BaseDB {
    let backend = Box::new(InMemory::new());
    let db = eidetica::basedb::BaseDB::new(backend);
    for key_id in key_ids {
        db.add_private_key(key_id).expect("Failed to add test key");
    }
    db
}

/// Creates a basic tree using an InMemory database with authentication
pub fn setup_tree() -> eidetica::Tree {
    let db = setup_db();
    db.new_tree_default(DEFAULT_TEST_KEY_ID)
        .expect("Failed to create tree for testing")
}

/// Creates a tree with a specific key
pub fn setup_tree_with_key(key_id: &str) -> eidetica::Tree {
    let db = setup_db_with_key(key_id);
    db.new_tree_default(key_id)
        .expect("Failed to create tree for testing")
}

/// Creates a tree and database with a specific key
pub fn setup_db_and_tree_with_key(key_id: &str) -> (eidetica::basedb::BaseDB, eidetica::Tree) {
    let db = setup_db_with_key(key_id);
    let tree = db
        .new_tree_default(key_id)
        .expect("Failed to create tree for testing");
    (db, tree)
}

/// Creates a tree with initial settings using Map with authentication
pub fn setup_tree_with_settings(settings: &[(&str, &str)]) -> eidetica::Tree {
    let db = setup_db();
    let tree = db
        .new_tree_default(DEFAULT_TEST_KEY_ID)
        .expect("Failed to create tree");

    // Add the user settings through an operation
    let op = tree.new_operation().expect("Failed to create operation");
    {
        let settings_store = op
            .get_subtree::<KVStore>("_settings")
            .expect("Failed to get settings subtree");

        for (key, value) in settings {
            settings_store
                .set(*key, *value)
                .expect("Failed to set setting");
        }
    }
    op.commit().expect("Failed to commit settings");

    tree
}

/// Creates a Map with the specified key-value pairs
pub fn create_kvnested(values: &[(&str, &str)]) -> Map {
    let mut kv = Map::new();

    for (key, value) in values {
        kv.set_string(*key, *value);
    }

    kv
}

/// Creates a nested Map structure
pub fn create_nested_kvnested(structure: &[(&str, &[(&str, &str)])]) -> Map {
    let mut root = Map::new();

    for (outer_key, inner_values) in structure {
        let mut inner = Map::new();

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
        .unwrap_or_else(|_| panic!("Failed to get key {key}"))
    {
        Value::Text(value) => assert_eq!(value, expected),
        _ => panic!("Expected text value for key {key}"),
    }
}

/// Helper for checking NotFound errors
pub fn assert_key_not_found(result: Result<Value, eidetica::Error>) {
    match result {
        Err(ref err) if err.is_not_found() => (), // Expected
        other => panic!("Expected NotFound error, got {other:?}"),
    }
}

/// Helper to check deep nested values inside a Map structure
pub fn assert_nested_value(kv: &Map, path: &[&str], expected: &str) {
    let mut current = kv;
    let last_idx = path.len() - 1;

    // Navigate through the nested maps
    for key in path.iter().take(last_idx) {
        match current.get(key) {
            Some(Value::Map(map)) => current = map,
            Some(other) => panic!("Expected map at path element '{key}', got {other:?}"),
            None => panic!("Path element '{key}' not found in nested structure"),
        }
    }

    // Check final value
    let final_key = path[last_idx];
    match current.get(final_key) {
        Some(Value::Text(value)) => assert_eq!(value, expected),
        Some(other) => panic!("Expected string at path end '{final_key}', got {other:?}"),
        None => panic!("Final path element '{final_key}' not found in nested structure"),
    }
}

/// Helper to validate that a path is deleted (has tombstone or is missing)
pub fn assert_path_deleted(kv: &Map, path: &[&str]) {
    if path.is_empty() {
        panic!("Empty path provided to assert_path_deleted");
    }

    let mut current = kv;
    let last_idx = path.len() - 1;

    // If early path doesn't exist, that's fine - the path is deleted
    for key in path.iter().take(last_idx) {
        match current.get(key) {
            Some(Value::Map(map)) => current = map,
            Some(Value::Deleted) => return, // Found tombstone
            Some(other) => panic!("Unexpected value at path element '{key}', got {other:?}"),
            None => return, // Path doesn't exist, which is valid for a deleted path
        }
    }

    // Check final key
    let final_key = path[last_idx];
    match current.get(final_key) {
        Some(Value::Deleted) => (), // Tombstone, as expected
        None => (),                 // Key doesn't exist, which is valid
        Some(other) => panic!("Expected tombstone at path end '{final_key}', got {other:?}"),
    }
}

/// Creates a tree with multiple KVStore subtrees and preset values
pub fn setup_tree_with_multiple_kvstores(
    subtree_values: &[(&str, &[(&str, &str)])],
) -> eidetica::Tree {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    for (subtree_name, values) in subtree_values {
        let kv_store = op
            .get_subtree::<KVStore>(subtree_name)
            .unwrap_or_else(|_| panic!("Failed to get KVStore '{subtree_name}'"));

        for (key, value) in *values {
            kv_store
                .set(*key, *value)
                .unwrap_or_else(|_| panic!("Failed to set value for '{subtree_name}.{key}'"));
        }
    }

    op.commit().expect("Failed to commit operation");
    tree
}
