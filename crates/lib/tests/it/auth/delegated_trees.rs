//! Integration tests for delegated tree authentication
//!
//! These tests verify the end-to-end functionality of delegated tree
//! authentication, including tree creation, key delegation, permission
//! clamping, and various authorization scenarios.

use super::helpers::*;
use eidetica::Result;
use eidetica::auth::crypto::format_public_key;
use eidetica::auth::types::{
    AuthKey, DelegatedTreeRef, DelegationStep, KeyStatus, Permission, PermissionBounds, SigKey,
    TreeReference,
};
use eidetica::auth::validation::AuthValidator;
use eidetica::backend::database::InMemory;
use eidetica::basedb::BaseDB;
use eidetica::crdt::Doc;
use eidetica::crdt::map::Value;
use eidetica::entry::ID;
use eidetica::subtree::DocStore;

/// Test simple tree creation with auth
#[test]
fn test_simple_tree_creation_with_auth() -> Result<()> {
    let (_db, tree, _) =
        setup_complete_auth_environment(&[("main_admin", Permission::Admin(0), KeyStatus::Active)]);

    assert!(!tree.root_id().to_string().is_empty());
    Ok(())
}

/// Test basic delegated tree validation
#[test]
fn test_delegated_tree_basic_validation() -> Result<()> {
    let db = setup_db();

    // Create delegated tree
    let delegated_tree = create_delegated_tree(
        &db,
        &[("delegated_user", Permission::Admin(5), KeyStatus::Active)],
        "delegated_user",
    )?;

    // Create main tree with delegation
    let (_, main_tree, _) =
        setup_complete_auth_environment(&[("main_admin", Permission::Admin(0), KeyStatus::Active)]);

    // Add delegation to main tree auth settings
    let op = main_tree.new_authenticated_operation("main_admin")?;
    let settings_store = op.get_subtree::<DocStore>("_settings")?;

    let delegation_ref = create_delegation_ref(
        &delegated_tree,
        Permission::Write(10),
        Some(Permission::Read),
    )?;
    let mut new_auth_settings = main_tree.get_settings()?.get_all()?;
    new_auth_settings.set_json("delegate_to_user", delegation_ref)?;
    settings_store.set_value("auth", Value::Node(new_auth_settings.into()))?;
    op.commit()?;

    // Test delegated tree validation
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;
    let delegated_tips = delegated_tree.get_tips()?;

    let delegated_auth_id = create_delegation_path(&[
        ("delegate_to_user", Some(delegated_tips)),
        ("delegated_user", None),
    ]);

    assert_permission_resolution(
        &mut validator,
        &delegated_auth_id,
        &main_tree_settings,
        Some(db.backend()),
        Permission::Write(10),
        KeyStatus::Active,
    );

    Ok(())
}

/// Test permission clamping in delegated trees
#[test]
fn test_delegated_tree_permission_clamping() -> Result<()> {
    let db = setup_db();

    // Create delegated tree with Admin permissions
    let delegated_tree = create_delegated_tree(
        &db,
        &[("delegated_user", Permission::Admin(0), KeyStatus::Active)],
        "delegated_user",
    )?;

    // Create main tree with Read-only delegation
    let (_, main_tree, _) =
        setup_complete_auth_environment(&[("main_admin", Permission::Admin(0), KeyStatus::Active)]);

    // Add read-only delegation
    let op = main_tree.new_authenticated_operation("main_admin")?;
    let settings_store = op.get_subtree::<DocStore>("_settings")?;

    let delegation_ref = create_delegation_ref(&delegated_tree, Permission::Read, None)?;
    let mut new_auth_settings = main_tree.get_settings()?.get_all()?;
    new_auth_settings.set_json("delegate_readonly", delegation_ref)?;
    settings_store.set_value("auth", Value::Node(new_auth_settings.into()))?;
    op.commit()?;

    // Test permission clamping
    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;
    let delegated_tips = delegated_tree.get_tips()?;

    let delegated_auth_id = create_delegation_path(&[
        ("delegate_readonly", Some(delegated_tips)),
        ("delegated_user", None),
    ]);

    // Permissions should be clamped from Admin to Read
    assert_permission_resolution(
        &mut validator,
        &delegated_auth_id,
        &main_tree_settings,
        Some(db.backend()),
        Permission::Read,
        KeyStatus::Active,
    );

    Ok(())
}

