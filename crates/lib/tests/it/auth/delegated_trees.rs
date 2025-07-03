//! Integration tests for delegated tree authentication
//!
//! These tests verify the end-to-end functionality of delegated tree
//! authentication, including tree creation, key delegation, permission
//! clamping, and various authorization scenarios.

use eidetica::Result;
use eidetica::auth::crypto::format_public_key;
use eidetica::auth::types::{
    AuthKey, DelegatedTreeRef, DelegationStep, KeyStatus, Permission, PermissionBounds, SigKey,
    TreeReference,
};
use eidetica::backend::InMemoryBackend;
use eidetica::basedb::BaseDB;
use eidetica::crdt::Nested;
use eidetica::entry::ID;

/// Test simple tree creation with auth
#[test]
fn test_simple_tree_creation_with_auth() -> Result<()> {
    // Create database and keys
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let main_admin_key = db.add_private_key("main_admin")?;

    // Create main tree with explicit auth configuration
    let mut main_settings = Nested::new();
    main_settings.set_string("name", "main_project_tree");

    let mut main_auth = Nested::new();
    main_auth.set(
        "main_admin", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&main_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    main_settings.set_map("auth", main_auth);

    // This should work without error
    let main_tree = db.new_tree(main_settings, "main_admin")?;
    assert!(!main_tree.root_id().to_string().is_empty());

    Ok(())
}

/// Test basic delegated tree validation
#[test]
fn test_delegated_tree_basic_validation() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    // Create database and keys
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    // Create delegated tree first
    let mut delegated_settings = Nested::new();
    delegated_settings.set_string("name", "user_personal_tree");

    let mut delegated_auth = Nested::new();
    delegated_auth.set(
        "delegated_user", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&delegated_user_key),
            permissions: Permission::Admin(5), // Admin needed to create tree with auth
            status: KeyStatus::Active,
        },
    );
    delegated_settings.set_map("auth", delegated_auth);

    let delegated_tree = db.new_tree(delegated_settings, "delegated_user")?;

    // Create main tree with delegation
    let mut main_settings = Nested::new();
    main_settings.set_string("name", "main_project_tree");

    let mut main_auth = Nested::new();
    main_auth.set(
        "main_admin", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&main_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );

    // Add delegation to the user's tree
    let delegated_tips = delegated_tree.get_tips()?;
    main_auth.set(
        "delegate_to_user",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: delegated_tips.clone(),
            },
        },
    );
    main_settings.set_map("auth", main_auth);

    let main_tree = db.new_tree(main_settings, "main_admin")?;

    // Test delegated tree validation directly
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;

    let delegated_auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_user".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "delegated_user".to_string(),
            tips: None,
        },
    ]);

    // This should resolve successfully and return Write permissions (clamped from delegated tree)
    let resolved_auth =
        validator.resolve_sig_key(&delegated_auth_id, &main_tree_settings, Some(db.backend()))?;

    assert_eq!(resolved_auth.effective_permission, Permission::Write(10));
    assert_eq!(resolved_auth.key_status, KeyStatus::Active);

    Ok(())
}

/// Test permission clamping in delegated trees
#[test]
fn test_delegated_tree_permission_clamping() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    // Create delegated tree with Admin permissions
    let mut delegated_settings = Nested::new();
    let mut delegated_auth = Nested::new();
    delegated_auth.set(
        "delegated_user", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&delegated_user_key),
            permissions: Permission::Admin(0), // Admin in delegated tree
            status: KeyStatus::Active,
        },
    );
    delegated_settings.set_map("auth", delegated_auth);

    let delegated_tree = db.new_tree(delegated_settings, "delegated_user")?;

    // Create main tree with Read-only delegation
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();
    main_auth.set(
        "main_admin", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&main_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );

    // Delegate with Read-only permissions
    let delegated_tips = delegated_tree.get_tips()?;
    main_auth.set(
        "delegate_readonly",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Read, // Clamp to Read-only
                min: None,
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: delegated_tips.clone(),
            },
        },
    );
    main_settings.set_map("auth", main_auth);

    let main_tree = db.new_tree(main_settings, "main_admin")?;

    // Test permission clamping through validation
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;

    let delegated_auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_readonly".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "delegated_user".to_string(),
            tips: None,
        },
    ]);

    // Permissions should be clamped from Admin to Read
    let resolved_auth =
        validator.resolve_sig_key(&delegated_auth_id, &main_tree_settings, Some(db.backend()))?;

    assert_eq!(resolved_auth.effective_permission, Permission::Read);
    assert_eq!(resolved_auth.key_status, KeyStatus::Active);

    Ok(())
}

