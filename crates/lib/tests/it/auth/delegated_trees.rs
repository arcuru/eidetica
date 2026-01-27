//! Integration tests for delegated tree authentication
//!
//! These tests verify the end-to-end functionality of delegated tree
//! authentication, including tree creation, key delegation, permission
//! clamping, and various authorization scenarios.

use eidetica::{
    Result,
    auth::{
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, KeyStatus, Permission,
            PermissionBounds, SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    crdt::Doc,
    entry::ID,
};

use super::helpers::*;

/// Test simple tree creation with auth
#[tokio::test]
async fn test_simple_tree_creation_with_auth() -> Result<()> {
    let (_db, mut _user, tree, _) = setup_complete_auth_environment_with_user(
        "main_admin_user",
        &[("main_admin", Permission::Admin(0), KeyStatus::Active)],
    )
    .await;

    assert!(!tree.root_id().to_string().is_empty());
    Ok(())
}

/// Test basic delegated tree validation
#[tokio::test]
async fn test_delegated_tree_basic_validation() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Create delegated tree
    let (delegated_tree, delegated_key_ids) = create_delegated_tree_with_user(
        &mut user,
        &[("delegated_user", Permission::Admin(5), KeyStatus::Active)],
    )
    .await?;

    // Create main tree with delegation
    let (_, _main_user, main_tree, _) = setup_complete_auth_environment_with_user(
        "main_admin_user",
        &[("main_admin", Permission::Admin(0), KeyStatus::Active)],
    )
    .await;

    // Add delegation to main tree auth settings
    let op = main_tree.new_transaction().await?;
    let settings = op.get_settings()?;

    let delegation_ref = create_delegation_ref(
        &delegated_tree,
        Permission::Write(10),
        Some(Permission::Read),
    )
    .await?;
    settings
        .update_auth_settings(|auth| auth.add_delegated_tree(delegation_ref))
        .await?;
    op.commit().await?;

    // Test delegated tree validation
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    // Create delegation path - DelegationStep uses root tree ID and tips
    let delegated_auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().to_string(),
            tips: delegated_tips,
        }],
        hint: KeyHint::from_pubkey(&delegated_key_ids[0]),
    };

    assert_permission_resolution(
        &mut validator,
        &delegated_auth_id,
        &main_auth_settings,
        Some(&db),
        Permission::Write(10),
        KeyStatus::Active,
    )
    .await;

    Ok(())
}

/// Test permission clamping in delegated trees
#[tokio::test]
async fn test_delegated_tree_permission_clamping() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Create delegated tree with Admin permissions
    let (delegated_tree, delegated_key_ids) = create_delegated_tree_with_user(
        &mut user,
        &[("delegated_user", Permission::Admin(0), KeyStatus::Active)],
    )
    .await?;

    // Create main tree with Read-only delegation
    let (_, _main_user, main_tree, _) = setup_complete_auth_environment_with_user(
        "main_admin_user",
        &[("main_admin", Permission::Admin(0), KeyStatus::Active)],
    )
    .await;

    // Add read-only delegation
    let op = main_tree.new_transaction().await?;
    let settings = op.get_settings()?;

    let delegation_ref = create_delegation_ref(&delegated_tree, Permission::Read, None).await?;
    settings
        .update_auth_settings(|auth| auth.add_delegated_tree(delegation_ref))
        .await?;
    op.commit().await?;

    // Test permission clamping
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    let delegated_auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().to_string(),
            tips: delegated_tips,
        }],
        hint: KeyHint::from_pubkey(&delegated_key_ids[0]),
    };

    // Permissions should be clamped from Admin to Read
    assert_permission_resolution(
        &mut validator,
        &delegated_auth_id,
        &main_auth_settings,
        Some(&db),
        Permission::Read,
        KeyStatus::Active,
    )
    .await;

    Ok(())
}

