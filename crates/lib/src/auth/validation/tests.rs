//! Tests for authentication validation
//!
//! These tests directly generate keypairs to test auth primitives (signatures, permissions,
//! delegation) without going through the User API. This is intentional - we're testing the
//! underlying auth mechanics, not the User API layer.

use super::entry::AuthValidator;
use crate::{
    Database, Entry, Error,
    auth::{
        crypto::{format_public_key, generate_keypair, sign_entry},
        settings::AuthSettings,
        types::{
            AuthKey, DelegationStep, KeyHint, KeyStatus, Operation, Permission, ResolvedAuth,
            SigInfo, SigKey,
        },
    },
    crdt::Doc,
    entry::ID,
};

fn create_test_auth_with_key(pubkey: &str, auth_key: &AuthKey) -> AuthSettings {
    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(pubkey, auth_key.clone()).unwrap();
    auth_settings
}

#[tokio::test]
async fn test_basic_key_resolution() {
    let mut validator = AuthValidator::new();
    let (_, verifying_key) = generate_keypair();
    let pubkey = format_public_key(&verifying_key);

    let auth_key = AuthKey::active(Some("KEY_LAPTOP"), Permission::Write(10));

    let settings = create_test_auth_with_key(&pubkey, &auth_key);

    // Use pubkey hint for lookup since keys are stored by pubkey
    let sig_key = SigKey::from_pubkey(&pubkey);
    let resolved = validator
        .resolve_sig_key(&sig_key, &settings, None)
        .await
        .unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].effective_permission, Permission::Write(10));
    assert_eq!(resolved[0].key_status, KeyStatus::Active);
}

#[tokio::test]
async fn test_revoked_key_validation() {
    let mut validator = AuthValidator::new();
    let (_signing_key, verifying_key) = generate_keypair();
    let pubkey = format_public_key(&verifying_key);

    let auth_key = AuthKey::active(Some("KEY_LAPTOP"), Permission::Write(10));

    let settings = create_test_auth_with_key(&pubkey, &auth_key);
    let sig_key = SigKey::from_pubkey(&pubkey);
    let resolved = validator.resolve_sig_key(&sig_key, &settings, None).await;
    assert!(resolved.is_ok());
}

#[tokio::test]
async fn test_permission_levels() {
    let validator = AuthValidator::new();

    let admin_auth = ResolvedAuth {
        public_key: generate_keypair().1,
        effective_permission: Permission::Admin(5),
        key_status: KeyStatus::Active,
    };

    let write_auth = ResolvedAuth {
        public_key: generate_keypair().1,
        effective_permission: Permission::Write(10),
        key_status: KeyStatus::Active,
    };

    let read_auth = ResolvedAuth {
        public_key: generate_keypair().1,
        effective_permission: Permission::Read,
        key_status: KeyStatus::Active,
    };

    // Test admin permissions
    assert!(
        validator
            .check_permissions(&admin_auth, &Operation::WriteData)
            .unwrap()
    );
    assert!(
        validator
            .check_permissions(&admin_auth, &Operation::WriteSettings)
            .unwrap()
    );

    // Test write permissions
    assert!(
        validator
            .check_permissions(&write_auth, &Operation::WriteData)
            .unwrap()
    );
    assert!(
        !validator
            .check_permissions(&write_auth, &Operation::WriteSettings)
            .unwrap()
    );

    // Test read permissions
    assert!(
        !validator
            .check_permissions(&read_auth, &Operation::WriteData)
            .unwrap()
    );
    assert!(
        !validator
            .check_permissions(&read_auth, &Operation::WriteSettings)
            .unwrap()
    );
}

#[tokio::test]
async fn test_entry_validation_success() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();
    let pubkey = format_public_key(&verifying_key);

    let auth_key = AuthKey::active(Some("KEY_LAPTOP"), Permission::Write(20));

    let settings = create_test_auth_with_key(&pubkey, &auth_key);

    // Create a test entry using Entry::builder
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    // Set auth info without signature - use pubkey hint
    entry.sig = SigInfo::builder().key(SigKey::from_pubkey(&pubkey)).build();

    // Sign the entry
    let signature = sign_entry(&entry, &signing_key).unwrap();

    // Set the signature on the entry
    entry.sig.sig = Some(signature);

    // Validate the entry
    let result = validator.validate_entry(&entry, &settings, None).await;
    assert!(result.unwrap()); // Signature verified
}

