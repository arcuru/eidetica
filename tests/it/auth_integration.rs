use eidetica::auth::crypto::format_public_key;
use eidetica::backend::{Backend, InMemoryBackend};
use eidetica::basedb::BaseDB;

#[test]
fn test_key_management() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Initially no keys
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert!(keys.is_empty());

    // Generate a new key
    let public_key1 = db.add_private_key("KEY_LAPTOP").expect("Failed to add key");

    // Verify key was stored
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert!(keys.contains(&"KEY_LAPTOP".to_string()));

    // Get the public key back
    let retrieved_public_key = db
        .get_public_key("KEY_LAPTOP")
        .expect("Failed to get public key")
        .expect("Key not found");
    assert_eq!(public_key1, retrieved_public_key);

    // Get formatted public key
    let formatted = db
        .get_formatted_public_key("KEY_LAPTOP")
        .expect("Failed to get formatted key")
        .expect("Key not found");
    assert_eq!(formatted, format_public_key(&public_key1));

    // Add another key
    let _public_key2 = db
        .add_private_key("KEY_DESKTOP")
        .expect("Failed to add second key");

    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"KEY_LAPTOP".to_string()));
    assert!(keys.contains(&"KEY_DESKTOP".to_string()));

    // Test non-existent key
    let missing_key = db
        .get_public_key("KEY_NONEXISTENT")
        .expect("Failed to query missing key");
    assert!(missing_key.is_none());

    // Remove a key
    db.remove_private_key("KEY_LAPTOP")
        .expect("Failed to remove key");

    let keys = db
        .list_private_keys()
        .expect("Failed to list keys after removal");
    assert_eq!(keys.len(), 1);
    assert!(!keys.contains(&"KEY_LAPTOP".to_string()));
    assert!(keys.contains(&"KEY_DESKTOP".to_string()));

    // Verify removed key is gone
    let removed_key = db
        .get_public_key("KEY_LAPTOP")
        .expect("Failed to query removed key");
    assert!(removed_key.is_none());
}

#[test]
fn test_import_private_key() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate a key externally
    let external_key = SigningKey::generate(&mut OsRng);
    let external_public = external_key.verifying_key();

    // Import the key
    db.import_private_key("IMPORTED_KEY", external_key)
        .expect("Failed to import key");

    // Verify it was imported correctly
    let retrieved_public = db
        .get_public_key("IMPORTED_KEY")
        .expect("Failed to get imported key")
        .expect("Imported key not found");

    assert_eq!(external_public, retrieved_public);

    // Verify it shows up in the list
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert!(keys.contains(&"IMPORTED_KEY".to_string()));
}

#[test]
fn test_backend_serialization() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use tempfile::NamedTempFile;

    // Create a temporary file for testing serialization
    let temp_file = NamedTempFile::new().expect("Failed to create temp file");
    let temp_path = temp_file.path();

    // Original keys to store
    let key1 = SigningKey::generate(&mut OsRng);
    let key2 = SigningKey::generate(&mut OsRng);
    let public_key1 = key1.verifying_key();
    let public_key2 = key2.verifying_key();

    // Test backend-level key storage and serialization
    {
        // Create backend and add private keys directly through the Backend trait
        let mut backend = InMemoryBackend::new();

        // Store keys using the Backend trait methods
        backend
            .store_private_key("KEY_ONE", key1)
            .expect("Failed to store key1");
        backend
            .store_private_key("KEY_TWO", key2)
            .expect("Failed to store key2");

        // Verify keys are stored
        let stored_keys = backend.list_private_keys().expect("Failed to list keys");
        assert_eq!(stored_keys.len(), 2);
        assert!(stored_keys.contains(&"KEY_ONE".to_string()));
        assert!(stored_keys.contains(&"KEY_TWO".to_string()));

        // Verify we can retrieve the keys and they produce the correct public keys
        let retrieved_key1 = backend
            .get_private_key("KEY_ONE")
            .expect("Failed to get key1")
            .expect("Key1 not found");
        let retrieved_key2 = backend
            .get_private_key("KEY_TWO")
            .expect("Failed to get key2")
            .expect("Key2 not found");
        assert_eq!(retrieved_key1.verifying_key(), public_key1);
        assert_eq!(retrieved_key2.verifying_key(), public_key2);

        // Save the backend with keys to file
        backend
            .save_to_file(temp_path)
            .expect("Failed to save backend");
    }

    // Load from file and verify keys are preserved
    {
        let loaded_backend =
            InMemoryBackend::load_from_file(temp_path).expect("Failed to load backend");

        // Verify the loaded backend has the same keys
        let loaded_keys = loaded_backend
            .list_private_keys()
            .expect("Failed to list loaded keys");
        assert_eq!(loaded_keys.len(), 2);
        assert!(loaded_keys.contains(&"KEY_ONE".to_string()));
        assert!(loaded_keys.contains(&"KEY_TWO".to_string()));

        // Verify the loaded keys produce the correct public keys
        let loaded_key1 = loaded_backend
            .get_private_key("KEY_ONE")
            .expect("Failed to get loaded key1")
            .expect("Loaded key1 not found");
        let loaded_key2 = loaded_backend
            .get_private_key("KEY_TWO")
            .expect("Failed to get loaded key2")
            .expect("Loaded key2 not found");

        assert_eq!(loaded_key1.verifying_key(), public_key1);
        assert_eq!(loaded_key2.verifying_key(), public_key2);

        // Test that we can use the loaded backend through BaseDB
        let db = BaseDB::new(Box::new(loaded_backend));

        // Verify BaseDB can see the loaded keys
        let db_keys = db.list_private_keys().expect("Failed to list DB keys");
        assert_eq!(db_keys.len(), 2);

        // Verify we can get the correct public keys through BaseDB
        let db_public1 = db
            .get_public_key("KEY_ONE")
            .expect("Failed to get DB public key1")
            .expect("DB key1 not found");
        let db_public2 = db
            .get_public_key("KEY_TWO")
            .expect("Failed to get DB public key2")
            .expect("DB key2 not found");

        assert_eq!(db_public1, public_key1);
        assert_eq!(db_public2, public_key2);
    }
}

#[test]
fn test_overwrite_existing_key() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Add initial key
    let public_key1 = db
        .add_private_key("TEST_KEY")
        .expect("Failed to add initial key");

    // Overwrite with new key
    let new_private_key = SigningKey::generate(&mut OsRng);
    let new_public_key = new_private_key.verifying_key();

    db.import_private_key("TEST_KEY", new_private_key)
        .expect("Failed to overwrite key");

    // Verify the key was overwritten
    let retrieved_public = db
        .get_public_key("TEST_KEY")
        .expect("Failed to get public key")
        .expect("Key not found");

    assert_eq!(new_public_key, retrieved_public);
    assert_ne!(public_key1, retrieved_public); // Should be different from original

    // Should still only have one key
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
}

#[test]
fn test_remove_nonexistent_key() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Remove a key that doesn't exist - should succeed silently
    db.remove_private_key("NONEXISTENT_KEY")
        .expect("Failed to remove nonexistent key");

    // Should still have no keys
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert!(keys.is_empty());
}
