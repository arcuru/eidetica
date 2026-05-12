//! Integration tests for delegated tree authentication
//!
//! These tests verify the end-to-end functionality of delegated tree
//! authentication, including tree creation, key delegation, permission
//! clamping, and various authorization scenarios.

use eidetica::{
    Database, Instance, Result,
    auth::{
        crypto::{PrivateKey, PublicKey},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, KeyStatus, Permission,
            PermissionBounds, SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    crdt::Doc,
    database::DatabaseKey,
    entry::ID,
    store::DocStore,
    user::User,
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
    let txn = main_tree.new_transaction().await?;
    let settings = txn.get_settings()?;

    let delegation_ref = create_delegation_ref(
        &delegated_tree,
        Permission::Write(10),
        Some(Permission::Read),
    )
    .await?;
    settings.add_delegated_tree(delegation_ref).await?;
    txn.commit().await?;

    // Test delegated tree validation
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    // Create delegation path - DelegationStep uses root tree ID and tips
    let delegated_auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
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
    let txn = main_tree.new_transaction().await?;
    let settings = txn.get_settings()?;

    let delegation_ref = create_delegation_ref(&delegated_tree, Permission::Read, None).await?;
    settings.add_delegated_tree(delegation_ref).await?;
    txn.commit().await?;

    // Test permission clamping
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;
    let delegated_tips = delegated_tree.get_tips().await?;

    let delegated_auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
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
    let txn = org_tree.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
        let delegation_ref = create_delegation_ref(&user_tree, Permission::Write(20), None).await?;
        settings.add_delegated_tree(delegation_ref).await?;
    }
    txn.commit().await?;

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
    let txn = main_tree.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&org_tree, Permission::Write(15), Some(Permission::Read)).await?;
        settings.add_delegated_tree(delegation_ref).await?;
    }
    txn.commit().await?;

    // Test nested delegation: main -> org -> user
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;

    // Create a nested delegation chain: main -> org -> user
    let nested_auth_id = SigKey::Delegation {
        path: vec![
            DelegationStep {
                tree: org_tree_root.clone(),
                tips: org_tips.clone(),
            },
            DelegationStep {
                tree: user_tree_root.clone(),
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
    let txn = main_tree.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(10), None).await?;
        settings.add_delegated_tree(delegation_ref).await?;
    }
    txn.commit().await?;

    // Test with active key - should work
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;

    let delegated_auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.clone(),
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
    let txn = delegated_tree.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
        let revoked_key = AuthKey::new(
            Some("delegated_user"),
            Permission::Write(10),
            KeyStatus::Revoked,
        );
        settings
            .set_auth_key(&delegated_user_key, revoked_key)
            .await?;
    }
    txn.commit().await?;

    // Test validation against revoked key
    let revoked_auth_settings = delegated_tree.get_settings().await?.auth_snapshot().await?;
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
    let txn = main_tree.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(10), None).await?;
        settings.add_delegated_tree(delegation_ref).await?;
    }
    txn.commit().await?;

    // Create a deeply nested delegation that should exceed the limit
    // We'll create a chain with 12 levels (exceeds MAX_DELEGATION_DEPTH of 10)
    let mut delegation_steps = Vec::new();

    // Add 12 intermediate delegation steps (exceeds MAX_DELEGATION_DEPTH of 10)
    for _ in 0..12 {
        delegation_steps.push(DelegationStep {
            tree: delegated_tree_root.clone(),
            tips: delegated_tips.clone(),
        });
    }

    let nested_auth_id = SigKey::Delegation {
        path: delegation_steps,
        hint: KeyHint::from_pubkey(&user_key),
    };

    // Test depth limit validation
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;
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
    let txn = main_tree.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
        let delegation_ref = create_delegation_ref(
            &delegated_tree,
            Permission::Write(0),       // max: Highest possible Write permission
            Some(Permission::Write(7)), // min: Minimum permission level
        )
        .await?;
        settings.add_delegated_tree(delegation_ref).await?;
    }
    txn.commit().await?;

    // Validate
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;

    let auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.clone(),
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
    let txn = main_tree.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
        let delegation_ref =
            create_delegation_ref(&delegated_tree, Permission::Write(8), None).await?;
        settings.add_delegated_tree(delegation_ref).await?;
    }
    txn.commit().await?;

    // Validate
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;

    let auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.clone(),
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
        ID::from_bytes("sha256:0000000000000000000000000000000000000000000000000000000000000000");
    for _ in 0..10 {
        delegation_steps.push(DelegationStep {
            tree: bogus_root_id.clone(),
            tips: tips.clone(),
        });
    }

    let auth_id = SigKey::Delegation {
        path: delegation_steps,
        hint: KeyHint::from_pubkey(&admin_key),
    };

    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings().await?.auth_snapshot().await?;

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
    let bogus_tip = ID::from_bytes("nonexistent_tip_hash");

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
    let txn = main_tree.new_transaction().await?;
    {
        let settings = txn.get_settings()?;
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
        settings.add_delegated_tree(delegation_ref).await?;
    }
    txn.commit().await?;

    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree.get_settings().await?.auth_snapshot().await?;

    let auth_id = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree_root.clone(),
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

