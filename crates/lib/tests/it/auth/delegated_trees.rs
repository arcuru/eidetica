//! Integration tests for delegated tree authentication
//!
//! These tests verify the end-to-end functionality of delegated tree
//! authentication, including tree creation, key delegation, permission
//! clamping, and various authorization scenarios.

#![allow(deprecated)] // Uses LegacyInstanceOps

use eidetica::{
    Result,
    auth::{
        crypto::format_public_key,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyStatus, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    crdt::{Doc, doc::Value},
    entry::ID,
    store::DocStore,
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
    let (delegated_tree, _delegated_key_ids) = create_delegated_tree_with_user(
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
    let settings_store = op.get_store::<DocStore>("_settings").await?;

    let delegation_ref = create_delegation_ref(
        &delegated_tree,
        Permission::Write(10),
        Some(Permission::Read),
    )
    .await?;
    let mut new_auth_settings = main_tree.get_settings().await?.get_all().await?;
    new_auth_settings.set_json("delegate_to_user", delegation_ref)?;
    settings_store
        .set_value("auth", Value::Doc(new_auth_settings))
        .await?;
    op.commit().await?;

    // Test delegated tree validation
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    let delegated_auth_id = create_delegation_path(&[
        ("delegate_to_user", Some(delegated_tips)),
        ("delegated_user", None),
    ]);

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
    let (delegated_tree, _delegated_key_ids) = create_delegated_tree_with_user(
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
    let settings_store = op.get_store::<DocStore>("_settings").await?;

    let delegation_ref = create_delegation_ref(&delegated_tree, Permission::Read, None).await?;
    let mut new_auth_settings = main_tree.get_settings().await?.get_all().await?;
    new_auth_settings.set_json("delegate_readonly", delegation_ref)?;
    settings_store
        .set_value("auth", Value::Doc(new_auth_settings))
        .await?;
    op.commit().await?;

    // Test permission clamping
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    let delegated_auth_id = create_delegation_path(&[
        ("delegate_readonly", Some(delegated_tips)),
        ("delegated_user", None),
    ]);

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
    use eidetica::auth::validation::AuthValidator;

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
    let op = org_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref = create_delegation_ref(&user_tree, Permission::Write(20), None).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree("delegate_to_user", delegation_ref)?;
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
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&org_tree, Permission::Write(15), Some(Permission::Read)).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree("delegate_to_org", delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Test nested delegation: main -> org -> user
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    // Create a nested delegation chain: main -> org -> user
    // Use display names from each tree's auth settings
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
    let resolved_auth = validator
        .resolve_sig_key(&nested_auth_id, &main_auth_settings, Some(&db))
        .await?;

    // Permissions should be clamped: user has Admin(10) -> org clamps to Write(20) -> main doesn't clamp further
    // Final result should be Write(20) (clamped at org level)
    assert_eq!(resolved_auth.effective_permission, Permission::Write(20));
    assert_eq!(resolved_auth.key_status, KeyStatus::Active);

    Ok(())
}

/// Test delegated tree with revoked keys
#[tokio::test]
async fn test_delegated_tree_with_revoked_keys() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

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
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(10), None).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree("delegate_to_tree", delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Test with active key - should work
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    let delegated_auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_tree".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "delegated_user".to_string(), // Use display name from delegated tree auth settings
            tips: None,
        },
    ]);

    let resolved_auth = validator
        .resolve_sig_key(&delegated_auth_id, &main_auth_settings, Some(&db))
        .await?;

    assert_eq!(resolved_auth.effective_permission, Permission::Write(10));
    assert_eq!(resolved_auth.key_status, KeyStatus::Active);

    // Now revoke the key in the delegated tree using SettingsStore API
    let op = delegated_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        settings
            .update_auth_settings(|auth| {
                // Update the existing key to be revoked (use overwrite_key since it already exists)
                let public_key = eidetica::auth::crypto::parse_public_key(&delegated_user_key)?;
                let revoked_key = AuthKey::new(
                    format_public_key(&public_key),
                    Permission::Write(10),
                    KeyStatus::Revoked,
                )?;
                auth.overwrite_key("delegated_user", revoked_key)?;
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
            &SigKey::Direct("delegated_user".to_string()),
            &revoked_auth_settings,
            Some(&db),
        )
        .await?;

    assert_eq!(resolved_auth_revoked.key_status, KeyStatus::Revoked);

    Ok(())
}

/// Test delegation depth limits
#[tokio::test]
async fn test_delegation_depth_limits() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

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
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(10), None).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree("delegate_to_user", delegation_ref)?;
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
            key: "delegate_to_user".to_string(),
            tips: Some(delegated_tips.clone()),
        });
    }

    // Add final step
    delegation_steps.push(DelegationStep {
        key: "user".to_string(), // Use display name from delegated tree auth settings
        tips: None,
    });

    let nested_auth_id = SigKey::DelegationPath(delegation_steps);

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
    use eidetica::auth::validation::AuthValidator;

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
                auth.add_delegated_tree("delegate_user_min_upgrade", delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Validate
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    let auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_user_min_upgrade".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "delegated_user".to_string(), // Use display name from delegated tree auth settings
            tips: None,
        },
    ]);

    let resolved = validator
        .resolve_sig_key(&auth_id, &main_auth_settings, Some(&db))
        .await?;

    // Expect permission upgraded to Write(7)
    assert_eq!(resolved.effective_permission, Permission::Write(7));
    assert_eq!(resolved.key_status, KeyStatus::Active);

    Ok(())
}

/// Test that priority (the numeric part) is preserved when permission is already within bounds
#[tokio::test]
async fn test_delegated_tree_priority_preservation() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

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
    let op = main_tree.new_transaction().await?;
    {
        let settings = op.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(8), None).await?;
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree("delegate_user_priority", delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    // Validate
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    let auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_user_priority".to_string(),
            tips: Some(delegated_tips.clone()),
        },
        DelegationStep {
            key: "delegated_user".to_string(), // Use display name from delegated tree auth settings
            tips: None,
        },
    ]);

    let resolved = validator
        .resolve_sig_key(&auth_id, &main_auth_settings, Some(&db))
        .await?;

    // Because Write(12) is within bounds (less privileged than Write(8)), it is preserved
    assert_eq!(resolved.effective_permission, Permission::Write(12));

    Ok(())
}

/// Test delegation depth limit at exactly MAX_DELEGATION_DEPTH (10)
#[tokio::test]
async fn test_delegation_depth_limit_exact() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

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
    let auth_settings = tree.get_settings().await?.get_auth_settings().await?;

    let result = validator
        .resolve_sig_key(&auth_id, &auth_settings, Some(&db))
        .await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Maximum delegation depth") || msg.contains("not found"));

    Ok(())
}

/// Test that invalid (unknown) tips cause delegation validation to fail
#[tokio::test]
async fn test_delegated_tree_invalid_tips() -> Result<()> {
    use eidetica::auth::validation::AuthValidator;

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
                root: delegated_tree.root_id().clone(),
                tips: vec![bogus_tip.clone()],
            },
        };
        settings
            .update_auth_settings(|auth| {
                auth.add_delegated_tree("delegate_with_bad_tip", delegation_ref)?;
                Ok(())
            })
            .await?;
    }
    op.commit().await?;

    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.get_auth_settings().await?;

    let auth_id = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_with_bad_tip".to_string(),
            tips: Some(vec![bogus_tip]),
        },
        DelegationStep {
            key: "delegated_user".to_string(), // Use display name from delegated tree auth settings
            tips: None,
        },
    ]);

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