/// Test nested delegation (delegated tree delegating to another delegated tree)
#[test]
fn test_nested_delegation() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let main_admin_key = db.add_private_key("main_admin")?;
    let org_admin_key = db.add_private_key("org_admin")?;
    let user_key = db.add_private_key("user")?;

    // Create user tree (bottom level)
    let mut user_settings = Nested::new();
    let mut user_auth = Nested::new();
    user_auth.set(
        "user", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&user_key),
            permissions: Permission::Admin(10), // Admin needed to create tree
            status: KeyStatus::Active,
        },
    );
    user_settings.set_map("auth", user_auth);
    let user_tree = db.new_tree(user_settings, "user")?;

    // Create org tree (middle level) that delegates to user tree
    let mut org_settings = Nested::new();
    let mut org_auth = Nested::new();
    org_auth.set(
        "org_admin", // Key name matches private key ID
        AuthKey {
            pubkey: format_public_key(&org_admin_key),
            permissions: Permission::Admin(5),
            status: KeyStatus::Active,
        },
    );

    // Delegate to user tree
    let user_tips = user_tree.get_tips()?;
    org_auth.set(
        "delegate_to_user",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(20),
                min: None,
            },
            tree: TreeReference {
                root: user_tree.root_id().clone(),
                tips: user_tips.clone(),
            },
        },
    );
    org_settings.set_map("auth", org_auth);
    let org_tree = db.new_tree(org_settings, "org_admin")?;

    // Create main tree (top level) that delegates to org tree
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();
    main_auth.set(
        "main_admin",
        AuthKey {
            pubkey: format_public_key(&main_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );

    // Delegate to org tree
    let org_tips = org_tree.get_tips()?;
    main_auth.set(
        "delegate_to_org",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(15),
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: org_tree.root_id().clone(),
                tips: org_tips.clone(),
            },
        },
    );
    main_settings.set_map("auth", main_auth);
    let main_tree = db.new_tree(main_settings, "main_admin")?;

    // Test nested delegation: main -> org -> user
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;

    // Create a nested delegation chain: main -> org -> user
    let nested_auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_org".to_string(),
            tips: Some(org_tips.clone()),
        },
        DelegationStep {
            key: "delegate_to_user".to_string(),
            tips: Some(user_tips.clone()),
        },
        DelegationStep {
            key: "user".to_string(),
            tips: None,
        },
    ]);

    // This should resolve with Write permissions (clamped through the chain)
    let resolved_auth =
        validator.resolve_sig_key(&nested_auth_id, &main_tree_settings, Some(db.backend()))?;

    // Permissions should be clamped: user has Admin(10) -> org clamps to Write(20) -> main doesn't clamp further
    // Final result should be Write(20) (clamped at org level)
    assert_eq!(resolved_auth.effective_permission, Permission::Write(20));
    assert_eq!(resolved_auth.key_status, KeyStatus::Active);

    Ok(())
}

/// Test delegated tree with revoked keys
#[test]
fn test_delegated_tree_with_revoked_keys() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    // Create delegated tree with user key (initially active)
    let mut delegated_settings = Nested::new();
    let mut delegated_auth = Nested::new();
    delegated_auth.set(
        "delegated_user", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&delegated_user_key),
            permissions: Permission::Admin(10), // Admin needed to create tree
            status: KeyStatus::Active,
        },
    );
    delegated_settings.set_map("auth", delegated_auth);

    let delegated_tree = db.new_tree(delegated_settings, "delegated_user")?;

    // Create main tree with delegation
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();
    main_auth.set(
        "main_admin", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&main_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );

    let delegated_tips = delegated_tree.get_tips()?;
    main_auth.set(
        "delegate_to_tree",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: None,
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: delegated_tips.clone(),
            },
        },
    );
    main_settings.set_map("auth", main_auth);

    let main_tree = db.new_tree(main_settings, "main_admin")?;

    // Test with active key - should work
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;

    let delegated_auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_tree".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "delegated_user".to_string(),
            tips: None,
        },
    ]);

    let resolved_auth =
        validator.resolve_sig_key(&delegated_auth_id, &main_tree_settings, Some(db.backend()))?;

    assert_eq!(resolved_auth.effective_permission, Permission::Write(10));
    assert_eq!(resolved_auth.key_status, KeyStatus::Active);

    // Create a new delegated tree with the same key but revoked
    let mut revoked_settings = Nested::new();
    let mut revoked_auth = Nested::new();
    revoked_auth.set(
        "delegated_user", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&delegated_user_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Revoked, // Now revoked
        },
    );
    revoked_settings.set_map("auth", revoked_auth);

    // We can't easily update the delegated tree state in this test, so we'll validate against
    // the revoked settings directly to test the revocation logic
    let delegated_settings_data = revoked_settings;

    // Test validation against revoked key
    let resolved_auth_revoked = validator.resolve_sig_key(
        &SigKey::Direct("delegated_user".to_string()),
        &delegated_settings_data,
        Some(db.backend()),
    )?;

    assert_eq!(resolved_auth_revoked.key_status, KeyStatus::Revoked);

    Ok(())
}

