use eidetica::{
    auth::{
        crypto::{format_public_key, verify_entry_signature},
        types::{AuthKey, KeyStatus, Permission},
    },
    crdt::{Doc, doc::Value},
    store::DocStore,
};

use super::helpers::{
    assert_operation_permissions, setup_authenticated_tree, setup_db as auth_setup_db,
    setup_test_db_with_keys, test_operation_fails, test_operation_succeeds,
};
use crate::create_auth_keys;

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
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");
    let result = op.commit();
    assert!(result.is_err());
    let error_msg = format!("{:?}", result.unwrap_err());
    // Check for the new structured error format
    assert!(error_msg.contains("Transaction(SignatureVerificationFailed)"));
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
        .get_store::<DocStore>("_settings")
        .expect("Failed to get settings subtree");
    store
        .set("forbidden_setting", "value")
        .expect("Failed to set setting");
    let result = op.commit();
    assert!(result.is_err());
    let error_msg = format!("{:?}", result.unwrap_err());
    // Check for the new structured error format
    assert!(error_msg.contains("Transaction(InsufficientPermissions)"));
}

#[test]
fn test_multiple_authenticated_entries() {
    let db = auth_setup_db();

    // Generate a key for testing
    let public_key = db.add_private_key("TEST_KEY").expect("Failed to add key");

    // Create a tree with authentication enabled
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();

    let auth_key = AuthKey {
        pubkey: format_public_key(&public_key),
        permissions: Permission::Admin(0), // Admin needed to create tree with auth
        status: KeyStatus::Active,
    };
    auth_settings.set_json("TEST_KEY", auth_key).unwrap();
    settings.set_node("auth", auth_settings);

    let mut tree = db
        .new_database(settings, "TEST_KEY")
        .expect("Failed to create tree");

    // Clear default auth to test unsigned operation (should fail)
    tree.clear_default_auth_key();
    let op1 = tree.new_transaction().expect("Failed to create operation");
    let store1 = op1
        .get_store::<DocStore>("data")
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
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store2.set("signed", "value").expect("Failed to set value");
    let entry_id2 = op2.commit().expect("Failed to commit signed");

    // Verify the signed entry was stored correctly
    let entry2 = tree.get_entry(&entry_id2).expect("Failed to get entry2");
    assert!(entry2.sig.is_signed_by("TEST_KEY"));
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
    let mut entry = eidetica::Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    entry.sig = eidetica::auth::types::SigInfo::builder()
        .key(eidetica::auth::types::SigKey::Direct(
            "TEST_KEY".to_string(),
        ))
        .build();
    let signature = eidetica::auth::crypto::sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Test with no auth section at all
    let empty_settings = Doc::new();
    let result = validator.validate_entry(&entry, &empty_settings, None);
    assert!(
        result.is_ok(),
        "Should allow unsigned when no auth configured"
    );

    // Test with settings containing non-map auth section
    let mut corrupted_settings = Doc::new();
    corrupted_settings.set("auth", "invalid_string_value");

    let result = validator.validate_entry(&entry, &corrupted_settings, None);
    assert!(result.is_err(), "Should fail with corrupted auth section");

    // Test with settings containing deleted auth section
    let mut deleted_settings = Doc::new();
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

    // Test active key should work, revoked key should fail using assert_operation_permissions
    assert_operation_permissions(
        &tree,
        "ACTIVE_KEY",
        "data",
        true,
        "Active key should succeed",
    );
    assert_operation_permissions(
        &tree,
        "REVOKED_KEY",
        "data",
        false,
        "Revoked key should fail",
    );
}