// ===== END-TO-END DELEGATION WRITE TESTS =====

/// Delegation pair: an identity database the user owns and a target database
/// accessible only through delegation.
struct DelegationPair {
    identity_db: Database,
    target_db: Database,
    user_key_id: PublicKey,
    admin_signing_key: PrivateKey,
}

/// Create a (identity_db, target_db) pair wired for delegation.
///
/// - `identity_db` is created via the User API so the user holds its admin key.
/// - `target_db` is created with a standalone admin key (NOT in the user's keyring),
///   so the user's only access path is through delegation.
/// - A `DelegatedTreeRef` with `max_permission` is added to target_db's settings.
async fn setup_delegation_pair(
    instance: &Instance,
    user: &mut User,
    max_permission: Permission,
) -> Result<DelegationPair> {
    let user_key_id = user.add_private_key(Some("user")).await?;
    let identity_db = user.create_database(Doc::new(), &user_key_id).await?;

    let admin_signing_key = PrivateKey::generate();
    let target_db = Database::create(instance, admin_signing_key.clone(), Doc::new()).await?;

    let delegation_ref = create_delegation_ref(&identity_db, max_permission, None).await?;
    let admin_db_key = DatabaseKey::new(admin_signing_key.clone());
    let target_db_authed = Database::open(instance, target_db.root_id())
        .await?
        .with_key(admin_db_key);
    let txn = target_db_authed.new_transaction().await?;
    txn.get_settings()?
        .add_delegated_tree(delegation_ref)
        .await?;
    txn.commit().await?;

    Ok(DelegationPair {
        identity_db,
        target_db,
        user_key_id,
        admin_signing_key,
    })
}

/// Open target_db using a delegation identity derived from identity_db.
async fn open_via_delegation(
    instance: &Instance,
    pair: &DelegationPair,
    signing_key: PrivateKey,
    hint_key_id: &PublicKey,
) -> Result<Database> {
    let identity_db_tips = pair.identity_db.get_tips().await?;
    let delegation_sigkey = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: pair.identity_db.root_id().clone(),
            tips: identity_db_tips,
        }],
        hint: KeyHint::from_pubkey(hint_key_id),
    };
    let db_key = DatabaseKey::with_identity(signing_key, delegation_sigkey);
    Ok(Database::open(instance, pair.target_db.root_id())
        .await?
        .with_key(db_key))
}

/// Test writing entries via delegation identity using the Database API directly
#[tokio::test]
async fn test_delegated_write_database_api() -> Result<()> {
    let (instance, mut user) = crate::helpers::test_instance_with_user("test_user").await;
    let pair = setup_delegation_pair(&instance, &mut user, Permission::Write(10)).await?;

    let user_signing_key = user.get_signing_key(&pair.user_key_id)?;
    let target_via_delegation =
        open_via_delegation(&instance, &pair, user_signing_key, &pair.user_key_id).await?;

    // Verify the database is using a delegation identity
    assert!(matches!(
        target_via_delegation.auth_identity(),
        Some(SigKey::Delegation { .. })
    ));

    // Write data using delegation identity
    let txn = target_via_delegation.new_transaction().await?;
    let store = txn.get_store::<DocStore>("test_store").await?;
    store.set("greeting", "hello from delegated key").await?;
    txn.commit().await?;

    // Verify the entry is readable
    let txn = target_via_delegation.new_transaction().await?;
    let store = txn.get_store::<DocStore>("test_store").await?;
    let value = store.get("greeting").await?;
    assert_eq!(value.as_text(), Some("hello from delegated key"));

    Ok(())
}