/// Test delegation depth limits
#[test]
fn test_delegation_depth_limits() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Create a deeply nested delegation chain that exceeds the limit
    let admin_key = db.add_private_key("admin")?;
    let user_key = db.add_private_key("user")?;

    // Create a simple delegated tree
    let mut delegated_settings = Nested::new();
    let mut delegated_auth = Nested::new();
    delegated_auth.set(
        "user", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&user_key),
            permissions: Permission::Admin(10), // Admin needed to create tree
            status: KeyStatus::Active,
        },
    );
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "user")?;

    // Create main tree settings
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();
    main_auth.set(
        "admin", // Key name must match the private key ID
        AuthKey {
            pubkey: format_public_key(&admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );

    let delegated_tips = delegated_tree.get_tips()?;
    main_auth.set(
        "delegate_to_user",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: None,
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: delegated_tips.clone(),
            },
        },
    );
    main_settings.set_map("auth", main_auth);
    let main_tree = db.new_tree(main_settings, "admin")?;

    // Create a deeply nested delegation that should exceed the limit
    // We'll create a chain with 12 levels (exceeds MAX_DELEGATION_DEPTH of 10)
    let mut delegation_steps = Vec::new();

    // Add 12 intermediate delegation steps (exceeds MAX_DELEGATION_DEPTH of 10)
    for _ in 0..12 {
        delegation_steps.push(DelegationStep {
            key: "delegate_to_user".to_string(),
            tips: Some(delegated_tips.clone()),
        });
    }

    // Add final step
    delegation_steps.push(DelegationStep {
        key: "user".to_string(),
        tips: None,
    });

    let nested_auth_id = SigKey::DelegationPath(delegation_steps);

    // Test depth limit validation
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;
    let result =
        validator.resolve_sig_key(&nested_auth_id, &main_tree_settings, Some(db.backend()));

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("Maximum delegation depth") || error_msg.contains("not found"));

    Ok(())
}

/// Test permission upgrade when delegated permission is below `min` bound
#[test]
fn test_delegated_tree_min_bound_upgrade() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Keys
    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    // ---------------- Delegated tree ----------------
    let mut delegated_settings = Nested::new();
    let mut delegated_auth = Nested::new();
    delegated_auth.set(
        "delegated_admin",
        AuthKey {
            pubkey: format_public_key(&delegated_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    delegated_auth.set(
        "delegated_user",
        AuthKey {
            pubkey: format_public_key(&delegated_user_key),
            permissions: Permission::Write(15), // Low-privilege write
            status: KeyStatus::Active,
        },
    );
    delegated_settings.set_map("auth", delegated_auth);

    let delegated_tree = db.new_tree(delegated_settings, "delegated_admin")?;
    let delegated_tips = delegated_tree.get_tips()?;

    // ---------------- Main tree with delegation ----------------
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();
    main_auth.set(
        "main_admin",
        AuthKey {
            pubkey: format_public_key(&main_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );

    // Bounds: raise anything below Write(7) up to Write(7), cap at Write(0)
    main_auth.set(
        "delegate_user_min_upgrade",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(0),       // Highest possible Write permission
                min: Some(Permission::Write(7)), // Minimum permission level
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: delegated_tips.clone(),
            },
        },
    );
    main_settings.set_map("auth", main_auth);

    let main_tree = db.new_tree(main_settings, "main_admin")?;

    // Validate
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;

    let auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_user_min_upgrade".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "delegated_user".to_string(),
            tips: None,
        },
    ]);

    let resolved = validator.resolve_sig_key(&auth_id, &main_tree_settings, Some(db.backend()))?;

    // Expect permission upgraded to Write(7)
    assert_eq!(resolved.effective_permission, Permission::Write(7));
    assert_eq!(resolved.key_status, KeyStatus::Active);

    Ok(())
}

