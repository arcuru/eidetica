//! Tests for authentication validation.

use eidetica::{
    Database, Entry,
    auth::{
        AuthSettings,
        crypto::{
            PrivateKey, format_public_key, generate_keypair, sign_entry, verify_entry_signature,
        },
        types::{AuthKey, DelegationStep, KeyHint, KeyStatus, Permission, SigInfo, SigKey},
        validation::AuthValidator,
    },
    crdt::{Doc, doc::Value},
    database::DatabaseKey,
    store::DocStore,
};

use super::helpers::{
    assert_operation_permissions, setup_complete_auth_environment_with_user, test_operation_fails,
    test_operation_succeeds,
};
use crate::create_auth_keys;
use crate::helpers::setup_db;

#[tokio::test]
async fn test_authentication_validation_revoked_key() {
    let keys = create_auth_keys![
        ("ADMIN_KEY", Permission::Admin(0), KeyStatus::Active),
        ("REVOKED_KEY", Permission::Write(10), KeyStatus::Revoked)
    ];
    let (instance, user, tree, key_ids) =
        setup_complete_auth_environment_with_user("test_user", &keys).await;

    // Get the revoked signing key (index 1 = REVOKED_KEY)
    let revoked_signing_key = user
        .get_signing_key(&key_ids[1])
        .expect("Failed to get revoked key")
        .clone();

    let tree_with_revoked_key = Database::open(
        instance.clone(),
        tree.root_id(),
        DatabaseKey::with_name(revoked_signing_key, "REVOKED_KEY"),
    )
    .await
    .expect("Failed to load tree with revoked key");

    let txn = tree_with_revoked_key
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let store = txn
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let result = txn.commit().await;
    assert!(result.is_err(), "Revoked key test: Operation should fail");
}

#[tokio::test]
async fn test_permission_checking_admin_operations() {
    let keys = create_auth_keys![
        ("ADMIN_KEY", Permission::Admin(0), KeyStatus::Active),
        ("WRITE_KEY", Permission::Write(10), KeyStatus::Active),
        (
            "SECONDARY_ADMIN_KEY",
            Permission::Admin(1),
            KeyStatus::Active
        )
    ];
    let (instance, user, tree, key_ids) =
        setup_complete_auth_environment_with_user("test_user", &keys).await;

    // Get signing keys using User API (index 1 = WRITE_KEY)
    let write_signing_key = user
        .get_signing_key(&key_ids[1])
        .expect("Failed to get write key")
        .clone();

    let tree_with_write_key = Database::open(
        instance.clone(),
        tree.root_id(),
        DatabaseKey::with_name(write_signing_key, "WRITE_KEY"),
    )
    .await
    .expect("Failed to load tree with write key");

    test_operation_succeeds(
        &tree_with_write_key,
        "data",
        "Write key should be able to write data",
    )
    .await;

    // Test with SECONDARY_ADMIN_KEY (index 2)
    let secondary_admin_signing_key = user
        .get_signing_key(&key_ids[2])
        .expect("Failed to get secondary admin key")
        .clone();

    let tree_with_secondary_admin_key = Database::open(
        instance.clone(),
        tree.root_id(),
        DatabaseKey::with_name(secondary_admin_signing_key, "SECONDARY_ADMIN_KEY"),
    )
    .await
    .expect("Failed to load tree with secondary admin key");

    test_operation_succeeds(
        &tree_with_secondary_admin_key,
        "data",
        "Secondary admin key should be able to write data",
    )
    .await;
    test_operation_succeeds(
        &tree_with_secondary_admin_key,
        "_settings",
        "Secondary admin key should be able to modify settings",
    )
    .await;

    // Test with WRITE_KEY trying to modify settings - should fail
    test_operation_fails(
        &tree_with_write_key,
        "_settings",
        "Write key should NOT be able to modify settings",
    )
    .await;
}

