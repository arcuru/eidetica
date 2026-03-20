//! Cryptographic functions for user system
//!
//! Provides key derivation and encryption using:
//! - Argon2id for key derivation from password + salt
//! - AES-256-GCM for key encryption

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng},
};
use argon2::{Argon2, password_hash::SaltString};
use zeroize::Zeroize;

use super::errors::UserError;
use crate::{Result, auth::crypto::PrivateKey};

/// Salt string length for Argon2 (base64 encoded, 22 chars)
pub const SALT_LENGTH: usize = 22;

/// Nonce length for AES-GCM (12 bytes standard)
pub const NONCE_LENGTH: usize = 12;

/// Derived key length for AES-256 (32 bytes)
pub const KEY_LENGTH: usize = 32;

/// Generate a random salt string for Argon2id key derivation.
///
/// # Returns
/// A base64-encoded salt string suitable for `derive_encryption_key`.
pub fn generate_salt() -> String {
    use argon2::password_hash::rand_core;
    SaltString::generate(&mut rand_core::OsRng)
        .as_str()
        .to_string()
}

/// Derive an encryption key from a password and salt using Argon2id
///
/// # Arguments
/// * `password` - The user's password
/// * `salt` - The salt string (base64 encoded, from `generate_salt`)
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
        reason: format!("Invalid salt format: {e}"),
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
            reason: format!("Key derivation failed: {e}"),
        })?;

    Ok(key)
}

/// Encrypt a private key with a password-derived encryption key.
///
/// # Arguments
/// * `private_key` - The signing key to encrypt
/// * `encryption_key` - The 32-byte encryption key
///
/// # Returns
/// A tuple of (ciphertext, nonce) where:
/// - ciphertext is the encrypted private key
/// - nonce is the 12-byte nonce used for encryption
pub fn encrypt_private_key(
    private_key: &PrivateKey,
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

    // Encode private key as prefixed string (e.g. "ed25519:base64...")
    // This preserves the algorithm tag without a JSON wrapper.
    let serialized = private_key.to_prefixed_string();

    // Create cipher
    let cipher =
        Aes256Gcm::new_from_slice(encryption_key).map_err(|e| UserError::EncryptionFailed {
            reason: format!("Failed to create cipher: {e}"),
        })?;

    // Generate random nonce
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    // Encrypt (serialized is Zeroizing<String>, auto-zeroized on drop)
    let ciphertext =
        cipher
            .encrypt(&nonce, serialized.as_bytes())
            .map_err(|e| UserError::EncryptionFailed {
                reason: format!("Encryption failed: {e}"),
            })?;

    Ok((ciphertext, nonce.to_vec()))
}

/// Decrypt a private key.
///
/// # Arguments
/// * `ciphertext` - The encrypted private key
/// * `nonce` - The 12-byte nonce used for encryption
/// * `encryption_key` - The 32-byte encryption key
///
/// # Returns
/// The decrypted `PrivateKey`
pub fn decrypt_private_key(
    ciphertext: impl AsRef<[u8]>,
    nonce: impl AsRef<[u8]>,
    encryption_key: impl AsRef<[u8]>,
) -> Result<PrivateKey> {
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
            reason: format!("Failed to create cipher: {e}"),
        })?;

    // Create nonce - convert from fixed-size array
    let nonce_array: [u8; NONCE_LENGTH] =
        nonce_bytes
            .try_into()
            .map_err(|_| UserError::InvalidNonceLength {
                expected: NONCE_LENGTH,
                actual: nonce_bytes.len(),
            })?;
    let nonce = Nonce::from(nonce_array);

    // Decrypt
    let plaintext =
        cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|e| UserError::DecryptionFailed {
                reason: format!("Decryption failed: {e}"),
            })?;

    // Convert to string and zeroize the raw bytes immediately
    let mut prefixed = String::from_utf8(plaintext).map_err(|e| {
        let mut bytes = e.into_bytes();
        bytes.zeroize();
        UserError::DecryptionFailed {
            reason: "Decrypted key is not valid UTF-8".to_string(),
        }
    })?;

    let key = PrivateKey::from_prefixed_string(&prefixed).map_err(|e| {
        prefixed.zeroize();
        UserError::DecryptionFailed {
            reason: format!("Failed to parse decrypted private key: {e}"),
        }
    })?;

    prefixed.zeroize();
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::crypto::generate_keypair;

    #[test]
    #[cfg_attr(miri, ignore)] // Argon2 is extremely slow under Miri
    fn test_generate_salt() {
        let salt1 = generate_salt();
        let salt2 = generate_salt();

        // Salts should be the correct length
        assert_eq!(salt1.len(), SALT_LENGTH);
        assert_eq!(salt2.len(), SALT_LENGTH);

        // Salts should be different
        assert_ne!(salt1, salt2);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // Argon2 is extremely slow under Miri
    fn test_key_encryption_round_trip() {
        let (private_key, _) = generate_keypair();
        let password = "encryption_password";
        let salt = generate_salt();

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
    #[cfg_attr(miri, ignore)] // Argon2 is extremely slow under Miri
    fn test_encryption_wrong_key_fails() {
        let (private_key, _) = generate_keypair();
        let password1 = "password1";
        let password2 = "password2";
        let salt = generate_salt();

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
    #[cfg_attr(miri, ignore)] // Argon2 is extremely slow under Miri
    fn test_nonce_uniqueness() {
        let (private_key, _) = generate_keypair();
        let password = "password";
        let salt = generate_salt();
        let encryption_key = derive_encryption_key(password, &salt).unwrap();

        // Encrypt same key twice
        let (_, nonce1) = encrypt_private_key(&private_key, &encryption_key).unwrap();
        let (_, nonce2) = encrypt_private_key(&private_key, &encryption_key).unwrap();

        // Nonces should be different
        assert_ne!(nonce1, nonce2);
    }
}
