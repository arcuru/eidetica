//! Tests for authentication validation
//!
//! These tests directly generate keypairs to test auth primitives (signatures, permissions,
//! delegation) without going through the User API. This is intentional - we're testing the
//! underlying auth mechanics, not the User API layer.

use super::entry::AuthValidator;
use crate::{
    Database, Entry, Error,
    auth::{
        crypto::{PrivateKey, PublicKey, generate_keypair, sign_entry},
        settings::AuthSettings,
        types::{
            AuthKey, DelegationStep, KeyHint, KeyStatus, Operation, Permission, ResolvedAuth,
            SigInfo, SigKey,
        },
    },
    crdt::Doc,
    entry::ID,
};

fn create_test_auth_with_key(pubkey: &PublicKey, auth_key: &AuthKey) -> AuthSettings {
    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(pubkey, auth_key.clone()).unwrap();
    auth_settings
}

fn create_test_auth_with_global(auth_key: &AuthKey) -> AuthSettings {
    let mut auth_settings = AuthSettings::new();
    auth_settings.set_global_permission(auth_key.clone());
    auth_settings
}

#[tokio::test]
async fn test_basic_key_resolution() {
    let mut validator = AuthValidator::new();
    let (_, pubkey) = generate_keypair();

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
    let (_signing_key, pubkey) = generate_keypair();

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
        public_key: PrivateKey::generate().public_key(),
        effective_permission: Permission::Admin(5),
        key_status: KeyStatus::Active,
    };

    let write_auth = ResolvedAuth {
        public_key: PrivateKey::generate().public_key(),
        effective_permission: Permission::Write(10),
        key_status: KeyStatus::Active,
    };

    let read_auth = ResolvedAuth {
        public_key: PrivateKey::generate().public_key(),
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
    let (signing_key, pubkey) = generate_keypair();

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
            tree: ID::from_bytes("user1"),
            tips: vec![ID::from_bytes("tip1")],
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
async fn test_validate_entry_with_auth_info_against_empty_settings() {
    let mut validator = AuthValidator::new();
    let (signing_key, pubkey) = generate_keypair();

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
    let (signing_key, pubkey) = generate_keypair();

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
    let (_, pubkey) = generate_keypair();

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
    let (_, pubkey) = generate_keypair();
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
    let (instance, _admin) =
        Instance::create_backend(backend, crate::NewUser::passwordless("admin"))
            .await
            .expect("Failed to create test instance");

    // Generate a separate key for the delegated user role
    let (_, delegated_pubkey) = generate_keypair();

    // Create the delegated tree — device key bootstrapped as Admin(0), untouched
    let delegated_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Add the delegated user key at Admin(5)
    let txn = delegated_tree.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
        .set_auth_key(
            &delegated_pubkey,
            AuthKey::active(Some("delegated_user"), Permission::Admin(5)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Get the actual tips from the delegated tree
    let delegated_tips = delegated_tree.snapshot().await.unwrap().into_tips();

    // Create the main tree — signing key bootstrapped as Admin(0)
    let main_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Add delegation reference via follow-up transaction
    let txn = main_tree.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
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
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Test delegation resolution
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap();

    let delegated_sig_key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
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
    let (instance, _admin) =
        Instance::create_backend(backend, crate::NewUser::passwordless("admin"))
            .await
            .expect("Failed to create test instance");

    // Create a simple delegated tree
    let delegated_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Create the main tree — signing key bootstrapped as Admin(0)
    let main_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Add delegation reference via follow-up transaction
    let txn = main_tree.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: vec![ID::from_bytes("some_tip")],
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Build main_auth from the stored settings for the validator test
    let main_auth = main_tree
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap();

    // Create validator and test with empty tips
    let mut validator = AuthValidator::new();

    // Create a Delegation sig_key with empty tips
    let sig_key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
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
    let (instance, _admin) =
        Instance::create_backend(backend, crate::NewUser::passwordless("admin"))
            .await
            .expect("Failed to create test instance");

    // Generate a separate key for the delegated user
    let (_, user_pubkey) = generate_keypair();

    // 1. Create the final user tree (deepest level)
    // Device key bootstrapped as Admin(0), untouched
    let user_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Add the delegated user key at Admin(3)
    let txn = user_tree.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
        .set_auth_key(
            &user_pubkey,
            AuthKey::active(Some("final_user"), Permission::Admin(3)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let user_tips = user_tree.snapshot().await.unwrap().into_tips();

    // 2. Create intermediate delegated tree that delegates to user tree
    let intermediate_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Add delegation to user tree (no key overwrite needed)
    let txn = intermediate_tree.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
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
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let intermediate_tips = intermediate_tree.snapshot().await.unwrap().into_tips();

    // 3. Create main tree that delegates to intermediate tree
    // Signing key stays at Admin(0) — matches original test intent.
    let main_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Add delegation to intermediate tree via follow-up transaction
    let txn = main_tree.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
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
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // 4. Test nested delegation resolution: Main -> Intermediate -> User
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap();

    // Create nested delegation SigKey:
    // Main tree delegates to intermediate_tree (by root ID) ->
    // Intermediate tree delegates to user_tree (by root ID) ->
    // User tree resolves key by pubkey hint
    let nested_sig_key = SigKey::Delegation {
        path: vec![
            DelegationStep {
                tree: intermediate_tree.root_id().clone(),
                tips: intermediate_tips,
            },
            DelegationStep {
                tree: user_tree.root_id().clone(),
                tips: user_tips,
            },
        ],
        hint: KeyHint::from_pubkey(&user_pubkey),
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
    let (signing_key, actual_pubkey) = generate_keypair();

    // Create settings with global permission
    let global_auth_key = AuthKey::active(None, Permission::Write(10));

    let settings = create_test_auth_with_global(&global_auth_key);

    // Create an entry that uses global permission with actual signer pubkey in hint
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    // Use SigKey::global to indicate global permission with actual signer
    entry.sig = SigInfo {
        key: SigKey::global(&actual_pubkey),
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

    // Create settings with global permission
    let global_auth_key = AuthKey::active(None, Permission::Write(10));

    let settings = create_test_auth_with_global(&global_auth_key);

    // Create an entry that uses a name hint "*" without pubkey - should fail
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
    let actual_pubkey = PrivateKey::generate().public_key();

    // Create settings with global permission
    let global_auth_key = AuthKey::active(None, Permission::Write(10));

    let settings = create_test_auth_with_global(&global_auth_key);

    // Test resolving global hint - should succeed
    let sig_key = SigKey::global(&actual_pubkey);

    let result = validator.resolve_sig_key(&sig_key, &settings, None).await;

    assert!(
        result.is_ok(),
        "Global permission resolution should work with pubkey hint: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].public_key, actual_pubkey);
    assert_eq!(resolved[0].effective_permission, Permission::Write(10));
}

#[tokio::test]
async fn test_global_permission_insufficient_perms() {
    let mut validator = AuthValidator::new();
    let (signing_key, actual_pubkey) = generate_keypair();

    // Create settings with global permission but only Read access
    let global_auth_key = AuthKey::active(
        None,
        Permission::Read, // Only read permission
    );

    let settings = create_test_auth_with_global(&global_auth_key);

    // Test that global permissions still respect the permission level
    let sig_key = SigKey::global(&actual_pubkey);

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
        key: SigKey::global(&actual_pubkey),
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
    let (signing_key1, pubkey1) = generate_keypair();
    let (signing_key2, pubkey2) = generate_keypair();

    // Create settings with both a specific key and global permission
    let mut auth_settings = AuthSettings::new();

    // Add specific key
    let specific_key = AuthKey::active(Some("specific_key"), Permission::Admin(5));
    auth_settings.add_key(&pubkey1, specific_key).unwrap();

    // Set global permission
    let global_key = AuthKey::active(None, Permission::Write(10));
    auth_settings.set_global_permission(global_key);

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
    entry2.sig = SigInfo::builder()
        .key(SigKey::global(&pubkey2)) // Different key using global permission
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

/// Build an instance with a delegated tree (carrying `delegated_pubkey` as
/// Admin) and a main tree that declares the delegation pinned at `floor_tips`.
/// Returns everything needed to drive `resolve_sig_key` against it.
async fn setup_delegation_with_floor(
    floor_at_first_snapshot: bool,
) -> (
    crate::Instance,
    Database,
    PublicKey,
    Vec<ID>, // snapshot 1 tips (older)
    Vec<ID>, // snapshot 2 tips (newer)
    AuthSettings,
) {
    use crate::{
        Instance,
        auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference},
        backend::database::InMemory,
    };

    let backend = Box::new(InMemory::new());
    let (instance, _admin) =
        Instance::create_backend(backend, crate::NewUser::passwordless("admin"))
            .await
            .expect("Failed to create test instance");

    let (_, delegated_pubkey) = generate_keypair();

    let delegated_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Snapshot 1: delegated user added.
    let txn = delegated_tree.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &delegated_pubkey,
            AuthKey::active(Some("delegated_user"), Permission::Admin(5)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let snap1 = delegated_tree.snapshot().await.unwrap().into_tips();

    // Snapshot 2: an unrelated settings write advances the delegated tree.
    let txn = delegated_tree.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_name("delegated-renamed")
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let snap2 = delegated_tree.snapshot().await.unwrap().into_tips();
    assert_ne!(snap1, snap2, "second commit must advance the tips");

    let floor = if floor_at_first_snapshot {
        snap1.clone()
    } else {
        snap2.clone()
    };

    let main_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();
    let txn = main_tree.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: floor,
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let main_auth = main_tree
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap();

    (
        instance,
        delegated_tree,
        delegated_pubkey,
        snap1,
        snap2,
        main_auth,
    )
}

/// The claimed delegated-tree snapshot must not regress below the snapshot the
/// parent tree committed (the settings-pointer floor). Pinning at the floor (or
/// ahead of it) resolves; pinning behind it is rejected.
#[tokio::test]
async fn test_delegation_floor_rejects_snapshot_regression() {
    // Floor committed at snapshot 2 (the newer state).
    let (instance, delegated_tree, delegated_pubkey, snap1, snap2, main_auth) =
        setup_delegation_with_floor(false).await;

    // Regression: claim snapshot 1, which is an ancestor of the committed floor.
    let regress = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
            tips: snap1,
        }],
        hint: KeyHint::from_pubkey(&delegated_pubkey),
    };
    let err = AuthValidator::new()
        .resolve_sig_key(&regress, &main_auth, Some(&instance))
        .await
        .expect_err("claiming a snapshot behind the committed floor must be rejected");
    assert!(
        err.to_string().contains("Invalid delegation tips"),
        "expected InvalidDelegationTips, got: {err}"
    );

    // At the floor: claim snapshot 2 — accepted, and resolves the delegated key.
    let ok = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
            tips: snap2,
        }],
        hint: KeyHint::from_pubkey(&delegated_pubkey),
    };
    let resolved = AuthValidator::new()
        .resolve_sig_key(&ok, &main_auth, Some(&instance))
        .await
        .expect("claiming the committed snapshot must resolve");
    assert_eq!(resolved.len(), 1);
    // Admin(5) clamped to the delegation's Write(10) bound.
    assert_eq!(resolved[0].effective_permission, Permission::Write(10));
}

/// Tips pin resolution to the observed snapshot: a key that exists only at the
/// newer snapshot is invisible when the entry pins the older one (floor at S1).
#[tokio::test]
async fn test_delegation_resolves_settings_at_claimed_tips() {
    let (instance, delegated_tree, delegated_pubkey, snap1, _snap2, main_auth) =
        setup_delegation_with_floor(true).await;

    // Pinning exactly at the floor (snap1) resolves the key present there.
    let sig = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
            tips: snap1,
        }],
        hint: KeyHint::from_pubkey(&delegated_pubkey),
    };
    let resolved = AuthValidator::new()
        .resolve_sig_key(&sig, &main_auth, Some(&instance))
        .await
        .expect("resolution at the pinned snapshot should succeed");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].key_status, KeyStatus::Active);
}