#[tokio::test]
async fn test_multiple_authenticated_entries() {
    let (_instance, mut user) = setup_db().await;

    // Generate a key for testing
    let key_id = user
        .add_private_key(Some("TEST_KEY"))
        .await
        .expect("Failed to add key");

    // Create a tree with authentication enabled (signing key becomes Admin(0))
    let tree = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");

    // Create multiple signed entries (should all succeed)
    let txn1 = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let store1 = txn1
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store1
        .set("entry", "first")
        .await
        .expect("Failed to set value");
    let entry_id1 = txn1.commit().await.expect("Failed to commit first entry");

    let txn2 = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let store2 = txn2
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store2
        .set("entry", "second")
        .await
        .expect("Failed to set value");
    let entry_id2 = txn2.commit().await.expect("Failed to commit second entry");

    // Verify both entries were stored correctly
    let entry1 = tree
        .get_entry(&entry_id1)
        .await
        .expect("Failed to get entry1");
    assert_eq!(entry1.sig.key, SigKey::from_pubkey(key_id.to_string()));
    assert!(
        tree.verify_entry_signature(&entry_id1)
            .await
            .expect("Failed to verify entry1")
    );

    let entry2 = tree
        .get_entry(&entry_id2)
        .await
        .expect("Failed to get entry2");
    assert_eq!(entry2.sig.key, SigKey::from_pubkey(key_id.to_string()));
    assert!(
        tree.verify_entry_signature(&entry_id2)
            .await
            .expect("Failed to verify entry2")
    );
}

#[tokio::test]
async fn test_entry_validation_with_corrupted_auth_section() {
    let mut validator = AuthValidator::new();
    let (signing_key, _verifying_key) = generate_keypair();

    // Create a signed entry
    let mut entry = Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    entry.sig = SigInfo::builder()
        .key(SigKey::from_name("TEST_KEY"))
        .build();
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Test with no auth section at all
    let empty_auth_settings = AuthSettings::new();
    let result = validator
        .validate_entry(&entry, &empty_auth_settings, None)
        .await;
    assert!(
        result.is_ok(),
        "Should allow unsigned when no auth configured"
    );

    // Test with settings containing non-map auth section
    let mut corrupted_settings = Doc::new();
    corrupted_settings.set("auth", "invalid_string_value");

    // Extract auth settings (will be empty since auth is not a Doc)
    let corrupted_auth_settings = AuthSettings::new();
    let result = validator
        .validate_entry(&entry, &corrupted_auth_settings, None)
        .await;
    assert!(result.is_ok(), "Should allow unsigned when auth is invalid");

    // Test with settings containing deleted auth section
    let mut deleted_settings = Doc::new();
    deleted_settings.set("auth", Value::Deleted);

    // Extract auth settings (will be empty since auth is deleted)
    let deleted_auth_settings = AuthSettings::new();
    let result = validator
        .validate_entry(&entry, &deleted_auth_settings, None)
        .await;
    assert!(result.is_ok(), "Should allow unsigned when auth is deleted");
}

#[tokio::test]
async fn test_entry_validation_with_mixed_key_states() {
    let keys = create_auth_keys![
        ("ADMIN_KEY", Permission::Admin(0), KeyStatus::Active),
        ("ACTIVE_KEY", Permission::Write(10), KeyStatus::Active),
        ("REVOKED_KEY", Permission::Write(20), KeyStatus::Revoked),
    ];

    let (instance, user, tree, key_ids) =
        setup_complete_auth_environment_with_user("test_user", &keys).await;

    // Get signing keys using User API (index 1 = ACTIVE_KEY)
    let active_signing_key = user
        .get_signing_key(&key_ids[1])
        .expect("Failed to get active key")
        .clone();

    let tree_with_active_key = Database::open(
        instance.clone(),
        tree.root_id(),
        DatabaseKey::with_name(active_signing_key, "ACTIVE_KEY"),
    )
    .await
    .expect("Failed to load tree with active key");

    assert_operation_permissions(
        &tree_with_active_key,
        "data",
        true,
        "Active key should succeed",
    )
    .await;

    // Test revoked key should fail (index 2 = REVOKED_KEY)
    let revoked_signing_key = user
        .get_signing_key(&key_ids[2])
        .expect("Failed to get revoked key")
        .clone();

    let tree_with_revoked_key = Database::open(
        instance.clone(),
        tree.root_id(),
        DatabaseKey::with_name(revoked_signing_key, "REVOKED_KEY"),
    )
    .await
    .expect("Failed to load tree with revoked key");

    assert_operation_permissions(
        &tree_with_revoked_key,
        "data",
        false,
        "Revoked key should fail",
    )
    .await;
}

