//! Password Store integration tests
//!
//! Tests for PasswordStore functionality including core operations,
//! encryption/decryption, DocStore integration, and Table integration.

use eidetica::{
    Store,
    crdt::{Doc, doc::Value},
    store::{DocStore, PasswordStore, Table},
};
use serde::{Deserialize, Serialize};

use super::helpers::*;
use crate::helpers::*;

// ============================================================================
// Phase 1: Core Functionality Tests
// ============================================================================

#[test]
fn test_password_store_initialize() {
    let (_instance, database) = setup_tree();

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();

    assert!(!encrypted.is_initialized());
    assert!(!encrypted.is_open());

    encrypted
        .initialize("my_password", "docstore:v1", "{}")
        .unwrap();

    assert!(encrypted.is_initialized());
    assert!(encrypted.is_open());
    assert_eq!(encrypted.wrapped_type_id().unwrap(), "docstore:v1");

    tx.commit().unwrap();
}

#[test]
fn test_password_store_open_with_correct_password() {
    let (_instance, database) = setup_tree();
    init_password_store_docstore(&database, "secrets", "my_password");

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();

    assert!(encrypted.is_initialized());
    assert!(!encrypted.is_open());

    encrypted.open("my_password").unwrap();

    assert!(encrypted.is_open());
    assert_eq!(encrypted.wrapped_type_id().unwrap(), "docstore:v1");
}

#[test]
fn test_password_store_open_with_wrong_password() {
    let (_instance, database) = setup_tree();
    init_password_store_docstore(&database, "secrets", "correct_password");

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();

    let result = encrypted.open("wrong_password");

    assert!(result.is_err());
    assert!(!encrypted.is_open());
}

#[test]
fn test_password_store_unwrap_type_mismatch() {
    let (_instance, database) = setup_tree();

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();
    encrypted
        .initialize("my_password", "docstore:v1", "{}")
        .unwrap();

    #[derive(Serialize, Deserialize, Clone)]
    struct TestRecord {
        value: i32,
    }

    let result = encrypted.unwrap::<Table<TestRecord>>();
    assert!(result.is_err());
}

#[test]
fn test_password_store_operations_on_unopened_fail() {
    let (_instance, database) = setup_tree();
    init_password_store_docstore(&database, "secrets", "my_password");

    let tx = database.new_transaction().unwrap();
    let encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();

    assert!(encrypted.wrapped_type_id().is_err());
    assert!(encrypted.unwrap::<DocStore>().is_err());
}

#[test]
fn test_password_store_config_serialization() {
    let (_instance, database) = setup_tree();
    init_password_store_docstore(&database, "secrets", "my_password");

    let tx = database.new_transaction().unwrap();
    let index = tx.get_index_store().unwrap();

    let info = index.get_subtree_info("secrets").unwrap();
    assert_eq!(info.type_id, PasswordStore::type_id());

    let config_json = serde_json::from_str::<serde_json::Value>(&info.config);
    assert!(config_json.is_ok());
}

// ============================================================================
// Phase 2: Transparent Encryption Tests
// ============================================================================

#[test]
fn test_password_store_encrypt_decrypt_roundtrip() {
    let (_instance, database) = setup_tree();
    create_password_docstore_with_data(
        &database,
        "secrets",
        "my_password",
        &[("key1", "secret value"), ("key2", "another secret")],
    );

    assert_password_docstore_data(
        &database,
        "secrets",
        "my_password",
        &[("key1", "secret value"), ("key2", "another secret")],
    );
}

#[test]
fn test_password_store_data_is_encrypted_in_backend() {
    use base64ct::{Base64, Encoding};

    let (_instance, database) = setup_tree();

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();
    encrypted
        .initialize("my_password", "docstore:v1", "{}")
        .unwrap();

    let docstore = encrypted.unwrap::<DocStore>().unwrap();
    docstore.set("secret_key", "THIS_IS_SECRET").unwrap();

    let entry_id = tx.commit().unwrap();

    let backend = database.backend().unwrap();
    let entry = backend.get(&entry_id).unwrap();
    let stored_data = entry.data("secrets").unwrap();

    // Stored data should be base64-encoded, not contain plaintext
    assert!(!stored_data.contains("THIS_IS_SECRET"));
    assert!(!stored_data.contains("secret_key"));

    // Should be valid base64 and decode to at least 12 bytes (nonce size)
    let decoded = Base64::decode_vec(stored_data).unwrap();
    assert!(
        decoded.len() >= 12,
        "Encrypted data should have at least nonce (12 bytes)"
    );
}

