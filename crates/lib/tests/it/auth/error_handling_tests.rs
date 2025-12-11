//! Error handling and security tests for flat delegation structure
//!
//! These tests cover error propagation, validation failures, backend issues,
//! and security edge cases in the delegation system.

use eidetica::{
    Result,
    auth::{
        AuthSettings,
        crypto::format_public_key,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyStatus, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    crdt::{Doc, doc::Value},
    entry::ID,
    instance::LegacyInstanceOps,
};

use super::helpers::*;
use crate::helpers::test_instance;

/// Test delegation resolution with missing backend
#[test]
fn test_delegation_without_backend() {
    let delegation_path = create_delegation_path(&[
        ("some_tree", Some(vec![ID::from("tip1")])),
        ("final_key", None),
    ]);

    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();

    // Should fail when database is required but not provided
    assert_permission_resolution_fails(
        &mut validator,
        &delegation_path,
        &auth_settings,
        None,
        "database",
    );
}

/// Test delegation with non-existent delegated tree
#[test]
fn test_delegation_nonexistent_tree() -> Result<()> {
    let (db, mut _user, tree, _) = setup_complete_auth_environment_with_user(
        "admin_user",
        &[("admin", Permission::Admin(0), KeyStatus::Active)],
    );

    // Add delegation to non-existent tree using operations
    let op = tree.new_transaction()?;
    let settings_store = op.get_store::<eidetica::store::DocStore>("_settings")?;

    let nonexistent_delegation = DelegatedTreeRef {
        permission_bounds: PermissionBounds {
            min: None,
            max: Permission::Write(10),
        },
        tree: TreeReference {
            root: ID::from("nonexistent_root"),
            tips: vec![ID::from("nonexistent_tip")],
        },
    };

    let mut new_auth_settings = tree.get_settings()?.get_all()?;
    new_auth_settings.set_json("nonexistent_delegate", nonexistent_delegation)?;
    settings_store.set_value("auth", Value::Doc(new_auth_settings))?;
    op.commit()?;

    // Try to resolve delegation to non-existent tree
    let delegation_path = create_delegation_path(&[
        (
            "nonexistent_delegate",
            Some(vec![ID::from("nonexistent_tip")]),
        ),
        ("some_key", None),
    ]);

    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings()?.get_auth_settings()?;

    assert_permission_resolution_fails(
        &mut validator,
        &delegation_path,
        &auth_settings,
        Some(&db),
        "key",
    );

    Ok(())
}

/// Test delegation with corrupted tree references
#[test]
fn test_delegation_corrupted_tree_references() -> Result<()> {
    let db = test_instance();

    // Add private key to storage
    let admin_key = db.add_private_key("admin")?;

    // Create tree with manually corrupted delegation reference
    let mut auth = Doc::new();
    auth.set_json(
        "admin",
        AuthKey::active(format_public_key(&admin_key), Permission::Admin(0)).unwrap(),
    )
    .unwrap();

    // Add corrupted delegation (invalid tips)
    let mut corrupted_delegate = Doc::new();
    corrupted_delegate.set("permission-bounds", "invalid");
    corrupted_delegate.set("tree", "not_a_tree_ref");
    auth.set("corrupted_delegate", Value::Doc(corrupted_delegate));

    let mut settings = Doc::new();
    settings.set("auth", auth);
    let tree = db.new_database(settings, "admin")?;

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
    let auth_settings = tree.get_settings()?.get_auth_settings()?;
    let result = validator.resolve_sig_key(&delegation_path, &auth_settings, Some(&db));

    // Should fail with appropriate error
    assert!(result.is_err());

    Ok(())
}

/// Test privilege escalation attempt through delegation
#[test]
fn test_privilege_escalation_through_delegation() -> Result<()> {
    let db = test_instance();

    // Add private keys to storage
    let admin_key = db.add_private_key("admin_in_delegated_tree")?;
    let user_key = db.add_private_key("main_admin")?;

    // Create delegated tree with admin permissions
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "admin_in_delegated_tree",
            AuthKey::active(
                format_public_key(&admin_key),
                Permission::Admin(0), // Admin in delegated tree
            )
            .unwrap(),
        )
        .unwrap();

    let mut delegated_settings = Doc::new();
    delegated_settings.set("auth", delegated_auth);
    let delegated_tree = db.new_database(delegated_settings, "admin_in_delegated_tree")?;
    let delegated_tips = delegated_tree.get_tips()?;

    // Create main tree that delegates with restricted permissions
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "main_admin",
            AuthKey::active(format_public_key(&user_key), Permission::Admin(0)).unwrap(),
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

    let mut main_settings = Doc::new();
    main_settings.set("auth", main_auth);
    let main_tree = db.new_database(main_settings, "main_admin")?;

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
    let main_auth_settings = main_tree.get_settings()?.get_auth_settings()?;
    let result = validator.resolve_sig_key(&delegation_path, &main_auth_settings, Some(&db));

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
    let db = test_instance();

    // Add private keys to storage
    let user_key = db.add_private_key("user")?;
    let admin_key = db.add_private_key("admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;

    // Create delegated tree
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "delegated_admin",
            AuthKey::active(
                format_public_key(&delegated_admin_key),
                Permission::Admin(0), // Need admin to create tree
            )
            .unwrap(),
        )
        .unwrap();
    delegated_auth
        .set_json(
            "user",
            AuthKey::active(format_public_key(&user_key), Permission::Write(10)).unwrap(),
        )
        .unwrap();

    let mut delegated_settings = Doc::new();
    delegated_settings.set("auth", delegated_auth);
    let delegated_tree = db.new_database(delegated_settings, "delegated_admin")?;
    let real_tips = delegated_tree.get_tips()?;

    // Create main tree with delegation
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "admin",
            AuthKey::active(format_public_key(&admin_key), Permission::Admin(0)).unwrap(),
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

    let mut main_settings = Doc::new();
    main_settings.set("auth", main_auth);
    let main_tree = db.new_database(main_settings, "admin")?;

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
    let main_auth_settings = main_tree.get_settings()?.get_auth_settings()?;
    let result = validator.resolve_sig_key(&delegation_path, &main_auth_settings, Some(&db));

    // Should fail because tips are invalid
    assert!(result.is_err());

    Ok(())
}

