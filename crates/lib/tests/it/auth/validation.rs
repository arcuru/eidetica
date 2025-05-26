use super::helpers::*;
use crate::create_auth_keys;
use crate::helpers::*;
use eidetica::auth::crypto::{format_public_key, verify_entry_signature};
use eidetica::auth::types::{AuthKey, KeyStatus, Permission};
use eidetica::crdt::{Nested, Value};
use eidetica::subtree::KVStore;

#[test]
fn test_backend_authentication_validation() {
    let keys = create_auth_keys![
        ("ADMIN_KEY", Permission::Admin(0), KeyStatus::Active),
        ("TEST_KEY", Permission::Write(10), KeyStatus::Active)
    ];
    let (db, public_keys) = setup_test_db_with_keys(&keys);
    let tree = setup_authenticated_tree(&db, &keys, &public_keys);

    // This should succeed because the key is configured in auth settings
    let op = tree
        .new_authenticated_operation("TEST_KEY")
        .expect("Failed to create authenticated operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");
    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry was stored and signed
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert!(entry.auth.is_signed_by("TEST_KEY"));
}

#[test]
fn test_authentication_validation_revoked_key() {
    let keys = create_auth_keys![
        ("ADMIN_KEY", Permission::Admin(0), KeyStatus::Active),
        ("REVOKED_KEY", Permission::Write(10), KeyStatus::Revoked)
    ];
    let (db, public_keys) = setup_test_db_with_keys(&keys);
    let tree = setup_authenticated_tree(&db, &keys, &public_keys);

    // This should fail because the key is revoked
    test_operation_fails(&tree, "REVOKED_KEY", "data", "Revoked key test");

    // Additional check that the error mentions authentication validation failure
    let op = tree
        .new_authenticated_operation("REVOKED_KEY")
        .expect("Failed to create authenticated operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");
    let result = op.commit();
    assert!(result.is_err());
    let error_msg = format!("{:?}", result.unwrap_err());
    assert!(error_msg.contains("authentication validation failed"));
}

#[test]
fn test_permission_checking_admin_operations() {
    let keys = create_auth_keys![
        ("ADMIN_KEY", Permission::Admin(0), KeyStatus::Active),
        ("WRITE_KEY", Permission::Write(10), KeyStatus::Active),
        (
            "SECONDARY_ADMIN_KEY",
            Permission::Admin(1),
            KeyStatus::Active
        )
    ];
    let (db, public_keys) = setup_test_db_with_keys(&keys);
    let tree = setup_authenticated_tree(&db, &keys, &public_keys);

    // Test permission operations using helpers
    test_operation_succeeds(
        &tree,
        "WRITE_KEY",
        "data",
        "Write key should be able to write data",
    );
    test_operation_succeeds(
        &tree,
        "SECONDARY_ADMIN_KEY",
        "data",
        "Secondary admin key should be able to write data",
    );
    test_operation_succeeds(
        &tree,
        "SECONDARY_ADMIN_KEY",
        "_settings",
        "Secondary admin key should be able to modify settings",
    );
    test_operation_fails(
        &tree,
        "WRITE_KEY",
        "_settings",
        "Write key should NOT be able to modify settings",
    );

    // Additional check for specific error message
    let op = tree
        .new_authenticated_operation("WRITE_KEY")
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("_settings")
        .expect("Failed to get settings subtree");
    store
        .set("forbidden_setting", "value")
        .expect("Failed to set setting");
    let result = op.commit();
    assert!(result.is_err());
    let error_msg = format!("{:?}", result.unwrap_err());
    assert!(error_msg.contains("authentication validation failed"));
}

#[test]
fn test_mandatory_authentication_enforcement() {
    let db = setup_db();

    // Add test key and create tree
    db.add_private_key("TEST_KEY")
        .expect("Failed to add test key");
    let mut settings = Nested::new();
    let auth_settings = Nested::new();
    settings.set_map("auth", auth_settings);

    let mut tree = db
        .new_tree(settings, "TEST_KEY")
        .expect("Failed to create tree");

    // Test 1: Normal operation with default auth should succeed
    let op1 = tree.new_operation().expect("Failed to create operation");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store1.set("test", "value").expect("Failed to set value");
    let result1 = op1.commit();
    assert!(
        result1.is_ok(),
        "Operation with default auth should succeed"
    );

    // Test 2: Clear default auth and try again - should fail
    tree.clear_default_auth_key();
    let op2 = tree.new_operation().expect("Failed to create operation");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store2.set("test2", "value2").expect("Failed to set value");
    let result2 = op2.commit();
    assert!(result2.is_err(), "Operation without auth should fail");
}

