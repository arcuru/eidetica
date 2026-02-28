//! Error handling and security tests for flat delegation structure
//!
//! These tests cover error propagation, validation failures, backend issues,
//! and security edge cases in the delegation system.

use eidetica::{
    Result,
    auth::{
        AuthSettings,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, KeyStatus, Permission,
            PermissionBounds, SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    crdt::{Doc, doc::Value},
    entry::ID,
    store::DocStore,
};

use super::helpers::*;
use crate::helpers::{test_instance, test_instance_with_user};

/// Test delegation resolution with missing backend
#[tokio::test]
async fn test_delegation_without_backend() {
    // Create a delegation path with a fake root ID
    let fake_root_id =
        ID::new("sha256:0000000000000000000000000000000000000000000000000000000000000001");
    let delegation_path = create_delegation_path(
        &[(&fake_root_id, vec![ID::from("tip1")])],
        KeyHint::from_name("final_key"),
    );

    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();

    // Should fail when database is required but not provided
    assert_permission_resolution_fails(
        &mut validator,
        &delegation_path,
        &auth_settings,
        None,
        "database",
    )
    .await;
}

/// Test delegation with non-existent delegated tree
#[tokio::test]
async fn test_delegation_nonexistent_tree() -> Result<()> {
    let (db, mut _user, tree, _) = setup_complete_auth_environment_with_user(
        "admin_user",
        &[("admin", Permission::Admin(0), KeyStatus::Active)],
    )
    .await;

    // Add delegation to non-existent tree using operations
    let txn = tree.new_transaction().await?;
    let settings_store = txn.get_store::<DocStore>("_settings").await?;

    let nonexistent_root_id = ID::from("nonexistent_root");
    let nonexistent_delegation = DelegatedTreeRef {
        permission_bounds: PermissionBounds {
            min: None,
            max: Permission::Write(10),
        },
        tree: TreeReference {
            root: nonexistent_root_id.clone(),
            tips: vec![ID::from("nonexistent_tip")],
        },
    };

    let mut new_auth_settings = tree.get_settings().await?.get_all().await?;
    // Store by root ID (the new storage format)
    new_auth_settings.set_json(nonexistent_root_id.as_str(), nonexistent_delegation)?;
    settings_store
        .set_value("auth", Value::Doc(new_auth_settings))
        .await?;
    txn.commit().await?;

    // Try to resolve delegation to non-existent tree
    let delegation_path = create_delegation_path(
        &[(&nonexistent_root_id, vec![ID::from("nonexistent_tip")])],
        KeyHint::from_name("some_key"),
    );

    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings().await?.auth_snapshot().await?;

    assert_permission_resolution_fails(
        &mut validator,
        &delegation_path,
        &auth_settings,
        Some(&db),
        "delegation not found",
    )
    .await;

    Ok(())
}

/// Test delegation with corrupted tree references
#[tokio::test]
async fn test_delegation_corrupted_tree_references() -> Result<()> {
    let (db, mut user) = test_instance_with_user("test_user").await;

    // Add private key to storage
    let admin_key_id = user.add_private_key(Some("admin")).await?;

    // Create tree (signing key becomes Admin(0))
    let settings = Doc::new();
    let tree = user.create_database(settings, &admin_key_id).await?;

    // Add corrupted delegation via transaction
    let txn = tree.new_transaction().await?;
    let settings_store = txn.get_store::<DocStore>("_settings").await?;

    // Add corrupted delegation (invalid tips) - store under a valid-looking root ID
    let corrupted_root_id =
        ID::new("sha256:corrupted0000000000000000000000000000000000000000000000000000");
    let mut corrupted_delegate = Doc::new();
    corrupted_delegate.set("permission-bounds", "invalid");
    corrupted_delegate.set("tree", "not_a_tree_ref");

    let mut new_auth_settings = tree.get_settings().await?.get_all().await?;
    new_auth_settings.set(corrupted_root_id.as_str(), Value::Doc(corrupted_delegate));
    settings_store
        .set_value("auth", Value::Doc(new_auth_settings))
        .await?;
    txn.commit().await?;

    // Try to resolve corrupted delegation
    let delegation_path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: corrupted_root_id.to_string(),
            tips: vec![ID::from("some_tip")],
        }],
        hint: KeyHint::from_name("some_key"),
    };

    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings().await?.auth_snapshot().await?;
    let result = validator
        .resolve_sig_key(&delegation_path, &auth_settings, Some(&db))
        .await;

    // Should fail with appropriate error
    assert!(result.is_err());

    Ok(())
}

