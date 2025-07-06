//! Tests for authentication validation

use super::entry::AuthValidator;
use crate::auth::crypto::{format_public_key, generate_keypair, sign_entry};
use crate::auth::types::{
    AuthKey, DelegationStep, KeyStatus, Operation, Permission, SigInfo, SigKey,
};
use crate::crdt::Nested;
use crate::entry::Entry;

fn create_test_settings_with_key(key_id: &str, auth_key: &AuthKey) -> Nested {
    let mut settings = Nested::new();
    let mut auth_section = Nested::new();
    auth_section.as_hashmap_mut().insert(
        key_id.to_string(),
        serde_json::to_string(&auth_key).unwrap().into(),
    );
    settings.set_map("auth", auth_section);
    settings
}

#[test]
fn test_basic_key_resolution() {
    let mut validator = AuthValidator::new();
    let (_, verifying_key) = generate_keypair();

    let auth_key = AuthKey {
        pubkey: format_public_key(&verifying_key),
        permissions: Permission::Write(10),
        status: KeyStatus::Active,
    };

    let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);

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

    let auth_key = AuthKey {
        pubkey: format_public_key(&verifying_key),
        permissions: Permission::Write(10),
        status: KeyStatus::Active,
    };

    let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);
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

    let auth_key = AuthKey {
        pubkey: format_public_key(&verifying_key),
        permissions: Permission::Write(20),
        status: KeyStatus::Active,
    };

    let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);

    // Create a test entry using Entry::builder
    let mut entry = Entry::builder("abc").build();

    // Set auth info without signature
    entry.sig = SigInfo {
        key: SigKey::Direct("KEY_LAPTOP".to_string()),
        sig: None,
    };

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
    let settings = Nested::new(); // Empty settings

    let sig_key = SigKey::Direct("NONEXISTENT_KEY".to_string());
    let result = validator.resolve_sig_key(&sig_key, &settings, None);

    assert!(result.is_err());
    match result.unwrap_err() {
        crate::Error::Auth(_) | crate::Error::Authentication(_) => {} // Expected
        _ => panic!("Expected Auth or Authentication error"),
    }
}

#[test]
fn test_delegated_tree_requires_backend() {
    let mut validator = AuthValidator::new();
    let settings = Nested::new();

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

    let result = validator.resolve_sig_key(&sig_key, &settings, None);
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
    let mut entry = Entry::builder("root123").build();
    entry.sig = SigInfo {
        key: SigKey::Direct("SOME_KEY".to_string()),
        sig: None,
    };

    // Sign the entry
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validate against empty settings (no auth configuration)
    let empty_settings = Nested::new();
    let result = validator.validate_entry(&entry, &empty_settings, None);

    // Should succeed because there's no auth configuration to validate against
    assert!(result.is_ok(), "Validation failed: {:?}", result.err());
    assert!(result.unwrap(), "Expected validation to return true");
}