#[test]
fn test_password_store_cache_is_encrypted() {
    use base64ct::{Base64, Encoding};

    let (_instance, database) = setup_tree();

    // Create initial entry with data
    let _entry_id1 =
        create_password_docstore_with_data(&database, "secrets", "password", &[("key1", "value1")]);

    // Create second entry to build history (store already initialized, so use add_data)
    let entry_id2 =
        add_data_to_password_docstore(&database, "secrets", "password", &[("key2", "value2")]);

    // Force CRDT state computation which populates cache
    let tx = database.new_transaction().unwrap();
    let mut store = tx.get_store::<PasswordStore>("secrets").unwrap();
    store.open("password").unwrap();
    let docstore = store.unwrap::<DocStore>().unwrap();
    let _ = docstore.get("key1"); // triggers state computation and caching

    // Check cache contains encrypted data, not plaintext
    let backend = database.backend().unwrap();
    if let Some(cached) = backend
        .get_cached_crdt_state(&entry_id2, "secrets")
        .unwrap()
    {
        // Cache should NOT contain plaintext values
        assert!(
            !cached.contains("value1"),
            "Cache should not contain plaintext 'value1'"
        );
        assert!(
            !cached.contains("value2"),
            "Cache should not contain plaintext 'value2'"
        );
        assert!(
            !cached.contains("key1"),
            "Cache should not contain plaintext 'key1'"
        );

        // Cache should be valid base64 (encrypted data)
        let decoded = Base64::decode_vec(&cached);
        assert!(
            decoded.is_ok(),
            "Cached data should be valid base64-encoded encrypted data"
        );
    }
}

#[test]
fn test_password_store_nonce_uniqueness() {
    use base64ct::{Base64, Encoding};

    let (_instance, database) = setup_tree();

    let entry_id1 = create_password_docstore_with_data(
        &database,
        "secrets1",
        "password",
        &[("key", "same_value")],
    );
    let entry_id2 = create_password_docstore_with_data(
        &database,
        "secrets2",
        "password",
        &[("key", "same_value")],
    );

    let backend = database.backend().unwrap();
    let entry1 = backend.get(&entry_id1).unwrap();
    let entry2 = backend.get(&entry_id2).unwrap();

    // Encrypted data is stored as base64-encoded (nonce || ciphertext)
    let encoded1 = entry1.data("secrets1").unwrap();
    let encoded2 = entry2.data("secrets2").unwrap();

    let bytes1 = Base64::decode_vec(encoded1).unwrap();
    let bytes2 = Base64::decode_vec(encoded2).unwrap();

    // First 12 bytes are the nonce
    let nonce1 = &bytes1[..12];
    let nonce2 = &bytes2[..12];
    let ciphertext1 = &bytes1[12..];
    let ciphertext2 = &bytes2[12..];

    assert_ne!(nonce1, nonce2);
    assert_ne!(ciphertext1, ciphertext2);
}

#[test]
fn test_password_store_empty_data() {
    let (_instance, database) = setup_tree();
    init_password_store_docstore(&database, "empty", "password");

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("empty").unwrap();
    encrypted.open("password").unwrap();

    let docstore = encrypted.unwrap::<DocStore>().unwrap();
    let all = docstore.get_all().unwrap();
    assert!(all.as_hashmap().is_empty());
}

#[test]
fn test_password_store_multiple_operations() {
    let (_instance, database) = setup_tree();
    create_password_docstore_with_data(&database, "secrets", "password", &[("key1", "value1")]);
    add_data_to_password_docstore(
        &database,
        "secrets",
        "password",
        &[("key2", "value2"), ("key1", "updated")],
    );

    assert_password_docstore_data(
        &database,
        "secrets",
        "password",
        &[("key1", "updated"), ("key2", "value2")],
    );
}

// ============================================================================
// Phase 3: DocStore Integration Tests
// ============================================================================

#[test]
fn test_password_store_docstore_basic_operations() {
    let (_instance, database) = setup_tree();

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("docs").unwrap();
    encrypted.initialize("pass", "docstore:v1", "{}").unwrap();

    let docstore = encrypted.unwrap::<DocStore>().unwrap();
    docstore.set("name", "Alice").unwrap();
    docstore.set("age", 30).unwrap();
    docstore.set("active", true).unwrap();
    tx.commit().unwrap();

    let tx2 = database.new_transaction().unwrap();
    let encrypted2 = open_password_store(&tx2, "docs", "pass");
    let docstore2 = encrypted2.unwrap::<DocStore>().unwrap();

    assert_eq!(docstore2.get("name").unwrap().as_text(), Some("Alice"));
    assert_eq!(docstore2.get("age").unwrap().as_int(), Some(30));
    assert_eq!(docstore2.get("active").unwrap().as_bool(), Some(true));
}

