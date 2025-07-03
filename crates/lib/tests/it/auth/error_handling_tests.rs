//! Error handling and security tests for flat delegation structure
//!
//! These tests cover error propagation, validation failures, backend issues,
//! and security edge cases in the delegation system.

use eidetica::Result;
use eidetica::auth::crypto::format_public_key;
use eidetica::auth::types::{
    AuthKey, DelegatedTreeRef, DelegationStep, KeyStatus, Permission, PermissionBounds, SigKey,
    TreeReference,
};
use eidetica::auth::validation::AuthValidator;
use eidetica::backend::InMemoryBackend;
use eidetica::basedb::BaseDB;
use eidetica::crdt::{Nested, Value};
use eidetica::entry::ID;

/// Test delegation resolution with missing backend
#[test]
fn test_delegation_without_backend() {
    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "some_tree".to_string(),
            tips: Some(vec![ID::from("tip1")]),
        },
        DelegationStep {
            key: "final_key".to_string(),
            tips: None,
        },
    ]);

    let mut validator = AuthValidator::new();
    let settings = Nested::new();

    // Should fail when backend is required but not provided
    let result = validator.resolve_sig_key(&delegation_path, &settings, None);
    assert!(result.is_err());

    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("Backend required") || error_msg.contains("backend"));
}

/// Test delegation with non-existent delegated tree
#[test]
fn test_delegation_nonexistent_tree() -> Result<()> {
    let db = BaseDB::new(Box::new(InMemoryBackend::new()));

    // Add private key to storage
    let admin_key = db.add_private_key("admin")?;

    // Create main tree with delegation reference to non-existent tree
    let mut auth = Nested::new();
    auth.set_json(
        "admin",
        AuthKey {
            pubkey: format_public_key(&admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    )
    .unwrap();

    // Add delegation to non-existent tree
    auth.set_json(
        "nonexistent_delegate",
        DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                min: None,
                max: Permission::Write(10),
            },
            tree: TreeReference {
                root: ID::from("nonexistent_root"),
                tips: vec![ID::from("nonexistent_tip")],
            },
        },
    )
    .unwrap();

    let mut settings = Nested::new();
    settings.set_map("auth", auth);
    let tree = db.new_tree(settings, "admin")?;

    // Try to resolve delegation to non-existent tree
    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "nonexistent_delegate".to_string(),
            tips: Some(vec![ID::from("nonexistent_tip")]),
        },
        DelegationStep {
            key: "some_key".to_string(),
            tips: None,
        },
    ]);

    let mut validator = AuthValidator::new();
    let tree_settings = tree.get_settings()?.get_all()?;
    let result = validator.resolve_sig_key(&delegation_path, &tree_settings, Some(db.backend()));

    // Should fail gracefully
    assert!(result.is_err());

    Ok(())
}

/// Test delegation with corrupted tree references
#[test]
fn test_delegation_corrupted_tree_references() -> Result<()> {
    let db = BaseDB::new(Box::new(InMemoryBackend::new()));

    // Add private key to storage
    let admin_key = db.add_private_key("admin")?;

    // Create tree with manually corrupted delegation reference
    let mut auth = Nested::new();
    auth.set_json(
        "admin",
        AuthKey {
            pubkey: format_public_key(&admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    )
    .unwrap();

    // Add corrupted delegation (invalid tips)
    let mut corrupted_delegate = Nested::new();
    corrupted_delegate.set("permission-bounds", Value::String("invalid".to_string()));
    corrupted_delegate.set("tree", Value::String("not_a_tree_ref".to_string()));
    auth.set("corrupted_delegate", Value::Map(corrupted_delegate));

    let mut settings = Nested::new();
    settings.set_map("auth", auth);
    let tree = db.new_tree(settings, "admin")?;

    // Try to resolve corrupted delegation
    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "corrupted_delegate".to_string(),
            tips: Some(vec![ID::from("some_tip")]),
        },
        DelegationStep {
            key: "some_key".to_string(),
            tips: None,
        },
    ]);

    let mut validator = AuthValidator::new();
    let tree_settings = tree.get_settings()?.get_all()?;
    let result = validator.resolve_sig_key(&delegation_path, &tree_settings, Some(db.backend()));

    // Should fail with appropriate error
    assert!(result.is_err());

    Ok(())
}

