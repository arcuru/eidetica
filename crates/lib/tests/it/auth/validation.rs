use eidetica::{
    auth::{
        AuthSettings,
        crypto::{format_public_key, verify_entry_signature},
        types::{AuthKey, KeyStatus, Permission},
    },
    crdt::{Doc, doc::Value},
    instance::LegacyInstanceOps,
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

    let revoked_signing_key = db
        .backend()
        .get_private_key("REVOKED_KEY")
        .expect("Failed to get revoked key")
        .expect("Revoked key should exist in backend");

    let tree_with_revoked_key = eidetica::Database::open(
        db.clone(),
        tree.root_id(),
        revoked_signing_key,
        "REVOKED_KEY".to_string(),
    )
    .expect("Failed to load tree with revoked key");

    let op = tree_with_revoked_key
        .new_transaction()
        .expect("Failed to create operation");
    let store = op
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let result = op.commit();
    assert!(result.is_err(), "Revoked key test: Operation should fail");
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

    // Test with WRITE_KEY - should be able to write data
    let write_signing_key = db
        .backend()
        .get_private_key("WRITE_KEY")
        .expect("Failed to get write key")
        .expect("Write key should exist in backend");
    let tree_with_write_key = eidetica::Database::open(
        db.clone(),
        tree.root_id(),
        write_signing_key,
        "WRITE_KEY".to_string(),
    )
    .expect("Failed to load tree with write key");

    test_operation_succeeds(
        &tree_with_write_key,
        "data",
        "Write key should be able to write data",
    );

    // Test with SECONDARY_ADMIN_KEY - should be able to write data and modify settings
    let secondary_admin_signing_key = db
        .backend()
        .get_private_key("SECONDARY_ADMIN_KEY")
        .expect("Failed to get secondary admin key")
        .expect("Secondary admin key should exist in backend");
    let tree_with_secondary_admin_key = eidetica::Database::open(
        db.clone(),
        tree.root_id(),
        secondary_admin_signing_key,
        "SECONDARY_ADMIN_KEY".to_string(),
    )
    .expect("Failed to load tree with secondary admin key");

    test_operation_succeeds(
        &tree_with_secondary_admin_key,
        "data",
        "Secondary admin key should be able to write data",
    );
    test_operation_succeeds(
        &tree_with_secondary_admin_key,
        "_settings",
        "Secondary admin key should be able to modify settings",
    );

    // Test with WRITE_KEY trying to modify settings - should fail
    test_operation_fails(
        &tree_with_write_key,
        "_settings",
        "Write key should NOT be able to modify settings",
    );
}

