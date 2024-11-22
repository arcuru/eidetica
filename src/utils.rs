use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use std::io::Read;
use std::path::Path;

/// Generate a valid hash to use for testing
#[allow(dead_code)]
pub fn generate_hash(data: &[u8]) -> std::io::Result<String> {
    let hash = blake3::hash(data);
    Ok(format!("b3_{}", hash.to_hex()))
}

/// Generate a hash from a file
#[allow(dead_code)]
pub fn generate_hash_from_path<P: AsRef<Path>>(path: P) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 16384];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let hash_result = hasher.finalize();
    Ok(format!("b3_{}", hash_result.to_hex()))
}

/// Generate a new ed25519 keypair using the system's secure random number generator
#[allow(dead_code)]
pub fn generate_key() -> SigningKey {
    let mut csprng = OsRng;
    SigningKey::generate(&mut csprng)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_generate_hash() {
        let data = b"Hello, world!";
        let hash = generate_hash(data).unwrap();

        // Hash should start with b3_ prefix
        assert!(hash.starts_with("b3_"));

        // Same input should produce same hash
        let hash2 = generate_hash(data).unwrap();
        assert_eq!(hash, hash2);

        // Different input should produce different hash
        let hash3 = generate_hash(b"Different data").unwrap();
        assert_ne!(hash, hash3);
    }

    #[test]
    fn test_generate_hash_from_path() -> std::io::Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("test.txt");

        // Create test file
        let mut file = File::create(&file_path)?;
        file.write_all(b"Hello, world!")?;

        let hash = generate_hash_from_path(&file_path)?;

        // Hash should start with b3_ prefix
        assert!(hash.starts_with("b3_"));

        // Same file content should produce same hash
        let hash2 = generate_hash_from_path(&file_path)?;
        assert_eq!(hash, hash2);

        // Different file content should produce different hash
        let different_path = dir.path().join("different.txt");
        let mut file2 = File::create(&different_path)?;
        file2.write_all(b"Different content")?;

        let hash3 = generate_hash_from_path(&different_path)?;
        assert_ne!(hash, hash3);

        Ok(())
    }

    #[test]
    fn test_generate_hash_from_path_nonexistent() {
        let result = generate_hash_from_path("nonexistent_file.txt");
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_hash_empty_input() {
        let hash = generate_hash(&[]).unwrap();
        assert!(hash.starts_with("b3_"));

        // Empty input should produce consistent hash
        let hash2 = generate_hash(&[]).unwrap();
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_generate_hash_large_input() {
        // Test with 1MB of data
        let large_data = vec![0x42; 1024 * 1024];
        let hash = generate_hash(&large_data).unwrap();
        assert!(hash.starts_with("b3_"));
    }

    #[test]
    fn test_generate_hash_from_path_empty_file() -> std::io::Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("empty.txt");

        // Create empty file
        File::create(&file_path)?;

        let hash = generate_hash_from_path(&file_path)?;
        assert!(hash.starts_with("b3_"));

        // Empty file should produce consistent hash
        let hash2 = generate_hash_from_path(&file_path)?;
        assert_eq!(hash, hash2);

        Ok(())
    }

    #[test]
    fn test_generate_hash_from_path_large_file() -> std::io::Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("large.txt");

        // Create 10MB file
        let mut file = File::create(&file_path)?;
        let data = vec![0x42; 10 * 1024 * 1024];
        file.write_all(&data)?;

        let hash = generate_hash_from_path(&file_path)?;
        assert!(hash.starts_with("b3_"));

        Ok(())
    }

    #[test]
    fn test_generate_hash_from_path_directory() {
        let dir = tempdir().unwrap();
        let result = generate_hash_from_path(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_hash_special_characters() {
        let data = b"Hello\0\n\r\t!";
        let hash = generate_hash(data).unwrap();
        assert!(hash.starts_with("b3_"));
    }

    #[test]
    fn test_generate_hash_unicode() {
        let data = "Hello 🦀 World! 汉字".as_bytes();
        let hash = generate_hash(data).unwrap();
        assert!(hash.starts_with("b3_"));
    }

    #[test]
    fn test_generate_hash_from_path_permissions() -> std::io::Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("readonly.txt");

        // Create read-only file
        let mut file = File::create(&file_path)?;
        file.write_all(b"test data")?;
        let mut perms = fs::metadata(&file_path)?.permissions();
        perms.set_readonly(true);
        fs::set_permissions(&file_path, perms)?;

        let hash = generate_hash_from_path(&file_path)?;
        assert!(hash.starts_with("b3_"));

        Ok(())
    }

    use ed25519_dalek::{Signer, Verifier};
    use std::collections::HashSet;

    /// Test that a key generated by `generate_key` can sign and verify a message correctly.
    #[test]
    fn test_generate_key_signature_verification() {
        let signing_key = generate_key();
        let message = b"Test message for signature verification.";
        let signature = signing_key.sign(message);

        // Verify using the signing key's verifying key
        let verifying_key = signing_key.verifying_key();
        assert!(
            verifying_key.verify(message, &signature).is_ok(),
            "Signature should be valid for the original message and key"
        );
    }

    /// Test that a signature cannot be verified with a different verifying key.
    #[test]
    fn test_signature_with_different_key() {
        let signing_key1 = generate_key();
        let signing_key2 = generate_key();
        let message = b"Another test message.";

        let signature = signing_key1.sign(message);

        // Verify using the second signing key's verifying key
        let verifying_key2 = signing_key2.verifying_key();
        assert!(
            verifying_key2.verify(message, &signature).is_err(),
            "Signature should not be valid when verified with a different key"
        );
    }

    /// Test that altering the message after signing invalidates the signature.
    #[test]
    fn test_signature_with_altered_message() {
        let signing_key = generate_key();
        let original_message = b"Original message.";
        let altered_message = b"Altered message.";

        let signature = signing_key.sign(original_message);

        let verifying_key = signing_key.verifying_key();
        // Verification should fail with the altered message
        assert!(
            verifying_key.verify(altered_message, &signature).is_err(),
            "Signature should not be valid for an altered message"
        );
    }

    /// Test that multiple keys generated by `generate_key` are unique.
    #[test]
    fn test_generate_unique_keys() {
        let mut keys_set: HashSet<[u8; ed25519_dalek::SECRET_KEY_LENGTH]> = HashSet::new();
        let iterations = 100;

        for _ in 0..iterations {
            let signing_key = generate_key();
            let keypair_bytes = signing_key.to_bytes();
            assert!(
                keys_set.insert(keypair_bytes),
                "Duplicate keypair generated, which should be highly improbable"
            );
        }
    }
}
