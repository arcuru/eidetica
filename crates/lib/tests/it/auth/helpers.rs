use ed25519_dalek::VerifyingKey;
use eidetica::{
    Database, Instance,
    auth::{
        crypto::format_public_key,
        settings::AuthSettings,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyStatus, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    crdt::Doc,
    entry::ID,
    instance::LegacyInstanceOps,
    store::DocStore,
    user::User,
};

// Helper functions for auth testing
//
// This module provides utility functions for testing authentication features
// including key generation, permission checking, delegation, and auth-related operations.
//
// ## Key Naming and Aliases
//
// When using User API, `user.new_database()` automatically bootstraps auth by adding
// the signing key with its public key string as the name (e.g., "Ed25519:abc123...").
// For delegation paths to use readable names, auth settings can contain multiple names
// for the same public key:
//
// - Bootstrap entry: `auth["Ed25519:abc..."] = AuthKey(pubkey, Admin(0))`
// - Friendly alias: `auth["alice_key"] = AuthKey(pubkey, Write(2))`
//
// Both entries are valid and can have different permissions. Delegation paths reference
// keys by their name in auth settings, so friendly name aliases enable readable delegation
// chains like `["alice_key", "bob_key"]` instead of `["Ed25519:abc...", "Ed25519:xyz..."]`.
//
// Use `configure_database_auth()` to add friendly name aliases after database creation.

// ===== BASIC SETUP HELPERS =====

/// Create a database with a single test key
pub fn setup_db() -> Instance {
    crate::helpers::test_instance()
}

/// Create an instance with user and tree with a test key using User API
///
/// Returns (instance, user, tree, key_id) where key_id can be used for authenticated operations.
/// The database is automatically bootstrapped with auth settings containing the key under
/// its public key string name (e.g., "Ed25519:abc123..."), not the friendly display name.
///
/// Note: `key_name` is stored in the UserKey metadata but NOT in the database's auth settings.
/// For delegation to work with friendly names, use `configure_database_auth()` to add aliases.
pub fn setup_user_and_tree_with_key(
    username: &str,
    key_name: &str,
) -> (Instance, User, Database, String) {
    let (instance, mut user) = crate::helpers::test_instance_with_user(username);

    // Add a key with the specified display name
    let key_id = user
        .add_private_key(Some(key_name))
        .expect("Failed to add key");

    // Create database with that key - automatically bootstraps auth with:
    // - Key name: key_id (the public key string)
    // - Permission: Admin(0)
    // - Status: Active
    let tree = user
        .create_database(Doc::new(), &key_id)
        .expect("Failed to create tree");

    (instance, user, tree, key_id)
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

/// Create a user with multiple keys pre-configured for testing using User API
/// Returns (instance, user, key_ids) where key_ids[i] corresponds to key_names[i]
pub fn setup_test_user_with_keys(
    username: &str,
    key_names: &[&str],
) -> (Instance, User, Vec<String>) {
    let (instance, mut user) = crate::helpers::test_instance_with_user(username);

    let mut key_ids = Vec::new();

    // First key is the default key (already created)
    if !key_names.is_empty() {
        let default_key_id = user
            .get_default_key()
            .expect("User should have default key");
        key_ids.push(default_key_id);

        // Add additional keys
        for key_name in key_names.iter().skip(1) {
            let key_id = user
                .add_private_key(Some(key_name))
                .expect("Failed to add key");
            key_ids.push(key_id);
        }
    }

    (instance, user, key_ids)
}

/// Helper to configure database auth settings via SettingsStore API
///
/// Creates friendly name aliases for keys in the database's auth settings.
/// When using User API, `user.new_database()` automatically adds the signing key
/// with the public key string as the name (e.g., "Ed25519:abc123...").
/// This function adds additional entries with friendly names that reference the
/// same public key, enabling delegation paths to use readable names.
///
/// Auth settings can contain multiple names for the same public key:
/// - `auth["Ed25519:abc..."] = AuthKey(pubkey, Admin(0))` - Bootstrap entry
/// - `auth["alice_key"] = AuthKey(pubkey, Write(2))` - Friendly name alias
///
/// Both entries are valid and can have different permissions. Delegation paths
/// reference keys by their name in the auth settings, so friendly names enable
/// readable delegation chains like: `["alice_key", "bob_key"]` instead of
/// `["Ed25519:abc...", "Ed25519:xyz..."]`.
///
/// # Arguments
/// * `database` - The database to configure
/// * `auth_config` - Array of (display_name, key_id, permission, status) tuples
pub fn configure_database_auth(
    database: &Database,
    auth_config: &[(&str, &str, Permission, KeyStatus)],
) -> eidetica::Result<()> {
    let op = database.new_transaction()?;
    {
        let settings = op.get_settings()?;
        settings.update_auth_settings(|auth| {
            for (display_name, key_id, permission, status) in auth_config {
                let public_key = eidetica::auth::crypto::parse_public_key(key_id)?;
                let auth_key = AuthKey::new(
                    format_public_key(&public_key),
                    permission.clone(),
                    status.clone(),
                )?;
                auth.add_key(*display_name, auth_key)?;
            }
            Ok(())
        })?;
    }
    op.commit()?;
    Ok(())
}

/// Create a DB with keys pre-configured for testing (uses deprecated API for auth testing)
pub fn setup_test_db_with_keys(
    keys: &[(&str, Permission, KeyStatus)],
) -> (Instance, Vec<VerifyingKey>) {
    let db = crate::helpers::test_instance();

    let mut public_keys = Vec::new();
    for (key_name, _permission, _status) in keys {
        let public_key = db.add_private_key(key_name).expect("Failed to add key");
        public_keys.push(public_key);
    }

    (db, public_keys)
}

/// Create a tree with auth settings pre-configured (uses deprecated API for auth testing)
#[allow(deprecated)]
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
                key_name,
                auth_key(
                    &format_public_key(public_key),
                    permission.clone(),
                    status.clone(),
                ),
            )
            .unwrap();
    }

    settings.set_doc("auth", auth_settings);

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

