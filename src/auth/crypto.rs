//! Cryptographic operations for Eidetica authentication
//!
//! This module provides Ed25519 signature generation and verification
//! for authenticating entries in the database.

use crate::entry::Entry;
use crate::{Error, Result};
use base64ct::{Base64, Encoding};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand;

/// Parse a public key from string format
///
/// Expected format: "ed25519:<base64_encoded_key>"
/// The prefix "ed25519:" is required for crypto-agility
pub fn parse_public_key(key_str: &str) -> Result<VerifyingKey> {
    if !key_str.starts_with("ed25519:") {
        return Err(Error::InvalidKeyFormat(
            "Key must start with 'ed25519:' prefix".to_string(),
        ));
    }

    let key_data = &key_str[8..]; // Skip "ed25519:" prefix

    let key_bytes = Base64::decode_vec(key_data)
        .map_err(|e| Error::InvalidKeyFormat(format!("Invalid base64 for key: {e}")))?;

    if key_bytes.len() != 32 {
        return Err(Error::InvalidKeyFormat(
            "Ed25519 public key must be 32 bytes".to_string(),
        ));
    }

    let key_array: [u8; 32] = key_bytes.try_into().map_err(|_| {
        Error::InvalidKeyFormat("Invalid key length after base64 decoding".to_string())
    })?;

    VerifyingKey::from_bytes(&key_array)
        .map_err(|e| Error::InvalidKeyFormat(format!("Invalid Ed25519 key: {e}")))
}

/// Format a public key as string
///
/// Returns format: "ed25519:<base64_encoded_key>"
pub fn format_public_key(key: &VerifyingKey) -> String {
    let key_bytes = key.to_bytes();
    let encoded = Base64::encode_string(&key_bytes);
    format!("ed25519:{encoded}")
}

/// Generate an Ed25519 key pair
///
/// Uses cryptographically secure random number generation
pub fn generate_keypair() -> (SigningKey, VerifyingKey) {
    let mut rng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut rng);
    let verifying_key = signing_key.verifying_key();
    (signing_key, verifying_key)
}

/// Sign an entry with an Ed25519 private key
///
/// Returns base64-encoded signature string
pub fn sign_entry(entry: &Entry, signing_key: &SigningKey) -> Result<String> {
    let signing_bytes = entry.signing_bytes()?;
    let signature = signing_key.sign(&signing_bytes);
    Ok(Base64::encode_string(&signature.to_bytes()))
}

/// Verify an Ed25519 signature for an entry
///
/// # Arguments
/// * `entry` - The entry that was signed (with signature field set)
/// * `verifying_key` - Public key for verification
pub fn verify_entry_signature(entry: &Entry, verifying_key: &VerifyingKey) -> Result<bool> {
    let signature_base64 = entry
        .auth
        .signature
        .as_ref()
        .ok_or(Error::InvalidSignature)?;

    let signature_bytes =
        Base64::decode_vec(signature_base64).map_err(|_| Error::InvalidSignature)?;

    if signature_bytes.len() != 64 {
        return Err(Error::InvalidSignature);
    }

    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| Error::InvalidSignature)?;

    let signature = Signature::from_bytes(&signature_array);

    // Get the canonical signing bytes (without signature)
    let signing_bytes = entry.signing_bytes()?;

    match verifying_key.verify(&signing_bytes, &signature) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Sign data with an Ed25519 private key
///
/// Returns base64-encoded signature
pub fn sign_data(data: &[u8], signing_key: &SigningKey) -> String {
    let signature = signing_key.sign(data);
    Base64::encode_string(&signature.to_bytes())
}

/// Verify an Ed25519 signature
///
/// # Arguments
/// * `data` - The data that was signed
/// * `signature_base64` - Base64-encoded signature
/// * `verifying_key` - Public key for verification
pub fn verify_signature(
    data: &[u8],
    signature_base64: &str,
    verifying_key: &VerifyingKey,
) -> Result<bool> {
    let signature_bytes =
        Base64::decode_vec(signature_base64).map_err(|_| Error::InvalidSignature)?;

    if signature_bytes.len() != 64 {
        return Err(Error::InvalidSignature);
    }

    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| Error::InvalidSignature)?;

    let signature = Signature::from_bytes(&signature_array);

    match verifying_key.verify(data, &signature) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let (signing_key, verifying_key) = generate_keypair();

        // Test signing and verification
        let test_data = b"hello world";
        let signature = sign_data(test_data, &signing_key);

        assert!(verify_signature(test_data, &signature, &verifying_key).unwrap());

        // Test with wrong data
        let wrong_data = b"goodbye world";
        assert!(!verify_signature(wrong_data, &signature, &verifying_key).unwrap());
    }

    #[test]
    fn test_key_formatting() {
        let (_, verifying_key) = generate_keypair();
        let formatted = format_public_key(&verifying_key);

        assert!(formatted.starts_with("ed25519:"));

        // Should be able to parse it back
        let parsed = parse_public_key(&formatted);
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap(), verifying_key);
    }

    #[test]
    fn test_entry_signing() {
        let (signing_key, verifying_key) = generate_keypair();

        // Create a test entry with auth info but no signature
        let mut entry =
            crate::entry::Entry::builder("root123".to_string(), "{}".to_string()).build();

        // Set auth ID without signature
        entry.auth = crate::auth::types::AuthInfo {
            id: crate::auth::types::AuthId::Direct("KEY_LAPTOP".to_string()),
            signature: None,
        };

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();

        // Set the signature on the entry
        entry.auth.signature = Some(signature);

        // Verify the signature
        assert!(verify_entry_signature(&entry, &verifying_key).unwrap());

        // Test with wrong key
        let (_, wrong_key) = generate_keypair();
        assert!(!verify_entry_signature(&entry, &wrong_key).unwrap());
    }
}