#[tokio::test]
async fn test_missing_key() {
    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new(); // Empty auth settings

    let sig_key = SigKey::from_name("NONEXISTENT_KEY");
    let result = validator
        .resolve_sig_key(&sig_key, &auth_settings, None)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Auth(_) => {} // Expected
        _ => panic!("Expected Auth error"),
    }
}

#[tokio::test]
async fn test_delegated_tree_requires_backend() {
    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();

    let sig_key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "user1".to_string(),
            tips: vec![ID::new("tip1")],
        }],
        hint: KeyHint::from_name("KEY_LAPTOP"),
    };

    let result = validator
        .resolve_sig_key(&sig_key, &auth_settings, None)
        .await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Database required for delegated tree resolution")
    );
}

#[tokio::test]
async fn test_delegation_path_rejects_wildcard_in_path() {
    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();

    let sig_key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "*".to_string(), // Wildcard not allowed in path
            tips: vec![ID::new("tip1")],
        }],
        hint: KeyHint::from_name("final_key"),
    };

    let result = validator
        .resolve_sig_key(&sig_key, &auth_settings, None)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_validate_entry_with_auth_info_against_empty_settings() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();
    let pubkey = format_public_key(&verifying_key);

    // Create an entry with auth info (signed)
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    entry.sig = SigInfo::builder().key(SigKey::from_pubkey(&pubkey)).build();

    // Sign the entry
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validate against empty settings (no auth configuration)
    let empty_auth_settings = AuthSettings::new();
    let result = validator
        .validate_entry(&entry, &empty_auth_settings, None)
        .await;

    // Signed entry with no auth config should fail validation
    // (can't verify a signature without keys)
    assert!(result.is_ok());
    assert!(
        !result.unwrap(),
        "Expected validation to fail for signed entry with no auth config"
    );
}

#[tokio::test]
async fn test_entry_validation_with_revoked_key() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();
    let pubkey = format_public_key(&verifying_key);

    let revoked_key = AuthKey::new(
        Some("KEY_LAPTOP"),
        Permission::Write(10),
        KeyStatus::Revoked, // Key is revoked
    );

    let settings = create_test_auth_with_key(&pubkey, &revoked_key);

    // Create a test entry using Entry::builder
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    // Set auth info without signature
    entry.sig = SigInfo::builder().key(SigKey::from_pubkey(&pubkey)).build();

    // Sign the entry
    let signature = sign_entry(&entry, &signing_key).unwrap();

    // Set the signature on the entry
    entry.sig.sig = Some(signature);

    // Validation should fail with revoked key - returns Ok(false) since no active key could verify
    let result = validator.validate_entry(&entry, &settings, None).await;
    assert!(result.is_ok());
    assert!(
        !result.unwrap(),
        "Expected validation to fail for revoked key"
    );
}

#[tokio::test]
async fn test_performance_optimizations() {
    let mut validator = AuthValidator::new();
    let (_, verifying_key) = generate_keypair();
    let pubkey = format_public_key(&verifying_key);

    let auth_key = AuthKey::active(Some("PERF_KEY"), Permission::Write(10));

    let settings = create_test_auth_with_key(&pubkey, &auth_key);
    let sig_key = SigKey::from_pubkey(&pubkey);

    // Test that resolution works correctly
    let result1 = validator.resolve_sig_key(&sig_key, &settings, None).await;
    assert!(result1.is_ok());

    // Multiple resolutions should work consistently
    let result2 = validator.resolve_sig_key(&sig_key, &settings, None).await;
    assert!(result2.is_ok());

    // Results should be identical
    let resolved1 = result1.unwrap();
    let resolved2 = result2.unwrap();
    assert_eq!(resolved1.len(), 1);
    assert_eq!(resolved2.len(), 1);
    assert_eq!(
        resolved1[0].effective_permission,
        resolved2[0].effective_permission
    );
    assert_eq!(resolved1[0].key_status, resolved2[0].key_status);

    // Test cache clear functionality
    validator.clear_cache();
}