#[test]
fn test_password_store_docstore_nested_values() {
    let (_instance, database) = setup_tree();

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("nested").unwrap();
    encrypted.initialize("pass", "docstore:v1", "{}").unwrap();

    let docstore = encrypted.unwrap::<DocStore>().unwrap();

    let mut inner = Doc::new();
    inner.set_string("city", "Portland");
    inner.set("zip", Value::Int(97201));
    docstore.set_value("address", Value::Doc(inner)).unwrap();
    tx.commit().unwrap();

    let tx2 = database.new_transaction().unwrap();
    let encrypted2 = open_password_store(&tx2, "nested", "pass");
    let docstore2 = encrypted2.unwrap::<DocStore>().unwrap();

    let address = docstore2.get("address").unwrap();
    let address_doc = address.as_doc().unwrap();

    assert_eq!(address_doc.get("city").unwrap().as_text(), Some("Portland"));
    assert_eq!(address_doc.get("zip").unwrap().as_int(), Some(97201));
}

#[test]
fn test_password_store_docstore_delete() {
    let (_instance, database) = setup_tree();
    create_password_docstore_with_data(
        &database,
        "docs",
        "pass",
        &[("keep", "value1"), ("delete", "value2")],
    );

    let tx = database.new_transaction().unwrap();
    let encrypted = open_password_store(&tx, "docs", "pass");
    let docstore = encrypted.unwrap::<DocStore>().unwrap();
    docstore.delete("delete").unwrap();
    tx.commit().unwrap();

    let tx2 = database.new_transaction().unwrap();
    let encrypted2 = open_password_store(&tx2, "docs", "pass");
    let docstore2 = encrypted2.unwrap::<DocStore>().unwrap();

    assert!(docstore2.get("keep").is_ok());
    assert!(docstore2.get("delete").is_err());
}

// ============================================================================
// Phase 4: Table Integration Tests
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PasswordTestRecord {
    name: String,
    value: i32,
}

#[test]
fn test_password_store_table_basic_operations() {
    let (_instance, database) = setup_tree();

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("records").unwrap();
    encrypted.initialize("pass", "table:v1", "{}").unwrap();

    let table = encrypted.unwrap::<Table<PasswordTestRecord>>().unwrap();
    let id1 = table
        .insert(PasswordTestRecord {
            name: "Alice".to_string(),
            value: 42,
        })
        .unwrap();
    let id2 = table
        .insert(PasswordTestRecord {
            name: "Bob".to_string(),
            value: 99,
        })
        .unwrap();
    tx.commit().unwrap();

    let tx2 = database.new_transaction().unwrap();
    let encrypted2 = open_password_store(&tx2, "records", "pass");
    let table2 = encrypted2.unwrap::<Table<PasswordTestRecord>>().unwrap();

    let record1 = table2.get(&id1).unwrap();
    let record2 = table2.get(&id2).unwrap();

    assert_eq!(record1.name, "Alice");
    assert_eq!(record1.value, 42);
    assert_eq!(record2.name, "Bob");
    assert_eq!(record2.value, 99);
}

#[test]
fn test_password_store_table_update() {
    let (_instance, database) = setup_tree();

    // Create and insert
    let tx1 = database.new_transaction().unwrap();
    let mut encrypted1 = tx1.get_store::<PasswordStore>("records").unwrap();
    encrypted1.initialize("pass", "table:v1", "{}").unwrap();
    let table1 = encrypted1.unwrap::<Table<PasswordTestRecord>>().unwrap();
    let id = table1
        .insert(PasswordTestRecord {
            name: "Alice".to_string(),
            value: 42,
        })
        .unwrap();
    tx1.commit().unwrap();

    // Update
    let tx2 = database.new_transaction().unwrap();
    let encrypted2 = open_password_store(&tx2, "records", "pass");
    let table2 = encrypted2.unwrap::<Table<PasswordTestRecord>>().unwrap();
    table2
        .set(
            &id,
            PasswordTestRecord {
                name: "Alice Updated".to_string(),
                value: 100,
            },
        )
        .unwrap();
    tx2.commit().unwrap();

    // Verify
    let tx3 = database.new_transaction().unwrap();
    let encrypted3 = open_password_store(&tx3, "records", "pass");
    let table3 = encrypted3.unwrap::<Table<PasswordTestRecord>>().unwrap();

    let record = table3.get(&id).unwrap();
    assert_eq!(record.name, "Alice Updated");
    assert_eq!(record.value, 100);
}

#[test]
fn test_password_store_table_delete() {
    let (_instance, database) = setup_tree();

    // Create and insert
    let tx1 = database.new_transaction().unwrap();
    let mut encrypted1 = tx1.get_store::<PasswordStore>("records").unwrap();
    encrypted1.initialize("pass", "table:v1", "{}").unwrap();
    let table1 = encrypted1.unwrap::<Table<PasswordTestRecord>>().unwrap();
    let id = table1
        .insert(PasswordTestRecord {
            name: "Delete Me".to_string(),
            value: 42,
        })
        .unwrap();
    tx1.commit().unwrap();

    // Delete
    let tx2 = database.new_transaction().unwrap();
    let encrypted2 = open_password_store(&tx2, "records", "pass");
    let table2 = encrypted2.unwrap::<Table<PasswordTestRecord>>().unwrap();
    assert!(table2.delete(&id).unwrap());
    tx2.commit().unwrap();

    // Verify deleted
    let tx3 = database.new_transaction().unwrap();
    let encrypted3 = open_password_store(&tx3, "records", "pass");
    let table3 = encrypted3.unwrap::<Table<PasswordTestRecord>>().unwrap();
    assert!(table3.get(&id).is_err());
}