/// F5a: a delegation path longer than the cap is rejected before backend work.
#[tokio::test]
async fn test_delegation_path_length_capped() {
    let (instance, delegated_tree, delegated_pubkey, _s1, snap2, main_auth) =
        setup_delegation_with_floor(false).await;

    let step = DelegationStep {
        tree: delegated_tree.root_id().clone(),
        tips: snap2,
    };
    // 11 steps > MAX_DELEGATION_STEPS (10).
    let sig = SigKey::Delegation {
        path: vec![step; 11],
        hint: KeyHint::from_pubkey(&delegated_pubkey),
    };
    let err = AuthValidator::new()
        .resolve_sig_key(&sig, &main_auth, Some(&instance))
        .await
        .expect_err("over-long delegation path must be rejected");
    assert!(
        err.to_string().contains("Delegation path too long"),
        "expected DelegationPathTooLong, got: {err}"
    );
}

/// F5a: a step claiming more tips than the cap is rejected before backend work.
#[tokio::test]
async fn test_delegation_tips_count_capped() {
    let (instance, delegated_tree, delegated_pubkey, _s1, _s2, main_auth) =
        setup_delegation_with_floor(false).await;

    // 65 fabricated tips > MAX_DELEGATION_TIPS (64). The cap fires before any
    // backend traversal, so the tips not existing is irrelevant.
    let tips: Vec<ID> = (0u16..65)
        .map(|i| ID::from_bytes(i.to_le_bytes()))
        .collect();
    let sig = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
            tips,
        }],
        hint: KeyHint::from_pubkey(&delegated_pubkey),
    };
    let err = AuthValidator::new()
        .resolve_sig_key(&sig, &main_auth, Some(&instance))
        .await
        .expect_err("over-large tip set must be rejected");
    assert!(
        err.to_string().contains("too many tips"),
        "expected DelegationTipsTooMany, got: {err}"
    );
}