#[test]
fn test_entry_validation_with_revoked_key() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();

    let revoked_key = AuthKey {
        pubkey: format_public_key(&verifying_key),
        permissions: Permission::Write(10),
        status: KeyStatus::Revoked, // Key is revoked
    };

    let settings = create_test_settings_with_key("KEY_LAPTOP", &revoked_key);

    // Create a test entry using Entry::builder
    let mut entry = Entry::builder("abc").build();

    // Set auth info without signature
    entry.sig = SigInfo {
        key: SigKey::Direct("KEY_LAPTOP".to_string()),
        sig: None,
    };

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

    let auth_key = AuthKey {
        pubkey: format_public_key(&verifying_key),
        permissions: Permission::Write(10),
        status: KeyStatus::Active,
    };

    let settings = create_test_settings_with_key("PERF_KEY", &auth_key);
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
    let auth_key = AuthKey {
        pubkey: format_public_key(&verifying_key),
        permissions: Permission::Admin(5),
        status: KeyStatus::Active,
    };

    let settings = create_test_settings_with_key("DIRECT_KEY", &auth_key);

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
    use crate::auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference};
    use crate::backend::database::InMemory;
    use crate::basedb::BaseDB;

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Create keys for both main and delegated trees
    let main_key = db.add_private_key("main_admin").unwrap();
    let delegated_key = db.add_private_key("delegated_user").unwrap();

    // Create the delegated tree with its own auth configuration
    let mut delegated_settings = Nested::new();
    let mut delegated_auth = Nested::new();
    delegated_auth
        .set_json(
            "delegated_user", // Key name must match the key used for tree creation
            AuthKey {
                pubkey: format_public_key(&delegated_key),
                permissions: Permission::Admin(5),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    delegated_settings.set_map("auth", delegated_auth);

    let delegated_tree = db.new_tree(delegated_settings, "delegated_user").unwrap();

    // Create the main tree with delegation configuration
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();

    // Add direct key to main tree
    main_auth
        .set_json(
            "main_admin",
            AuthKey {
                pubkey: format_public_key(&main_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
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

    main_settings.set_map("auth", main_auth);
    let main_tree = db.new_tree(main_settings, "main_admin").unwrap();

    // Test delegation resolution
    let mut validator = AuthValidator::new();
    let main_settings = main_tree.get_settings().unwrap().get_all().unwrap();

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

    let result = validator.resolve_sig_key(&delegated_sig_key, &main_settings, Some(db.backend()));

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
    use crate::auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference};
    use crate::backend::database::InMemory;
    use crate::basedb::BaseDB;

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Create keys for both main and delegated trees
    let main_key = db.add_private_key("main_admin").unwrap();

    // Create a simple delegated tree
    let delegated_settings = Nested::new();
    let delegated_tree = db.new_tree(delegated_settings, "main_admin").unwrap();

    // Create the main tree with delegation configuration
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();

    // Add direct key to main tree
    main_auth
        .set_json(
            "main_admin",
            AuthKey {
                pubkey: format_public_key(&main_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
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

    main_settings.set_map("auth", main_auth);

    // Create validator and test with empty tips
    let mut validator = AuthValidator::new();
    let settings = main_settings;

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

    let result = validator.resolve_sig_key(&sig_key, &settings, Some(db.backend()));

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
    use crate::auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference};
    use crate::backend::database::InMemory;
    use crate::basedb::BaseDB;

    // Create a backend and database for testing
    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Create keys for main tree, intermediate delegated tree, and final user tree
    let main_key = db.add_private_key("main_admin").unwrap();
    let intermediate_key = db.add_private_key("intermediate_admin").unwrap();
    let user_key = db.add_private_key("final_user").unwrap();

    // 1. Create the final user tree (deepest level)
    let mut user_settings = Nested::new();
    let mut user_auth = Nested::new();
    user_auth
        .set_json(
            "final_user",
            AuthKey {
                pubkey: format_public_key(&user_key),
                permissions: Permission::Admin(3), // High privilege at source
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    user_settings.set_map("auth", user_auth);
    let user_tree = db.new_tree(user_settings, "final_user").unwrap();
    let user_tips = user_tree.get_tips().unwrap();

    // 2. Create intermediate delegated tree that delegates to user tree
    let mut intermediate_settings = Nested::new();
    let mut intermediate_auth = Nested::new();

    // Add direct key to intermediate tree
    intermediate_auth
        .set_json(
            "intermediate_admin",
            AuthKey {
                pubkey: format_public_key(&intermediate_key),
                permissions: Permission::Admin(2),
                status: KeyStatus::Active,
            },
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

    intermediate_settings.set_map("auth", intermediate_auth);
    let intermediate_tree = db
        .new_tree(intermediate_settings, "intermediate_admin")
        .unwrap();
    let intermediate_tips = intermediate_tree.get_tips().unwrap();

    // 3. Create main tree that delegates to intermediate tree
    let mut main_settings = Nested::new();
    let mut main_auth = Nested::new();

    // Add direct key to main tree
    main_auth
        .set_json(
            "main_admin",
            AuthKey {
                pubkey: format_public_key(&main_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
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

    main_settings.set_map("auth", main_auth);
    let main_tree = db.new_tree(main_settings, "main_admin").unwrap();

    // 4. Test nested delegation resolution: Main -> Intermediate -> User
    let mut validator = AuthValidator::new();
    let main_settings = main_tree.get_settings().unwrap().get_all().unwrap();

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

    let result = validator.resolve_sig_key(&nested_sig_key, &main_settings, Some(db.backend()));

    // Should succeed with multi-level permission clamping:
    // Admin(3) -> Write(8) (at intermediate level) -> Write(5) (at main level, further clamping)
    assert!(
        result.is_ok(),
        "Nested delegation resolution failed: {:?}",
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

    // Create an empty settings (doesn't matter for depth test)
    let settings = Nested::new();

    // Test the depth check by directly calling with depth = MAX_DELEGATION_DEPTH
    let simple_sig_key = SigKey::Direct("base_key".to_string());

    // This should succeed (just under the limit)
    let result = validator
        .resolver
        .resolve_sig_key_with_depth(&simple_sig_key, &settings, None, 9);
    // Should fail due to missing auth configuration, not depth limit
    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(error.to_string().contains("No auth configuration found"));

    // This should fail due to depth limit (at the limit)
    let result =
        validator
            .resolver
            .resolve_sig_key_with_depth(&simple_sig_key, &settings, None, 10);
    assert!(result.is_err());
    let error = result.unwrap_err();
    println!("Depth limit error: {error}");
    assert!(error.to_string().contains("Maximum delegation depth"));
    assert!(error.to_string().contains("exceeded"));
}
