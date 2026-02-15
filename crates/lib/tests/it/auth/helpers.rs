use eidetica::{
    Database, Instance, Result,
    auth::{
        crypto::{format_public_key, parse_public_key},
        settings::AuthSettings,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, KeyStatus, Permission,
            PermissionBounds, SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    crdt::Doc,
    entry::ID,
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

/// Create an instance with user and tree with a test key using User API
///
/// Returns (instance, user, tree, key_id) where key_id can be used for authenticated operations.
/// The database is automatically bootstrapped with auth settings containing the key under
/// its public key string name (e.g., "Ed25519:abc123..."), not the friendly display name.
///
/// Note: `key_name` is stored in the UserKey metadata but NOT in the database's auth settings.
/// For delegation to work with friendly names, use `configure_database_auth()` to add aliases.
pub async fn setup_user_and_tree_with_key(
    username: &str,
    key_name: &str,
) -> (Instance, User, Database, String) {
    let (instance, mut user) = crate::helpers::test_instance_with_user(username).await;

    // Add a key with the specified display name
    let key_id = user
        .add_private_key(Some(key_name))
        .await
        .expect("Failed to add key");

    // Create database with that key - automatically bootstraps auth with:
    // - Key name: key_id (the public key string)
    // - Permission: Admin(0)
    // - Status: Active
    let tree = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");

    (instance, user, tree, key_id)
}

/// Create an AuthKey with commonly used defaults
pub fn auth_key(permission: Permission, status: KeyStatus) -> AuthKey {
    AuthKey::new(None::<String>, permission, status)
}

/// Create a user with multiple keys pre-configured for testing using User API
/// Returns (instance, user, key_ids) where key_ids[i] corresponds to key_names[i]
pub async fn setup_test_user_with_keys(
    username: &str,
    key_names: &[&str],
) -> (Instance, User, Vec<String>) {
    let (instance, mut user) = crate::helpers::test_instance_with_user(username).await;

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
                .await
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
pub async fn configure_database_auth(
    database: &Database,
    auth_config: &[(&str, &str, Permission, KeyStatus)],
) -> Result<()> {
    let txn = database.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
        for (display_name, key_id, permission, status) in auth_config {
            let public_key = parse_public_key(key_id)?;
            let pubkey_str = format_public_key(&public_key);
            let auth_key = AuthKey::new(Some(*display_name), permission.clone(), status.clone());
            settings.set_auth_key(&pubkey_str, auth_key).await?;
        }
    }
    txn.commit().await?;
    Ok(())
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
pub async fn setup_complete_auth_environment_with_user(
    username: &str,
    keys: &[(&str, Permission, KeyStatus)],
) -> (Instance, User, Database, Vec<String>) {
    // Extract key display names
    let key_names: Vec<&str> = keys.iter().map(|(name, _, _)| *name).collect();

    let (instance, mut user, key_ids) = setup_test_user_with_keys(username, &key_names).await;

    // Find an Admin key for tree creation
    let admin_key_idx = keys
        .iter()
        .position(|(_, perm, _)| matches!(perm, Permission::Admin(_)))
        .expect("At least one Admin key is required");
    let admin_key_id = &key_ids[admin_key_idx];

    // Create database - automatically bootstraps auth with admin key as Admin(0)
    let database = user
        .create_database(Doc::new(), admin_key_id)
        .await
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

    configure_database_auth(&database, &auth_config)
        .await
        .expect("Failed to configure auth");

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
pub async fn create_delegated_tree_with_user(
    user: &mut User,
    keys: &[(&str, Permission, KeyStatus)],
) -> Result<(Database, Vec<String>)> {
    let mut key_ids = Vec::new();

    // Use existing default key for first key, or create new ones
    for (i, (key_name, _, _)) in keys.iter().enumerate() {
        let key_id = if i == 0 {
            // Use default key for first entry
            user.get_default_key()?
        } else {
            // Create additional keys
            user.add_private_key(Some(key_name)).await?
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
    let database = user.create_database(Doc::new(), admin_key_id).await?;

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

    configure_database_auth(&database, &auth_config).await?;

    Ok((database, key_ids))
}

/// Create delegation reference for a tree
pub async fn create_delegation_ref(
    tree: &Database,
    max_permission: Permission,
    min_permission: Option<Permission>,
) -> Result<DelegatedTreeRef> {
    let tips = tree.get_tips().await?;
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
///
/// Each step is a (root_id, tips) tuple where root_id is the delegated tree's root entry ID.
pub fn create_delegation_path(steps: &[(&ID, Vec<ID>)], final_hint: KeyHint) -> SigKey {
    let delegation_steps: Vec<DelegationStep> = steps
        .iter()
        .map(|(root_id, tips)| DelegationStep {
            tree: root_id.to_string(),
            tips: tips.clone(),
        })
        .collect();

    SigKey::Delegation {
        path: delegation_steps,
        hint: final_hint,
    }
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
    pub async fn new_with_user(username: &str, levels: usize) -> Result<Self> {
        let (db, mut user) = crate::helpers::test_instance_with_user(username).await;
        let mut trees = Vec::new();
        let mut keys = Vec::new(); // Will store display names

        for i in 0..levels {
            let display_name = format!("level_{i}_admin");

            let key_id = if i == 0 {
                // Use default key for first level
                user.get_default_key()?
            } else {
                // Add new key for subsequent levels
                user.add_private_key(Some(&display_name)).await?
            };

            keys.push(display_name.clone()); // Store display name for delegation paths

            // Create database - automatically bootstraps auth with:
            // - Key name: key_id (the public key string)
            // - Permission: Admin(0)
            // - Status: Active
            // Note: Bootstrap always uses Admin(0), not the level-specific Admin(i)
            let database = user.create_database(Doc::new(), &key_id).await?;

            trees.push(database);
        }

        Ok(DelegationChain { db, trees, keys })
    }

    pub async fn create_chain_delegation(&self, final_key: &str) -> SigKey {
        let mut steps = Vec::new();

        for tree in self.trees.iter() {
            let tips = tree.get_tips().await.expect("Failed to get tips");
            steps.push(DelegationStep {
                tree: tree.root_id().to_string(),
                tips,
            });
        }

        SigKey::Delegation {
            path: steps,
            hint: KeyHint::from_name(final_key),
        }
    }
}

// ===== ASSERTION HELPERS =====

/// Test that an operation succeeds
pub async fn test_operation_succeeds(tree: &Database, subtree_name: &str, test_name: &str) {
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let store = txn
        .get_store::<DocStore>(subtree_name)
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let result = txn.commit().await;
    assert!(result.is_ok(), "{test_name}: Operation should succeed");
}

/// Test that an operation fails
pub async fn test_operation_fails(tree: &Database, subtree_name: &str, test_name: &str) {
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let store = txn
        .get_store::<DocStore>(subtree_name)
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let result = txn.commit().await;
    assert!(result.is_err(), "{test_name}: Operation should fail");
}

/// Assert that permission resolution works correctly
pub async fn assert_permission_resolution(
    validator: &mut AuthValidator,
    sig_key: &SigKey,
    auth_settings: &AuthSettings,
    instance: Option<&Instance>,
    expected_permission: Permission,
    expected_status: KeyStatus,
) {
    let results = validator
        .resolve_sig_key(sig_key, auth_settings, instance)
        .await
        .expect("Permission resolution should succeed");

    assert!(!results.is_empty(), "Expected at least one resolved auth");
    let result = &results[0];

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
pub async fn assert_permission_resolution_fails(
    validator: &mut AuthValidator,
    sig_key: &SigKey,
    auth_settings: &AuthSettings,
    instance: Option<&Instance>,
    expected_error_pattern: &str,
) {
    let result = validator
        .resolve_sig_key(sig_key, auth_settings, instance)
        .await;
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
pub async fn assert_operation_permissions(
    tree: &Database,

    subtree_name: &str,
    should_succeed: bool,
    test_description: &str,
) {
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let store = txn
        .get_store::<DocStore>(subtree_name)
        .await
        .expect("Failed to get subtree");
    store
        .set("test", test_description)
        .await
        .expect("Failed to set value");

    let result = txn.commit().await;
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
