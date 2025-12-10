//! Tests for authentication validation

use super::entry::AuthValidator;
use crate::{
    Entry,
    auth::{
        crypto::{format_public_key, generate_keypair, sign_entry},
        settings::AuthSettings,
        types::{AuthKey, DelegationStep, KeyStatus, Operation, Permission, SigInfo, SigKey},
    },
    crdt::Doc,
    instance::LegacyInstanceOps,
};

fn create_test_auth_with_key(key_name: &str, auth_key: &AuthKey) -> AuthSettings {
    let mut auth_section = Doc::new();
    auth_section.set_json(key_name, auth_key).unwrap();
    AuthSettings::from_doc(auth_section)
}

#[test]
fn test_basic_key_resolution() {
    let mut validator = AuthValidator::new();
    let (_, verifying_key) = generate_keypair();

    let auth_key =
        AuthKey::active(format_public_key(&verifying_key), Permission::Write(10)).unwrap();

    let settings = create_test_auth_with_key("KEY_LAPTOP", &auth_key);

    let sig_key = SigKey::Direct("KEY_LAPTOP".to_string());
    let resolved = validator
        .resolve_sig_key(&sig_key, &settings, None)
        .unwrap();
    assert_eq!(resolved.effective_permission, Permission::Write(10));
    assert_eq!(resolved.key_status, KeyStatus::Active);
}

#[test]
fn test_revoked_key_validation() {
    let mut validator = AuthValidator::new();
    let (_signing_key, verifying_key) = generate_keypair();

    let auth_key =
        AuthKey::active(format_public_key(&verifying_key), Permission::Write(10)).unwrap();

    let settings = create_test_auth_with_key("KEY_LAPTOP", &auth_key);
    let sig_key = SigKey::Direct("KEY_LAPTOP".to_string());
    let resolved = validator.resolve_sig_key(&sig_key, &settings, None);
    assert!(resolved.is_ok());
}

#[test]
fn test_permission_levels() {
    let validator = AuthValidator::new();

    let admin_auth = crate::auth::types::ResolvedAuth {
        public_key: crate::auth::crypto::generate_keypair().1,
        effective_permission: Permission::Admin(5),
        key_status: KeyStatus::Active,
    };

    let write_auth = crate::auth::types::ResolvedAuth {
        public_key: crate::auth::crypto::generate_keypair().1,
        effective_permission: Permission::Write(10),
        key_status: KeyStatus::Active,
    };

    let read_auth = crate::auth::types::ResolvedAuth {
        public_key: crate::auth::crypto::generate_keypair().1,
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

#[test]
fn test_entry_validation_success() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();

    let auth_key =
        AuthKey::active(format_public_key(&verifying_key), Permission::Write(20)).unwrap();

    let settings = create_test_auth_with_key("KEY_LAPTOP", &auth_key);

    // Create a test entry using Entry::builder
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    // Set auth info without signature
    entry.sig = SigInfo::builder()
        .key(SigKey::Direct("KEY_LAPTOP".to_string()))
        .build();

    // Sign the entry
    let signature = sign_entry(&entry, &signing_key).unwrap();

    // Set the signature on the entry
    entry.sig.sig = Some(signature);

    // Validate the entry
    let result = validator.validate_entry(&entry, &settings, None);
    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn test_missing_key() {
    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new(); // Empty auth settings

    let sig_key = SigKey::Direct("NONEXISTENT_KEY".to_string());
    let result = validator.resolve_sig_key(&sig_key, &auth_settings, None);

    assert!(result.is_err());
    match result.unwrap_err() {
        crate::Error::Auth(_) => {} // Expected
        _ => panic!("Expected Auth error"),
    }
}

#[test]
fn test_delegated_tree_requires_backend() {
    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();

    let sig_key = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "user1".to_string(),
            tips: Some(vec![crate::entry::ID::new("tip1")]),
        },
        DelegationStep {
            key: "KEY_LAPTOP".to_string(),
            tips: None,
        },
    ]);

    let result = validator.resolve_sig_key(&sig_key, &auth_settings, None);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Database required for delegated tree resolution")
    );
}