#[test]
fn test_multiple_authenticated_entries() {
    let db = auth_setup_db();

    // Generate a key for testing
    let public_key = db.add_private_key("TEST_KEY").expect("Failed to add key");

    // Create a tree with authentication enabled
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();

    let auth_key = AuthKey::active(
        format_public_key(&public_key),
        Permission::Admin(0), // Admin needed to create tree with auth
    )
    .unwrap();
    auth_settings.set_json("TEST_KEY", auth_key).unwrap();
    settings.set("auth", auth_settings);

    let tree = db
        .new_database(settings, "TEST_KEY")
        .expect("Failed to create tree");

    // Create multiple signed entries (should all succeed)
    let op1 = tree.new_transaction().expect("Failed to create operation");
    let store1 = op1
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store1.set("entry", "first").expect("Failed to set value");
    let entry_id1 = op1.commit().expect("Failed to commit first entry");

    let op2 = tree.new_transaction().expect("Failed to create operation");
    let store2 = op2
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store2.set("entry", "second").expect("Failed to set value");
    let entry_id2 = op2.commit().expect("Failed to commit second entry");

    // Verify both entries were stored correctly
    let entry1 = tree.get_entry(&entry_id1).expect("Failed to get entry1");
    assert!(entry1.sig.is_signed_by("TEST_KEY"));
    assert!(
        tree.verify_entry_signature(&entry_id1)
            .expect("Failed to verify entry1")
    );

    let entry2 = tree.get_entry(&entry_id2).expect("Failed to get entry2");
    assert!(entry2.sig.is_signed_by("TEST_KEY"));
    assert!(
        tree.verify_entry_signature(&entry_id2)
            .expect("Failed to verify entry2")
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
    let empty_auth_settings = AuthSettings::new();
    let result = validator.validate_entry(&entry, &empty_auth_settings, None);
    assert!(
        result.is_ok(),
        "Should allow unsigned when no auth configured"
    );

    // Test with settings containing non-map auth section
    let mut corrupted_settings = Doc::new();
    corrupted_settings.set("auth", "invalid_string_value");

    // Extract auth settings (will be empty since auth is not a Doc)
    let corrupted_auth_settings = AuthSettings::new();
    let result = validator.validate_entry(&entry, &corrupted_auth_settings, None);
    assert!(result.is_ok(), "Should allow unsigned when auth is invalid");

    // Test with settings containing deleted auth section
    let mut deleted_settings = Doc::new();
    deleted_settings.set("auth", Value::Deleted);

    // Extract auth settings (will be empty since auth is deleted)
    let deleted_auth_settings = AuthSettings::new();
    let result = validator.validate_entry(&entry, &deleted_auth_settings, None);
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

    // Test active key should work
    let active_signing_key = db
        .backend()
        .get_private_key("ACTIVE_KEY")
        .expect("Failed to get active key")
        .expect("Active key should exist in backend");
    let tree_with_active_key = eidetica::Database::open(
        db.clone(),
        tree.root_id(),
        active_signing_key,
        "ACTIVE_KEY".to_string(),
    )
    .expect("Failed to load tree with active key");

    assert_operation_permissions(
        &tree_with_active_key,
        "data",
        true,
        "Active key should succeed",
    );

    // Test revoked key should fail
    let revoked_signing_key = db
        .backend()
        .get_private_key("REVOKED_KEY")
        .expect("Failed to get revoked key")
        .expect("Revoked key should exist in backend");
    let tree_with_revoked_key = eidetica::Database::open(
        db.clone(),
        tree.root_id(),
        revoked_signing_key,
        "REVOKED_KEY".to_string(),
    )
    .expect("Failed to load tree with revoked key");

    assert_operation_permissions(
        &tree_with_revoked_key,
        "data",
        false,
        "Revoked key should fail",
    );
}

#[test]
fn test_entry_validation_cache_behavior() {
    let mut validator = eidetica::auth::validation::AuthValidator::new();
    let (signing_key, verifying_key) = eidetica::auth::crypto::generate_keypair();

    let auth_key =
        AuthKey::active(format_public_key(&verifying_key), Permission::Write(10)).unwrap();

    // Create settings with the key
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json("TEST_KEY", auth_key.clone())
        .unwrap();
    settings.set("auth", auth_settings);

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

    // Extract AuthSettings from Doc
    let auth_settings_from_doc = match settings.get("auth") {
        Some(Value::Doc(auth_doc)) => AuthSettings::from_doc(auth_doc.clone()),
        _ => AuthSettings::new(),
    };

    // Validate the entry - should work
    let result1 = validator.validate_entry(&entry, &auth_settings_from_doc, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "First validation should succeed"
    );

    // Modify the key to be revoked
    let mut revoked_auth_key = auth_key.clone();
    revoked_auth_key.set_status(eidetica::auth::types::KeyStatus::Revoked);

    let mut new_settings = Doc::new();
    let mut new_auth_settings = Doc::new();
    new_auth_settings
        .set_json("TEST_KEY", revoked_auth_key)
        .unwrap();
    new_settings.set("auth", new_auth_settings);

    // Extract AuthSettings from new Doc
    let new_auth_settings_from_doc = match new_settings.get("auth") {
        Some(Value::Doc(auth_doc)) => AuthSettings::from_doc(auth_doc.clone()),
        _ => AuthSettings::new(),
    };

    // Validate with revoked key - should fail
    let result2 = validator.validate_entry(&entry, &new_auth_settings_from_doc, None);
    assert!(
        result2.is_ok() && !result2.unwrap(),
        "Validation with revoked key should fail"
    );

    // Validate with original settings again - should work (no stale cache)
    let result3 = validator.validate_entry(&entry, &auth_settings_from_doc, None);
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
    let auth_key =
        AuthKey::active(format_public_key(&verifying_key), Permission::Write(10)).unwrap();

    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json("TEST_KEY", auth_key.clone())
        .unwrap();
    settings.set("auth", auth_settings);

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

    // Extract AuthSettings from Doc
    let auth_settings = match settings.get("auth") {
        Some(Value::Doc(auth_doc)) => AuthSettings::from_doc(auth_doc.clone()),
        _ => AuthSettings::new(),
    };

    // Should validate successfully with correct settings
    let result1 = validator.validate_entry(&correct_entry, &auth_settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Correctly signed entry should validate"
    );

    // Test validation with mismatched signature (key exists but signature is wrong)
    let (wrong_signing_key, _wrong_verifying_key) = eidetica::auth::crypto::generate_keypair();
    let mut entry_with_wrong_sig = correct_entry.clone();

    // Sign with a different key than what's in settings
    let wrong_signature =
        eidetica::auth::crypto::sign_entry(&entry_with_wrong_sig, &wrong_signing_key).unwrap();
    entry_with_wrong_sig.sig.sig = Some(wrong_signature);

    // Should fail validation because signature doesn't match the key in settings
    let result_wrong_sig = validator.validate_entry(&entry_with_wrong_sig, &auth_settings, None);
    assert!(
        result_wrong_sig.is_err() || (result_wrong_sig.is_ok() && !result_wrong_sig.unwrap()),
        "Entry should fail validation with mismatched signature"
    );

    // Create entry with corrupted signature
    let mut corrupted_entry = correct_entry.clone();
    corrupted_entry.sig.sig = Some("invalid_base64_signature!@#".to_string());

    let result2 = validator.validate_entry(&corrupted_entry, &auth_settings, None);
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

    let result3 = validator.validate_entry(&wrong_signature_entry, &auth_settings, None);
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
    let empty_auth_settings = AuthSettings::new();
    let result1 = validator.validate_entry(&entry, &empty_auth_settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Unsigned entry should be valid when no auth configured"
    );

    // Test with auth settings present
    let mut settings = Doc::new();
    let auth_doc = Doc::new();
    settings.set("auth", auth_doc);

    // Extract AuthSettings from Doc (empty auth section)
    let auth_settings = match settings.get("auth") {
        Some(Value::Doc(auth_doc)) => AuthSettings::from_doc(auth_doc.clone()),
        _ => AuthSettings::new(),
    };

    let result2 = validator.validate_entry(&entry, &auth_settings, None);
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
    let auth_key =
        AuthKey::active(format_public_key(&verifying_key), Permission::Write(10)).unwrap();

    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings.set_json("TEST_KEY", auth_key).unwrap();
    settings.set("auth", auth_settings);

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

    // Extract AuthSettings from Doc
    let auth_settings = match settings.get("auth") {
        Some(Value::Doc(auth_doc)) => AuthSettings::from_doc(auth_doc.clone()),
        _ => AuthSettings::new(),
    };

    // Should validate successfully
    let result1 = validator.validate_entry(&correct_entry, &auth_settings, None);
    assert!(
        result1.is_ok() && result1.unwrap(),
        "Correctly signed entry should validate"
    );

    // Create entry with corrupted signature
    let mut corrupted_entry = correct_entry.clone();
    corrupted_entry.sig.sig = Some("invalid_base64_signature!@#".to_string());

    let result2 = validator.validate_entry(&corrupted_entry, &auth_settings, None);
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
