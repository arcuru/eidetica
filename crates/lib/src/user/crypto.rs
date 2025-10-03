//! Cryptographic functions for user system
//!
//! Provides password hashing and key encryption using:
//! - Argon2id for password hashing
//! - AES-256-GCM for key encryption

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng},
};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core},
};
use ed25519_dalek::SigningKey;
use zeroize::Zeroize;

use super::errors::UserError;
use crate::Result;

/// Salt string length for Argon2 (base64 encoded, 22 chars)
pub const SALT_LENGTH: usize = 22;

/// Nonce length for AES-GCM (12 bytes standard)
pub const NONCE_LENGTH: usize = 12;

/// Derived key length for AES-256 (32 bytes)
pub const KEY_LENGTH: usize = 32;

/// Hash a password using Argon2id
///
/// # Arguments
/// * `password` - The password to hash
///
/// # Returns
/// A tuple of (password_hash, salt_string) where:
/// - password_hash is the Argon2 hash string (PHC format)
/// - salt_string is the random salt used (base64 encoded string)
pub fn hash_password(password: impl AsRef<str>) -> Result<(String, String)> {
    let salt = SaltString::generate(&mut rand_core::OsRng);

    let argon2 = Argon2::default();

    let password_hash = argon2
        .hash_password(password.as_ref().as_bytes(), &salt)
        .map_err(|e| UserError::EncryptionFailed {
            reason: format!("Password hashing failed: {}", e),
        })?
        .to_string();

    let salt_string = salt.as_str().to_string();

    Ok((password_hash, salt_string))
}

/// Verify a password against its hash
///
/// # Arguments
/// * `password` - The password to verify
/// * `password_hash` - The stored password hash (PHC format)
///
/// # Returns
/// Ok(()) if password is correct, Err otherwise
pub fn verify_password(password: impl AsRef<str>, password_hash: impl AsRef<str>) -> Result<()> {
    let parsed_hash = PasswordHash::new(password_hash.as_ref())
        .map_err(|_| UserError::PasswordVerificationFailed)?;

    Argon2::default()
        .verify_password(password.as_ref().as_bytes(), &parsed_hash)
        .map_err(|_| UserError::InvalidPassword.into())
}

/// Derive an encryption key from a password and salt using Argon2id
///
/// # Arguments
/// * `password` - The user's password
/// * `salt` - The salt string (base64 encoded, from hash_password)
///
/// # Returns
/// A 32-byte encryption key suitable for AES-256
pub fn derive_encryption_key(password: impl AsRef<str>, salt: impl AsRef<str>) -> Result<Vec<u8>> {
    let salt_str = salt.as_ref();
    if salt_str.len() != SALT_LENGTH {
        return Err(UserError::InvalidSaltLength {
            expected: SALT_LENGTH,
            actual: salt_str.len(),
        }
        .into());
    }

    let salt = SaltString::from_b64(salt_str).map_err(|e| UserError::EncryptionFailed {
        reason: format!("Invalid salt format: {}", e),
    })?;

    let argon2 = Argon2::default();

    let mut key = vec![0u8; KEY_LENGTH];
    argon2
        .hash_password_into(
            password.as_ref().as_bytes(),
            salt.as_str().as_bytes(),
            &mut key,
        )
        .map_err(|e| UserError::EncryptionFailed {
            reason: format!("Key derivation failed: {}", e),
        })?;

    Ok(key)
}

/// Encrypt a private key with a password-derived encryption key
///
/// # Arguments
/// * `private_key` - The Ed25519 signing key to encrypt
/// * `encryption_key` - The 32-byte encryption key
///
/// # Returns
/// A tuple of (ciphertext, nonce) where:
/// - ciphertext is the encrypted private key
/// - nonce is the 12-byte nonce used for encryption
pub fn encrypt_private_key(
    private_key: &SigningKey,
    encryption_key: impl AsRef<[u8]>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let encryption_key = encryption_key.as_ref();
    if encryption_key.len() != KEY_LENGTH {
        return Err(UserError::EncryptionFailed {
            reason: format!(
                "Invalid key length: expected {}, got {}",
                KEY_LENGTH,
                encryption_key.len()
            ),
        }
        .into());
    }

    // Serialize private key to bytes
    let key_bytes = private_key.to_bytes();

    // Create cipher
    let cipher =
        Aes256Gcm::new_from_slice(encryption_key).map_err(|e| UserError::EncryptionFailed {
            reason: format!("Failed to create cipher: {}", e),
        })?;

    // Generate random nonce
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    // Encrypt
    let ciphertext =
        cipher
            .encrypt(&nonce, key_bytes.as_ref())
            .map_err(|e| UserError::EncryptionFailed {
                reason: format!("Encryption failed: {}", e),
            })?;

    Ok((ciphertext, nonce.to_vec()))
}