/// Test privilege escalation attempt through delegation
#[test]
fn test_privilege_escalation_through_delegation() -> Result<()> {
    let db = BaseDB::new(Box::new(InMemoryBackend::new()));

    // Add private keys to storage
    let admin_key = db.add_private_key("admin_in_delegated_tree")?;
    let user_key = db.add_private_key("main_admin")?;

    // Create delegated tree with admin permissions
    let mut delegated_auth = Nested::new();
    delegated_auth
        .set_json(
            "admin_in_delegated_tree",
            AuthKey {
                pubkey: format_public_key(&admin_key),
                permissions: Permission::Admin(0), // Admin in delegated tree
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    let mut delegated_settings = Nested::new();
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "admin_in_delegated_tree")?;
    let delegated_tips = delegated_tree.get_tips()?;

    // Create main tree that delegates with restricted permissions
    let mut main_auth = Nested::new();
    main_auth
        .set_json(
            "main_admin",
            AuthKey {
                pubkey: format_public_key(&user_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    // Add delegation with permission restriction (should clamp admin to write)
    main_auth
        .set_json(
            "restricted_delegate",
            DelegatedTreeRef {
                permission_bounds: PermissionBounds {
                    min: None,
                    max: Permission::Write(10), // Restrict to Write only
                },
                tree: TreeReference {
                    root: delegated_tree.root_id().clone(),
                    tips: delegated_tips.clone(),
                },
            },
        )
        .unwrap();

    let mut main_settings = Nested::new();
    main_settings.set_map("auth", main_auth);
    let main_tree = db.new_tree(main_settings, "main_admin")?;

    // Try to use admin key from delegated tree through restricted delegation
    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "restricted_delegate".to_string(),
            tips: Some(delegated_tips),
        },
        DelegationStep {
            key: "admin_in_delegated_tree".to_string(),
            tips: None,
        },
    ]);

    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;
    let result =
        validator.resolve_sig_key(&delegation_path, &main_tree_settings, Some(db.backend()));

    // Should succeed but with clamped permissions
    assert!(result.is_ok());
    let resolved = result.unwrap();

    // Effective permission should be Write (clamped), not Admin
    assert_eq!(resolved.effective_permission, Permission::Write(10));
    assert!(!resolved.effective_permission.can_admin()); // Should not have admin privileges

    Ok(())
}

/// Test delegation with tampered tips
#[test]
fn test_delegation_with_tampered_tips() -> Result<()> {
    let db = BaseDB::new(Box::new(InMemoryBackend::new()));

    // Add private keys to storage
    let user_key = db.add_private_key("user")?;
    let admin_key = db.add_private_key("admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;

    // Create delegated tree
    let mut delegated_auth = Nested::new();
    delegated_auth
        .set_json(
            "delegated_admin",
            AuthKey {
                pubkey: format_public_key(&delegated_admin_key),
                permissions: Permission::Admin(0), // Need admin to create tree
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_auth
        .set_json(
            "user",
            AuthKey {
                pubkey: format_public_key(&user_key),
                permissions: Permission::Write(10),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    let mut delegated_settings = Nested::new();
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "delegated_admin")?;
    let real_tips = delegated_tree.get_tips()?;

    // Create main tree with delegation
    let mut main_auth = Nested::new();
    main_auth
        .set_json(
            "admin",
            AuthKey {
                pubkey: format_public_key(&admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    main_auth
        .set_json(
            "delegate_to_user",
            DelegatedTreeRef {
                permission_bounds: PermissionBounds {
                    min: None,
                    max: Permission::Write(10),
                },
                tree: TreeReference {
                    root: delegated_tree.root_id().clone(),
                    tips: real_tips.clone(),
                },
            },
        )
        .unwrap();

    let mut main_settings = Nested::new();
    main_settings.set_map("auth", main_auth);
    let main_tree = db.new_tree(main_settings, "admin")?;

    // Try to use delegation with fake/tampered tips
    let fake_tips = vec![ID::from("fake_tip_1"), ID::from("fake_tip_2")];
    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_user".to_string(),
            tips: Some(fake_tips), // Using fake tips instead of real ones
        },
        DelegationStep {
            key: "user".to_string(),
            tips: None,
        },
    ]);

    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;
    let result =
        validator.resolve_sig_key(&delegation_path, &main_tree_settings, Some(db.backend()));

    // Should fail because tips are invalid
    assert!(result.is_err());

    Ok(())
}

/// Test delegation chain with mixed key statuses
#[test]
fn test_delegation_mixed_key_statuses() -> Result<()> {
    let db = BaseDB::new(Box::new(InMemoryBackend::new()));

    // Add private keys to storage
    let active_user_key = db.add_private_key("active_user")?;
    let revoked_key = db.add_private_key("revoked_user")?;
    let admin_key = db.add_private_key("admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;

    // Create delegated tree with mix of active and revoked keys
    let mut delegated_auth = Nested::new();
    delegated_auth
        .set_json(
            "delegated_admin",
            AuthKey {
                pubkey: format_public_key(&delegated_admin_key),
                permissions: Permission::Admin(0), // Need admin to create tree
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_auth
        .set_json(
            "active_user",
            AuthKey {
                pubkey: format_public_key(&active_user_key),
                permissions: Permission::Write(10),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_auth
        .set_json(
            "revoked_user",
            AuthKey {
                pubkey: format_public_key(&revoked_key),
                permissions: Permission::Write(10),
                status: KeyStatus::Revoked, // Revoked key
            },
        )
        .unwrap();

    let mut delegated_settings = Nested::new();
    delegated_settings.set_map("auth", delegated_auth);
    let delegated_tree = db.new_tree(delegated_settings, "delegated_admin")?;
    let delegated_tips = delegated_tree.get_tips()?;

    // Create main tree with delegation
    let mut main_auth = Nested::new();
    main_auth
        .set_json(
            "admin",
            AuthKey {
                pubkey: format_public_key(&admin_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();

    main_auth
        .set_json(
            "delegate_to_users",
            DelegatedTreeRef {
                permission_bounds: PermissionBounds {
                    min: None,
                    max: Permission::Write(10),
                },
                tree: TreeReference {
                    root: delegated_tree.root_id().clone(),
                    tips: delegated_tips.clone(),
                },
            },
        )
        .unwrap();

    let mut main_settings = Nested::new();
    main_settings.set_map("auth", main_auth);
    let main_tree = db.new_tree(main_settings, "admin")?;

    // Test accessing active key through delegation
    let active_delegation = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_users".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "active_user".to_string(),
            tips: None,
        },
    ]);

    let mut validator = AuthValidator::new();
    let main_tree_settings = main_tree.get_settings()?.get_all()?;
    let result =
        validator.resolve_sig_key(&active_delegation, &main_tree_settings, Some(db.backend()));

    // Should succeed for active key
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.key_status, KeyStatus::Active);

    // Test accessing revoked key through delegation
    let revoked_delegation = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_users".to_string(),
            tips: Some(delegated_tips),
        },
        DelegationStep {
            key: "revoked_user".to_string(),
            tips: None,
        },
    ]);

    let result =
        validator.resolve_sig_key(&revoked_delegation, &main_tree_settings, Some(db.backend()));

    // Should succeed in resolving but key should be marked as revoked
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.key_status, KeyStatus::Revoked);

    Ok(())
}

/// Test validation cache behavior under error conditions
#[test]
fn test_validation_cache_error_conditions() -> Result<()> {
    let db = BaseDB::new(Box::new(InMemoryBackend::new()));

    // Add private key to storage
    let admin_key = db.add_private_key("admin")?;

    // Create simple tree
    let mut auth = Nested::new();
    auth.set_json(
        "admin",
        AuthKey {
            pubkey: format_public_key(&admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    )
    .unwrap();

    let mut settings = Nested::new();
    settings.set_map("auth", auth);
    let tree = db.new_tree(settings, "admin")?;

    let mut validator = AuthValidator::new();
    let tree_settings = tree.get_settings()?.get_all()?;

    // First resolution should succeed and populate cache
    let sig_key = SigKey::Direct("admin".to_string());
    let result1 = validator.resolve_sig_key(&sig_key, &tree_settings, Some(db.backend()));
    assert!(result1.is_ok());

    // Try to resolve non-existent key (should fail but not corrupt cache)
    let fake_key = SigKey::Direct("nonexistent".to_string());
    let result2 = validator.resolve_sig_key(&fake_key, &tree_settings, Some(db.backend()));
    assert!(result2.is_err());

    // Original key should still resolve correctly (cache should be intact)
    let result3 = validator.resolve_sig_key(&sig_key, &tree_settings, Some(db.backend()));
    assert!(result3.is_ok());

    Ok(())
}

/// Test error message quality and consistency
#[test]
fn test_error_message_consistency() {
    let test_cases = vec![
        (SigKey::DelegationPath(vec![]), "empty"),
        (SigKey::Direct("".to_string()), "empty"),
        (
            SigKey::DelegationPath(vec![
                DelegationStep {
                    key: "nonexistent".to_string(),
                    tips: Some(vec![ID::from("fake_tip")]),
                },
                DelegationStep {
                    key: "final".to_string(),
                    tips: None,
                },
            ]),
            "not found",
        ),
    ];

    let mut validator = AuthValidator::new();
    let settings = Nested::new();
    let db = BaseDB::new(Box::new(InMemoryBackend::new()));

    for (sig_key, expected_error_type) in test_cases {
        let result = validator.resolve_sig_key(&sig_key, &settings, Some(db.backend()));
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string().to_lowercase();

        // Error messages should be descriptive and consistent
        assert!(
            error_msg.contains(expected_error_type)
                || error_msg.contains("authentication")
                || error_msg.contains("validation")
                || error_msg.contains("failed"),
            "Error message '{error_msg}' doesn't contain expected type '{expected_error_type}'"
        );
    }
}

/// Test concurrent validation scenarios (basic thread safety)
#[test]
fn test_concurrent_validation_basic() -> Result<()> {
    use std::sync::Arc;
    use std::thread;

    let db = Arc::new(BaseDB::new(Box::new(InMemoryBackend::new())));

    // Add private key to storage
    let admin_key = db.add_private_key("admin")?;

    // Create tree
    let mut auth = Nested::new();
    auth.set_json(
        "admin",
        AuthKey {
            pubkey: format_public_key(&admin_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    )
    .unwrap();

    let mut settings = Nested::new();
    settings.set_map("auth", auth);
    let tree = db.new_tree(settings, "admin")?;
    let tree_settings = Arc::new(tree.get_settings()?.get_all()?);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let db_clone = Arc::clone(&db);
            let settings_clone = Arc::clone(&tree_settings);

            thread::spawn(move || {
                let mut validator = AuthValidator::new();
                let sig_key = SigKey::Direct("admin".to_string());

                // Each thread should be able to validate independently
                for _ in 0..10 {
                    let result = validator.resolve_sig_key(
                        &sig_key,
                        &settings_clone,
                        Some(db_clone.backend()),
                    );
                    assert!(result.is_ok());
                }
            })
        })
        .collect();

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    Ok(())
}