/// Test privilege escalation attempt through delegation
#[tokio::test]
async fn test_privilege_escalation_through_delegation() -> Result<()> {
    let (db, mut user) = test_instance_with_user("test_user").await;

    // Add private keys to storage
    let admin_key_id = user
        .add_private_key(Some("admin_in_delegated_tree"))
        .await?;
    let user_key_id = user.add_private_key(Some("main_admin")).await?;

    // Create delegated tree (signing key becomes Admin(0))
    let delegated_settings = Doc::new();
    let delegated_tree = user
        .create_database(delegated_settings, &admin_key_id)
        .await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    // Create main tree (signing key becomes Admin(0))
    let main_settings = Doc::new();
    let main_tree = user.create_database(main_settings, &user_key_id).await?;

    // Add delegation with permission restriction via transaction
    let txn = main_tree.new_transaction().await?;
    let settings_store = txn.get_settings()?;
    let delegated_tree_root = delegated_tree.root_id().clone();
    settings_store
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                min: None,
                max: Permission::Write(10), // Restrict to Write only
            },
            tree: TreeReference {
                root: delegated_tree_root.clone(),
                tips: delegated_tips.clone(),
            },
        })
        .await?;
    txn.commit().await?;

    // Try to use admin key from delegated tree through restricted delegation
    let delegation_path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: delegated_tips,
        }],
        hint: KeyHint::from_pubkey(admin_key_id.to_string()),
    };

    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;
    let result = validator
        .resolve_sig_key(&delegation_path, &main_auth_settings, Some(&db))
        .await;

    // Should succeed but with clamped permissions
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.len(), 1);

    // Effective permission should be Write (clamped), not Admin
    assert_eq!(resolved[0].effective_permission, Permission::Write(10));
    assert!(!resolved[0].effective_permission.can_admin()); // Should not have admin privileges

    Ok(())
}

/// Test delegation with tampered tips
#[tokio::test]
async fn test_delegation_with_tampered_tips() -> Result<()> {
    let (db, mut user) = test_instance_with_user("test_user").await;

    // Add private keys to storage
    let user_key_id = user.add_private_key(Some("user")).await?;
    let admin_key_id = user.add_private_key(Some("admin")).await?;
    let delegated_admin_key_id = user.add_private_key(Some("delegated_admin")).await?;

    // Create delegated tree (signing key becomes Admin(0))
    let delegated_settings = Doc::new();
    let delegated_tree = user
        .create_database(delegated_settings, &delegated_admin_key_id)
        .await?;

    // Add user key via transaction
    let txn = delegated_tree.new_transaction().await?;
    let settings_store = txn.get_settings()?;
    settings_store
        .set_auth_key(
            &user_key_id.to_string(),
            AuthKey::active(Some("user"), Permission::Write(10)),
        )
        .await?;
    txn.commit().await?;

    let real_tips = delegated_tree.get_tips().await?;

    // Create main tree (signing key becomes Admin(0))
    let main_settings = Doc::new();
    let main_tree = user.create_database(main_settings, &admin_key_id).await?;

    // Add delegation via transaction
    let txn = main_tree.new_transaction().await?;
    let settings_store = txn.get_settings()?;
    let delegated_tree_root = delegated_tree.root_id().clone();
    settings_store
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                min: None,
                max: Permission::Write(10),
            },
            tree: TreeReference {
                root: delegated_tree_root.clone(),
                tips: real_tips.clone(),
            },
        })
        .await?;
    txn.commit().await?;

    // Try to use delegation with fake/tampered tips
    let fake_tips = vec![ID::from("fake_tip_1"), ID::from("fake_tip_2")];
    let delegation_path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: fake_tips, // Using fake tips instead of real ones
        }],
        hint: KeyHint::from_pubkey(user_key_id.to_string()),
    };

    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;
    let result = validator
        .resolve_sig_key(&delegation_path, &main_auth_settings, Some(&db))
        .await;

    // Should fail because tips are invalid
    assert!(result.is_err());

    Ok(())
}