#[test]
fn test_multiple_authenticated_entries() {
    let db = setup_db();

    // Generate a key for testing
    let public_key = db.add_private_key("TEST_KEY").expect("Failed to add key");

    // Create a tree with authentication enabled
    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();

    let auth_key = AuthKey {
        key: format_public_key(&public_key),
        permissions: Permission::Admin(0), // Admin needed to create tree with auth
        status: KeyStatus::Active,
    };
    auth_settings.set("TEST_KEY".to_string(), auth_key);
    settings.set_map("auth", auth_settings);

    let mut tree = db
        .new_tree(settings, "TEST_KEY")
        .expect("Failed to create tree");

    // Clear default auth to test unsigned operation (should fail)
    tree.clear_default_auth_key();
    let op1 = tree.new_operation().expect("Failed to create operation");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store1
        .set("unsigned", "value")
        .expect("Failed to set value");
    let result1 = op1.commit();
    assert!(result1.is_err(), "Unsigned operation should fail");

    // Create a signed entry (should succeed)
    let op2 = tree
        .new_authenticated_operation("TEST_KEY")
        .expect("Failed to create authenticated operation");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store2.set("signed", "value").expect("Failed to set value");
    let entry_id2 = op2.commit().expect("Failed to commit signed");

    // Verify the signed entry was stored correctly
    let entry2 = tree.get_entry(&entry_id2).expect("Failed to get entry2");
    assert!(entry2.auth.is_signed_by("TEST_KEY"));
    assert!(
        tree.verify_entry_signature(&entry_id2)
            .expect("Failed to verify")
    );
}

#[test]
fn test_entry_validation_with_corrupted_auth_section() {
    use eidetica::auth::validation::AuthValidator;

    let mut validator = AuthValidator::new();
    let (signing_key, _verifying_key) = eidetica::auth::crypto::generate_keypair();

    // Create a signed entry
    let mut entry = eidetica::entry::Entry::builder("root123".to_string()).build();
    entry.auth = eidetica::auth::types::AuthInfo {
        id: eidetica::auth::types::AuthId::Direct("TEST_KEY".to_string()),
        signature: None,
    };
    let signature = eidetica::auth::crypto::sign_entry(&entry, &signing_key).unwrap();
    entry.auth.signature = Some(signature);

    // Test with no auth section at all
    let empty_settings = Nested::new();
    let result = validator.validate_entry(&entry, &empty_settings, None);
    assert!(
        result.is_ok(),
        "Should allow unsigned when no auth configured"
    );

    // Test with settings containing non-map auth section
    let mut corrupted_settings = Nested::new();
    corrupted_settings.set("auth", "invalid_string_value".to_string());

    let result = validator.validate_entry(&entry, &corrupted_settings, None);
    assert!(result.is_err(), "Should fail with corrupted auth section");

    // Test with settings containing deleted auth section
    let mut deleted_settings = Nested::new();
    deleted_settings.set("auth", Value::Deleted);

    let result = validator.validate_entry(&entry, &deleted_settings, None);
    assert!(result.is_ok(), "Should allow unsigned when auth is deleted");
}

#[test]
fn test_entry_validation_with_mixed_key_states() {
    let keys = create_auth_keys![
        ("ADMIN_KEY", Permission::Admin(0), KeyStatus::Active),
        ("ACTIVE_KEY", Permission::Write(10), KeyStatus::Active),
        ("REVOKED_KEY", Permission::Write(20), KeyStatus::Revoked),
    ];

    let (db, public_keys) = setup_test_db_with_keys(&keys);
    let tree = setup_authenticated_tree(&db, &keys, &public_keys);

    // Test active key should work, revoked key should fail
    test_operation_succeeds(&tree, "ACTIVE_KEY", "data", "Active key test");
    test_operation_fails(&tree, "REVOKED_KEY", "data", "Revoked key test");
}