#[tokio::test]
async fn test_entry_validation_cache_behavior() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();
    let pubkey_str = format_public_key(&verifying_key);

    let auth_key = AuthKey::active(Some("TEST_KEY"), Permission::Write(10));

    // Create auth settings with the key for validation testing
    let mut auth_settings = AuthSettings::new();
    auth_settings
        .add_key(&pubkey_str, auth_key.clone())
        .unwrap();

    // Create a signed entry
    let mut entry = Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    entry.sig = SigInfo::builder()
        .key(SigKey::from_pubkey(&pubkey_str))
        .build();
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Validate the entry - should work
    let result1 = validator.validate_entry(&entry, &auth_settings, None).await;
    assert!(result1.unwrap(), "First validation should succeed");

    // Modify the key to be revoked
    let mut revoked_auth_key = auth_key.clone();
    revoked_auth_key.set_status(KeyStatus::Revoked);

    let mut revoked_auth_settings = AuthSettings::new();
    revoked_auth_settings
        .add_key(&pubkey_str, revoked_auth_key)
        .unwrap();

    // Validate with revoked key - should fail (returns Ok(false) since no active key could verify)
    let result2 = validator
        .validate_entry(&entry, &revoked_auth_settings, None)
        .await;
    assert!(
        result2.is_ok() && !result2.unwrap(),
        "Validation with revoked key should fail"
    );

    // Validate with original settings again - should work (no stale cache)
    let result3 = validator.validate_entry(&entry, &auth_settings, None).await;
    assert!(
        result3.unwrap(),
        "Validation should work again with active key"
    );
}

#[tokio::test]
async fn test_entry_validation_with_malformed_keys() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();
    let pubkey_str = format_public_key(&verifying_key);

    // Create settings with a valid key for comparison
    let auth_key = AuthKey::active(Some("TEST_KEY"), Permission::Write(10));

    let mut auth_settings = AuthSettings::new();
    auth_settings
        .add_key(&pubkey_str, auth_key.clone())
        .unwrap();

    // Create entry signed with correct key
    let mut correct_entry = Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    correct_entry.sig = SigInfo::builder()
        .key(SigKey::from_pubkey(&pubkey_str))
        .build();
    let correct_signature = sign_entry(&correct_entry, &signing_key).unwrap();
    correct_entry.sig.sig = Some(correct_signature);

    // Should validate successfully with correct settings
    let result1 = validator
        .validate_entry(&correct_entry, &auth_settings, None)
        .await;
    assert!(result1.unwrap(), "Correctly signed entry should validate");

    // Test validation with mismatched signature (key exists but signature is wrong)
    let (wrong_signing_key, _wrong_verifying_key) = generate_keypair();
    let mut entry_with_wrong_sig = correct_entry.clone();

    // Sign with a different key than what's in settings
    let wrong_signature = sign_entry(&entry_with_wrong_sig, &wrong_signing_key).unwrap();
    entry_with_wrong_sig.sig.sig = Some(wrong_signature);

    // Should fail validation because signature doesn't match the key in settings
    let result_wrong_sig = validator
        .validate_entry(&entry_with_wrong_sig, &auth_settings, None)
        .await;
    assert!(
        result_wrong_sig.is_ok() && !result_wrong_sig.unwrap(),
        "Entry should fail validation with mismatched signature"
    );

    // Create entry with corrupted signature
    let mut corrupted_entry = correct_entry.clone();
    corrupted_entry.sig.sig = Some("invalid_base64_signature!@#".to_string());

    let result2 = validator
        .validate_entry(&corrupted_entry, &auth_settings, None)
        .await;
    // The validation should return Ok(false) for invalid base64 or invalid signature
    assert!(
        result2.is_ok() && !result2.unwrap(),
        "Entry with corrupted signature should not validate"
    );

    // Test signature created with wrong key
    let (wrong_signing_key, _wrong_verifying_key) = generate_keypair();

    let mut wrong_signature_entry = Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    wrong_signature_entry.sig = SigInfo::builder()
        .key(SigKey::from_pubkey(&pubkey_str))
        .build();

    // Sign with wrong key but try to validate against correct key
    let wrong_signature = sign_entry(&wrong_signature_entry, &wrong_signing_key).unwrap();
    wrong_signature_entry.sig.sig = Some(wrong_signature);

    let result3 = validator
        .validate_entry(&wrong_signature_entry, &auth_settings, None)
        .await;
    assert!(
        result3.is_ok() && !result3.unwrap(),
        "Entry with wrong key signature should fail validation"
    );
}