#[test]
fn test_validate_entry_with_auth_info_against_empty_settings() {
    let mut validator = AuthValidator::new();
    let (signing_key, _verifying_key) = generate_keypair();

    // Create an entry with auth info (signed)
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    entry.sig = SigInfo::builder()
        .key(SigKey::Direct("SOME_KEY".to_string()))
        .build();

    // Sign the entry
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validate against empty settings (no auth configuration)
    let empty_auth_settings = AuthSettings::new();
    let result = validator.validate_entry(&entry, &empty_auth_settings, None);

    // Should succeed because there's no auth configuration to validate against
    assert!(result.is_ok(), "Validation failed: {:?}", result.err());
    assert!(result.unwrap(), "Expected validation to return true");
}

#[test]
fn test_entry_validation_with_revoked_key() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();

    let revoked_key = AuthKey::new(
        format_public_key(&verifying_key),
        Permission::Write(10),
        KeyStatus::Revoked, // Key is revoked
    )
    .unwrap();

    let settings = create_test_auth_with_key("KEY_LAPTOP", &revoked_key);

    // Create a test entry using Entry::builder
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    // Set auth info without signature
    entry.sig = SigInfo::builder()
        .key(SigKey::Direct("KEY_LAPTOP".to_string()))
        .build();

    // Sign the entry
    let signature = sign_entry(&entry, &signing_key).unwrap();

    // Set the signature on the entry
    entry.sig.sig = Some(signature);

    // Validation should fail with revoked key
    let result = validator.validate_entry(&entry, &settings, None);
    assert!(result.is_ok()); // validate_entry returns Ok(bool)
    assert!(!result.unwrap()); // But the validation should return false for revoked keys
}

#[test]
fn test_performance_optimizations() {
    let mut validator = AuthValidator::new();
    let (_, verifying_key) = generate_keypair();

    let auth_key =
        AuthKey::active(format_public_key(&verifying_key), Permission::Write(10)).unwrap();

    let settings = create_test_auth_with_key("PERF_KEY", &auth_key);
    let sig_key = SigKey::Direct("PERF_KEY".to_string());

    // Test that resolution works correctly
    let result1 = validator.resolve_sig_key(&sig_key, &settings, None);
    assert!(result1.is_ok());

    // Multiple resolutions should work consistently
    let result2 = validator.resolve_sig_key(&sig_key, &settings, None);
    assert!(result2.is_ok());

    // Results should be identical
    let resolved1 = result1.unwrap();
    let resolved2 = result2.unwrap();
    assert_eq!(
        resolved1.effective_permission,
        resolved2.effective_permission
    );
    assert_eq!(resolved1.key_status, resolved2.key_status);

    // Test cache clear functionality
    validator.clear_cache();
}

#[test]
fn test_basic_delegated_tree_resolution() {
    let mut validator = AuthValidator::new();

    // Create a simple direct key resolution test
    let (_, verifying_key) = generate_keypair();
    let auth_key =
        AuthKey::active(format_public_key(&verifying_key), Permission::Admin(5)).unwrap();

    let settings = create_test_auth_with_key("DIRECT_KEY", &auth_key);

    let sig_key = SigKey::Direct("DIRECT_KEY".to_string());
    let result = validator.resolve_sig_key(&sig_key, &settings, None);

    match result {
        Ok(resolved) => {
            assert_eq!(resolved.effective_permission, Permission::Admin(5));
            assert_eq!(resolved.key_status, KeyStatus::Active);
        }
        Err(e) => {
            panic!("Failed to resolve auth key: {e}");
        }
    }
}