// ============================================================================
// Phase 5: Error Handling and Edge Cases Tests
// ============================================================================

#[test]
fn test_password_store_data_decryption_failure() {
    let (_instance, database) = setup_tree();
    create_password_docstore_with_data(
        &database,
        "secrets",
        "correct_password",
        &[("secret", "confidential")],
    );

    // Verify correct password works
    assert_password_docstore_data(
        &database,
        "secrets",
        "correct_password",
        &[("secret", "confidential")],
    );

    // Wrong password should fail
    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();
    let result = encrypted.open("wrong_password");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(e.is_store_error(), "Expected store error, got: {}", e);
    }
}

#[test]
fn test_password_store_invalid_salt_config() {
    let (_instance, database) = setup_tree();
    set_invalid_password_store_config(&database, "bad_salt", invalid_configs::INVALID_SALT);

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("bad_salt").unwrap();
    let result = encrypted.open("password");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_error(),
            "Expected store error for invalid salt, got: {}",
            e
        );
    }
}

#[test]
fn test_password_store_invalid_nonce_config() {
    let (_instance, database) = setup_tree();
    set_invalid_password_store_config(
        &database,
        "bad_nonce",
        invalid_configs::INVALID_NONCE_LENGTH,
    );

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("bad_nonce").unwrap();
    let result = encrypted.open("password");

    assert!(result.is_err());
}

#[test]
fn test_password_store_invalid_wrapped_config() {
    let (_instance, database) = setup_tree();
    set_invalid_password_store_config(
        &database,
        "bad_wrapped",
        invalid_configs::CORRUPTED_CIPHERTEXT,
    );

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("bad_wrapped").unwrap();
    let result = encrypted.open("password");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_error(),
            "Expected store error for invalid wrapped_config, got: {}",
            e
        );
    }
}

#[test]
fn test_password_store_unsupported_algorithm() {
    let (_instance, database) = setup_tree();
    set_invalid_password_store_config(
        &database,
        "bad_algo",
        invalid_configs::UNSUPPORTED_ALGORITHM,
    );

    let tx = database.new_transaction().unwrap();
    let result = tx.get_store::<PasswordStore>("bad_algo");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_error(),
            "Expected store error for unsupported algorithm, got: {}",
            e
        );
    }
}

#[test]
fn test_password_store_unsupported_kdf() {
    let (_instance, database) = setup_tree();
    set_invalid_password_store_config(&database, "bad_kdf", invalid_configs::UNSUPPORTED_KDF);

    let tx = database.new_transaction().unwrap();
    let result = tx.get_store::<PasswordStore>("bad_kdf");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_error(),
            "Expected store error for unsupported KDF, got: {}",
            e
        );
    }
}

#[test]
fn test_password_store_malformed_config_json() {
    let (_instance, database) = setup_tree();
    set_invalid_password_store_config(&database, "bad_json", invalid_configs::MALFORMED_JSON);

    let tx = database.new_transaction().unwrap();
    let result = tx.get_store::<PasswordStore>("bad_json");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_serialization_error(),
            "Expected store serialization error for malformed JSON, got: {}",
            e
        );
    }
}

#[test]
fn test_password_store_open_already_open_fails() {
    let (_instance, database) = setup_tree();

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();
    encrypted
        .initialize("password123", "docstore:v1", r#"{"title":"Test"}"#)
        .unwrap();

    assert!(encrypted.is_open());

    // Re-opening should fail
    let result = encrypted.open("password123");
    assert!(result.is_err());

    // Should still be open and usable
    assert!(encrypted.is_open());
    assert!(encrypted.unwrap::<DocStore>().is_ok());
}

#[test]
fn test_password_store_initialize_already_initialized_fails() {
    let (_instance, database) = setup_tree();
    init_password_store_docstore(&database, "secrets", "password");

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();

    assert!(encrypted.is_initialized());
    assert!(!encrypted.is_open());

    let result = encrypted.initialize("new_password", "docstore:v1", "{}");
    assert!(result.is_err());
}

#[test]
fn test_password_store_open_uninitialized_fails() {
    let (_instance, database) = setup_tree();

    let tx = database.new_transaction().unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").unwrap();

    assert!(!encrypted.is_initialized());

    let result = encrypted.open("password");
    assert!(result.is_err());
}