#[test]
fn test_entry_validation_cache_behavior() {
    let mut validator = eidetica::auth::validation::AuthValidator::new();
    let (signing_key, verifying_key) = eidetica::auth::crypto::generate_keypair();

    let auth_key = eidetica::auth::types::AuthKey {
        key: eidetica::auth::crypto::format_public_key(&verifying_key),
        permissions: eidetica::auth::types::Permission::Write(10),
        status: eidetica::auth::types::KeyStatus::Active,
    };

    // Create settings with the key
    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();
    auth_settings.set("TEST_KEY".to_string(), auth_key.clone());
    settings.set_map("auth", auth_settings);

    // Create a signed entry
    let mut entry = eidetica::entry::Entry::builder("root123".to_string()).build();
    entry.auth = eidetica::auth::types::AuthInfo {
        id: eidetica::auth::types::AuthId::Direct("TEST_KEY".to_string()),
        signature: None,
    };
    let signature = eidetica::auth::crypto::sign_entry(&entry, &signing_key).unwrap();
    entry.auth.signature = Some(signature);

    // Validate the entry - should work
    let result1 = validator.validate_entry(&entry, &settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "First validation should succeed"
    );

    // Modify the key to be revoked
    let mut revoked_auth_key = auth_key.clone();
    revoked_auth_key.status = eidetica::auth::types::KeyStatus::Revoked;

    let mut new_settings = Nested::new();
    let mut new_auth_settings = Nested::new();
    new_auth_settings.set("TEST_KEY".to_string(), revoked_auth_key);
    new_settings.set_map("auth", new_auth_settings);

    // Validate with revoked key - should fail
    let result2 = validator.validate_entry(&entry, &new_settings, None);
    assert!(
        result2.is_ok() && !result2.unwrap(),
        "Validation with revoked key should fail"
    );

    // Validate with original settings again - should work (no stale cache)
    let result3 = validator.validate_entry(&entry, &settings, None);
    assert!(
        result3.is_ok() && result3.unwrap(),
        "Validation should work again with active key"
    );
}

#[test]
fn test_entry_validation_with_malformed_keys() {
    let mut validator = eidetica::auth::validation::AuthValidator::new();
    let (signing_key, verifying_key) = eidetica::auth::crypto::generate_keypair();

    // Create settings with a valid key for comparison
    let auth_key = eidetica::auth::types::AuthKey {
        key: eidetica::auth::crypto::format_public_key(&verifying_key),
        permissions: eidetica::auth::types::Permission::Write(10),
        status: eidetica::auth::types::KeyStatus::Active,
    };

    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();
    auth_settings.set("TEST_KEY".to_string(), auth_key.clone());
    settings.set_map("auth", auth_settings);

    // Create entry signed with correct key
    let mut correct_entry = eidetica::entry::Entry::builder("root123".to_string()).build();
    correct_entry.auth = eidetica::auth::types::AuthInfo {
        id: eidetica::auth::types::AuthId::Direct("TEST_KEY".to_string()),
        signature: None,
    };
    let correct_signature =
        eidetica::auth::crypto::sign_entry(&correct_entry, &signing_key).unwrap();
    correct_entry.auth.signature = Some(correct_signature);

    // Should validate successfully with correct settings
    let result1 = validator.validate_entry(&correct_entry, &settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Correctly signed entry should validate"
    );

    // Now test with malformed key in settings
    let malformed_auth_key = eidetica::auth::types::AuthKey {
        key: "ed25519:invalid_base64_data!@#$".to_string(),
        permissions: eidetica::auth::types::Permission::Write(10),
        status: eidetica::auth::types::KeyStatus::Active,
    };

    let mut malformed_settings = Nested::new();
    let mut malformed_auth_settings = Nested::new();
    malformed_auth_settings.set("TEST_KEY".to_string(), malformed_auth_key.clone());
    malformed_settings.set_map("auth", malformed_auth_settings);

    // Entry with same ID but malformed key in settings should fail
    let result_malformed = validator.validate_entry(&correct_entry, &malformed_settings, None);
    assert!(
        result_malformed.is_err() || (result_malformed.is_ok() && !result_malformed.unwrap()),
        "Entry should fail validation with malformed key settings"
    );

    // Create entry with corrupted signature
    let mut corrupted_entry = correct_entry.clone();
    corrupted_entry.auth.signature = Some("invalid_base64_signature!@#".to_string());

    let result2 = validator.validate_entry(&corrupted_entry, &settings, None);
    // The validation might return an error for invalid base64, or false for invalid signature
    // Let's check both cases
    if let Ok(valid) = result2 {
        assert!(!valid, "Entry with corrupted signature should not validate")
    } // This is also acceptable - invalid format should error

    // Test signature created with wrong key
    let (wrong_signing_key, _wrong_verifying_key) = eidetica::auth::crypto::generate_keypair();

    let mut wrong_signature_entry = eidetica::entry::Entry::builder("root456".to_string()).build();
    wrong_signature_entry.auth = eidetica::auth::types::AuthInfo {
        id: eidetica::auth::types::AuthId::Direct("TEST_KEY".to_string()),
        signature: None,
    };

    // Sign with wrong key but try to validate against correct key
    let wrong_signature =
        eidetica::auth::crypto::sign_entry(&wrong_signature_entry, &wrong_signing_key).unwrap();
    wrong_signature_entry.auth.signature = Some(wrong_signature);

    let result3 = validator.validate_entry(&wrong_signature_entry, &settings, None);
    assert!(
        result3.is_ok() && !result3.unwrap(),
        "Entry with wrong key signature should fail validation"
    );
}