/// Test delegation chain with mixed key statuses
#[test]
fn test_delegation_mixed_key_statuses() -> Result<()> {
    let db = test_instance();

    // Add private keys to storage
    let active_user_key = db.add_private_key("active_user")?;
    let revoked_key = db.add_private_key("revoked_user")?;
    let admin_key = db.add_private_key("admin")?;
    let delegated_admin_key = db.add_private_key("delegated_admin")?;

    // Create delegated tree with mix of active and revoked keys
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "delegated_admin",
            AuthKey::active(
                format_public_key(&delegated_admin_key),
                Permission::Admin(0), // Need admin to create tree
            )
            .unwrap(),
        )
        .unwrap();
    delegated_auth
        .set_json(
            "active_user",
            AuthKey::active(format_public_key(&active_user_key), Permission::Write(10)).unwrap(),
        )
        .unwrap();
    delegated_auth
        .set_json(
            "revoked_user",
            AuthKey::new(
                format_public_key(&revoked_key),
                Permission::Write(10),
                KeyStatus::Revoked, // Revoked key
            )
            .unwrap(),
        )
        .unwrap();

    let mut delegated_settings = Doc::new();
    delegated_settings.set("auth", delegated_auth);
    let delegated_tree = db.new_database(delegated_settings, "delegated_admin")?;
    let delegated_tips = delegated_tree.get_tips()?;

    // Create main tree with delegation
    let mut main_auth = Doc::new();
    main_auth
        .set_json(
            "admin",
            AuthKey::active(format_public_key(&admin_key), Permission::Admin(0)).unwrap(),
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

    let mut main_settings = Doc::new();
    main_settings.set("auth", main_auth);
    let main_tree = db.new_database(main_settings, "admin")?;

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
    let main_auth_settings = main_tree.get_settings()?.get_auth_settings()?;
    let result = validator.resolve_sig_key(&active_delegation, &main_auth_settings, Some(&db));

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

    let result = validator.resolve_sig_key(&revoked_delegation, &main_auth_settings, Some(&db));

    // Should succeed in resolving but key should be marked as revoked
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.key_status, KeyStatus::Revoked);

    Ok(())
}

/// Test validation cache behavior under error conditions
#[test]
fn test_validation_cache_error_conditions() -> Result<()> {
    let db = test_instance();

    // Add private key to storage
    let admin_key = db.add_private_key("admin")?;

    // Create simple tree
    let mut auth = Doc::new();
    auth.set_json(
        "admin",
        AuthKey::active(format_public_key(&admin_key), Permission::Admin(0)).unwrap(),
    )
    .unwrap();

    let mut settings = Doc::new();
    settings.set("auth", auth);
    let tree = db.new_database(settings, "admin")?;

    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings()?.get_auth_settings()?;

    // First resolution should succeed and populate cache
    let sig_key = SigKey::Direct("admin".to_string());
    let result1 = validator.resolve_sig_key(&sig_key, &auth_settings, Some(&db));
    assert!(result1.is_ok());

    // Try to resolve non-existent key (should fail but not corrupt cache)
    let fake_key = SigKey::Direct("nonexistent".to_string());
    let result2 = validator.resolve_sig_key(&fake_key, &auth_settings, Some(&db));
    assert!(result2.is_err());

    // Original key should still resolve correctly (cache should be intact)
    let result3 = validator.resolve_sig_key(&sig_key, &auth_settings, Some(&db));
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
    let auth_settings = AuthSettings::new();
    let db = test_instance();

    for (sig_key, expected_error_type) in test_cases {
        let result = validator.resolve_sig_key(&sig_key, &auth_settings, Some(&db));
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string().to_lowercase();

        // Error messages should be descriptive and consistent
        assert!(
            error_msg.contains(expected_error_type)
                || error_msg.contains("authentication")
                || error_msg.contains("validation")
                || error_msg.contains("failed")
                || error_msg.contains("auth")
                || error_msg.contains("key")
                || error_msg.contains("configuration"),
            "Error message '{error_msg}' doesn't contain expected type '{expected_error_type}'"
        );
    }
}

/// Test concurrent validation scenarios (basic thread safety)
#[test]
fn test_concurrent_validation_basic() -> Result<()> {
    use std::{sync::Arc, thread};

    let db = Arc::new(test_instance());

    // Add private key to storage
    let admin_key = db.add_private_key("admin")?;

    // Create tree
    let mut auth = Doc::new();
    auth.set_json(
        "admin",
        AuthKey::active(format_public_key(&admin_key), Permission::Admin(0)).unwrap(),
    )
    .unwrap();

    let mut settings = Doc::new();
    settings.set("auth", auth);
    let tree = db.new_database(settings, "admin")?;
    let auth_settings = Arc::new(tree.get_settings()?.get_auth_settings()?);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let db_clone = Arc::clone(&db);
            let settings_clone = Arc::clone(&auth_settings);

            thread::spawn(move || {
                let mut validator = AuthValidator::new();
                let sig_key = SigKey::Direct("admin".to_string());

                // Each thread should be able to validate independently
                for _ in 0..10 {
                    let result =
                        validator.resolve_sig_key(&sig_key, &settings_clone, Some(&db_clone));
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