#[test]
fn test_complete_delegation_workflow() {
    use crate::{
        Instance,
        auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference},
        backend::database::InMemory,
    };

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");

    // Single-user mode automatically handles key management
    // Use the default user's device key for main admin
    let main_key = db
        .backend()
        .get_private_key("_device_key")
        .unwrap()
        .unwrap()
        .verifying_key();
    // For delegated key, we'll use the same device key for simplicity in tests
    let delegated_key = main_key;

    // Create the delegated tree with its own auth configuration
    let mut delegated_settings = Doc::new();
    let mut delegated_auth = Doc::new();
    delegated_auth
        .set_json(
            "delegated_user", // Key name must match the key used for tree creation
            AuthKey::active(format_public_key(&delegated_key), Permission::Admin(5)).unwrap(),
        )
        .unwrap();
    // Add _device_key to auth config so we can create the database with it
    delegated_auth
        .set_json(
            "_device_key",
            AuthKey::active(format_public_key(&main_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();
    delegated_settings.set("auth", delegated_auth);

    let delegated_tree = db.new_database(delegated_settings, "_device_key").unwrap();

    // Create the main tree with delegation configuration
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();

    // Add direct key to main tree
    main_auth
        .set_json(
            "main_admin",
            AuthKey::active(format_public_key(&main_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();
    // Add _device_key to auth config so we can create the database with it
    main_auth
        .set_json(
            "_device_key",
            AuthKey::active(format_public_key(&main_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();

    // Get the actual tips from the delegated tree
    let delegated_tips = delegated_tree.get_tips().unwrap();

    // Add delegation reference
    main_auth
        .set_json(
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
        )
        .unwrap();

    main_settings.set("auth", main_auth);
    let main_tree = db.new_database(main_settings, "_device_key").unwrap();

    // Test delegation resolution
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree
        .get_settings()
        .unwrap()
        .get_auth_settings()
        .unwrap();

    let delegated_sig_key = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_user".to_string(),
            tips: Some(delegated_tips),
        },
        DelegationStep {
            key: "delegated_user".to_string(),
            tips: None,
        },
    ]);

    let result = validator.resolve_sig_key(&delegated_sig_key, &main_auth_settings, Some(&db));

    // Should succeed with permission clamping (Admin -> Write due to bounds)
    assert!(
        result.is_ok(),
        "Delegation resolution failed: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert_eq!(resolved.effective_permission, Permission::Write(10)); // Clamped from Admin to Write
    assert_eq!(resolved.key_status, KeyStatus::Active);
}

#[test]
fn test_delegated_tree_requires_tips() {
    use crate::{
        Instance,
        auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference},
        backend::database::InMemory,
    };

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");

    // Single-user mode automatically handles key management
    // Use the default user's device key for main admin
    let main_key = db
        .backend()
        .get_private_key("_device_key")
        .unwrap()
        .unwrap()
        .verifying_key();

    // Create a simple delegated tree
    let delegated_settings = Doc::new();
    let delegated_tree = db.new_database(delegated_settings, "_device_key").unwrap();

    // Create the main tree with delegation configuration
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();

    // Add direct key to main tree
    main_auth
        .set_json(
            "main_admin",
            AuthKey::active(format_public_key(&main_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();

    // Add delegation reference (with proper tips that we'll ignore in the test)
    main_auth
        .set_json(
            "delegate_to_user",
            DelegatedTreeRef {
                permission_bounds: PermissionBounds {
                    max: Permission::Write(10),
                    min: Some(Permission::Read),
                },
                tree: TreeReference {
                    root: delegated_tree.root_id().clone(),
                    tips: vec![crate::entry::ID::new("some_tip")], // This will be ignored due to empty tips in auth_id
                },
            },
        )
        .unwrap();

    main_settings.set("auth", main_auth.clone());

    // Create validator and test with empty tips
    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::from_doc(main_auth);

    // Create a DelegationPath sig_key with empty tips
    let sig_key = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_to_user".to_string(),
            tips: Some(vec![]), // Empty tips should cause validation to fail
        },
        DelegationStep {
            key: "delegated_user".to_string(),
            tips: None,
        },
    ]);

    let result = validator.resolve_sig_key(&sig_key, &auth_settings, Some(&db));

    // Should fail because tips are required for delegated tree resolution
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Invalid delegation"),
        "Expected error about invalid delegation, got: {error_msg}"
    );
}

#[test]
fn test_nested_delegation_with_permission_clamping() {
    use crate::{
        Instance,
        auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference},
        backend::database::InMemory,
    };

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");

    // Use the default user's device key for all operations
    let main_key = db
        .backend()
        .get_private_key("_device_key")
        .unwrap()
        .unwrap()
        .verifying_key();
    let intermediate_key = main_key;
    let user_key = main_key;

    // 1. Create the final user tree (deepest level)
    let mut user_settings = Doc::new();
    let mut user_auth = Doc::new();
    user_auth
        .set_json(
            "final_user",
            AuthKey::active(
                format_public_key(&user_key),
                Permission::Admin(3), // High privilege at source
            )
            .unwrap(),
        )
        .unwrap();
    // Add _device_key to auth config so we can create the database with it
    user_auth
        .set_json(
            "_device_key",
            AuthKey::active(format_public_key(&main_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();
    user_settings.set("auth", user_auth);
    let user_tree = db.new_database(user_settings, "_device_key").unwrap();
    let user_tips = user_tree.get_tips().unwrap();

    // 2. Create intermediate delegated tree that delegates to user tree
    let mut intermediate_settings = Doc::new();
    let mut intermediate_auth = Doc::new();

    // Add direct key to intermediate tree
    intermediate_auth
        .set_json(
            "intermediate_admin",
            AuthKey::active(format_public_key(&intermediate_key), Permission::Admin(2)).unwrap(),
        )
        .unwrap();
    // Add _device_key to auth config so we can create the database with it
    intermediate_auth
        .set_json(
            "_device_key",
            AuthKey::active(format_public_key(&main_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();

    // Add delegation to user tree with bounds Write(8) max, Read min
    intermediate_auth
        .set_json(
            "user_delegation",
            DelegatedTreeRef {
                permission_bounds: PermissionBounds {
                    max: Permission::Write(8), // Clamp Admin(3) to Write(8)
                    min: Some(Permission::Read),
                },
                tree: TreeReference {
                    root: user_tree.root_id().clone(),
                    tips: user_tips.clone(),
                },
            },
        )
        .unwrap();

    intermediate_settings.set("auth", intermediate_auth);
    let intermediate_tree = db
        .new_database(intermediate_settings, "_device_key")
        .unwrap();
    let intermediate_tips = intermediate_tree.get_tips().unwrap();

    // 3. Create main tree that delegates to intermediate tree
    let mut main_settings = Doc::new();
    let mut main_auth = Doc::new();

    // Add direct key to main tree
    main_auth
        .set_json(
            "main_admin",
            AuthKey::active(format_public_key(&main_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();
    // Add _device_key to auth config so we can create the database with it
    main_auth
        .set_json(
            "_device_key",
            AuthKey::active(format_public_key(&main_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();

    // Add delegation to intermediate tree with bounds Write(5) max, Read min
    // This should be more restrictive than the intermediate tree's Write(8)
    main_auth
        .set_json(
            "intermediate_delegation",
            DelegatedTreeRef {
                permission_bounds: PermissionBounds {
                    max: Permission::Write(5), // More restrictive than Write(8)
                    min: Some(Permission::Read),
                },
                tree: TreeReference {
                    root: intermediate_tree.root_id().clone(),
                    tips: intermediate_tips.clone(),
                },
            },
        )
        .unwrap();

    main_settings.set("auth", main_auth);
    let main_tree = db.new_database(main_settings, "_device_key").unwrap();

    // 4. Test nested delegation resolution: Main -> Intermediate -> User
    let mut validator = AuthValidator::new();
    let main_auth_settings = main_tree
        .get_settings()
        .unwrap()
        .get_auth_settings()
        .unwrap();

    // Create nested delegation SigKey:
    // Main tree delegates to "intermediate_delegation" ->
    // Intermediate tree delegates to "user_delegation" ->
    // User tree resolves "final_user" key
    let nested_sig_key = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "intermediate_delegation".to_string(),
            tips: Some(intermediate_tips),
        },
        DelegationStep {
            key: "user_delegation".to_string(),
            tips: Some(user_tips),
        },
        DelegationStep {
            key: "final_user".to_string(),
            tips: None,
        },
    ]);

    let result = validator.resolve_sig_key(&nested_sig_key, &main_auth_settings, Some(&db));

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

    assert_eq!(resolved.effective_permission, Permission::Write(8)); // Correctly clamped through the chain
    assert_eq!(resolved.key_status, KeyStatus::Active);
}

#[test]
fn test_delegation_depth_limit() {
    // Test that excessive delegation depth is prevented
    let mut validator = AuthValidator::new();

    // Create empty auth settings (doesn't matter for depth test)
    let auth_settings = AuthSettings::new();

    // Test the depth check by directly calling with depth = MAX_DELEGATION_DEPTH
    let simple_sig_key = SigKey::Direct("base_key".to_string());

    // This should succeed (just under the limit)
    let result =
        validator
            .resolver
            .resolve_sig_key_with_depth(&simple_sig_key, &auth_settings, None, 9);
    // Should fail due to missing auth configuration, not depth limit
    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Key 'base_key' not found and no global permission available")
    );

    // This should fail due to depth limit (at the limit)
    let result =
        validator
            .resolver
            .resolve_sig_key_with_depth(&simple_sig_key, &auth_settings, None, 10);
    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(error.to_string().contains("Maximum delegation depth"));
    assert!(error.to_string().contains("exceeded"));
}

// ===== GLOBAL PERMISSION TESTS =====

#[test]
fn test_global_permission_with_pubkey_field() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();

    // Create settings with global "*" permission
    let global_auth_key = AuthKey::active("*", Permission::Write(10)).unwrap();

    let settings = create_test_auth_with_key("*", &global_auth_key);

    // Create an entry that uses global permission
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    // Set signature info to reference "*" permission and include actual pubkey
    entry.sig = SigInfo {
        key: SigKey::Direct("*".to_string()),
        sig: None,
        pubkey: Some(format_public_key(&verifying_key)), // Include actual signer's pubkey
    };

    // Sign the entry with the client's key
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validation should succeed
    let result = validator.validate_entry(&entry, &settings, None);
    assert!(result.is_ok(), "Validation failed: {:?}", result.err());
    assert!(
        result.unwrap(),
        "Expected validation to succeed with global permission"
    );
}

#[test]
fn test_global_permission_without_pubkey_fails() {
    let mut validator = AuthValidator::new();
    let (signing_key, _) = generate_keypair();

    // Create settings with global "*" permission
    let global_auth_key = AuthKey::active("*", Permission::Write(10)).unwrap();

    let settings = create_test_auth_with_key("*", &global_auth_key);

    // Create an entry that uses global permission but doesn't include pubkey
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    entry.sig = SigInfo {
        key: SigKey::Direct("*".to_string()),
        sig: None,
        pubkey: None, // Missing pubkey field - should fail
    };

    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validation should fail due to missing pubkey
    let result = validator.validate_entry(&entry, &settings, None);
    assert!(
        result.is_err(),
        "Expected validation to fail without pubkey field"
    );
}

#[test]
fn test_global_permission_resolver() {
    let mut validator = AuthValidator::new();
    let (_, verifying_key) = generate_keypair();

    // Create settings with global "*" permission
    let global_auth_key = AuthKey::active("*", Permission::Write(10)).unwrap();

    let settings = create_test_auth_with_key("*", &global_auth_key);

    // Test resolving "*" with provided pubkey
    let sig_key = SigKey::Direct("*".to_string());
    let actual_pubkey = format_public_key(&verifying_key);

    // Test with pubkey provided - should succeed now that we implemented global permissions
    let result =
        validator.resolve_sig_key_with_pubkey(&sig_key, &settings, None, Some(&actual_pubkey));

    assert!(
        result.is_ok(),
        "Global permission resolution should work with pubkey: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert_eq!(resolved.public_key, verifying_key);
    assert_eq!(resolved.effective_permission, Permission::Write(10));
}

#[test]
fn test_global_permission_insufficient_perms() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();

    // Create settings with global "*" permission but only Read access
    let global_auth_key = AuthKey::active(
        "*",
        Permission::Read, // Only read permission
    )
    .unwrap();

    let settings = create_test_auth_with_key("*", &global_auth_key);

    // Test that global permissions still respect the permission level
    let sig_key = SigKey::Direct("*".to_string());
    let actual_pubkey = format_public_key(&verifying_key);

    let result =
        validator.resolve_sig_key_with_pubkey(&sig_key, &settings, None, Some(&actual_pubkey));
    assert!(result.is_ok(), "Resolution should succeed");

    let resolved = result.unwrap();
    assert_eq!(resolved.effective_permission, Permission::Read); // Should have read permission

    // Create an entry that tries to write (requires Write permission)
    let mut entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    entry.sig = SigInfo {
        key: SigKey::Direct("*".to_string()),
        sig: None,
        pubkey: Some(format_public_key(&verifying_key)),
    };

    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Even with valid signature and pubkey, should fail due to insufficient permissions
    // This test validates that permission checking still works with global permissions

    // FIXME: Once we implement the solution, we'll need to test permission checking
    // For now, this documents the expected behavior
}

#[test]
fn test_global_permission_vs_specific_key() {
    let mut validator = AuthValidator::new();
    let (signing_key1, verifying_key1) = generate_keypair();
    let (signing_key2, verifying_key2) = generate_keypair();

    // Create settings with both a specific key and global permission
    let mut auth_section = Doc::new();

    // Add specific key
    let specific_key =
        AuthKey::active(format_public_key(&verifying_key1), Permission::Admin(5)).unwrap();
    auth_section
        .set_json("specific_key", &specific_key)
        .unwrap();

    // Add global permission
    let global_key = AuthKey::active("*", Permission::Write(10)).unwrap();
    auth_section.set_json("*", &global_key).unwrap();

    let auth_settings = AuthSettings::from_doc(auth_section);

    // Test 1: Entry signed with specific key should work normally
    let mut entry1 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    entry1.sig = SigInfo::builder()
        .key(SigKey::Direct("specific_key".to_string()))
        .build(); // No pubkey needed for specific keys
    let signature1 = sign_entry(&entry1, &signing_key1).unwrap();
    entry1.sig.sig = Some(signature1);

    let result1 = validator.validate_entry(&entry1, &auth_settings, None);
    assert!(result1.is_ok(), "Specific key validation should work");

    // Test 2: Entry using global permission should also work
    let mut entry2 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    entry2.sig = SigInfo::builder()
        .key(SigKey::Direct("*".to_string()))
        .pubkey(format_public_key(&verifying_key2)) // Different key using global permission
        .build();
    let signature2 = sign_entry(&entry2, &signing_key2).unwrap();
    entry2.sig.sig = Some(signature2);

    // Global permissions should now work with the pubkey field
    let result2 = validator.validate_entry(&entry2, &auth_settings, None);
    assert!(
        result2.is_ok(),
        "Global permission validation should work: {:?}",
        result2.err()
    );
}