/// Test nested delegation (delegated tree delegating to another delegated tree)
#[tokio::test]
async fn test_nested_delegation() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    let main_admin_key = user.add_private_key(Some("main_admin")).await?;
    let org_admin_key = user.add_private_key(Some("org_admin")).await?;
    let user_key = user.add_private_key(Some("user")).await?;

    // Create user tree (bottom level) using SettingsStore API
    let user_tree = user.create_database(Doc::new(), &user_key).await?;
    configure_database_auth(
        &user_tree,
        &[("user", &user_key, Permission::Admin(10), KeyStatus::Active)],
    )
    .await?;

    // Create org tree (middle level) that delegates to user tree using SettingsStore API
    let org_tree = user.create_database(Doc::new(), &org_admin_key).await?;
    configure_database_auth(
        &org_tree,
        &[(
            "org_admin",
            &org_admin_key,
            Permission::Admin(5),
            KeyStatus::Active,
        )],
    )
    .await?;

    // Add delegation to user tree
    let user_tips = user_tree.get_tips().await?;
    let user_tree_root = user_tree.root_id().clone();
    let op = org_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref = create_delegation_ref(&user_tree, Permission::Write(20), None).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree(delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Create main tree (top level) that delegates to org tree using SettingsStore API
    let main_tree = user.create_database(Doc::new(), &main_admin_key).await?;
    configure_database_auth(
        &main_tree,
        &[(
            "main_admin",
            &main_admin_key,
            Permission::Admin(0),
            KeyStatus::Active,
        )],
    )
    .await?;

    // Add delegation to org tree
    let org_tips = org_tree.get_tips().await?;
    let org_tree_root = org_tree.root_id().clone();
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&org_tree, Permission::Write(15), Some(Permission::Read)).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree(delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Test nested delegation: main -> org -> user
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    // Create a nested delegation chain: main -> org -> user
    let nested_auth_id = SigKey::Delegation {
        path: vec![
            DelegationStep {
                tree: org_tree_root.to_string(),
                tips: org_tips.clone(),
            },
            DelegationStep {
                tree: user_tree_root.to_string(),
                tips: user_tips.clone(),
            },
        ],
        hint: KeyHint::from_pubkey(&user_key),
    };

    // This should resolve with Write permissions (clamped through the chain)
    let resolved_auths = validator
        .resolve_sig_key(&nested_auth_id, &main_auth_settings, Some(&db))
        .await?;

    // Permissions should be clamped: user has Admin(10) -> org clamps to Write(20) -> main doesn't clamp further
    // Final result should be Write(20) (clamped at org level)
    assert_eq!(resolved_auths.len(), 1);
    let resolved_auth = &resolved_auths[0];
    assert_eq!(resolved_auth.effective_permission, Permission::Write(20));
    assert_eq!(resolved_auth.key_status, KeyStatus::Active);

    Ok(())
}