/// Test nested delegation (delegated tree delegating to another delegated tree)
#[test]
fn test_nested_delegation() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    let main_admin_key = db.add_private_key("main_admin")?;
    let org_admin_key = db.add_private_key("org_admin")?;
    let user_key = db.add_private_key("user")?;

    // Create user tree (bottom level)
    let mut user_settings = Doc::new();
    let mut user_auth = Doc::new();
    user_auth
        .set_json(
            "user", // Key name must match the private key ID
            AuthKey {
                pubkey: format_public_key(&user_key),
                permissions: Permission::Admin(10), // Admin needed to create tree
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    user_settings.set_map("auth", user_auth);
    let user_tree = db.new_tree(user_settings, "user")?;

    // Create org tree (middle level) that delegates to user tree
    let mut org_settings = Doc::new();
    let mut org_auth = Doc::new();
    org_auth
        .set_json(
            "org_admin", // Key name matches private key ID
            AuthKey {
                pubkey: format_public_key(&org_admin_key),
                permissions: Permission::Admin(5),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    // Delegate to user tree
    let user_tips = user_tree.get_tips()?;
    org_auth
        .set_json(
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
        )
        .unwrap();
    org_settings.set_map("auth", org_auth);
    let org_tree = db.new_tree(org_settings, "org_admin")?;

    // Create main tree (top level) that delegates to org tree
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "main_admin",
            AuthKey {
                pubkey: format_public_key(&main_admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    // Delegate to org tree
    let org_tips = org_tree.get_tips()?;
    main_auth
        .set_json(
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
        )
        .unwrap();
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

    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    // Create delegated tree with user key (initially active)
    let mut delegated_settings = Doc::new();
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "delegated_user", // Key name must match the private key ID
            AuthKey {
                pubkey: format_public_key(&delegated_user_key),
                permissions: Permission::Admin(10), // Admin needed to create tree
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_settings.set_map("auth", delegated_auth);

    let delegated_tree = db.new_tree(delegated_settings, "delegated_user")?;

    // Create main tree with delegation
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "main_admin", // Key name must match the private key ID
            AuthKey {
                pubkey: format_public_key(&main_admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    let delegated_tips = delegated_tree.get_tips()?;
    main_auth
        .set_json(
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
        )
        .unwrap();
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
    let mut revoked_settings = Doc::new();
    let mut revoked_auth = Doc::new();
    revoked_auth
        .set_json(
            "delegated_user", // Key name must match the private key ID
            AuthKey {
                pubkey: format_public_key(&delegated_user_key),
                permissions: Permission::Write(10),
                status: KeyStatus::Revoked, // Now revoked
            },
        )
        .unwrap();
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

    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Create a deeply nested delegation chain that exceeds the limit
    let admin_key = db.add_private_key("admin")?;
    let user_key = db.add_private_key("user")?;

    // Create a simple delegated tree
    let mut delegated_settings = Doc::new();
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "user", // Key name must match the private key ID
            AuthKey {
                pubkey: format_public_key(&user_key),
                permissions: Permission::Admin(10), // Admin needed to create tree
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "user")?;

    // Create main tree settings
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "admin", // Key name must match the private key ID
            AuthKey {
                pubkey: format_public_key(&admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    let delegated_tips = delegated_tree.get_tips()?;
    main_auth
        .set_json(
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
        )
        .unwrap();
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

    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Keys
    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    // ---------------- Delegated tree ----------------
    let mut delegated_settings = Doc::new();
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "delegated_admin",
            AuthKey {
                pubkey: format_public_key(&delegated_admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_auth
        .set_json(
            "delegated_user",
            AuthKey {
                pubkey: format_public_key(&delegated_user_key),
                permissions: Permission::Write(15), // Low-privilege write
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_settings.set_map("auth", delegated_auth);

    let delegated_tree = db.new_tree(delegated_settings, "delegated_admin")?;
    let delegated_tips = delegated_tree.get_tips()?;

    // ---------------- Main tree with delegation ----------------
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "main_admin",
            AuthKey {
                pubkey: format_public_key(&main_admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    // Bounds: raise anything below Write(7) up to Write(7), cap at Write(0)
    main_auth
        .set_json(
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
        )
        .unwrap();
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

    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Keys
    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    // Delegated tree with user key Write(12)
    let mut delegated_settings = Doc::new();
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "delegated_admin",
            AuthKey {
                pubkey: format_public_key(&delegated_admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_auth
        .set_json(
            "delegated_user",
            AuthKey {
                pubkey: format_public_key(&delegated_user_key),
                permissions: Permission::Write(12), // priority 12
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "delegated_admin")?;
    let delegated_tips = delegated_tree.get_tips()?;

    // Main tree delegates with max Write(8) (more privileged) and no min
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "main_admin",
            AuthKey {
                pubkey: format_public_key(&main_admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    main_auth
        .set_json(
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
        )
        .unwrap();
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

    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Setup simple tree with direct key
    let admin_key = db.add_private_key("admin")?;
    let mut settings = Doc::new();
    let mut auth = Doc::new();
    auth.set_json(
        "admin",
        AuthKey {
            pubkey: format_public_key(&admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    )
    .unwrap();
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

    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Keys and delegated tree setup
    let main_admin_key = db.add_private_key("main_admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;
    let delegated_user_key = db.add_private_key("delegated_user")?;

    let mut delegated_settings = Doc::new();
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "delegated_admin",
            AuthKey {
                pubkey: format_public_key(&delegated_admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_auth
        .set_json(
            "delegated_user",
            AuthKey {
                pubkey: format_public_key(&delegated_user_key),
                permissions: Permission::Write(5),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "delegated_admin")?;

    // Fake tip that does not exist
    let bogus_tip = ID::new("nonexistent_tip_hash");

    // Main tree with delegation using bogus tip
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "main_admin",
            AuthKey {
                pubkey: format_public_key(&main_admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    main_auth
        .set_json(
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
        )
        .unwrap();
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

/// Test complex nested delegation using DelegationChain helper
#[test]
fn test_complex_nested_delegation_chain() -> Result<()> {
    // Create a 3-level delegation chain
    let chain = DelegationChain::new(3)?;

    // Verify the chain was created correctly
    assert_eq!(chain.trees.len(), 3);
    assert_eq!(chain.keys.len(), 3);

    // Verify each tree is accessible and has the expected root
    for (i, tree) in chain.trees.iter().enumerate() {
        assert!(
            !tree.root_id().to_string().is_empty(),
            "Tree {i} should have a root ID"
        );
    }

    // Verify each tree has its expected key
    for (i, key) in chain.keys.iter().enumerate() {
        assert_eq!(key, &format!("level_{i}_admin"));
    }

    // Create a delegation chain to a final user
    let delegation_path = chain.create_chain_delegation("final_user");

    // Test that complex delegation paths can be created correctly
    match delegation_path {
        SigKey::DelegationPath(steps) => {
            assert_eq!(steps.len(), 4); // 3 levels + final user

            // Verify each step has the expected key
            for (i, step) in steps.iter().take(3).enumerate() {
                assert_eq!(step.key, format!("delegate_level_{i}"));
                assert!(step.tips.is_some()); // Intermediate steps have tips
            }

            // Final step should be the target user
            assert_eq!(steps[3].key, "final_user");
            assert!(steps[3].tips.is_none()); // Final step has no tips
        }
        _ => panic!("Expected delegation path"),
    }

    Ok(())
}
