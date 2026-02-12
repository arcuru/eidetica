//! Cryptographic operations for Eidetica authentication
//!
//! This module provides signature generation and verification for authenticating
//! entries in the database. The `PublicKey` and `PrivateKey` enums enable
//! crypto-agility by dispatching to algorithm-specific implementations.

use base64ct::{Base64, Encoding};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::Rng;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use zeroize::{ZeroizeOnDrop, Zeroizing};

use super::errors::AuthError;
use crate::{Entry, Error};

/// Size of Ed25519 public keys in bytes
pub const ED25519_PUBLIC_KEY_SIZE: usize = 32;

/// Size of Ed25519 private keys in bytes
pub const ED25519_PRIVATE_KEY_SIZE: usize = 32;

/// Size of Ed25519 signatures in bytes
pub const ED25519_SIGNATURE_SIZE: usize = 64;

/// Size of authentication challenges in bytes
pub const CHALLENGE_SIZE: usize = 32;

// ==================== Algorithm-Agnostic Key Types ====================

/// Algorithm-agnostic public key for signature verification.
///
/// Wraps algorithm-specific verifying key types in a single enum,
/// enabling crypto-agility while maintaining zero-cost dispatch for
/// the common Ed25519 case.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PublicKey {
    /// Ed25519 public key (32 bytes)
    Ed25519(VerifyingKey),
}

impl PublicKey {
    /// Verify a signature over the given data.
    ///
    /// Returns `Ok(())` if the signature is valid, or `Err` if verification
    /// fails for any reason (malformed signature, wrong key, etc.).
    pub fn verify(&self, data: &[u8], signature: &[u8]) -> Result<(), AuthError> {
        match self {
            PublicKey::Ed25519(key) => {
                let sig_array: [u8; ED25519_SIGNATURE_SIZE] = signature
                    .try_into()
                    .map_err(|_| AuthError::InvalidSignature)?;
                let sig = Signature::from_bytes(&sig_array);
                key.verify(data, &sig)
                    .map_err(|_| AuthError::InvalidSignature)
            }
        }
    }

    /// Format the public key as a prefixed string (e.g. `"ed25519:base64..."`).
    pub fn to_prefixed_string(&self) -> String {
        match self {
            PublicKey::Ed25519(key) => {
                let encoded = Base64::encode_string(&key.to_bytes());
                format!("ed25519:{encoded}")
            }
        }
    }

    /// Parse a public key from a prefixed string (e.g. `"ed25519:base64..."`).
    pub fn from_prefixed_string(s: &str) -> Result<Self, AuthError> {
        let (prefix, key_data) = s
            .split_once(':')
            .ok_or_else(|| AuthError::InvalidKeyFormat {
                reason: "Expected 'algorithm:key' format".to_string(),
            })?;
        match prefix {
            "ed25519" => {
                let key_bytes =
                    Base64::decode_vec(key_data).map_err(|e| AuthError::InvalidKeyFormat {
                        reason: format!("Invalid base64 for key: {e}"),
                    })?;
                let key_array: [u8; ED25519_PUBLIC_KEY_SIZE] = key_bytes.try_into().map_err(
                    |v: Vec<u8>| AuthError::InvalidKeyFormat {
                        reason: format!(
                            "Ed25519 public key must be {ED25519_PUBLIC_KEY_SIZE} bytes, got {}",
                            v.len()
                        ),
                    },
                )?;
                let verifying_key = VerifyingKey::from_bytes(&key_array).map_err(|e| {
                    AuthError::KeyParsingFailed {
                        reason: e.to_string(),
                    }
                })?;
                Ok(PublicKey::Ed25519(verifying_key))
            }
            _ => Err(AuthError::InvalidKeyFormat {
                reason: format!("Unknown key algorithm prefix: '{prefix}'"),
            }),
        }
    }

    /// Get the algorithm name for this key.
    pub fn algorithm(&self) -> &'static str {
        match self {
            PublicKey::Ed25519(_) => "ed25519",
        }
    }
}

impl std::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_prefixed_string())
    }
}

/// Serializes as the prefixed string format (e.g. `"ed25519:base64..."`).
impl Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_prefixed_string())
    }
}

/// Deserializes from the prefixed string format (e.g. `"ed25519:base64..."`).
impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        PublicKey::from_prefixed_string(&s).map_err(serde::de::Error::custom)
    }
}