#[test]
fn test_entry_validation_cache_behavior() {
    let mut validator = eidetica::auth::validation::AuthValidator::new();
    let (signing_key, verifying_key) = eidetica::auth::crypto::generate_keypair();

    let auth_key = eidetica::auth::types::AuthKey {
        pubkey: eidetica::auth::crypto::format_public_key(&verifying_key),
        permissions: eidetica::auth::types::Permission::Write(10),
        status: eidetica::auth::types::KeyStatus::Active,
    };

    // Create settings with the key
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json("TEST_KEY", auth_key.clone())
        .unwrap();
    settings.set_node("auth", auth_settings);

    // Create a signed entry
    let mut entry = eidetica::Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    entry.sig = eidetica::auth::types::SigInfo::builder()
        .key(eidetica::auth::types::SigKey::Direct(
            "TEST_KEY".to_string(),
        ))
        .build();
    let signature = eidetica::auth::crypto::sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validate the entry - should work
    let result1 = validator.validate_entry(&entry, &settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "First validation should succeed"
    );

    // Modify the key to be revoked
    let mut revoked_auth_key = auth_key.clone();
    revoked_auth_key.status = eidetica::auth::types::KeyStatus::Revoked;

    let mut new_settings = Doc::new();
    let mut new_auth_settings = Doc::new();
    new_auth_settings
        .set_json("TEST_KEY", revoked_auth_key)
        .unwrap();
    new_settings.set_node("auth", new_auth_settings);

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
        pubkey: eidetica::auth::crypto::format_public_key(&verifying_key),
        permissions: eidetica::auth::types::Permission::Write(10),
        status: eidetica::auth::types::KeyStatus::Active,
    };

    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json("TEST_KEY", auth_key.clone())
        .unwrap();
    settings.set_node("auth", auth_settings);

    // Create entry signed with correct key
    let mut correct_entry = eidetica::Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    correct_entry.sig = eidetica::auth::types::SigInfo::builder()
        .key(eidetica::auth::types::SigKey::Direct(
            "TEST_KEY".to_string(),
        ))
        .build();
    let correct_signature =
        eidetica::auth::crypto::sign_entry(&correct_entry, &signing_key).unwrap();
    correct_entry.sig.sig = Some(correct_signature);

    // Should validate successfully with correct settings
    let result1 = validator.validate_entry(&correct_entry, &settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Correctly signed entry should validate"
    );

    // Now test with malformed key in settings
    let malformed_auth_key = eidetica::auth::types::AuthKey {
        pubkey: "ed25519:invalid_base64_data!@#$".to_string(),
        permissions: eidetica::auth::types::Permission::Write(10),
        status: eidetica::auth::types::KeyStatus::Active,
    };

    let mut malformed_settings = Doc::new();
    let mut malformed_auth_settings = Doc::new();
    malformed_auth_settings
        .set_json("TEST_KEY", malformed_auth_key.clone())
        .unwrap();
    malformed_settings.set_node("auth", malformed_auth_settings);

    // Entry with same ID but malformed key in settings should fail
    let result_malformed = validator.validate_entry(&correct_entry, &malformed_settings, None);
    assert!(
        result_malformed.is_err() || (result_malformed.is_ok() && !result_malformed.unwrap()),
        "Entry should fail validation with malformed key settings"
    );

    // Create entry with corrupted signature
    let mut corrupted_entry = correct_entry.clone();
    corrupted_entry.sig.sig = Some("invalid_base64_signature!@#".to_string());

    let result2 = validator.validate_entry(&corrupted_entry, &settings, None);
    // The validation might return an error for invalid base64, or false for invalid signature
    // Let's check both cases
    if let Ok(valid) = result2 {
        assert!(!valid, "Entry with corrupted signature should not validate")
    } // This is also acceptable - invalid format should error

    // Test signature created with wrong key
    let (wrong_signing_key, _wrong_verifying_key) = eidetica::auth::crypto::generate_keypair();

    let mut wrong_signature_entry = eidetica::Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    wrong_signature_entry.sig = eidetica::auth::types::SigInfo::builder()
        .key(eidetica::auth::types::SigKey::Direct(
            "TEST_KEY".to_string(),
        ))
        .build();

    // Sign with wrong key but try to validate against correct key
    let wrong_signature =
        eidetica::auth::crypto::sign_entry(&wrong_signature_entry, &wrong_signing_key).unwrap();
    wrong_signature_entry.sig.sig = Some(wrong_signature);

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
    let entry = eidetica::Entry::root_builder()
        .build()
        .expect("Entry should build successfully");

    // Test with no auth settings
    let empty_settings = Doc::new();
    let result1 = validator.validate_entry(&entry, &empty_settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Unsigned entry should be valid when no auth configured"
    );

    // Test with auth settings present
    let mut settings = Doc::new();
    let auth_settings = Doc::new();
    settings.set_node("auth", auth_settings);

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
        pubkey: eidetica::auth::crypto::format_public_key(&verifying_key),
        permissions: eidetica::auth::types::Permission::Write(10),
        status: eidetica::auth::types::KeyStatus::Active,
    };

    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings.set_json("TEST_KEY", auth_key).unwrap();
    settings.set_node("auth", auth_settings);

    // Create entry signed with correct key
    let mut correct_entry = eidetica::Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    correct_entry.sig = eidetica::auth::types::SigInfo::builder()
        .key(eidetica::auth::types::SigKey::Direct(
            "TEST_KEY".to_string(),
        ))
        .build();
    let correct_signature =
        eidetica::auth::crypto::sign_entry(&correct_entry, &signing_key).unwrap();
    correct_entry.sig.sig = Some(correct_signature);

    // Should validate successfully
    let result1 = validator.validate_entry(&correct_entry, &settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Correctly signed entry should validate"
    );

    // Create entry with corrupted signature
    let mut corrupted_entry = correct_entry.clone();
    corrupted_entry.sig.sig = Some("invalid_base64_signature!@#".to_string());

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