#[test]
fn test_entry_validation_unsigned_entry_detection() {
    let mut validator = eidetica::auth::validation::AuthValidator::new();

    // Create an unsigned entry
    let entry = eidetica::entry::Entry::builder("root123".to_string()).build();

    // Test with no auth settings
    let empty_settings = Nested::new();
    let result1 = validator.validate_entry(&entry, &empty_settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Unsigned entry should be valid when no auth configured"
    );

    // Test with auth settings present
    let mut settings = Nested::new();
    let auth_settings = Nested::new();
    settings.set_map("auth", auth_settings);

    let result2 = validator.validate_entry(&entry, &settings, None);
    assert!(
        result2.is_ok() && result2.unwrap(),
        "Unsigned entry should still be valid for backward compatibility"
    );
}

#[test]
fn test_entry_validation_with_invalid_signatures() {
    let mut validator = eidetica::auth::validation::AuthValidator::new();
    let (signing_key, verifying_key) = eidetica::auth::crypto::generate_keypair();
    let (_wrong_signing_key, wrong_verifying_key) = eidetica::auth::crypto::generate_keypair();

    // Create settings with the correct public key
    let auth_key = eidetica::auth::types::AuthKey {
        key: eidetica::auth::crypto::format_public_key(&verifying_key),
        permissions: eidetica::auth::types::Permission::Write(10),
        status: eidetica::auth::types::KeyStatus::Active,
    };

    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();
    auth_settings.set("TEST_KEY".to_string(), auth_key);
    settings.set_map("auth", auth_settings);

    // Create entry signed with correct key
    let mut correct_entry = eidetica::entry::Entry::builder("root123".to_string()).build();
    correct_entry.auth = eidetica::auth::types::AuthInfo {
        id: eidetica::auth::types::AuthId::Direct("TEST_KEY".to_string()),
        signature: None,
    };
    let correct_signature =
        eidetica::auth::crypto::sign_entry(&correct_entry, &signing_key).unwrap();
    correct_entry.auth.signature = Some(correct_signature);

    // Should validate successfully
    let result1 = validator.validate_entry(&correct_entry, &settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Correctly signed entry should validate"
    );

    // Create entry with corrupted signature
    let mut corrupted_entry = correct_entry.clone();
    corrupted_entry.auth.signature = Some("invalid_base64_signature!@#".to_string());

    let result2 = validator.validate_entry(&corrupted_entry, &settings, None);
    // The validation might return an error for invalid base64, or false for invalid signature
    // Let's check both cases
    if let Ok(valid) = result2 {
        assert!(!valid, "Entry with corrupted signature should not validate")
    } // This is also acceptable - invalid format should error

    // Test signature verification function directly
    assert!(verify_entry_signature(&correct_entry, &verifying_key).expect("Failed to verify"));
    assert!(
        !verify_entry_signature(&correct_entry, &wrong_verifying_key).expect("Failed to verify")
    );
}