/// Create a complete authentication environment with multiple keys and permission levels using User API
///
/// Returns (instance, user, database, key_ids) where key_ids[i] corresponds to keys[i].
///
/// This function creates friendly name aliases for all keys in the database's auth settings.
/// The bootstrap process adds one key with its public key string as the name, then this
/// function adds all keys (including the bootstrap key) with their friendly names and
/// specified permissions. This enables delegation paths to reference keys by readable names.
///
/// Auth settings will contain:
/// - `auth[key_id] = AuthKey(pubkey, Admin(0))` - Bootstrap entry (public key string name)
/// - `auth[friendly_name] = AuthKey(pubkey, specified_perm)` - Friendly name alias for each key
pub fn setup_complete_auth_environment_with_user(
    username: &str,
    keys: &[(&str, Permission, KeyStatus)],
) -> (Instance, User, Database, Vec<String>) {
    // Extract key display names
    let key_names: Vec<&str> = keys.iter().map(|(name, _, _)| *name).collect();

    let (instance, mut user, key_ids) = setup_test_user_with_keys(username, &key_names);

    // Find an Admin key for tree creation
    let admin_key_idx = keys
        .iter()
        .position(|(_, perm, _)| matches!(perm, Permission::Admin(_)))
        .expect("At least one Admin key is required");
    let admin_key_id = &key_ids[admin_key_idx];

    // Create database - automatically bootstraps auth with admin key as Admin(0)
    let database = user
        .create_database(Doc::new(), admin_key_id)
        .expect("Failed to create database");

    // Add friendly name aliases for ALL keys
    // Bootstrap added: auth[key_id] = AuthKey(pubkey, Admin(0))
    // We add: auth[friendly_name] = AuthKey(pubkey, specified_permission)
    // These are both valid - same key, different names/permissions
    let auth_config: Vec<(&str, &str, Permission, KeyStatus)> = keys
        .iter()
        .zip(key_ids.iter())
        .map(|((name, perm, status), key_id)| {
            (*name, key_id.as_str(), perm.clone(), status.clone())
        })
        .collect();

    configure_database_auth(&database, &auth_config).expect("Failed to configure auth");

    (instance, user, database, key_ids)
}