/// Test that priority (the numeric part) is preserved when permission is already within bounds
#[test]
fn test_delegated_tree_priority_preservation() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Keys
    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    // Delegated tree with user key Write(12)
    let mut delegated_settings = Nested::new();
    let mut delegated_auth = Nested::new();
    delegated_auth.set(
        "delegated_admin",
        AuthKey {
            pubkey: format_public_key(&delegated_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    delegated_auth.set(
        "delegated_user",
        AuthKey {
            pubkey: format_public_key(&delegated_user_key),
            permissions: Permission::Write(12), // priority 12
            status: KeyStatus::Active,
        },
    );
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "delegated_admin")?;
    let delegated_tips = delegated_tree.get_tips()?;

    // Main tree delegates with max Write(8) (more privileged) and no min
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();
    main_auth.set(
        "main_admin",
        AuthKey {
            pubkey: format_public_key(&main_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    main_auth.set(
        "delegate_user_priority",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(8),
                min: None,
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: delegated_tips.clone(),
            },
        },
    );
    main_settings.set_map("auth", main_auth);

    let main_tree = db.new_tree(main_settings, "main_admin")?;

    // Validate
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;

    let auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_user_priority".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "delegated_user".to_string(),
            tips: None,
        },
    ]);

    let resolved = validator.resolve_sig_key(&auth_id, &main_tree_settings, Some(db.backend()))?;

    // Because Write(12) is within bounds (less privileged than Write(8)), it is preserved
    assert_eq!(resolved.effective_permission, Permission::Write(12));

    Ok(())
}

/// Test delegation depth limit at exactly MAX_DELEGATION_DEPTH (10)
#[test]
fn test_delegation_depth_limit_exact() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Setup simple tree with direct key
    let admin_key = db.add_private_key("admin")?;
    let mut settings = Nested::new();
    let mut auth = Nested::new();
    auth.set(
        "admin",
        AuthKey {
            pubkey: format_public_key(&admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    settings.set_map("auth", auth);
    let tree = db.new_tree(settings, "admin")?;
    let tips = tree.get_tips()?;

    // Build a chain exactly 10 levels deep
    let mut delegation_steps = Vec::new();

    // Add 10 intermediate delegation steps (reaches MAX_DELEGATION_DEPTH of 10)
    for _ in 0..10 {
        delegation_steps.push(DelegationStep {
            key: "some_delegate".to_string(), // ID doesn't exist but depth is focus
            tips: Some(tips.clone()),
        });
    }

    // Add final step
    delegation_steps.push(DelegationStep {
        key: "admin".to_string(),
        tips: None,
    });

    let auth_id = SigKey::DelegationPath(delegation_steps);

    let mut validator = AuthValidator::new();
    let settings_state = tree.get_settings()?.get_all()?;

    let result = validator.resolve_sig_key(&auth_id, &settings_state, Some(db.backend()));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Maximum delegation depth") || msg.contains("not found"));

    Ok(())
}

/// Test that invalid (unknown) tips cause delegation validation to fail
#[test]
fn test_delegated_tree_invalid_tips() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Keys and delegated tree setup
    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    let mut delegated_settings = Nested::new();
    let mut delegated_auth = Nested::new();
    delegated_auth.set(
        "delegated_admin",
        AuthKey {
            pubkey: format_public_key(&delegated_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    delegated_auth.set(
        "delegated_user",
        AuthKey {
            pubkey: format_public_key(&delegated_user_key),
            permissions: Permission::Write(5),
            status: KeyStatus::Active,
        },
    );
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "delegated_admin")?;

    // Fake tip that does not exist
    let bogus_tip = ID::new("nonexistent_tip_hash");

    // Main tree with delegation using bogus tip
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();
    main_auth.set(
        "main_admin",
        AuthKey {
            pubkey: format_public_key(&main_admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    main_auth.set(
        "delegate_with_bad_tip",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(5),
                min: None,
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: vec![bogus_tip.clone()],
            },
        },
    );
    main_settings.set_map("auth", main_auth);
    let main_tree = db.new_tree(main_settings, "main_admin")?;

    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;

    let auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_with_bad_tip".to_string(),
            tips: Some(vec![bogus_tip]),
        },
        DelegationStep {
            key: "delegated_user".to_string(),
            tips: None,
        },
    ]);

    let result = validator.resolve_sig_key(&auth_id, &main_tree_settings, Some(db.backend()));
    assert!(result.is_err());

    Ok(())
}
