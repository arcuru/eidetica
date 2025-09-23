use ed25519_dalek::VerifyingKey;
use eidetica::{
    Database, Instance,
    auth::{
        crypto::format_public_key,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyStatus, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    backend::database::InMemory,
    crdt::Doc,
    entry::ID,
    store::DocStore,
};

// Helper functions for auth testing
//
// This module provides utility functions for testing authentication features
// including key generation, permission checking, delegation, and auth-related operations.

// ===== BASIC SETUP HELPERS =====

/// Create a database with a single test key
pub fn setup_db() -> Instance {
    Instance::new(Box::new(InMemory::new()))
}

/// Create a database with a pre-added test key
pub fn setup_db_with_key(key_name: &str) -> Instance {
    let db = setup_db();
    let _ = db.add_private_key(key_name).expect("Failed to add key");
    db
}

/// Create a database and tree with a test key
pub fn setup_db_and_tree_with_key(key_name: &str) -> (Instance, Database) {
    let db = setup_db_with_key(key_name);
    let tree = db
        .new_database(Doc::new(), key_name)
        .expect("Failed to create tree");
    (db, tree)
}

/// Create an AuthKey with commonly used defaults
pub fn auth_key(key_str: &str, permission: Permission, status: KeyStatus) -> AuthKey {
    // Use provided key if it's valid (or the wildcard "*") otherwise generate one.
    let chosen = if key_str == "*" || eidetica::auth::crypto::parse_public_key(key_str).is_ok() {
        key_str.to_string()
    } else {
        let (_, verifying_key) = eidetica::auth::crypto::generate_keypair();
        format_public_key(&verifying_key)
    };

    AuthKey::new(chosen, permission, status).unwrap()
}

/// Create a DB with keys pre-configured for testing
pub fn setup_test_db_with_keys(
    keys: &[(&str, Permission, KeyStatus)],
) -> (Instance, Vec<VerifyingKey>) {
    let backend = Box::new(InMemory::new());
    let db = Instance::new(backend);

    let mut public_keys = Vec::new();
    for (key_name, _permission, _status) in keys {
        let public_key = db.add_private_key(key_name).expect("Failed to add key");
        public_keys.push(public_key);
    }

    (db, public_keys)
}

/// Create a tree with auth settings pre-configured
pub fn setup_authenticated_tree(
    db: &Instance,
    keys: &[(&str, Permission, KeyStatus)],
    public_keys: &[VerifyingKey],
) -> Database {
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();

    for ((key_name, permission, status), public_key) in keys.iter().zip(public_keys.iter()) {
        auth_settings
            .set_json(
                key_name.to_string(),
                auth_key(
                    &format_public_key(public_key),
                    permission.clone(),
                    status.clone(),
                ),
            )
            .unwrap();
    }

    settings.set_node("auth", auth_settings);

    // Find the first key with Admin permissions for tree creation
    let admin_key = keys
        .iter()
        .find(|(_, permission, _)| matches!(permission, Permission::Admin(_)))
        .map(|(key_name, _, _)| *key_name)
        .unwrap_or_else(|| {
            panic!("setup_authenticated_tree requires at least one key with Admin permissions for tree creation")
        });

    db.new_database(settings, admin_key)
        .expect("Failed to create tree")
}

// ===== DELEGATION HELPERS =====

/// Create a complete authentication environment with multiple keys and permission levels
pub fn setup_complete_auth_environment(
    keys: &[(&str, Permission, KeyStatus)],
) -> (Instance, Database, Vec<VerifyingKey>) {
    let db = setup_db();
    let mut public_keys = Vec::new();

    // Add all keys to the database
    for (key_name, _, _) in keys {
        let public_key = db
            .add_private_key(key_name)
            .expect("Failed to add private key");
        public_keys.push(public_key);
    }

    // Create auth settings
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();

    for ((key_name, permission, status), public_key) in keys.iter().zip(public_keys.iter()) {
        auth_settings
            .set_json(
                *key_name,
                AuthKey::new(
                    format_public_key(public_key),
                    permission.clone(),
                    status.clone(),
                )
                .unwrap(),
            )
            .unwrap();
    }

    settings.set_node("auth", auth_settings);

    // Use the first admin key as the creator
    let admin_key = keys
        .iter()
        .find(|(_, perm, _)| perm.can_admin())
        .map(|(name, _, _)| *name)
        .expect("No admin key found");

    let tree = db
        .new_database(settings, admin_key)
        .expect("Failed to create authenticated tree");

    (db, tree, public_keys)
}

/// Create a delegated tree with specified keys and permissions
pub fn create_delegated_tree(
    db: &Instance,
    keys: &[(&str, Permission, KeyStatus)],
    creator_key: &str,
) -> eidetica::Result<Database> {
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();

    for (key_name, permission, status) in keys {
        let public_key = db.add_private_key(key_name)?;
        auth_settings
            .set_json(
                *key_name,
                AuthKey::new(
                    format_public_key(&public_key),
                    permission.clone(),
                    status.clone(),
                )
                .unwrap(),
            )
            .unwrap();
    }

    settings.set_node("auth", auth_settings);
    db.new_database(settings, creator_key)
}

/// Create delegation reference for a tree
pub fn create_delegation_ref(
    tree: &Database,
    max_permission: Permission,
    min_permission: Option<Permission>,
) -> eidetica::Result<DelegatedTreeRef> {
    let tips = tree.get_tips()?;
    Ok(DelegatedTreeRef {
        permission_bounds: PermissionBounds {
            max: max_permission,
            min: min_permission,
        },
        tree: TreeReference {
            root: tree.root_id().clone(),
            tips,
        },
    })
}

/// Create a delegation path with specified steps
pub fn create_delegation_path(steps: &[(&str, Option<Vec<ID>>)]) -> SigKey {
    let delegation_steps: Vec<DelegationStep> = steps
        .iter()
        .map(|(key, tips)| DelegationStep {
            key: key.to_string(),
            tips: tips.clone(),
        })
        .collect();

    SigKey::DelegationPath(delegation_steps)
}

/// Create nested delegation chain for testing complex scenarios
pub struct DelegationChain {
    #[allow(dead_code)]
    pub db: Instance,
    pub trees: Vec<Database>,
    pub keys: Vec<String>,
}

impl DelegationChain {
    pub fn new(levels: usize) -> eidetica::Result<Self> {
        let db = setup_db();
        let mut trees = Vec::new();
        let mut keys = Vec::new();

        for i in 0..levels {
            let key_name = format!("level_{i}_admin");
            let public_key = db.add_private_key(&key_name)?;
            keys.push(key_name.clone());

            let mut settings = Doc::new();
            let mut auth_settings = Doc::new();

            auth_settings
                .set_json(
                    &key_name,
                    AuthKey::active(format_public_key(&public_key), Permission::Admin(i as u32))
                        .unwrap(),
                )
                .unwrap();

            settings.set_node("auth", auth_settings);
            let tree = db.new_database(settings, &key_name)?;
            trees.push(tree);
        }

        Ok(DelegationChain { db, trees, keys })
    }

    pub fn create_chain_delegation(&self, final_key: &str) -> SigKey {
        let mut steps = Vec::new();

        for (i, tree) in self.trees.iter().enumerate() {
            let tips = tree.get_tips().expect("Failed to get tips");
            steps.push(DelegationStep {
                key: format!("delegate_level_{i}"),
                tips: Some(tips),
            });
        }

        steps.push(DelegationStep {
            key: final_key.to_string(),
            tips: None,
        });

        SigKey::DelegationPath(steps)
    }
}

// ===== ASSERTION HELPERS =====

/// Test that an operation succeeds
pub fn test_operation_succeeds(
    tree: &Database,
    key_name: &str,
    subtree_name: &str,
    test_name: &str,
) {
    let op = tree
        .new_authenticated_operation(key_name)
        .expect("Failed to create operation");
    let store = op
        .get_store::<DocStore>(subtree_name)
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let result = op.commit();
    assert!(result.is_ok(), "{test_name}: Operation should succeed");
}

/// Test that an operation fails
pub fn test_operation_fails(tree: &Database, key_name: &str, subtree_name: &str, test_name: &str) {
    let op = tree
        .new_authenticated_operation(key_name)
        .expect("Failed to create operation");
    let store = op
        .get_store::<DocStore>(subtree_name)
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let result = op.commit();
    assert!(result.is_err(), "{test_name}: Operation should fail");
}

/// Assert that permission resolution works correctly
pub fn assert_permission_resolution(
    validator: &mut AuthValidator,
    sig_key: &SigKey,
    settings: &Doc,
    backend: Option<&std::sync::Arc<dyn eidetica::backend::BackendDB>>,
    expected_permission: Permission,
    expected_status: KeyStatus,
) {
    let result = validator
        .resolve_sig_key(sig_key, settings, backend)
        .expect("Permission resolution should succeed");

    assert_eq!(
        result.effective_permission, expected_permission,
        "Permission mismatch for {sig_key:?}"
    );
    assert_eq!(
        result.key_status, expected_status,
        "Status mismatch for {sig_key:?}"
    );
}

/// Assert that permission resolution fails with expected error pattern
pub fn assert_permission_resolution_fails(
    validator: &mut AuthValidator,
    sig_key: &SigKey,
    settings: &Doc,
    backend: Option<&std::sync::Arc<dyn eidetica::backend::BackendDB>>,
    expected_error_pattern: &str,
) {
    let result = validator.resolve_sig_key(sig_key, settings, backend);
    assert!(
        result.is_err(),
        "Permission resolution should fail for {sig_key:?}"
    );

    let error_msg = result.unwrap_err().to_string().to_lowercase();
    assert!(
        error_msg.contains(expected_error_pattern),
        "Error message '{error_msg}' doesn't contain expected pattern '{expected_error_pattern}'"
    );
}

/// Test operation permissions for a specific key and subtree
pub fn assert_operation_permissions(
    tree: &Database,
    key_name: &str,
    subtree_name: &str,
    should_succeed: bool,
    test_description: &str,
) {
    let op = tree
        .new_authenticated_operation(key_name)
        .expect("Failed to create operation");
    let store = op
        .get_store::<DocStore>(subtree_name)
        .expect("Failed to get subtree");
    store
        .set("test", test_description)
        .expect("Failed to set value");

    let result = op.commit();
    if should_succeed {
        assert!(
            result.is_ok(),
            "Operation should succeed: {test_description} - {result:?}"
        );
    } else {
        assert!(result.is_err(), "Operation should fail: {test_description}");
    }
}

// ===== MACROS =====

/// Macro for creating multiple similar auth keys
#[macro_export]
macro_rules! create_auth_keys {
    ($(($id:expr, $perm:expr, $status:expr)),+ $(,)?) => {
        vec![
            $(($id, $perm, $status)),+
        ]
    };
}