#[tokio::test]
async fn test_basic_delegated_tree_resolution() {
    let mut validator = AuthValidator::new();

    // Create a simple direct key resolution test
    let (_, verifying_key) = generate_keypair();
    let pubkey = format_public_key(&verifying_key);
    let auth_key = AuthKey::active(Some("DIRECT_KEY"), Permission::Admin(5));

    let settings = create_test_auth_with_key(&pubkey, &auth_key);

    let sig_key = SigKey::from_pubkey(&pubkey);
    let result = validator.resolve_sig_key(&sig_key, &settings, None).await;

    match result {
        Ok(resolved) => {
            assert_eq!(resolved.len(), 1);
            assert_eq!(resolved[0].effective_permission, Permission::Admin(5));
            assert_eq!(resolved[0].key_status, KeyStatus::Active);
        }
        Err(e) => {
            panic!("Failed to resolve auth key: {e}");
        }
    }
}

#[tokio::test]
async fn test_complete_delegation_workflow() {
    use crate::{
        Instance,
        auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference},
        backend::database::InMemory,
    };

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let instance = Instance::open(backend)
        .await
        .expect("Failed to create test instance");

    // Use the device key for testing
    let main_key = instance.device_id();
    let main_pubkey = format_public_key(&main_key);
    // For delegated key, we'll use the same device key for simplicity in tests
    let delegated_key = main_key;
    let delegated_pubkey = format_public_key(&delegated_key);

    // Create the delegated tree with its own auth configuration
    let mut delegated_settings = Doc::new();
    let mut delegated_auth = AuthSettings::new();
    delegated_auth
        .add_key(
            &delegated_pubkey,
            AuthKey::active(Some("delegated_user"), Permission::Admin(5)),
        )
        .unwrap();
    // Add admin to auth config so we can create the database with it
    // Note: main_pubkey == delegated_pubkey in this test, so we overwrite
    delegated_auth
        .overwrite_key(
            &main_pubkey,
            AuthKey::active(Some("admin"), Permission::Admin(0)),
        )
        .unwrap();
    delegated_settings.set("auth", delegated_auth.as_doc().clone());

    let delegated_tree = Database::create(
        delegated_settings,
        &instance,
        instance.device_key().clone(),
        main_pubkey.clone(),
    )
    .await
    .unwrap();

    // Create the main tree with delegation configuration
    let mut main_settings = Doc::new();
    let mut main_auth = AuthSettings::new();

    // Add direct key to main tree
    main_auth
        .add_key(
            &main_pubkey,
            AuthKey::active(Some("main_admin"), Permission::Admin(0)),
        )
        .unwrap();

    // Get the actual tips from the delegated tree
    let delegated_tips = delegated_tree.get_tips().await.unwrap();

    // Add delegation reference
    main_auth
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: delegated_tips.clone(),
            },
        })
        .unwrap();

    main_settings.set("auth", main_auth.as_doc().clone());
    let main_tree = Database::create(
        main_settings,
        &instance,
        instance.device_key().clone(),
        main_pubkey.clone(),
    )
    .await
    .unwrap();

    // Test delegation resolution
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree
        .get_settings()
        .await
        .unwrap()
        .get_auth_settings()
        .await
        .unwrap();

    let delegated_sig_key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().to_string(),
            tips: delegated_tips,
        }],
        hint: KeyHint::from_pubkey(&delegated_pubkey),
    };

    let result = validator
        .resolve_sig_key(&delegated_sig_key, &main_auth_settings, Some(&instance))
        .await;

    // Should succeed with permission clamping (Admin -> Write due to bounds)
    assert!(
        result.is_ok(),
        "Delegation resolution failed: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].effective_permission, Permission::Write(10)); // Clamped from Admin to Write
    assert_eq!(resolved[0].key_status, KeyStatus::Active);
}