#[tokio::test]
async fn test_entry_validation_unsigned_entry_detection() {
    let mut validator = AuthValidator::new();

    // Create an unsigned entry
    let entry = Entry::root_builder()
        .build()
        .expect("Entry should build successfully");

    // Test with no auth settings - unsigned entry should be allowed
    let empty_auth_settings = AuthSettings::new();
    let result1 = validator
        .validate_entry(&entry, &empty_auth_settings, None)
        .await;
    assert!(
        result1.unwrap(),
        "Unsigned entry should be valid when no auth configured"
    );

    // Test with auth settings present (but empty auth section - still no keys)
    let mut settings = Doc::new();
    let auth_doc = Doc::new();
    settings.set("auth", auth_doc);

    // Extract AuthSettings from Doc (empty auth section = no keys configured)
    let auth_settings = match settings.get("auth") {
        Some(Value::Doc(auth_doc)) => AuthSettings::from(auth_doc.clone()),
        _ => AuthSettings::new(),
    };

    let result2 = validator.validate_entry(&entry, &auth_settings, None).await;
    assert!(
        result2.unwrap(),
        "Unsigned entry should be valid when no auth keys configured"
    );
}

#[tokio::test]
async fn test_entry_validation_with_invalid_signatures() {
    let mut validator = AuthValidator::new();
    let (signing_key, verifying_key) = generate_keypair();
    let wrong_pubkey = PrivateKey::generate().public_key();
    let pubkey_str = format_public_key(&verifying_key);

    // Create settings with the correct public key
    let auth_key = AuthKey::active(Some("TEST_KEY"), Permission::Write(10));

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(&pubkey_str, auth_key).unwrap();

    // Create entry signed with correct key
    let mut correct_entry = Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    correct_entry.sig = SigInfo::builder()
        .key(SigKey::from_pubkey(&pubkey_str))
        .build();
    let correct_signature = sign_entry(&correct_entry, &signing_key).unwrap();
    correct_entry.sig.sig = Some(correct_signature);

    // Should validate successfully
    let result1 = validator
        .validate_entry(&correct_entry, &auth_settings, None)
        .await;
    assert!(result1.unwrap(), "Correctly signed entry should validate");

    // Create entry with corrupted signature
    let mut corrupted_entry = correct_entry.clone();
    corrupted_entry.sig.sig = Some("invalid_base64_signature!@#".to_string());

    let result2 = validator
        .validate_entry(&corrupted_entry, &auth_settings, None)
        .await;
    // The validation should return Ok(false) for invalid base64 or invalid signature
    assert!(
        result2.is_ok() && !result2.unwrap(),
        "Entry with corrupted signature should not validate"
    );

    // Test signature verification function directly
    verify_entry_signature(&correct_entry, &verifying_key).expect("Valid signature should verify");
    assert!(
        verify_entry_signature(&correct_entry, &wrong_pubkey).is_err(),
        "Wrong key should fail verification"
    );
}

/// Test that tampering with the SigKey after signing invalidates the signature.
///
/// This is a critical security property: the SigKey (pubkey, name hints)
/// is included in the signed data, so any modification should cause verification to fail.
#[tokio::test]
async fn test_sigkey_tampering_invalidates_signature() {
    let (signing_key, verifying_key) = generate_keypair();
    let other_pubkey_str = PrivateKey::generate().public_key().to_prefixed_string();
    let pubkey_str = format_public_key(&verifying_key);

    // Create and sign an entry with a pubkey hint
    let mut entry = Entry::root_builder()
        .build()
        .expect("Entry should build successfully");
    entry.sig = SigInfo::builder()
        .key(SigKey::from_pubkey(&pubkey_str))
        .build();
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);

    // Original entry should verify with the correct key
    verify_entry_signature(&entry, &verifying_key)
        .expect("Original entry should verify successfully");

    // Tamper with pubkey hint - should fail verification
    let mut tampered_pubkey = entry.clone();
    tampered_pubkey.sig.key = SigKey::from_pubkey(&other_pubkey_str);
    assert!(
        verify_entry_signature(&tampered_pubkey, &verifying_key).is_err(),
        "Tampering with pubkey hint should invalidate signature"
    );

    // Tamper with name hint - should fail verification
    let mut tampered_name = entry.clone();
    tampered_name.sig.key = SigKey::from_name("tampered_name");
    assert!(
        verify_entry_signature(&tampered_name, &verifying_key).is_err(),
        "Tampering with name hint should invalidate signature"
    );

    // Tamper by changing from Direct to Delegation - should fail verification
    let mut tampered_delegation = entry.clone();
    tampered_delegation.sig.key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "fake_tree".to_string(),
            tips: vec![],
        }],
        hint: KeyHint::from_pubkey(&pubkey_str),
    };
    assert!(
        verify_entry_signature(&tampered_delegation, &verifying_key).is_err(),
        "Changing SigKey variant should invalidate signature"
    );
}