/// Algorithm-agnostic signing key for creating signatures.
///
/// Wraps algorithm-specific signing key types in a single enum.
/// Secret material is volatile-zeroed on drop via the inner key types'
/// [`ZeroizeOnDrop`] implementations.
#[non_exhaustive]
pub enum PrivateKey {
    /// Ed25519 signing key (32 bytes)
    Ed25519(SigningKey),
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivateKey::Ed25519(_) => f.write_str("PrivateKey::Ed25519([REDACTED])"),
        }
    }
}

impl PrivateKey {
    /// Sign the given data and return the raw signature bytes.
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        match self {
            PrivateKey::Ed25519(key) => {
                let signature: Signature = key.sign(data);
                signature.to_bytes().to_vec()
            }
        }
    }

    /// Derive the corresponding public key.
    pub fn public_key(&self) -> PublicKey {
        match self {
            PrivateKey::Ed25519(key) => PublicKey::Ed25519(key.verifying_key()),
        }
    }

    /// Generate a new key using the default algorithm (Ed25519).
    pub fn generate() -> Self {
        PrivateKey::Ed25519(SigningKey::generate(&mut OsRng))
    }

    /// Export the raw key bytes (for encryption/storage).
    ///
    /// The returned buffer is wrapped in [`Zeroizing`] so the key material
    /// is automatically cleared from memory when dropped.
    fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        match self {
            PrivateKey::Ed25519(key) => Zeroizing::new(key.to_bytes().to_vec()),
        }
    }

    /// Reconstruct a private key from raw bytes and an algorithm identifier.
    fn from_bytes(algorithm: &str, bytes: &[u8]) -> Result<Self, AuthError> {
        match algorithm {
            "ed25519" => {
                let key_array: [u8; ED25519_PRIVATE_KEY_SIZE] =
                    bytes.try_into().map_err(|_| AuthError::InvalidKeyFormat {
                        reason: format!(
                            "Ed25519 private key must be {ED25519_PRIVATE_KEY_SIZE} bytes, got {}",
                            bytes.len()
                        ),
                    })?;
                Ok(PrivateKey::Ed25519(SigningKey::from_bytes(&key_array)))
            }
            _ => Err(AuthError::InvalidKeyFormat {
                reason: format!("Unknown key algorithm: {algorithm}"),
            }),
        }
    }

    /// Format the private key as a prefixed string (e.g. `"ed25519:base64..."`).
    ///
    /// The returned string is wrapped in [`Zeroizing`] so the key material
    /// is automatically cleared from memory when dropped.
    pub fn to_prefixed_string(&self) -> Zeroizing<String> {
        let bytes = self.to_bytes();
        let encoded = Base64::encode_string(&bytes);
        Zeroizing::new(format!("{}:{encoded}", self.algorithm()))
    }

    /// Parse a private key from a prefixed string (e.g. `"ed25519:base64..."`).
    pub fn from_prefixed_string(s: &str) -> Result<Self, AuthError> {
        let (prefix, key_data) = s
            .split_once(':')
            .ok_or_else(|| AuthError::InvalidKeyFormat {
                reason: "Expected 'algorithm:key' format".to_string(),
            })?;
        match prefix {
            "ed25519" => {
                let key_bytes =
                    Base64::decode_vec(key_data).map_err(|e| AuthError::InvalidKeyFormat {
                        reason: format!("Invalid base64 for key: {e}"),
                    })?;
                Self::from_bytes("ed25519", &key_bytes)
            }
            _ => Err(AuthError::InvalidKeyFormat {
                reason: format!("Unknown key algorithm prefix: '{prefix}'"),
            }),
        }
    }

    /// Get the algorithm name for this key.
    pub fn algorithm(&self) -> &'static str {
        match self {
            PrivateKey::Ed25519(_) => "ed25519",
        }
    }
}

/// Zeroization is handled by the inner key types' `Drop` impls.
/// For Ed25519, the `zeroize` feature on `ed25519-dalek` ensures
/// `SigningKey::Drop` uses volatile writes to clear the secret bytes.
///
/// **Invariant:** all inner key types must implement [`ZeroizeOnDrop`].
impl ZeroizeOnDrop for PrivateKey {}

/// Serializes as the prefixed string format (e.g. `"ed25519:base64..."`).
impl Serialize for PrivateKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_prefixed_string())
    }
}

/// Deserializes from the prefixed string format (e.g. `"ed25519:base64..."`).
impl<'de> Deserialize<'de> for PrivateKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        PrivateKey::from_prefixed_string(&s).map_err(serde::de::Error::custom)
    }
}