#[tokio::test]
async fn test_delegated_tree_requires_tips() {
    use crate::{
        Instance,
        auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference},
        backend::database::InMemory,
    };

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let instance = Instance::open(backend)
        .await
        .expect("Failed to create test instance");

    // Use the device key for testing
    let main_key = instance.device_id();
    let main_pubkey = format_public_key(&main_key);

    // Create a simple delegated tree
    let delegated_settings = Doc::new();
    let delegated_tree = Database::create(
        delegated_settings,
        &instance,
        instance.device_key().clone(),
        main_pubkey.clone(),
    )
    .await
    .unwrap();

    // Create the main tree with delegation configuration
    let mut main_settings = Doc::new();
    let mut main_auth = AuthSettings::new();

    // Add direct key to main tree
    main_auth
        .add_key(
            &main_pubkey,
            AuthKey::active(Some("main_admin"), Permission::Admin(0)),
        )
        .unwrap();

    // Add delegation reference
    main_auth
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: vec![ID::new("some_tip")], // This will be ignored due to empty tips in auth_id
            },
        })
        .unwrap();

    main_settings.set("auth", main_auth.as_doc().clone());

    // Create validator and test with empty tips
    let mut validator = AuthValidator::new();

    // Create a Delegation sig_key with empty tips
    let sig_key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().to_string(),
            tips: vec![], // Empty tips should cause validation to fail
        }],
        hint: KeyHint::from_name("delegated_user"),
    };

    let result = validator
        .resolve_sig_key(&sig_key, &main_auth, Some(&instance))
        .await;

    // Should fail because tips are required for delegated tree resolution
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Invalid delegation"),
        "Expected error about invalid delegation, got: {error_msg}"
    );
}

#[tokio::test]
async fn test_nested_delegation_with_permission_clamping() {
    use crate::{
        Instance,
        auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference},
        backend::database::InMemory,
    };

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let instance = Instance::open(backend)
        .await
        .expect("Failed to create test instance");

    // Use the device key for all operations
    let main_key = instance.device_id();
    let main_pubkey = format_public_key(&main_key);

    // 1. Create the final user tree (deepest level)
    let mut user_settings = Doc::new();
    let mut user_auth = AuthSettings::new();
    user_auth
        .add_key(
            &main_pubkey,
            AuthKey::active(
                Some("final_user"),
                Permission::Admin(3), // High privilege at source
            ),
        )
        .unwrap();
    user_settings.set("auth", user_auth.as_doc().clone());
    let user_tree = Database::create(
        user_settings,
        &instance,
        instance.device_key().clone(),
        main_pubkey.clone(),
    )
    .await
    .unwrap();
    let user_tips = user_tree.get_tips().await.unwrap();

    // 2. Create intermediate delegated tree that delegates to user tree
    let mut intermediate_settings = Doc::new();
    let mut intermediate_auth = AuthSettings::new();

    // Add direct key to intermediate tree
    intermediate_auth
        .add_key(
            &main_pubkey,
            AuthKey::active(Some("intermediate_admin"), Permission::Admin(2)),
        )
        .unwrap();

    // Add delegation to user tree with bounds Write(8) max, Read min
    intermediate_auth
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(8), // Clamp Admin(3) to Write(8)
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: user_tree.root_id().clone(),
                tips: user_tips.clone(),
            },
        })
        .unwrap();

    intermediate_settings.set("auth", intermediate_auth.as_doc().clone());
    let intermediate_tree = Database::create(
        intermediate_settings,
        &instance,
        instance.device_key().clone(),
        main_pubkey.clone(),
    )
    .await
    .unwrap();
    let intermediate_tips = intermediate_tree.get_tips().await.unwrap();

    // 3. Create main tree that delegates to intermediate tree
    let mut main_settings = Doc::new();
    let mut main_auth = AuthSettings::new();

    // Add direct key to main tree
    main_auth
        .add_key(
            &main_pubkey,
            AuthKey::active(Some("main_admin"), Permission::Admin(0)),
        )
        .unwrap();

    // Add delegation to intermediate tree with bounds Write(5) max, Read min
    // This should be less restrictive than the intermediate tree's Write(8)
    main_auth
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(5), // Less restrictive than Write(8)
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: intermediate_tree.root_id().clone(),
                tips: intermediate_tips.clone(),
            },
        })
        .unwrap();

    main_settings.set("auth", main_auth.as_doc().clone());
    let main_tree = Database::create(
        main_settings,
        &instance,
        instance.device_key().clone(),
        main_pubkey.clone(),
    )
    .await
    .unwrap();

    // 4. Test nested delegation resolution: Main -> Intermediate -> User
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree
        .get_settings()
        .await
        .unwrap()
        .get_auth_settings()
        .await
        .unwrap();

    // Create nested delegation SigKey:
    // Main tree delegates to intermediate_tree (by root ID) ->
    // Intermediate tree delegates to user_tree (by root ID) ->
    // User tree resolves key by pubkey hint
    let nested_sig_key = SigKey::Delegation {
        path: vec![
            DelegationStep {
                tree: intermediate_tree.root_id().to_string(),
                tips: intermediate_tips,
            },
            DelegationStep {
                tree: user_tree.root_id().to_string(),
                tips: user_tips,
            },
        ],
        hint: KeyHint::from_pubkey(&main_pubkey),
    };

    let result = validator
        .resolve_sig_key(&nested_sig_key, &main_auth_settings, Some(&instance))
        .await;

    // Should succeed with multi-level permission clamping:
    // Admin(3) -> Write(8) (at intermediate level) -> Write(5) (at main level, further clamping)
    assert!(
        result.is_ok(),
        "Map delegation resolution failed: {:?}",
        result.err()
    );
    let resolved = result.unwrap();

    // The permission should be clamped at each level:
    // 1. User tree has Admin(3) (high permission)
    // 2. Intermediate tree clamps Admin(3) to Write(8) due to max bound
    // 3. Main tree clamps Write(8) with max bound Write(5) -> no change since Write(8) is more restrictive
    // Final result should be Write(8) - the most restrictive bound in the chain

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].effective_permission, Permission::Write(8)); // Correctly clamped through the chain
    assert_eq!(resolved[0].key_status, KeyStatus::Active);
}

