//! Password Store integration tests
//!
//! Tests for PasswordStore functionality including core operations,
//! encryption/decryption, DocStore integration, and Table integration.

use eidetica::{
    Registered,
    crdt::{Doc, doc::Value},
    store::{DocStore, PasswordStore, Table},
};
use serde::{Deserialize, Serialize};

use super::helpers::*;
use crate::helpers::*;

// ============================================================================
// Phase 1: Core Functionality Tests
// ============================================================================

#[tokio::test]
async fn test_password_store_initialize() {
    let (_instance, database) = setup_tree().await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();

    assert!(!encrypted.is_initialized());
    assert!(!encrypted.is_open());

    encrypted
        .initialize("my_password", DocStore::type_id(), Doc::new())
        .await
        .unwrap();

    assert!(encrypted.is_initialized());
    assert!(encrypted.is_open());
    assert_eq!(encrypted.wrapped_type_id().unwrap(), DocStore::type_id());

    tx.commit().await.unwrap();
}

#[tokio::test]
async fn test_password_store_open_with_correct_password() {
    let (_instance, database) = setup_tree().await;
    init_password_store_docstore(&database, "secrets", "my_password").await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();

    assert!(encrypted.is_initialized());
    assert!(!encrypted.is_open());

    encrypted.open("my_password").unwrap();

    assert!(encrypted.is_open());
    assert_eq!(encrypted.wrapped_type_id().unwrap(), DocStore::type_id());
}

#[tokio::test]
async fn test_password_store_open_with_wrong_password() {
    let (_instance, database) = setup_tree().await;
    init_password_store_docstore(&database, "secrets", "correct_password").await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();

    let result = encrypted.open("wrong_password");

    assert!(result.is_err());
    assert!(!encrypted.is_open());
}