/// Test full User API flow: track_database discovers delegation, open_database uses it
#[tokio::test]
async fn test_delegated_write_user_api_flow() -> Result<()> {
    let (instance, mut user) = crate::helpers::test_instance_with_user("test_user").await;
    let pair = setup_delegation_pair(&instance, &mut user, Permission::Write(10)).await?;

    // Track target_db with user_key -- should discover delegation path
    user.track_database(
        pair.target_db.root_id().clone(),
        &pair.user_key_id,
        Default::default(),
    )
    .await?;

    // Open target_db via User API -- should use delegation SigKey
    let opened_db = user.open_database(pair.target_db.root_id()).await?;

    // Verify the database was opened with a delegation identity pointing to identity_db
    let identity = opened_db
        .auth_identity()
        .expect("should have auth identity");
    match identity {
        SigKey::Delegation { path, hint } => {
            assert_eq!(path.len(), 1);
            assert_eq!(path[0].tree, *pair.identity_db.root_id());
            assert_eq!(hint, &KeyHint::from_pubkey(&pair.user_key_id));
        }
        other => panic!("Expected delegation SigKey, got {other:?}"),
    }

    // Write data and commit
    let txn = opened_db.new_transaction().await?;
    let store = txn.get_store::<DocStore>("data").await?;
    store.set("key", "value_via_user_api").await?;
    txn.commit().await?;

    // Verify the data is readable
    let txn = opened_db.new_transaction().await?;
    let store = txn.get_store::<DocStore>("data").await?;
    let value = store.get("key").await?;
    assert_eq!(value.as_text(), Some("value_via_user_api"));

    Ok(())
}

/// Test permission clamping on delegated writes.
///
/// The user holds Admin(0) in identity_db (via create_database bootstrap), but the
/// delegation grants at most Write(10). Data writes should succeed; settings writes
/// (which require Admin) should be rejected.
#[tokio::test]
async fn test_delegated_write_permission_clamping() -> Result<()> {
    let (instance, mut user) = crate::helpers::test_instance_with_user("test_user").await;
    let pair = setup_delegation_pair(&instance, &mut user, Permission::Write(10)).await?;

    let user_signing_key = user.get_signing_key(&pair.user_key_id)?;
    let target_via_delegation =
        open_via_delegation(&instance, &pair, user_signing_key, &pair.user_key_id).await?;

    // Writing data should succeed (Write permission allows data writes)
    let txn = target_via_delegation.new_transaction().await?;
    let store = txn.get_store::<DocStore>("data").await?;
    store.set("test", "data_write").await?;
    let result = txn.commit().await;
    assert!(
        result.is_ok(),
        "Data write should succeed with Write permission"
    );

    // Writing settings should fail (Write permission does not allow settings writes)
    let txn = target_via_delegation.new_transaction().await?;
    let settings = txn.get_settings()?;
    settings.set_name("should_fail").await?;
    let result = txn.commit().await;
    assert!(
        result.is_err(),
        "Settings write should fail with Write permission"
    );

    Ok(())
}