#[tokio::test]
async fn test_delegation_depth_limit() {
    // Test that excessive delegation depth is prevented
    let mut validator = AuthValidator::new();

    // Create empty auth settings (doesn't matter for depth test)
    let auth_settings = AuthSettings::new();

    // Test the depth check by directly calling with depth = MAX_DELEGATION_DEPTH
    let simple_sig_key = SigKey::from_name("base_key");

    // This should succeed (just under the limit)
    let result = validator
        .resolver
        .resolve_sig_key_with_depth(&simple_sig_key, &auth_settings, None, 9)
        .await;
    // Should fail due to missing auth configuration, not depth limit
    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(error.to_string().contains("not found"));

    // This should fail due to depth limit (at the limit)
    let result = validator
        .resolver
        .resolve_sig_key_with_depth(&simple_sig_key, &auth_settings, None, 10)
        .await;
    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(error.to_string().contains("Maximum delegation depth"));
    assert!(error.to_string().contains("exceeded"));
}

// ===== GLOBAL PERMISSION TESTS =====

#[tokio::test]
async fn test_global_permission_with_pubkey_hint() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();
    let actual_pubkey = format_public_key(&verifying_key);

    // Create settings with global "*" permission
    let global_auth_key = AuthKey::active(None::<String>, Permission::Write(10));

    let settings = create_test_auth_with_key("*", &global_auth_key);

    // Create an entry that uses global permission with actual signer pubkey in hint
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    // Use "*:pubkey" format in hint to indicate global permission with actual signer
    let global_pubkey = format!("*:{}", actual_pubkey);
    entry.sig = SigInfo {
        key: SigKey::from_pubkey(&global_pubkey),
        sig: None,
    };

    // Sign the entry with the client's key
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validation should succeed
    let result = validator.validate_entry(&entry, &settings, None).await;
    assert!(
        result.unwrap(),
        "Expected validation to succeed with global permission"
    );
}

#[tokio::test]
async fn test_global_permission_without_pubkey_fails() {
    let mut validator = AuthValidator::new();
    let (signing_key, _) = generate_keypair();

    // Create settings with global "*" permission
    let global_auth_key = AuthKey::active(None::<String>, Permission::Write(10));

    let settings = create_test_auth_with_key("*", &global_auth_key);

    // Create an entry that uses global permission but doesn't include pubkey in hint
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    entry.sig = SigInfo {
        key: SigKey::from_name("*"), // Just "*" without pubkey - should fail
        sig: None,
    };

    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validation should fail due to missing pubkey in global permission
    let result = validator.validate_entry(&entry, &settings, None).await;
    assert!(result.is_ok());
    assert!(
        !result.unwrap(),
        "Expected validation to fail without pubkey in global permission hint"
    );
}