#[tokio::test]
async fn test_password_store_unwrap_type_mismatch() {
    let (_instance, database) = setup_tree().await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();
    encrypted
        .initialize("my_password", DocStore::type_id(), Doc::new())
        .await
        .unwrap();

    #[derive(Serialize, Deserialize, Clone)]
    struct TestRecord {
        value: i32,
    }

    let result = encrypted.unwrap::<Table<TestRecord>>().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_password_store_operations_on_unopened_fail() {
    let (_instance, database) = setup_tree().await;
    init_password_store_docstore(&database, "secrets", "my_password").await;

    let tx = database.new_transaction().await.unwrap();
    let encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();

    assert!(encrypted.wrapped_type_id().is_err());
    assert!(encrypted.unwrap::<DocStore>().await.is_err());
}

#[tokio::test]
async fn test_password_store_config_serialization() {
    let (_instance, database) = setup_tree().await;
    init_password_store_docstore(&database, "secrets", "my_password").await;

    let tx = database.new_transaction().await.unwrap();
    let index = tx.get_index().await.unwrap();

    let info = index.get_entry("secrets").await.unwrap();
    assert_eq!(info.type_id, PasswordStore::type_id());

    // Config should be a non-empty Doc with a "data" key containing serialized JSON
    assert!(!info.config.is_empty());
    let config_result = info
        .config
        .get_json::<eidetica::store::PasswordStoreConfig>("data");
    assert!(config_result.is_ok());
}

// ============================================================================
// Phase 2: Transparent Encryption Tests
// ============================================================================

#[tokio::test]
async fn test_password_store_encrypt_decrypt_roundtrip() {
    let (_instance, database) = setup_tree().await;
    create_password_docstore_with_data(
        &database,
        "secrets",
        "my_password",
        &[("key1", "secret value"), ("key2", "another secret")],
    )
    .await;

    assert_password_docstore_data(
        &database,
        "secrets",
        "my_password",
        &[("key1", "secret value"), ("key2", "another secret")],
    )
    .await;
}

#[tokio::test]
async fn test_password_store_data_is_encrypted_in_backend() {
    use base64ct::{Base64, Encoding};

    let (_instance, database) = setup_tree().await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();
    encrypted
        .initialize("my_password", DocStore::type_id(), Doc::new())
        .await
        .unwrap();

    let docstore = encrypted.unwrap::<DocStore>().await.unwrap();
    docstore.set("secret_key", "THIS_IS_SECRET").await.unwrap();

    let entry_id = tx.commit().await.unwrap();

    let backend = database.backend().unwrap();
    let entry = backend.get(&entry_id).await.unwrap();
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

#[tokio::test]
async fn test_password_store_cache_is_encrypted() {
    use base64ct::{Base64, Encoding};

    let (_instance, database) = setup_tree().await;

    // Create initial entry with data
    let _entry_id1 =
        create_password_docstore_with_data(&database, "secrets", "password", &[("key1", "value1")])
            .await;

    // Create second entry to build history (store already initialized, so use add_data)
    let entry_id2 =
        add_data_to_password_docstore(&database, "secrets", "password", &[("key2", "value2")])
            .await;

    // Force CRDT state computation which populates cache
    let tx = database.new_transaction().await.unwrap();
    let mut store = tx.get_store::<PasswordStore>("secrets").await.unwrap();
    store.open("password").unwrap();
    let docstore = store.unwrap::<DocStore>().await.unwrap();
    let _ = docstore.get("key1").await; // triggers state computation and caching

    // Check cache contains encrypted data, not plaintext
    let backend = database.backend().unwrap();
    if let Some(cached) = backend
        .get_cached_crdt_state(&entry_id2, "secrets")
        .await
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

#[tokio::test]
async fn test_password_store_nonce_uniqueness() {
    use base64ct::{Base64, Encoding};

    let (_instance, database) = setup_tree().await;

    let entry_id1 = create_password_docstore_with_data(
        &database,
        "secrets1",
        "password",
        &[("key", "same_value")],
    )
    .await;
    let entry_id2 = create_password_docstore_with_data(
        &database,
        "secrets2",
        "password",
        &[("key", "same_value")],
    )
    .await;

    let backend = database.backend().unwrap();
    let entry1 = backend.get(&entry_id1).await.unwrap();
    let entry2 = backend.get(&entry_id2).await.unwrap();

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

#[tokio::test]
async fn test_password_store_empty_data() {
    let (_instance, database) = setup_tree().await;
    init_password_store_docstore(&database, "empty", "password").await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("empty").await.unwrap();
    encrypted.open("password").unwrap();

    let docstore = encrypted.unwrap::<DocStore>().await.unwrap();
    let all = docstore.get_all().await.unwrap();
    assert!(all.is_empty());
}

#[tokio::test]
async fn test_password_store_multiple_operations() {
    let (_instance, database) = setup_tree().await;
    create_password_docstore_with_data(&database, "secrets", "password", &[("key1", "value1")])
        .await;
    add_data_to_password_docstore(
        &database,
        "secrets",
        "password",
        &[("key2", "value2"), ("key1", "updated")],
    )
    .await;

    assert_password_docstore_data(
        &database,
        "secrets",
        "password",
        &[("key1", "updated"), ("key2", "value2")],
    )
    .await;
}

// ============================================================================
// Phase 3: DocStore Integration Tests
// ============================================================================

#[tokio::test]
async fn test_password_store_docstore_basic_operations() {
    let (_instance, database) = setup_tree().await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("docs").await.unwrap();
    encrypted
        .initialize("pass", DocStore::type_id(), Doc::new())
        .await
        .unwrap();

    let docstore = encrypted.unwrap::<DocStore>().await.unwrap();
    docstore.set("name", "Alice").await.unwrap();
    docstore.set("age", 30).await.unwrap();
    docstore.set("active", true).await.unwrap();
    tx.commit().await.unwrap();

    let tx2 = database.new_transaction().await.unwrap();
    let encrypted2 = open_password_store(&tx2, "docs", "pass").await;
    let docstore2 = encrypted2.unwrap::<DocStore>().await.unwrap();

    assert_eq!(
        docstore2.get("name").await.unwrap().as_text(),
        Some("Alice")
    );
    assert_eq!(docstore2.get("age").await.unwrap().as_int(), Some(30));
    assert_eq!(docstore2.get("active").await.unwrap().as_bool(), Some(true));
}

#[tokio::test]
async fn test_password_store_docstore_nested_values() {
    let (_instance, database) = setup_tree().await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("nested").await.unwrap();
    encrypted
        .initialize("pass", DocStore::type_id(), Doc::new())
        .await
        .unwrap();

    let docstore = encrypted.unwrap::<DocStore>().await.unwrap();

    let mut inner = Doc::new();
    inner.set("city", "Portland");
    inner.set("zip", Value::Int(97201));
    docstore
        .set_value("address", Value::Doc(inner))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let tx2 = database.new_transaction().await.unwrap();
    let encrypted2 = open_password_store(&tx2, "nested", "pass").await;
    let docstore2 = encrypted2.unwrap::<DocStore>().await.unwrap();

    let address = docstore2.get("address").await.unwrap();
    let address_doc = address.as_doc().unwrap();

    assert_eq!(address_doc.get("city").unwrap().as_text(), Some("Portland"));
    assert_eq!(address_doc.get("zip").unwrap().as_int(), Some(97201));
}

#[tokio::test]
async fn test_password_store_docstore_delete() {
    let (_instance, database) = setup_tree().await;
    create_password_docstore_with_data(
        &database,
        "docs",
        "pass",
        &[("keep", "value1"), ("delete", "value2")],
    )
    .await;

    let tx = database.new_transaction().await.unwrap();
    let encrypted = open_password_store(&tx, "docs", "pass").await;
    let docstore = encrypted.unwrap::<DocStore>().await.unwrap();
    docstore.delete("delete").await.unwrap();
    tx.commit().await.unwrap();

    let tx2 = database.new_transaction().await.unwrap();
    let encrypted2 = open_password_store(&tx2, "docs", "pass").await;
    let docstore2 = encrypted2.unwrap::<DocStore>().await.unwrap();

    assert!(docstore2.get("keep").await.is_ok());
    assert!(docstore2.get("delete").await.is_err());
}

// ============================================================================
// Phase 4: Table Integration Tests
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PasswordTestRecord {
    name: String,
    value: i32,
}

#[tokio::test]
async fn test_password_store_table_basic_operations() {
    let (_instance, database) = setup_tree().await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("records").await.unwrap();
    encrypted
        .initialize("pass", Table::<()>::type_id(), Doc::new())
        .await
        .unwrap();

    let table = encrypted
        .unwrap::<Table<PasswordTestRecord>>()
        .await
        .unwrap();
    let id1 = table
        .insert(PasswordTestRecord {
            name: "Alice".to_string(),
            value: 42,
        })
        .await
        .unwrap();
    let id2 = table
        .insert(PasswordTestRecord {
            name: "Bob".to_string(),
            value: 99,
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let tx2 = database.new_transaction().await.unwrap();
    let encrypted2 = open_password_store(&tx2, "records", "pass").await;
    let table2 = encrypted2
        .unwrap::<Table<PasswordTestRecord>>()
        .await
        .unwrap();

    let record1 = table2.get(&id1).await.unwrap();
    let record2 = table2.get(&id2).await.unwrap();

    assert_eq!(record1.name, "Alice");
    assert_eq!(record1.value, 42);
    assert_eq!(record2.name, "Bob");
    assert_eq!(record2.value, 99);
}

#[tokio::test]
async fn test_password_store_table_update() {
    let (_instance, database) = setup_tree().await;

    // Create and insert
    let tx1 = database.new_transaction().await.unwrap();
    let mut encrypted1 = tx1.get_store::<PasswordStore>("records").await.unwrap();
    encrypted1
        .initialize("pass", Table::<()>::type_id(), Doc::new())
        .await
        .unwrap();
    let table1 = encrypted1
        .unwrap::<Table<PasswordTestRecord>>()
        .await
        .unwrap();
    let id = table1
        .insert(PasswordTestRecord {
            name: "Alice".to_string(),
            value: 42,
        })
        .await
        .unwrap();
    tx1.commit().await.unwrap();

    // Update
    let tx2 = database.new_transaction().await.unwrap();
    let encrypted2 = open_password_store(&tx2, "records", "pass").await;
    let table2 = encrypted2
        .unwrap::<Table<PasswordTestRecord>>()
        .await
        .unwrap();
    table2
        .set(
            &id,
            PasswordTestRecord {
                name: "Alice Updated".to_string(),
                value: 100,
            },
        )
        .await
        .unwrap();
    tx2.commit().await.unwrap();

    // Verify
    let tx3 = database.new_transaction().await.unwrap();
    let encrypted3 = open_password_store(&tx3, "records", "pass").await;
    let table3 = encrypted3
        .unwrap::<Table<PasswordTestRecord>>()
        .await
        .unwrap();

    let record = table3.get(&id).await.unwrap();
    assert_eq!(record.name, "Alice Updated");
    assert_eq!(record.value, 100);
}

#[tokio::test]
async fn test_password_store_table_delete() {
    let (_instance, database) = setup_tree().await;

    // Create and insert
    let tx1 = database.new_transaction().await.unwrap();
    let mut encrypted1 = tx1.get_store::<PasswordStore>("records").await.unwrap();
    encrypted1
        .initialize("pass", Table::<()>::type_id(), Doc::new())
        .await
        .unwrap();
    let table1 = encrypted1
        .unwrap::<Table<PasswordTestRecord>>()
        .await
        .unwrap();
    let id = table1
        .insert(PasswordTestRecord {
            name: "Delete Me".to_string(),
            value: 42,
        })
        .await
        .unwrap();
    tx1.commit().await.unwrap();

    // Delete
    let tx2 = database.new_transaction().await.unwrap();
    let encrypted2 = open_password_store(&tx2, "records", "pass").await;
    let table2 = encrypted2
        .unwrap::<Table<PasswordTestRecord>>()
        .await
        .unwrap();
    assert!(table2.delete(&id).await.unwrap());
    tx2.commit().await.unwrap();

    // Verify deleted
    let tx3 = database.new_transaction().await.unwrap();
    let encrypted3 = open_password_store(&tx3, "records", "pass").await;
    let table3 = encrypted3
        .unwrap::<Table<PasswordTestRecord>>()
        .await
        .unwrap();
    assert!(table3.get(&id).await.is_err());
}

// ============================================================================
// Phase 5: Error Handling and Edge Cases Tests
// ============================================================================

#[tokio::test]
async fn test_password_store_data_decryption_failure() {
    let (_instance, database) = setup_tree().await;
    create_password_docstore_with_data(
        &database,
        "secrets",
        "correct_password",
        &[("secret", "confidential")],
    )
    .await;

    // Verify correct password works
    assert_password_docstore_data(
        &database,
        "secrets",
        "correct_password",
        &[("secret", "confidential")],
    )
    .await;

    // Wrong password should fail
    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();
    let result = encrypted.open("wrong_password");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(e.is_store_error(), "Expected store error, got: {e}");
    }
}

#[tokio::test]
async fn test_password_store_invalid_salt_config() {
    let (_instance, database) = setup_tree().await;
    set_invalid_password_store_config(&database, "bad_salt", invalid_configs::INVALID_SALT).await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("bad_salt").await.unwrap();
    let result = encrypted.open("password");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_error(),
            "Expected store error for invalid salt, got: {e}"
        );
    }
}

#[tokio::test]
async fn test_password_store_invalid_nonce_config() {
    let (_instance, database) = setup_tree().await;
    set_invalid_password_store_config(
        &database,
        "bad_nonce",
        invalid_configs::INVALID_NONCE_LENGTH,
    )
    .await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("bad_nonce").await.unwrap();
    let result = encrypted.open("password");

    assert!(result.is_err());
}

#[tokio::test]
async fn test_password_store_invalid_wrapped_config() {
    let (_instance, database) = setup_tree().await;
    set_invalid_password_store_config(
        &database,
        "bad_wrapped",
        invalid_configs::CORRUPTED_CIPHERTEXT,
    )
    .await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("bad_wrapped").await.unwrap();
    let result = encrypted.open("password");

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_error(),
            "Expected store error for invalid wrapped_config, got: {e}"
        );
    }
}

#[tokio::test]
async fn test_password_store_unsupported_algorithm() {
    let (_instance, database) = setup_tree().await;
    set_invalid_password_store_config(
        &database,
        "bad_algo",
        invalid_configs::UNSUPPORTED_ALGORITHM,
    )
    .await;

    let tx = database.new_transaction().await.unwrap();
    let result = tx.get_store::<PasswordStore>("bad_algo").await;

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_error(),
            "Expected store error for unsupported algorithm, got: {e}"
        );
    }
}