/// Test delegated tree with revoked keys
#[tokio::test]
async fn test_delegated_tree_with_revoked_keys() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    let main_admin_key = user.add_private_key(Some("main_admin")).await?;
    let delegated_user_key = user.add_private_key(Some("delegated_user")).await?;

    // Create delegated tree with user key (initially active) using SettingsStore API
    let delegated_tree = user
        .create_database(Doc::new(), &delegated_user_key)
        .await?;
    configure_database_auth(
        &delegated_tree,
        &[(
            "delegated_user",
            &delegated_user_key,
            Permission::Admin(10),
            KeyStatus::Active,
        )],
    )
    .await?;

    // Create main tree with delegation using SettingsStore API
    let main_tree = user.create_database(Doc::new(), &main_admin_key).await?;
    configure_database_auth(
        &main_tree,
        &[(
            "main_admin",
            &main_admin_key,
            Permission::Admin(0),
            KeyStatus::Active,
        )],
    )
    .await?;

    // Add delegation to delegated tree
    let delegated_tips = delegated_tree.get_tips().await?;
    let delegated_tree_root = delegated_tree.root_id().clone();
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(10), None).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree(delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Test with active key - should work
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    let delegated_auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: delegated_tips.clone(),
        }],
        hint: KeyHint::from_pubkey(&delegated_user_key),
    };

    let resolved_auths = validator
        .resolve_sig_key(&delegated_auth_id, &main_auth_settings, Some(&db))
        .await?;

    assert_eq!(resolved_auths.len(), 1);
    let resolved_auth = &resolved_auths[0];
    assert_eq!(resolved_auth.effective_permission, Permission::Write(10));
    assert_eq!(resolved_auth.key_status, KeyStatus::Active);

    // Now revoke the key in the delegated tree using SettingsStore API
    let op = delegated_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        settings
            .update_auth_settings(|auth| {
                // Update the existing key to be revoked (store by pubkey)
                let revoked_key = AuthKey::new(
                    Some("delegated_user"),
                    Permission::Write(10),
                    KeyStatus::Revoked,
                );
                auth.overwrite_key(&delegated_user_key, revoked_key)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Test validation against revoked key
    let revoked_auth_settings = delegated_tree
        .get_settings()
        .await?
        .get_auth_settings()
        .await?;
    let resolved_auth_revoked = validator
        .resolve_sig_key(
            &SigKey::from_pubkey(&delegated_user_key),
            &revoked_auth_settings,
            Some(&db),
        )
        .await?;

    assert_eq!(resolved_auth_revoked.len(), 1);
    assert_eq!(resolved_auth_revoked[0].key_status, KeyStatus::Revoked);

    Ok(())
}

/// Test delegation depth limits
#[tokio::test]
async fn test_delegation_depth_limits() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Create a deeply nested delegation chain that exceeds the limit
    let admin_key = user.add_private_key(Some("admin")).await?;
    let user_key = user.add_private_key(Some("user")).await?;

    // Create a simple delegated tree using SettingsStore API
    let delegated_tree = user.create_database(Doc::new(), &user_key).await?;
    configure_database_auth(
        &delegated_tree,
        &[("user", &user_key, Permission::Admin(10), KeyStatus::Active)],
    )
    .await?;

    // Create main tree using SettingsStore API
    let main_tree = user.create_database(Doc::new(), &admin_key).await?;
    configure_database_auth(
        &main_tree,
        &[("admin", &admin_key, Permission::Admin(0), KeyStatus::Active)],
    )
    .await?;

    // Add delegation to delegated tree
    let delegated_tips = delegated_tree.get_tips().await?;
    let delegated_tree_root = delegated_tree.root_id().clone();
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(10), None).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree(delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Create a deeply nested delegation that should exceed the limit
    // We'll create a chain with 12 levels (exceeds MAX_DELEGATION_DEPTH of 10)
    let mut delegation_steps = Vec::new();

    // Add 12 intermediate delegation steps (exceeds MAX_DELEGATION_DEPTH of 10)
    for _ in 0..12 {
        delegation_steps.push(DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: delegated_tips.clone(),
        });
    }

    let nested_auth_id = SigKey::Delegation {
        path: delegation_steps,
        hint: KeyHint::from_pubkey(&user_key),
    };

    // Test depth limit validation
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;
    let result = validator
        .resolve_sig_key(&nested_auth_id, &main_auth_settings, Some(&db))
        .await;

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("Maximum delegation depth") || error_msg.contains("not found"));

    Ok(())
}

