use eidetica::backend::database::InMemory;
use eidetica::crdt::doc::Value;
use eidetica::store::DocStore;

const DEFAULT_TEST_KEY_NAME: &str = "test_key";

/// Creates a basic authenticated database with the default test key
pub fn setup_db() -> eidetica::Instance {
    let backend = Box::new(InMemory::new());
    let db = eidetica::Instance::new(backend);
    db.add_private_key(DEFAULT_TEST_KEY_NAME)
        .expect("Failed to add default test key");
    db
}

/// Creates a database without any default keys (for tests that manage keys manually)
pub fn setup_empty_db() -> eidetica::Instance {
    let backend = Box::new(InMemory::new());
    eidetica::Instance::new(backend)
}

/// Creates an authenticated database with a specific key
pub fn setup_db_with_key(key_name: &str) -> eidetica::Instance {
    let backend = Box::new(InMemory::new());
    let db = eidetica::Instance::new(backend);
    db.add_private_key(key_name)
        .expect("Failed to add test key");
    db
}

/// Creates a basic tree using an InMemory database with authentication
pub fn setup_tree() -> eidetica::Database {
    let db = setup_db();
    db.new_tree_default(DEFAULT_TEST_KEY_NAME)
        .expect("Failed to create tree for testing")
}

/// Creates a tree with a specific key
pub fn setup_tree_with_key(key_name: &str) -> eidetica::Database {
    let db = setup_db_with_key(key_name);
    db.new_tree_default(key_name)
        .expect("Failed to create tree for testing")
}

/// Creates a tree and database with a specific key
pub fn setup_db_and_tree_with_key(
    key_name: &str,
) -> (eidetica::Instance, eidetica::Database) {
    let db = setup_db_with_key(key_name);
    let tree = db
        .new_tree_default(key_name)
        .expect("Failed to create tree for testing");
    (db, tree)
}

/// Creates a tree with initial settings using Map with authentication
pub fn setup_tree_with_settings(settings: &[(&str, &str)]) -> eidetica::Database {
    let db = setup_db();
    let tree = db
        .new_tree_default(DEFAULT_TEST_KEY_NAME)
        .expect("Failed to create tree");

    // Add the user settings through an operation
    let op = tree.new_operation().expect("Failed to create operation");
    {
        let settings_store = op
            .get_subtree::<DocStore>("_settings")
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

/// Helper for common assertions around DocStore value retrieval
pub fn assert_dict_value(store: &DocStore, key: &str, expected: &str) {
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