#[tokio::test]
async fn test_password_store_unsupported_kdf() {
    let (_instance, database) = setup_tree().await;
    set_invalid_password_store_config(&database, "bad_kdf", invalid_configs::UNSUPPORTED_KDF).await;

    let tx = database.new_transaction().await.unwrap();
    let result = tx.get_store::<PasswordStore>("bad_kdf").await;

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_error(),
            "Expected store error for unsupported KDF, got: {e}"
        );
    }
}

#[tokio::test]
async fn test_password_store_malformed_config_json() {
    let (_instance, database) = setup_tree().await;
    set_invalid_password_store_config(&database, "bad_json", invalid_configs::MALFORMED_JSON).await;

    let tx = database.new_transaction().await.unwrap();
    let result = tx.get_store::<PasswordStore>("bad_json").await;

    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.is_store_serialization_error(),
            "Expected store serialization error for malformed JSON, got: {e}"
        );
    }
}

#[tokio::test]
async fn test_password_store_open_already_open_fails() {
    let (_instance, database) = setup_tree().await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();
    let mut wrapped_config = Doc::new();
    wrapped_config.set("title", "Test");
    encrypted
        .initialize("password123", DocStore::type_id(), wrapped_config)
        .await
        .unwrap();

    assert!(encrypted.is_open());

    // Re-opening should fail
    let result = encrypted.open("password123");
    assert!(result.is_err());

    // Should still be open and usable
    assert!(encrypted.is_open());
    assert!(encrypted.unwrap::<DocStore>().await.is_ok());
}

#[tokio::test]
async fn test_password_store_initialize_already_initialized_fails() {
    let (_instance, database) = setup_tree().await;
    init_password_store_docstore(&database, "secrets", "password").await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();

    assert!(encrypted.is_initialized());
    assert!(!encrypted.is_open());

    let result = encrypted
        .initialize("new_password", DocStore::type_id(), Doc::new())
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_password_store_open_uninitialized_fails() {
    let (_instance, database) = setup_tree().await;

    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>("secrets").await.unwrap();

    assert!(!encrypted.is_initialized());

    let result = encrypted.open("password");
    assert!(result.is_err());
}
