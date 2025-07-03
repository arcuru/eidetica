use eidetica::auth::crypto::format_public_key;
use eidetica::auth::types::{AuthKey, KeyStatus, Permission};
use eidetica::backend::InMemoryBackend;
use eidetica::basedb::BaseDB;
use eidetica::crdt::Nested;
use eidetica::subtree::KVStore;

// ===== TEST HELPERS AND MACROS =====

/// Create an AuthKey with commonly used defaults
pub fn auth_key(key_str: &str, permission: Permission, status: KeyStatus) -> AuthKey {
    AuthKey {
        pubkey: key_str.to_string(),
        permissions: permission,
        status,
    }
}

/// Create a DB with keys pre-configured for testing
pub fn setup_test_db_with_keys(
    keys: &[(&str, Permission, KeyStatus)],
) -> (BaseDB, Vec<ed25519_dalek::VerifyingKey>) {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let mut public_keys = Vec::new();
    for (key_id, _permission, _status) in keys {
        let public_key = db.add_private_key(key_id).expect("Failed to add key");
        public_keys.push(public_key);
    }

    (db, public_keys)
}

/// Create a tree with auth settings pre-configured
pub fn setup_authenticated_tree(
    db: &BaseDB,
    keys: &[(&str, Permission, KeyStatus)],
    public_keys: &[ed25519_dalek::VerifyingKey],
) -> eidetica::tree::Tree {
    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();

    for ((key_id, permission, status), public_key) in keys.iter().zip(public_keys.iter()) {
        auth_settings
            .set_json(
                key_id.to_string(),
                auth_key(
                    &format_public_key(public_key),
                    permission.clone(),
                    status.clone(),
                ),
            )
            .unwrap();
    }

    settings.set_map("auth", auth_settings);

    // Find the first key with Admin permissions for tree creation
    let admin_key = keys
        .iter()
        .find(|(_, permission, _)| matches!(permission, Permission::Admin(_)))
        .map(|(key_id, _, _)| *key_id)
        .unwrap_or_else(|| {
            panic!("setup_authenticated_tree requires at least one key with Admin permissions for tree creation")
        });

    db.new_tree(settings, admin_key)
        .expect("Failed to create tree")
}

/// Test that an operation succeeds
pub fn test_operation_succeeds(
    tree: &eidetica::tree::Tree,
    key_id: &str,
    subtree_name: &str,
    test_name: &str,
) {
    let op = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>(subtree_name)
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let result = op.commit();
    assert!(result.is_ok(), "{test_name}: Operation should succeed");
}

/// Test that an operation fails
pub fn test_operation_fails(
    tree: &eidetica::tree::Tree,
    key_id: &str,
    subtree_name: &str,
    test_name: &str,
) {
    let op = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>(subtree_name)
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let result = op.commit();
    assert!(result.is_err(), "{test_name}: Operation should fail");
}

/// Macro for creating multiple similar auth keys
#[macro_export]
macro_rules! create_auth_keys {
    ($(($id:expr, $perm:expr, $status:expr)),+ $(,)?) => {
        vec![
            $(($id, $perm, $status)),+
        ]
    };
}