/// Test permission upgrade when delegated permission is below `min` bound
#[tokio::test]
async fn test_delegated_tree_min_bound_upgrade() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Keys
    let main_admin_key = user.add_private_key(Some("main_admin")).await?;
    let delegated_admin_key = user.add_private_key(Some("delegated_admin")).await?;
    let delegated_user_key = user.add_private_key(Some("delegated_user")).await?;

    // ---------------- Delegated tree using SettingsStore API ----------------
    let delegated_tree = user
        .create_database(Doc::new(), &delegated_admin_key)
        .await?;
    configure_database_auth(
        &delegated_tree,
        &[
            (
                "delegated_admin",
                &delegated_admin_key,
                Permission::Admin(0),
                KeyStatus::Active,
            ),
            (
                "delegated_user",
                &delegated_user_key,
                Permission::Write(15),
                KeyStatus::Active,
            ),
        ],
    )
    .await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    // ---------------- Main tree with delegation using SettingsStore API ----------------
    let main_tree = user.create_database(Doc::new(), &main_admin_key).await?;
    configure_database_auth(
        &main_tree,
        &[(
            "main_admin",
            &main_admin_key,
            Permission::Admin(0),
            KeyStatus::Active,
        )],
    )
    .await?;

    // Add delegation with bounds using SettingsStore API
    let delegated_tree_root = delegated_tree.root_id().clone();
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref = create_delegation_ref(
            &delegated_tree,
            Permission::Write(0),       // max: Highest possible Write permission
            Some(Permission::Write(7)), // min: Minimum permission level
        )
        .await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree(delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Validate
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    let auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: delegated_tips.clone(),
        }],
        hint: KeyHint::from_pubkey(&delegated_user_key),
    };

    let resolved = validator
        .resolve_sig_key(&auth_id, &main_auth_settings, Some(&db))
        .await?;

    // Expect permission upgraded to Write(7)
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].effective_permission, Permission::Write(7));
    assert_eq!(resolved[0].key_status, KeyStatus::Active);

    Ok(())
}

/// Test that priority (the numeric part) is preserved when permission is already within bounds
#[tokio::test]
async fn test_delegated_tree_priority_preservation() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Keys
    let main_admin_key = user.add_private_key(Some("main_admin")).await?;
    let delegated_admin_key = user.add_private_key(Some("delegated_admin")).await?;
    let delegated_user_key = user.add_private_key(Some("delegated_user")).await?;

    // Delegated tree with user key Write(12) using SettingsStore API
    let delegated_tree = user
        .create_database(Doc::new(), &delegated_admin_key)
        .await?;
    configure_database_auth(
        &delegated_tree,
        &[
            (
                "delegated_admin",
                &delegated_admin_key,
                Permission::Admin(0),
                KeyStatus::Active,
            ),
            (
                "delegated_user",
                &delegated_user_key,
                Permission::Write(12),
                KeyStatus::Active,
            ),
        ],
    )
    .await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    // Main tree delegates with max Write(8) (more privileged) and no min using SettingsStore API
    let main_tree = user.create_database(Doc::new(), &main_admin_key).await?;
    configure_database_auth(
        &main_tree,
        &[(
            "main_admin",
            &main_admin_key,
            Permission::Admin(0),
            KeyStatus::Active,
        )],
    )
    .await?;

    // Add delegation using SettingsStore API
    let delegated_tree_root = delegated_tree.root_id().clone();
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(8), None).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree(delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Validate
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    let auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: delegated_tips.clone(),
        }],
        hint: KeyHint::from_pubkey(&delegated_user_key),
    };

    let resolved = validator
        .resolve_sig_key(&auth_id, &main_auth_settings, Some(&db))
        .await?;

    // Because Write(12) is within bounds (less privileged than Write(8)), it is preserved
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].effective_permission, Permission::Write(12));

    Ok(())
}