/// Test delegation chain with mixed key statuses
#[tokio::test]
async fn test_delegation_mixed_key_statuses() -> Result<()> {
    let (db, mut user) = test_instance_with_user("test_user").await;

    // Add private keys to storage
    let active_user_key_id = user.add_private_key(Some("active_user")).await?;
    let revoked_key_id = user.add_private_key(Some("revoked_user")).await?;
    let admin_key_id = user.add_private_key(Some("admin")).await?;
    let delegated_admin_key_id = user.add_private_key(Some("delegated_admin")).await?;

    // Create delegated tree (signing key becomes Admin(0))
    let delegated_settings = Doc::new();
    let delegated_tree = user
        .create_database(delegated_settings, &delegated_admin_key_id)
        .await?;

    // Add active and revoked keys via transaction
    let txn = delegated_tree.new_transaction().await?;
    let settings_store = txn.get_settings()?;
    settings_store
        .set_auth_key(
            &active_user_key_id.to_string(),
            AuthKey::active(Some("active_user"), Permission::Write(10)),
        )
        .await?;
    settings_store
        .set_auth_key(
            &revoked_key_id.to_string(),
            AuthKey::new(
                Some("revoked_user"),
                Permission::Write(10),
                KeyStatus::Revoked,
            ),
        )
        .await?;
    txn.commit().await?;

    let delegated_tips = delegated_tree.get_tips().await?;

    // Create main tree (signing key becomes Admin(0))
    let main_settings = Doc::new();
    let main_tree = user.create_database(main_settings, &admin_key_id).await?;

    // Add delegation via transaction
    let txn = main_tree.new_transaction().await?;
    let settings_store = txn.get_settings()?;
    let delegated_tree_root = delegated_tree.root_id().clone();
    settings_store
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                min: None,
                max: Permission::Write(10),
            },
            tree: TreeReference {
                root: delegated_tree_root.clone(),
                tips: delegated_tips.clone(),
            },
        })
        .await?;
    txn.commit().await?;

    // Test accessing active key through delegation
    let active_delegation = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: delegated_tips.clone(),
        }],
        hint: KeyHint::from_pubkey(active_user_key_id.to_string()),
    };

    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;
    let result = validator
        .resolve_sig_key(&active_delegation, &main_auth_settings, Some(&db))
        .await;

    // Should succeed for active key
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].key_status, KeyStatus::Active);

    // Test accessing revoked key through delegation
    let revoked_delegation = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: delegated_tips,
        }],
        hint: KeyHint::from_pubkey(revoked_key_id.to_string()),
    };

    let result = validator
        .resolve_sig_key(&revoked_delegation, &main_auth_settings, Some(&db))
        .await;

    // Should succeed in resolving but key should be marked as revoked
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].key_status, KeyStatus::Revoked);

    Ok(())
}

/// Test validation cache behavior under error conditions
#[tokio::test]
async fn test_validation_cache_error_conditions() -> Result<()> {
    let (db, mut user) = test_instance_with_user("test_user").await;

    // Add private key to storage
    let admin_key_id = user.add_private_key(Some("admin")).await?;

    // Create simple tree (signing key becomes Admin(0))
    let settings = Doc::new();
    let tree = user.create_database(settings, &admin_key_id).await?;

    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings().await?.auth_snapshot().await?;

    // First resolution should succeed and populate cache
    let sig_key = SigKey::from_pubkey(admin_key_id.to_string());
    let result1 = validator
        .resolve_sig_key(&sig_key, &auth_settings, Some(&db))
        .await;
    assert!(result1.is_ok());

    // Try to resolve non-existent key (should fail but not corrupt cache)
    let fake_key = SigKey::from_pubkey("nonexistent");
    let result2 = validator
        .resolve_sig_key(&fake_key, &auth_settings, Some(&db))
        .await;
    assert!(result2.is_err());

    // Original key should still resolve correctly (cache should be intact)
    let result3 = validator
        .resolve_sig_key(&sig_key, &auth_settings, Some(&db))
        .await;
    assert!(result3.is_ok());

    Ok(())
}

/// Test error message quality and consistency
#[tokio::test]
async fn test_error_message_consistency() {
    let test_cases = vec![
        (
            SigKey::Delegation {
                path: vec![],
                hint: KeyHint::from_name("final"),
            },
            "empty",
        ),
        (SigKey::from_pubkey(""), "empty"),
        (
            SigKey::Delegation {
                path: vec![DelegationStep {
                    tree: "nonexistent".to_string(),
                    tips: vec![ID::from("fake_tip")],
                }],
                hint: KeyHint::from_name("final"),
            },
            "not found",
        ),
    ];

    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();
    let db = test_instance().await;

    for (sig_key, expected_error_type) in test_cases {
        let result = validator
            .resolve_sig_key(&sig_key, &auth_settings, Some(&db))
            .await;
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
#[tokio::test(flavor = "current_thread")]
async fn test_concurrent_validation_basic() -> Result<()> {
    use std::sync::Arc;

    let (db, mut user) = test_instance_with_user("test_user").await;
    let db = Arc::new(db);

    // Add private key to storage
    let admin_key_id = user.add_private_key(Some("admin")).await?;

    // Create tree (signing key becomes Admin(0))
    let settings = Doc::new();
    let tree = user.create_database(settings, &admin_key_id).await?;
    let auth_settings = Arc::new(tree.get_settings().await?.auth_snapshot().await?);
    let admin_key_id = Arc::new(admin_key_id);

    let local = tokio::task::LocalSet::new();
    let mut handles = Vec::new();

    for _ in 0..4 {
        let db_clone = Arc::clone(&db);
        let settings_clone = Arc::clone(&auth_settings);
        let admin_key_clone = Arc::clone(&admin_key_id);

        let handle = local.spawn_local(async move {
            let mut validator = AuthValidator::new();
            let sig_key = SigKey::from_pubkey(admin_key_clone.to_string());

            // Each task should be able to validate independently
            for _ in 0..10 {
                let result = validator
                    .resolve_sig_key(&sig_key, &settings_clone, Some(&db_clone))
                    .await;
                assert!(result.is_ok());
            }
        });

        handles.push(handle);
    }

    local
        .run_until(async {
            for handle in handles {
                handle.await.unwrap();
            }
        })
        .await;

    Ok(())
}
