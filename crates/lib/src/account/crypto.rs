//! Cryptographic functions for account system
//!
//! Pinned Account Argon2id key derivation. Authenticated bundle wrapping lives
//! in the credential module; no separate per-key storage format is retained.

use argon2::{Algorithm, Argon2, Params, Version, password_hash::SaltString};
use zeroize::Zeroizing;

use super::errors::AccountError;
use crate::Result;

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
/// * `password` - The account's password
/// * `salt` - The salt string (base64 encoded, from `generate_salt`)
///
/// # Returns
/// A 32-byte encryption key suitable for AES-256
pub fn derive_encryption_key(password: impl AsRef<str>, salt: impl AsRef<str>) -> Result<Vec<u8>> {
    let salt_str = salt.as_ref();
    if salt_str.len() != SALT_LENGTH {
        return Err(AccountError::InvalidSaltLength {
            expected: SALT_LENGTH,
            actual: salt_str.len(),
        }
        .into());
    }

    let salt = SaltString::from_b64(salt_str).map_err(|e| AccountError::EncryptionFailed {
        reason: format!("Invalid salt format: {e}"),
    })?;

    // Pin the existing Account format rather than inheriting future library
    // defaults. PasswordStore has its own independently versioned KDF envelope.
    let params = Params::new(19 * 1024, 2, 1, Some(KEY_LENGTH)).map_err(|_| {
        AccountError::EncryptionFailed {
            reason: "Invalid account KDF parameters".to_owned(),
        }
    })?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = Zeroizing::new(vec![0u8; KEY_LENGTH]);
    argon2
        .hash_password_into(
            password.as_ref().as_bytes(),
            salt.as_str().as_bytes(),
            &mut key,
        )
        .map_err(|e| AccountError::EncryptionFailed {
            reason: format!("Key derivation failed: {e}"),
        })?;

    Ok(std::mem::take(&mut *key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_account_kdf_v1_keeps_existing_text_salt_convention() {
        let salt = "AAAAAAAAAAAAAAAAAAAAAA";
        let password = "synthetic fixture password";
        let actual = derive_encryption_key(password, salt).unwrap();
        let mut legacy = [0; KEY_LENGTH];
        Argon2::default()
            .hash_password_into(password.as_bytes(), salt.as_bytes(), &mut legacy)
            .unwrap();
        assert_eq!(actual, legacy);
        let mut raw_salt = [0; KEY_LENGTH];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &[0; 16], &mut raw_salt)
            .unwrap();
        assert_ne!(
            actual, raw_salt,
            "raw salt would silently change stored Account credentials"
        );
    }

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
}