// ==================== Legacy Free Functions ====================

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
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    (signing_key, verifying_key)
}

/// Sign an entry with an Ed25519 private key
///
/// Returns base64-encoded signature string
pub fn sign_entry(entry: &Entry, signing_key: &SigningKey) -> Result<String, Error> {
    let signing_bytes = entry.signing_bytes()?;
    let signature = signing_key.sign(&signing_bytes);
    Ok(Base64::encode_string(&signature.to_bytes()))
}

/// Verify an entry's signature using an algorithm-agnostic `PublicKey`.
///
/// Returns `Ok(())` if the signature is valid, or `Err(AuthError)` if
/// verification fails (missing signature, malformed data, or wrong key).
pub fn verify_entry_signature(entry: &Entry, public_key: &PublicKey) -> Result<(), AuthError> {
    let signature_base64 = entry.sig.sig.as_ref().ok_or(AuthError::InvalidSignature)?;

    let signature_bytes =
        Base64::decode_vec(signature_base64).map_err(|_| AuthError::InvalidSignature)?;

    let signing_bytes = entry
        .signing_bytes()
        .map_err(|e| AuthError::InvalidAuthConfiguration {
            reason: format!("Failed to get signing bytes: {e}"),
        })?;

    public_key.verify(&signing_bytes, &signature_bytes)
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
    let mut challenge = vec![0u8; CHALLENGE_SIZE];
    OsRng.fill(&mut challenge[..]);
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
    use crate::auth::types::{SigInfo, SigKey};

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
        entry.sig = SigInfo::builder()
            .key(SigKey::from_name("KEY_LAPTOP"))
            .build();

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();

        // Set the signature on the entry
        entry.sig.sig = Some(signature);

        // Verify the signature using algorithm-agnostic PublicKey
        let pubkey = PublicKey::Ed25519(verifying_key);
        verify_entry_signature(&entry, &pubkey).unwrap();

        // Test with wrong key
        let wrong_pubkey = PrivateKey::generate().public_key();
        assert!(verify_entry_signature(&entry, &wrong_pubkey).is_err());
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

    // ==================== PublicKey / PrivateKey Enum Tests ====================

    #[test]
    fn test_private_key_generate_and_sign() {
        let key = PrivateKey::generate();
        let pubkey = key.public_key();
        let data = b"hello world";

        let signature = key.sign(data);
        pubkey.verify(data, &signature).unwrap();

        // Wrong data should not verify
        assert!(pubkey.verify(b"wrong data", &signature).is_err());
    }

    #[test]
    fn test_public_key_prefixed_string_roundtrip() {
        let key = PrivateKey::generate();
        let pubkey = key.public_key();

        let formatted = pubkey.to_prefixed_string();
        assert!(formatted.starts_with("ed25519:"));

        let parsed = PublicKey::from_prefixed_string(&formatted).unwrap();
        assert_eq!(parsed, pubkey);
    }

    #[test]
    fn test_public_key_from_prefixed_string_invalid() {
        // Unknown prefix
        assert!(PublicKey::from_prefixed_string("rsa:abc").is_err());

        // No prefix
        assert!(PublicKey::from_prefixed_string("abc").is_err());

        // Invalid base64
        assert!(PublicKey::from_prefixed_string("ed25519:!!!invalid!!!").is_err());

        // Wrong length
        assert!(PublicKey::from_prefixed_string("ed25519:AAAA").is_err());
    }

    #[test]
    fn test_private_key_algorithm() {
        let key = PrivateKey::generate();
        assert_eq!(key.algorithm(), "ed25519");
        assert_eq!(key.public_key().algorithm(), "ed25519");
    }

    #[test]
    fn test_private_key_bytes_roundtrip() {
        let key = PrivateKey::generate();
        let bytes = key.to_bytes();
        let algorithm = key.algorithm();

        let restored = PrivateKey::from_bytes(algorithm, &bytes).unwrap();
        assert_eq!(
            key.public_key().to_prefixed_string(),
            restored.public_key().to_prefixed_string()
        );
    }

    #[test]
    fn test_private_key_from_bytes_invalid() {
        // Unknown algorithm
        assert!(PrivateKey::from_bytes("rsa", &[0u8; 32]).is_err());

        // Wrong length
        assert!(PrivateKey::from_bytes("ed25519", &[0u8; 16]).is_err());
    }

    #[test]
    fn test_private_key_prefixed_string_roundtrip() {
        let key = PrivateKey::generate();
        let formatted = key.to_prefixed_string();
        assert!(formatted.starts_with("ed25519:"));

        let restored = PrivateKey::from_prefixed_string(&formatted).unwrap();
        assert_eq!(
            key.public_key().to_prefixed_string(),
            restored.public_key().to_prefixed_string()
        );
    }

    #[test]
    fn test_private_key_from_prefixed_string_invalid() {
        // Unknown prefix
        assert!(PrivateKey::from_prefixed_string("rsa:abc").is_err());

        // No prefix
        assert!(PrivateKey::from_prefixed_string("abc").is_err());

        // Invalid base64
        assert!(PrivateKey::from_prefixed_string("ed25519:!!!invalid!!!").is_err());

        // Wrong length
        assert!(PrivateKey::from_prefixed_string("ed25519:AAAA").is_err());
    }

    #[test]
    fn test_private_key_serde_roundtrip() {
        let key = PrivateKey::generate();
        let pubkey_str = key.public_key().to_prefixed_string();

        let serialized = serde_json::to_string(&key).unwrap();
        // Should serialize as a prefixed string, same format as PublicKey
        assert!(serialized.starts_with("\"ed25519:"));

        let deserialized: PrivateKey = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.public_key().to_prefixed_string(), pubkey_str);
    }

    #[test]
    fn test_private_key_debug_redacted() {
        let key = PrivateKey::generate();
        let debug_str = format!("{key:?}");
        assert_eq!(debug_str, "PrivateKey::Ed25519([REDACTED])");
        assert!(!debug_str.contains(&format!("{:?}", key.to_bytes())));
    }

    #[test]
    fn test_public_key_display() {
        let key = PrivateKey::generate();
        let pubkey = key.public_key();
        assert_eq!(format!("{pubkey}"), pubkey.to_prefixed_string());
    }

    #[test]
    fn test_public_key_verify_malformed_signature() {
        let key = PrivateKey::generate();
        let pubkey = key.public_key();

        // Too short
        assert!(pubkey.verify(b"data", &[0u8; 10]).is_err());

        // Wrong length
        assert!(pubkey.verify(b"data", &[0u8; 63]).is_err());

        // Correct length but invalid
        assert!(pubkey.verify(b"data", &[0u8; 64]).is_err());
    }

    #[test]
    fn test_public_key_serde_roundtrip() {
        let key = PrivateKey::generate();
        let pubkey = key.public_key();

        let serialized = serde_json::to_string(&pubkey).unwrap();
        // Should serialize as a plain prefixed string
        assert_eq!(serialized, format!("\"{}\"", pubkey.to_prefixed_string()));

        let deserialized: PublicKey = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, pubkey);
    }

    #[test]
    fn test_public_key_hash() {
        use std::collections::HashSet;

        let key1 = PrivateKey::generate();
        let key2 = PrivateKey::generate();
        let pubkey1 = key1.public_key();
        let pubkey2 = key2.public_key();

        let mut set = HashSet::new();
        set.insert(pubkey1.clone());
        set.insert(pubkey2.clone());
        set.insert(pubkey1.clone()); // duplicate

        assert_eq!(set.len(), 2);
        assert!(set.contains(&pubkey1));
        assert!(set.contains(&pubkey2));
    }

    #[test]
    fn test_public_key_compat_with_legacy_format() {
        // PublicKey.to_prefixed_string() should produce the same format as format_public_key()
        let (_, verifying_key) = generate_keypair();
        let legacy = format_public_key(&verifying_key);
        let pubkey = PublicKey::Ed25519(verifying_key);
        assert_eq!(pubkey.to_prefixed_string(), legacy);
    }

    #[test]
    fn test_private_key_compat_with_legacy_keypair() {
        // PrivateKey should produce the same signatures as raw SigningKey
        let (signing_key, verifying_key) = generate_keypair();
        let private_key = PrivateKey::Ed25519(signing_key.clone());

        let data = b"test data for signing";
        let legacy_sig = sign_data(data, &signing_key);
        let enum_sig = private_key.sign(data);

        // The enum produces raw bytes; legacy produces base64.
        // Verify that the enum signature verifies with both old and new APIs.
        assert!(verify_signature(data, Base64::encode_string(&enum_sig), &verifying_key).unwrap());
        private_key
            .public_key()
            .verify(data, &Base64::decode_vec(&legacy_sig).unwrap())
            .unwrap();
    }
}