/// F5b-1: a claimed tip that is a real entry of *another* tree is rejected.
/// The prior `backend.get().is_ok()` check accepted any entry that existed
/// anywhere in the backend; the tree-scoped check rejects it.
#[tokio::test]
async fn test_delegation_rejects_foreign_tip() {
    let (instance, delegated_tree, delegated_pubkey, _s1, _s2, main_auth) =
        setup_delegation_with_floor(false).await;

    // A separate, real tree. Its root is a genuine backend entry (so the old
    // existence-only check would have passed) but it does not belong to the
    // delegated tree.
    let other_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();
    let foreign_tip = other_tree.root_id().clone();

    let sig = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
            tips: vec![foreign_tip],
        }],
        hint: KeyHint::from_pubkey(&delegated_pubkey),
    };
    let err = AuthValidator::new()
        .resolve_sig_key(&sig, &main_auth, Some(&instance))
        .await
        .expect_err("a tip from another tree must be rejected");
    assert!(
        err.to_string().contains("Invalid delegation tips"),
        "expected InvalidDelegationTips, got: {err}"
    );
}

/// Tips pin the *auth state*, not merely tip identity: when the delegated key is
/// downgraded at a newer snapshot, an entry pinning the older snapshot must still
/// resolve with the older (higher) permission. This is the regression test for
/// "claimed tips were ignored" -- pre-fix, resolution read the delegated tree's
/// live head and saw the downgrade regardless of which tips the signer claimed.
#[tokio::test]
async fn test_delegation_reads_auth_state_at_pinned_tips_not_live_head() {
    use crate::{
        Instance,
        auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference},
        backend::database::InMemory,
    };

    let backend = Box::new(InMemory::new());
    let (instance, _admin) =
        Instance::create_backend(backend, crate::NewUser::passwordless("admin"))
            .await
            .unwrap();

    let (_, delegated_pubkey) = generate_keypair();

    let delegated_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();

    // Snapshot 1: the delegated key is Admin(5).
    let txn = delegated_tree.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &delegated_pubkey,
            AuthKey::active(Some("delegated_user"), Permission::Admin(5)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let snap1 = delegated_tree.snapshot().await.unwrap().into_tips();

    // Snapshot 2: the SAME key is downgraded to Read on the live head.
    let txn = delegated_tree.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &delegated_pubkey,
            AuthKey::active(Some("delegated_user"), Permission::Read),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let snap2 = delegated_tree.snapshot().await.unwrap().into_tips();
    assert_ne!(snap1, snap2, "the downgrade commit must advance the tips");

    // Floor committed at snapshot 1, so pinning snapshot 1 sits exactly at the
    // floor (allowed by the monotonicity check) while still being behind the
    // live head.
    let main_tree = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();
    let txn = main_tree.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: delegated_tree.root_id().clone(),
                tips: snap1.clone(),
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let main_auth = main_tree
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap();

    // Pin snapshot 1, where the key is Admin(5). Resolution must read auth state
    // AS OF the pinned tips: Admin(5) clamped to the Write(10) bound -- NOT the
    // live-head Read downgrade.
    let sig = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_tree.root_id().clone(),
            tips: snap1,
        }],
        hint: KeyHint::from_pubkey(&delegated_pubkey),
    };
    let resolved = AuthValidator::new()
        .resolve_sig_key(&sig, &main_auth, Some(&instance))
        .await
        .expect("resolution at the pinned snapshot should succeed");
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].effective_permission,
        Permission::Write(10),
        "pinned tips must resolve the Admin(5) state at snapshot 1, not the live-head Read downgrade"
    );
}
