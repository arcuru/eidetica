//! Cryptographic operations for Eidetica authentication
//!
//! This module provides Ed25519 signature generation and verification
//! for authenticating entries in the database.

use base64ct::{Base64, Encoding};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand;

use super::errors::AuthError;
use crate::Entry;

/// Size of Ed25519 public keys in bytes
pub const ED25519_PUBLIC_KEY_SIZE: usize = 32;

/// Size of Ed25519 private keys in bytes
pub const ED25519_PRIVATE_KEY_SIZE: usize = 32;

/// Size of Ed25519 signatures in bytes
pub const ED25519_SIGNATURE_SIZE: usize = 64;

/// Size of authentication challenges in bytes
pub const CHALLENGE_SIZE: usize = 32;

/// Parse a public key from string format
///
/// Expected format: "ed25519:<base64_encoded_key>"
/// The prefix "ed25519:" is required for crypto-agility
pub fn parse_public_key(key_str: impl AsRef<str>) -> Result<VerifyingKey, AuthError> {
    let key_str = key_str.as_ref();
    if !key_str.starts_with("ed25519:") {
        return Err(AuthError::InvalidKeyFormat {
            reason: "Key must start with 'ed25519:' prefix".to_string(),
        });
    }

    let key_data = &key_str[8..]; // Skip "ed25519:" prefix

    let key_bytes = Base64::decode_vec(key_data).map_err(|e| AuthError::InvalidKeyFormat {
        reason: format!("Invalid base64 for key: {e}"),
    })?;

    if key_bytes.len() != ED25519_PUBLIC_KEY_SIZE {
        return Err(AuthError::InvalidKeyFormat {
            reason: format!("Ed25519 public key must be {ED25519_PUBLIC_KEY_SIZE} bytes"),
        });
    }

    let key_array: [u8; ED25519_PUBLIC_KEY_SIZE] =
        key_bytes
            .try_into()
            .map_err(|_| AuthError::InvalidKeyFormat {
                reason: "Invalid key length after base64 decoding".to_string(),
            })?;

    VerifyingKey::from_bytes(&key_array).map_err(|e| AuthError::KeyParsingFailed {
        reason: e.to_string(),
    })
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
pub fn sign_entry(entry: &Entry, signing_key: &SigningKey) -> Result<String, crate::Error> {
    let signing_bytes = entry.signing_bytes()?;
    let signature = signing_key.sign(&signing_bytes);
    Ok(Base64::encode_string(&signature.to_bytes()))
}

/// Verify an Ed25519 signature for an entry
///
/// # Arguments
/// * `entry` - The entry that was signed (with signature field set)
/// * `verifying_key` - Public key for verification
pub fn verify_entry_signature(
    entry: &Entry,
    verifying_key: &VerifyingKey,
) -> Result<bool, AuthError> {
    let signature_base64 = entry.sig.sig.as_ref().ok_or(AuthError::InvalidSignature)?;

    let signature_bytes =
        Base64::decode_vec(signature_base64).map_err(|_| AuthError::InvalidSignature)?;

    if signature_bytes.len() != ED25519_SIGNATURE_SIZE {
        return Err(AuthError::InvalidSignature);
    }

    let signature_array: [u8; ED25519_SIGNATURE_SIZE] = signature_bytes
        .try_into()
        .map_err(|_| AuthError::InvalidSignature)?;

    let signature = Signature::from_bytes(&signature_array);

    // Get the canonical signing bytes (without signature)
    let signing_bytes = entry
        .signing_bytes()
        .map_err(|e| AuthError::InvalidAuthConfiguration {
            reason: format!("Failed to get signing bytes: {e}"),
        })?;

    match verifying_key.verify(&signing_bytes, &signature) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Sign data with an Ed25519 private key
///
/// Returns base64-encoded signature
pub fn sign_data(data: impl AsRef<[u8]>, signing_key: &SigningKey) -> String {
    let signature = signing_key.sign(data.as_ref());
    Base64::encode_string(&signature.to_bytes())
}

/// Verify an Ed25519 signature
///
/// # Arguments
/// * `data` - The data that was signed
/// * `signature_base64` - Base64-encoded signature
/// * `verifying_key` - Public key for verification
pub fn verify_signature(
    data: impl AsRef<[u8]>,
    signature_base64: impl AsRef<str>,
    verifying_key: &VerifyingKey,
) -> Result<bool, AuthError> {
    let signature_bytes =
        Base64::decode_vec(signature_base64.as_ref()).map_err(|_| AuthError::InvalidSignature)?;

    if signature_bytes.len() != ED25519_SIGNATURE_SIZE {
        return Err(AuthError::InvalidSignature);
    }

    let signature_array: [u8; ED25519_SIGNATURE_SIZE] = signature_bytes
        .try_into()
        .map_err(|_| AuthError::InvalidSignature)?;

    let signature = Signature::from_bytes(&signature_array);

    match verifying_key.verify(data.as_ref(), &signature) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Generate random challenge bytes for authentication
///
/// Generates 32 bytes of cryptographically secure random data using
/// `rand::rngs::OsRng` for use in challenge-response authentication protocols.
/// The challenge serves as a nonce to prevent replay attacks during handshakes.
///
/// # Security
/// Uses `OsRng` which provides the highest quality randomness available on the
/// platform by interfacing directly with the operating system's random number
/// generator (e.g., `/dev/urandom` on Unix systems, `CryptGenRandom` on Windows).
///
/// # Example
/// ```rust,ignore
/// use eidetica::auth::crypto::generate_challenge;
///
/// let challenge = generate_challenge();
/// assert_eq!(challenge.len(), 32);
/// ```
pub fn generate_challenge() -> Vec<u8> {
    use rand::Rng;
    let mut rng = rand::rngs::OsRng;
    let mut challenge = vec![0u8; CHALLENGE_SIZE];
    rng.fill(&mut challenge[..]);
    challenge
}

/// Create a challenge response by signing a challenge
///
/// Signs the challenge with the given key and returns the raw signature bytes.
/// This is used in sync handshake protocols where the signature needs to be
/// transmitted as binary data rather than base64 strings.
///
/// # Arguments
/// * `challenge` - The challenge bytes to sign
/// * `signing_key` - The private key to sign with
///
/// # Returns
/// Raw signature bytes (not base64 encoded)
pub fn create_challenge_response(challenge: impl AsRef<[u8]>, signing_key: &SigningKey) -> Vec<u8> {
    let signature = signing_key.sign(challenge.as_ref());
    signature.to_bytes().to_vec()
}

/// Verify a challenge response
///
/// Verifies that the given response bytes are a valid signature of the challenge
/// using the provided public key string.
///
/// # Arguments
/// * `challenge` - The original challenge bytes
/// * `response` - The signature bytes to verify
/// * `public_key_str` - Public key in "ed25519:base64" format
///
/// # Returns
/// * `Ok(true)` - The signature is cryptographically valid
/// * `Ok(false)` - The signature is invalid (wrong signature or key mismatch)
/// * `Err(AuthError)` - Failed to parse the public key format or other structural errors
///
/// # Errors
/// Returns `AuthError::InvalidKeyFormat` if the public key string cannot be parsed.
/// Returns `AuthError::InvalidSignature` if the signature bytes are malformed.
///
/// # Example
/// ```rust,ignore
/// use eidetica::auth::crypto::{generate_challenge, create_challenge_response, verify_challenge_response};
///
/// let challenge = generate_challenge();
/// let response = create_challenge_response(&challenge, &signing_key);
/// let public_key = "ed25519:base64_encoded_key";
///
/// match verify_challenge_response(&challenge, &response, public_key) {
///     Ok(true) => println!("Signature verified"),
///     Ok(false) => println!("Invalid signature"),
///     Err(e) => println!("Parse error: {}", e),
/// }
/// ```
pub fn verify_challenge_response(
    challenge: impl AsRef<[u8]>,
    response: impl AsRef<[u8]>,
    public_key_str: impl AsRef<str>,
) -> Result<bool, AuthError> {
    let verifying_key = parse_public_key(public_key_str)?;
    let signature_b64 = Base64::encode_string(response.as_ref());
    verify_signature(challenge, signature_b64, &verifying_key)
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
        let mut entry = Entry::root_builder()
            .build()
            .expect("Root entry should build successfully");

        // Set auth ID without signature
        entry.sig = crate::auth::types::SigInfo::builder()
            .key(crate::auth::types::SigKey::Direct("KEY_LAPTOP".to_string()))
            .build();

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();

        // Set the signature on the entry
        entry.sig.sig = Some(signature);

        // Verify the signature
        assert!(verify_entry_signature(&entry, &verifying_key).unwrap());

        // Test with wrong key
        let (_, wrong_key) = generate_keypair();
        assert!(!verify_entry_signature(&entry, &wrong_key).unwrap());
    }

    #[test]
    fn test_challenge_generation() {
        let challenge1 = generate_challenge();
        let challenge2 = generate_challenge();

        // Should be CHALLENGE_SIZE bytes
        assert_eq!(challenge1.len(), CHALLENGE_SIZE);
        assert_eq!(challenge2.len(), CHALLENGE_SIZE);

        // Should be different each time
        assert_ne!(challenge1, challenge2);
    }

    #[test]
    fn test_challenge_response() {
        let (signing_key, verifying_key) = generate_keypair();
        let public_key_str = format_public_key(&verifying_key);
        let challenge = generate_challenge();

        // Create and verify challenge response
        let response = create_challenge_response(&challenge, &signing_key);

        // Response should be ED25519_SIGNATURE_SIZE bytes (Ed25519 signature)
        assert_eq!(response.len(), ED25519_SIGNATURE_SIZE);

        // Should verify correctly
        assert!(verify_challenge_response(&challenge, &response, &public_key_str).unwrap());

        // Should fail with wrong challenge
        let wrong_challenge = generate_challenge();
        assert!(!verify_challenge_response(&wrong_challenge, &response, &public_key_str).unwrap());

        // Should fail with wrong key
        let (_, wrong_verifying_key) = generate_keypair();
        let wrong_public_key_str = format_public_key(&wrong_verifying_key);
        assert!(!verify_challenge_response(&challenge, &response, &wrong_public_key_str).unwrap());
    }
}