#[tokio::test]
async fn test_global_permission_resolver() {
    let mut validator = AuthValidator::new();
    let (_, verifying_key) = generate_keypair();
    let actual_pubkey = format_public_key(&verifying_key);

    // Create settings with global "*" permission
    let global_auth_key = AuthKey::active(None::<String>, Permission::Write(10));

    let settings = create_test_auth_with_key("*", &global_auth_key);

    // Test resolving "*:pubkey" format - should succeed
    let global_pubkey = format!("*:{}", actual_pubkey);
    let sig_key = SigKey::from_pubkey(&global_pubkey);

    let result = validator.resolve_sig_key(&sig_key, &settings, None).await;

    assert!(
        result.is_ok(),
        "Global permission resolution should work with pubkey hint: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].public_key, verifying_key);
    assert_eq!(resolved[0].effective_permission, Permission::Write(10));
}

#[tokio::test]
async fn test_global_permission_insufficient_perms() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();
    let actual_pubkey = format_public_key(&verifying_key);

    // Create settings with global "*" permission but only Read access
    let global_auth_key = AuthKey::active(
        None::<String>,
        Permission::Read, // Only read permission
    );

    let settings = create_test_auth_with_key("*", &global_auth_key);

    // Test that global permissions still respect the permission level
    let global_pubkey = format!("*:{}", actual_pubkey);
    let sig_key = SigKey::from_pubkey(&global_pubkey);

    let result = validator.resolve_sig_key(&sig_key, &settings, None).await;
    assert!(result.is_ok(), "Resolution should succeed");

    let resolved = result.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].effective_permission, Permission::Read); // Should have read permission

    // Create an entry that tries to write (requires Write permission)
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    entry.sig = SigInfo {
        key: SigKey::from_pubkey(&global_pubkey),
        sig: None,
    };

    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Even with valid signature and pubkey, should fail due to insufficient permissions
    // This test validates that permission checking still works with global permissions
}

#[tokio::test]
async fn test_global_permission_vs_specific_key() {
    let mut validator = AuthValidator::new();
    let (signing_key1, verifying_key1) = generate_keypair();
    let (signing_key2, verifying_key2) = generate_keypair();
    let pubkey1 = format_public_key(&verifying_key1);
    let pubkey2 = format_public_key(&verifying_key2);

    // Create settings with both a specific key and global permission
    let mut auth_settings = AuthSettings::new();

    // Add specific key
    let specific_key = AuthKey::active(Some("specific_key"), Permission::Admin(5));
    auth_settings.add_key(&pubkey1, specific_key).unwrap();

    // Add global permission
    let global_key = AuthKey::active(None::<String>, Permission::Write(10));
    auth_settings.add_key("*", global_key).unwrap();

    // Test 1: Entry signed with specific key should work normally
    let mut entry1 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    entry1.sig = SigInfo::builder()
        .key(SigKey::from_pubkey(&pubkey1))
        .build();
    let signature1 = sign_entry(&entry1, &signing_key1).unwrap();
    entry1.sig.sig = Some(signature1);

    let result1 = validator
        .validate_entry(&entry1, &auth_settings, None)
        .await;
    assert!(result1.is_ok(), "Specific key validation should work");

    // Test 2: Entry using global permission should also work
    let mut entry2 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let global_pubkey2 = format!("*:{}", pubkey2);
    entry2.sig = SigInfo::builder()
        .key(SigKey::from_pubkey(&global_pubkey2)) // Different key using global permission
        .build();
    let signature2 = sign_entry(&entry2, &signing_key2).unwrap();
    entry2.sig.sig = Some(signature2);

    // Global permissions should now work with the pubkey hint
    let result2 = validator
        .validate_entry(&entry2, &auth_settings, None)
        .await;
    assert!(
        result2.is_ok(),
        "Global permission validation should work: {:?}",
        result2.err()
    );
}