/// Decrypt a private key
///
/// # Arguments
/// * `ciphertext` - The encrypted private key
/// * `nonce` - The 12-byte nonce used for encryption
/// * `encryption_key` - The 32-byte encryption key
///
/// # Returns
/// The decrypted SigningKey
pub fn decrypt_private_key(
    ciphertext: impl AsRef<[u8]>,
    nonce: impl AsRef<[u8]>,
    encryption_key: impl AsRef<[u8]>,
) -> Result<SigningKey> {
    let encryption_key = encryption_key.as_ref();
    let nonce_bytes = nonce.as_ref();
    let ciphertext = ciphertext.as_ref();

    if encryption_key.len() != KEY_LENGTH {
        return Err(UserError::DecryptionFailed {
            reason: format!(
                "Invalid key length: expected {}, got {}",
                KEY_LENGTH,
                encryption_key.len()
            ),
        }
        .into());
    }

    if nonce_bytes.len() != NONCE_LENGTH {
        return Err(UserError::InvalidNonceLength {
            expected: NONCE_LENGTH,
            actual: nonce_bytes.len(),
        }
        .into());
    }

    // Create cipher
    let cipher =
        Aes256Gcm::new_from_slice(encryption_key).map_err(|e| UserError::DecryptionFailed {
            reason: format!("Failed to create cipher: {}", e),
        })?;

    // Create nonce
    let nonce = Nonce::from_slice(nonce_bytes);

    // Decrypt
    let mut plaintext =
        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| UserError::DecryptionFailed {
                reason: format!("Decryption failed: {}", e),
            })?;

    // Convert to SigningKey
    if plaintext.len() != 32 {
        // Zero out the plaintext before returning error
        plaintext.zeroize();
        return Err(UserError::DecryptionFailed {
            reason: format!(
                "Invalid key length after decryption: expected 32, got {}",
                plaintext.len()
            ),
        }
        .into());
    }

    let key_bytes: [u8; 32] = plaintext
        .try_into()
        .map_err(|_| UserError::DecryptionFailed {
            reason: "Failed to convert plaintext to key bytes".to_string(),
        })?;

    let signing_key = SigningKey::from_bytes(&key_bytes);

    Ok(signing_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::crypto::generate_keypair;

    #[test]
    fn test_password_hash_and_verify() {
        let password = "test_password_123";

        let (hash, _salt) = hash_password(password).unwrap();

        // Verify correct password
        assert!(verify_password(password, &hash).is_ok());

        // Verify incorrect password
        assert!(verify_password("wrong_password", &hash).is_err());
    }

    #[test]
    fn test_password_hash_unique() {
        let password = "test_password_123";

        let (hash1, _) = hash_password(password).unwrap();
        let (hash2, _) = hash_password(password).unwrap();

        // Hashes should be different (different salts)
        assert_ne!(hash1, hash2);

        // But both should verify
        assert!(verify_password(password, &hash1).is_ok());
        assert!(verify_password(password, &hash2).is_ok());
    }

    #[test]
    fn test_key_encryption_round_trip() {
        let (private_key, _) = generate_keypair();
        let password = "encryption_password";
        let (_, salt) = hash_password(password).unwrap();

        // Derive encryption key
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        // Encrypt
        let (ciphertext, nonce) = encrypt_private_key(&private_key, &encryption_key).unwrap();

        // Decrypt
        let decrypted_key = decrypt_private_key(&ciphertext, &nonce, &encryption_key).unwrap();

        // Verify keys match
        assert_eq!(private_key.to_bytes(), decrypted_key.to_bytes());
    }

    #[test]
    fn test_encryption_wrong_key_fails() {
        let (private_key, _) = generate_keypair();
        let password1 = "password1";
        let password2 = "password2";
        let (_, salt) = hash_password(password1).unwrap();

        // Encrypt with password1
        let encryption_key1 = derive_encryption_key(password1, &salt).unwrap();
        let (ciphertext, nonce) = encrypt_private_key(&private_key, &encryption_key1).unwrap();

        // Try to decrypt with password2
        let encryption_key2 = derive_encryption_key(password2, &salt).unwrap();
        let result = decrypt_private_key(&ciphertext, &nonce, &encryption_key2);

        // Should fail
        assert!(result.is_err());
    }

    #[test]
    fn test_nonce_uniqueness() {
        let (private_key, _) = generate_keypair();
        let password = "password";
        let (_, salt) = hash_password(password).unwrap();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        // Encrypt same key twice
        let (_, nonce1) = encrypt_private_key(&private_key, &encryption_key).unwrap();
        let (_, nonce2) = encrypt_private_key(&private_key, &encryption_key).unwrap();

        // Nonces should be different
        assert_ne!(nonce1, nonce2);
    }
}