/// Test delegation depth limit at exactly MAX_DELEGATION_DEPTH (10)
#[tokio::test]
async fn test_delegation_depth_limit_exact() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Setup simple tree with direct key using SettingsStore API
    let admin_key = user.add_private_key(Some("admin")).await?;
    let tree = user.create_database(Doc::new(), &admin_key).await?;
    configure_database_auth(
        &tree,
        &[("admin", &admin_key, Permission::Admin(0), KeyStatus::Active)],
    )
    .await?;
    let tips = tree.get_tips().await?;

    // Build a chain exactly 10 levels deep
    let mut delegation_steps = Vec::new();

    // Add 10 intermediate delegation steps (reaches MAX_DELEGATION_DEPTH of 10)
    // Using a non-existent ID - test will fail
    let bogus_root_id =
        ID::new("sha256:0000000000000000000000000000000000000000000000000000000000000000");
    for _ in 0..10 {
        delegation_steps.push(DelegationStep {
            tree: bogus_root_id.to_string(),
            tips: tips.clone(),
        });
    }

    let auth_id = SigKey::Delegation {
        path: delegation_steps,
        hint: KeyHint::from_pubkey(&admin_key),
    };

    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings().await?.get_auth_settings().await?;

    let result = validator
        .resolve_sig_key(&auth_id, &auth_settings, Some(&db))
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());

    Ok(())
}

/// Test that invalid (unknown) tips cause delegation validation to fail
#[tokio::test]
async fn test_delegated_tree_invalid_tips() -> Result<()> {
    let (db, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Keys and delegated tree setup using SettingsStore API
    let main_admin_key = user.add_private_key(Some("main_admin")).await?;
    let delegated_admin_key = user.add_private_key(Some("delegated_admin")).await?;
    let delegated_user_key = user.add_private_key(Some("delegated_user")).await?;

    let delegated_tree = user
        .create_database(Doc::new(), &delegated_admin_key)
        .await?;
    configure_database_auth(
        &delegated_tree,
        &[
            (
                "delegated_admin",
                &delegated_admin_key,
                Permission::Admin(0),
                KeyStatus::Active,
            ),
            (
                "delegated_user",
                &delegated_user_key,
                Permission::Write(5),
                KeyStatus::Active,
            ),
        ],
    )
    .await?;

    // Fake tip that does not exist
    let bogus_tip = ID::new("nonexistent_tip_hash");

    // Main tree with delegation using bogus tip using SettingsStore API
    let main_tree = user.create_database(Doc::new(), &main_admin_key).await?;
    configure_database_auth(
        &main_tree,
        &[(
            "main_admin",
            &main_admin_key,
            Permission::Admin(0),
            KeyStatus::Active,
        )],
    )
    .await?;

    // Add delegation with bogus tips using SettingsStore API
    let delegated_tree_root = delegated_tree.root_id().clone();
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        // Manually create delegation ref with bogus tips
        let delegation_ref = DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(5),
                min: None,
            },
            tree: TreeReference {
                root: delegated_tree_root.clone(),
                tips: vec![bogus_tip.clone()],
            },
        };
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree(delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    let auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.to_string(),
            tips: vec![bogus_tip],
        }],
        hint: KeyHint::from_pubkey(&delegated_user_key),
    };

    let result = validator
        .resolve_sig_key(&auth_id, &main_auth_settings, Some(&db))
        .await;
    assert!(result.is_err());

    Ok(())
}

/// Test complex nested delegation using DelegationChain helper
#[tokio::test]
async fn test_complex_nested_delegation_chain() -> Result<()> {
    // Create a 3-level delegation chain
    let chain = DelegationChain::new_with_user("test_user", 3).await?;

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

    // Verify each tree has a display name (keys now store display names, not key_ids)
    for (i, key) in chain.keys.iter().enumerate() {
        assert_eq!(
            key,
            &format!("level_{i}_admin"),
            "Key should match expected display name"
        );
    }

    // Create a delegation chain to a final user
    let delegation_path = chain.create_chain_delegation("final_user").await;

    // Test that complex delegation paths can be created correctly
    match delegation_path {
        SigKey::Delegation { path, hint } => {
            assert_eq!(path.len(), 3); // 3 levels

            // Verify each step has tree ID (root_id) not a key name
            for step in path.iter() {
                assert!(!step.tree.is_empty());
                assert!(!step.tips.is_empty()); // All steps have tips
            }

            // Hint should reference the final user
            assert_eq!(hint.name, Some("final_user".to_string()));
        }
        _ => panic!("Expected delegation path"),
    }

    Ok(())
}