/// Test delegated entry validation across instances (simulating sync).
///
/// Entries are copied between instances with `put_verified` (skip-validation insert)
/// to isolate what this test cares about: whether `AuthValidator` can resolve the
/// delegation chain on an instance that never held the signing key. Real sync would
/// use the full `put` path, which is covered by sync-level tests.
#[tokio::test]
async fn test_delegated_entry_validation_across_instances() -> Result<()> {
    let (instance_a, mut user) = crate::helpers::test_instance_with_user("test_user").await;
    let pair = setup_delegation_pair(&instance_a, &mut user, Permission::Write(10)).await?;

    // Write an entry using delegation on instance A
    let user_signing_key = user.get_signing_key(&pair.user_key_id)?;
    let target_via_delegation =
        open_via_delegation(&instance_a, &pair, user_signing_key, &pair.user_key_id).await?;

    let txn = target_via_delegation.new_transaction().await?;
    let store = txn.get_store::<DocStore>("data").await?;
    store.set("synced_key", "synced_value").await?;
    txn.commit().await?;

    // Create instance B and replicate all entries from both trees
    let instance_b = crate::helpers::test_instance().await;

    // Copy identity_db entries (needed for delegation resolution on instance B)
    let identity_entries = instance_a
        .backend()
        .get_tree(pair.identity_db.root_id())
        .await?;
    for entry in identity_entries {
        instance_b.backend().put_verified(entry).await?;
    }

    // Copy target_db entries
    let target_entries = instance_a
        .backend()
        .get_tree(pair.target_db.root_id())
        .await?;
    for entry in target_entries {
        instance_b.backend().put_verified(entry).await?;
    }

    // Open target_db on instance B with the admin key to read settings
    let admin_db_key = DatabaseKey::new(pair.admin_signing_key);
    let target_db_b = Database::open(&instance_b, pair.target_db.root_id())
        .await?
        .with_key(admin_db_key);

    let settings_b = target_db_b.get_settings().await?;
    let auth_settings_b = settings_b.auth_snapshot().await?;

    let mut validator = AuthValidator::new();

    // Get tip entries from target_db on instance B and validate them
    let target_tips = instance_b
        .backend()
        .get_tips(pair.target_db.root_id())
        .await?;
    assert!(
        !target_tips.is_empty(),
        "Target DB should have tips on instance B"
    );

    // Validate all tip entries (including the delegation-signed one)
    for tip_id in &target_tips {
        let entry = instance_b.backend().get(tip_id).await?;
        let is_valid = validator
            .validate_entry(&entry, &auth_settings_b, Some(&instance_b))
            .await?;
        assert!(is_valid, "Entry {tip_id} should validate on instance B");
    }

    Ok(())
}

/// Test delegation write using a non-primary key added to the identity database
#[tokio::test]
async fn test_delegated_write_secondary_identity_key() -> Result<()> {
    let (instance, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Create identity_db with a primary key
    let primary_key_id = user.add_private_key(Some("primary")).await?;
    let identity_db = user.create_database(Doc::new(), &primary_key_id).await?;

    // Add a second key to the identity_db's auth settings
    let secondary_key_id = user.add_private_key(Some("secondary")).await?;
    let txn = identity_db.new_transaction().await?;
    txn.get_settings()?
        .set_auth_key(
            &secondary_key_id,
            AuthKey::new(Some("secondary"), Permission::Write(5), KeyStatus::Active),
        )
        .await?;
    txn.commit().await?;

    // Create target_db with a standalone admin key and delegate to identity_db
    let admin_signing_key = PrivateKey::generate();
    let target_db = Database::create(&instance, admin_signing_key.clone(), Doc::new()).await?;

    let delegation_ref = create_delegation_ref(&identity_db, Permission::Write(10), None).await?;
    let admin_db_key = DatabaseKey::new(admin_signing_key);
    let target_db_authed = Database::open(&instance, target_db.root_id())
        .await?
        .with_key(admin_db_key);
    let txn = target_db_authed.new_transaction().await?;
    txn.get_settings()?
        .add_delegated_tree(delegation_ref)
        .await?;
    txn.commit().await?;

    // Open target_db using the secondary key via delegation
    let identity_db_tips = identity_db.get_tips().await?;
    let delegation_sigkey = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: identity_db.root_id().clone(),
            tips: identity_db_tips,
        }],
        hint: KeyHint::from_pubkey(&secondary_key_id),
    };

    let secondary_signing_key = user.get_signing_key(&secondary_key_id)?;
    let delegation_db_key =
        DatabaseKey::with_identity(secondary_signing_key, delegation_sigkey.clone());
    let target_via_delegation = Database::open(&instance, target_db.root_id())
        .await?
        .with_key(delegation_db_key);

    // Verify the database is using the secondary key's delegation identity
    assert_eq!(
        target_via_delegation.auth_identity(),
        Some(&delegation_sigkey)
    );

    // Write data using the secondary key's delegation identity
    let txn = target_via_delegation.new_transaction().await?;
    let store = txn.get_store::<DocStore>("data").await?;
    store.set("author", "secondary key").await?;
    txn.commit().await?;

    // Verify the entry is readable
    let txn = target_via_delegation.new_transaction().await?;
    let store = txn.get_store::<DocStore>("data").await?;
    let value = store.get("author").await?;
    assert_eq!(value.as_text(), Some("secondary key"));

    Ok(())
}