/// Create a delegated tree with specified keys and permissions using User API
///
/// Returns the created database and a vector of key_ids corresponding to the input keys.
///
/// This function is specifically designed for delegation testing. It creates friendly name
/// aliases for all keys in the database's auth settings, enabling delegation paths to
/// reference keys by readable names (e.g., "delegated_user") instead of public key strings.
///
/// The bootstrap process adds the admin key with its public key string as the name,
/// then this function adds all keys (including the bootstrap key) with their friendly
/// names and specified permissions.
///
/// Auth settings will contain both:
/// - `auth[key_id] = AuthKey(pubkey, Admin(0))` - Bootstrap entry
/// - `auth[friendly_name] = AuthKey(pubkey, specified_perm)` - Friendly name alias for each key
pub fn create_delegated_tree_with_user(
    user: &mut User,
    keys: &[(&str, Permission, KeyStatus)],
) -> eidetica::Result<(Database, Vec<String>)> {
    let mut key_ids = Vec::new();

    // Use existing default key for first key, or create new ones
    for (i, (key_name, _, _)) in keys.iter().enumerate() {
        let key_id = if i == 0 {
            // Use default key for first entry
            user.get_default_key()?
        } else {
            // Create additional keys
            user.add_private_key(Some(key_name))?
        };
        key_ids.push(key_id);
    }

    // Find an Admin key for tree creation
    let admin_key_idx = keys
        .iter()
        .position(|(_, perm, _)| matches!(perm, Permission::Admin(_)))
        .expect("At least one Admin key is required to create tree");
    let admin_key_id = &key_ids[admin_key_idx];

    // Create database - automatically bootstraps auth with admin key as Admin(0)
    let database = user.create_database(Doc::new(), admin_key_id)?;

    // Add friendly name aliases for ALL keys
    // Bootstrap added: auth[key_id] = AuthKey(pubkey, Admin(0))
    // We add: auth[friendly_name] = AuthKey(pubkey, specified_permission)
    // These are both valid - same key, different names/permissions
    let auth_config: Vec<(&str, &str, Permission, KeyStatus)> = keys
        .iter()
        .zip(key_ids.iter())
        .map(|((name, perm, status), key_id)| {
            (*name, key_id.as_str(), perm.clone(), status.clone())
        })
        .collect();

    configure_database_auth(&database, &auth_config)?;

    Ok((database, key_ids))
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
    /// Create delegation chain using User API
    ///
    /// Returns chain where keys[i] contains the display name for the key used in trees[i].
    ///
    /// Note: This helper does NOT add friendly name aliases to database auth settings.
    /// Each database only contains the bootstrap entry with the public key string as the name.
    /// The `create_chain_delegation()` method uses hardcoded delegation step names
    /// (`"delegate_level_{i}"`) that won't match any keys in the auth settings.
    pub fn new_with_user(username: &str, levels: usize) -> eidetica::Result<Self> {
        let (db, mut user) = crate::helpers::test_instance_with_user(username);
        let mut trees = Vec::new();
        let mut keys = Vec::new(); // Will store display names

        for i in 0..levels {
            let display_name = format!("level_{i}_admin");

            let key_id = if i == 0 {
                // Use default key for first level
                user.get_default_key()?
            } else {
                // Add new key for subsequent levels
                user.add_private_key(Some(&display_name))?
            };

            keys.push(display_name.clone()); // Store display name for delegation paths

            // Create database - automatically bootstraps auth with:
            // - Key name: key_id (the public key string)
            // - Permission: Admin(0)
            // - Status: Active
            // Note: Bootstrap always uses Admin(0), not the level-specific Admin(i)
            let database = user.create_database(Doc::new(), &key_id)?;

            trees.push(database);
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
pub fn test_operation_succeeds(tree: &Database, subtree_name: &str, test_name: &str) {
    let op = tree.new_transaction().expect("Failed to create operation");
    let store = op
        .get_store::<DocStore>(subtree_name)
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let result = op.commit();
    assert!(result.is_ok(), "{test_name}: Operation should succeed");
}

/// Test that an operation fails
pub fn test_operation_fails(tree: &Database, subtree_name: &str, test_name: &str) {
    let op = tree.new_transaction().expect("Failed to create operation");
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
    auth_settings: &AuthSettings,
    instance: Option<&Instance>,
    expected_permission: Permission,
    expected_status: KeyStatus,
) {
    let result = validator
        .resolve_sig_key(sig_key, auth_settings, instance)
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
    auth_settings: &AuthSettings,
    instance: Option<&Instance>,
    expected_error_pattern: &str,
) {
    let result = validator.resolve_sig_key(sig_key, auth_settings, instance);
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

/// Test operation permissions for a specific subtree
pub fn assert_operation_permissions(
    tree: &Database,

    subtree_name: &str,
    should_succeed: bool,
    test_description: &str,
) {
    let op = tree.new_transaction().expect("Failed to create operation");
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
