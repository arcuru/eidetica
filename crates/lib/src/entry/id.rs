//! Content-addressable identifier type used throughout Eidetica.
//!
//! The `ID` type represents a content-addressable hash that supports multiple algorithms
//! including SHA-256, Blake3, and future hash types.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Hash algorithm identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HashAlgorithm {
    /// SHA-256 (default for backward compatibility)
    Sha256,
    /// Blake3 (faster alternative)
    Blake3,
}

impl HashAlgorithm {
    /// Get the string prefix for this algorithm
    pub fn prefix(&self) -> &'static str {
        match self {
            HashAlgorithm::Sha256 => "sha256",
            HashAlgorithm::Blake3 => "blake3",
        }
    }

    /// Get expected hash length in bytes
    pub fn hash_len(&self) -> usize {
        match self {
            HashAlgorithm::Sha256 => 32,
            HashAlgorithm::Blake3 => 32,
        }
    }

    /// Get expected hex string length
    pub fn hex_len(&self) -> usize {
        self.hash_len() * 2
    }
}

/// Error types for ID parsing and validation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// Invalid format - not a valid hex string or prefixed format
    InvalidFormat(String),
    /// Invalid length for the hash algorithm
    InvalidLength { expected: usize, got: usize },
    /// Unknown hash algorithm prefix
    UnknownAlgorithm(String),
    /// Invalid hex characters
    InvalidHex(String),
}

impl std::fmt::Display for IdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdError::InvalidFormat(s) => write!(f, "Invalid ID format: {s}"),
            IdError::InvalidLength { expected, got } => {
                write!(f, "Invalid ID length: expected {expected}, got {got}")
            }
            IdError::UnknownAlgorithm(alg) => write!(f, "Unknown hash algorithm: {alg}"),
            IdError::InvalidHex(s) => write!(f, "Invalid hex characters: {s}"),
        }
    }
}

impl std::error::Error for IdError {}

/// A content-addressable identifier for an `Entry` or other database object.
///
/// Supports multiple hash algorithms including SHA-256 and Blake3. IDs can be created
/// from raw data using various hash algorithms, or parsed from string representations.
///
/// String format:
/// - Current: `sha256:deadbeef123...` or `blake3:abcdef456...` (algorithm prefix required)
/// - Legacy: `deadbeef123...` (64 hex chars, assumed SHA-256, parsing only)
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ID {
    /// String representation for compatibility and serialization
    repr: String,
    /// Cached algorithm for efficiency
    algorithm: HashAlgorithm,
}

impl Default for ID {
    fn default() -> Self {
        Self {
            repr: String::new(),
            algorithm: HashAlgorithm::Sha256,
        }
    }
}

impl ID {
    /// Creates a new ID from any string-like input without validation.
    ///
    /// For validated creation, use `parse()` or `try_from()`.
    pub fn new(s: impl Into<String>) -> Self {
        let repr = s.into();
        let algorithm = Self::detect_algorithm(&repr);
        Self { repr, algorithm }
    }

    /// Creates an ID by hashing the given bytes with SHA-256.
    pub fn from_bytes(data: impl AsRef<[u8]>) -> Self {
        Self::from_bytes_with(data, HashAlgorithm::Sha256)
    }

    /// Creates an ID by hashing the given bytes with the specified algorithm.
    pub fn from_bytes_with(data: impl AsRef<[u8]>, algorithm: HashAlgorithm) -> Self {
        let data = data.as_ref();
        let hash_bytes = match algorithm {
            HashAlgorithm::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(data);
                hasher.finalize().to_vec()
            }
            HashAlgorithm::Blake3 => blake3::hash(data).as_bytes().to_vec(),
        };

        let hex = hex::encode(&hash_bytes);
        let repr = format!("{}:{}", algorithm.prefix(), hex);

        Self { repr, algorithm }
    }

    /// Parses an ID from a string, validating the format.
    ///
    /// Requires algorithm prefix format: `algorithm:hexhash`
    pub fn parse(s: &str) -> Result<Self, IdError> {
        if s.is_empty() {
            return Ok(Self::default());
        }

        // Require prefixed format
        let Some(colon_pos) = s.find(':') else {
            return Err(IdError::InvalidFormat(
                "ID must have algorithm prefix (e.g., 'sha256:' or 'blake3:')".to_string(),
            ));
        };

        let (prefix, hex_part) = s.split_at(colon_pos);
        let hex_part = &hex_part[1..]; // Skip the ':'

        let algorithm = match prefix {
            "sha256" => HashAlgorithm::Sha256,
            "blake3" => HashAlgorithm::Blake3,
            _ => return Err(IdError::UnknownAlgorithm(prefix.to_string())),
        };

        Self::validate_hex_format(hex_part, algorithm)?;

        Ok(Self {
            repr: s.to_string(),
            algorithm,
        })
    }

    /// Validates that a hex string matches the expected format for an algorithm.
    fn validate_hex_format(hex: &str, algorithm: HashAlgorithm) -> Result<(), IdError> {
        let expected_len = algorithm.hex_len();

        if hex.len() != expected_len {
            return Err(IdError::InvalidLength {
                expected: expected_len,
                got: hex.len(),
            });
        }

        // Check that all characters are valid lowercase hex
        if !hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(IdError::InvalidHex(hex.to_string()));
        }

        Ok(())
    }

    /// Detects the algorithm from a string representation (for parsing existing IDs).
    fn detect_algorithm(s: &str) -> HashAlgorithm {
        if let Some(colon_pos) = s.find(':') {
            let prefix = &s[..colon_pos];
            match prefix {
                "blake3" => HashAlgorithm::Blake3,
                _ => HashAlgorithm::Sha256, // Default fallback
            }
        } else {
            HashAlgorithm::Sha256 // Default for empty/malformed IDs
        }
    }

    /// Returns the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.repr
    }

    /// Returns true if the ID is empty.
    pub fn is_empty(&self) -> bool {
        self.repr.is_empty()
    }

    /// Gets the hash algorithm used for this ID.
    pub fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }

    /// Gets the raw hex string without the algorithm prefix.
    pub fn hex(&self) -> &str {
        if let Some(colon_pos) = self.repr.find(':') {
            &self.repr[colon_pos + 1..]
        } else {
            &self.repr
        }
    }

    /// Gets the hash bytes if the hex is valid.
    pub fn as_bytes(&self) -> Result<Vec<u8>, hex::FromHexError> {
        hex::decode(self.hex())
    }
}

// Backward compatibility trait implementations
impl From<String> for ID {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for ID {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<&ID> for ID {
    fn from(id: &ID) -> Self {
        id.clone()
    }
}

impl AsRef<str> for ID {
    fn as_ref(&self) -> &str {
        &self.repr
    }
}

impl std::fmt::Display for ID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.repr)
    }
}

impl std::ops::Deref for ID {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.repr
    }
}

impl PartialEq<str> for ID {
    fn eq(&self, other: &str) -> bool {
        self.repr == other
    }
}

impl PartialEq<&str> for ID {
    fn eq(&self, other: &&str) -> bool {
        self.repr == *other
    }
}

impl PartialEq<String> for ID {
    fn eq(&self, other: &String) -> bool {
        &self.repr == other
    }
}

impl PartialEq<ID> for str {
    fn eq(&self, other: &ID) -> bool {
        self == other.repr
    }
}

impl PartialEq<ID> for &str {
    fn eq(&self, other: &ID) -> bool {
        *self == other.repr
    }
}

impl PartialEq<ID> for String {
    fn eq(&self, other: &ID) -> bool {
        self == &other.repr
    }
}

impl From<ID> for String {
    fn from(id: ID) -> Self {
        id.repr
    }
}

impl PartialEq<&ID> for ID {
    fn eq(&self, other: &&ID) -> bool {
        self == *other
    }
}

impl From<&ID> for String {
    fn from(id: &ID) -> Self {
        id.repr.clone()
    }
}

// Note: TryFrom implementations are not provided to avoid conflicts with blanket implementations.
// Use ID::parse() directly for validated parsing.

// Serialize/Deserialize implementations - serialize as string for compatibility
impl Serialize for ID {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.repr.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ID {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::new(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_prefixed_format() {
        let data = b"hello world";
        let id = ID::from_bytes(data);

        // Should have sha256: prefix
        assert!(id.as_str().starts_with("sha256:"));
        assert_eq!(id.algorithm(), HashAlgorithm::Sha256);
        assert_eq!(id.as_str().len(), 71); // "sha256:" (7) + hex (64) = 71
    }

    #[test]
    fn test_blake3_prefixed_format() {
        let data = b"hello world";
        let id = ID::from_bytes_with(data, HashAlgorithm::Blake3);

        // Should have blake3: prefix
        assert!(id.as_str().starts_with("blake3:"));
        assert_eq!(id.algorithm(), HashAlgorithm::Blake3);
    }

    #[test]
    fn test_parse_sha256_prefixed() {
        let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let prefixed = format!("sha256:{hex}");
        let id = ID::parse(&prefixed).unwrap();

        assert_eq!(id.algorithm(), HashAlgorithm::Sha256);
        assert_eq!(id.hex(), hex);
        assert_eq!(id.as_str(), prefixed);
    }

    #[test]
    fn test_parse_prefixed_blake3() {
        let hex = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
        let prefixed = format!("blake3:{hex}");
        let id = ID::parse(&prefixed).unwrap();

        assert_eq!(id.algorithm(), HashAlgorithm::Blake3);
        assert_eq!(id.hex(), hex);
        assert_eq!(id.as_str(), prefixed);
    }

    #[test]
    fn test_from_bytes_deterministic() {
        let id1 = ID::from_bytes("test_data_foo");
        let id2 = ID::from_bytes("test_data_foo");
        let id3 = ID::from_bytes("test_data_bar");

        // Same data should produce same ID
        assert_eq!(id1, id2);
        // Different data should produce different IDs
        assert_ne!(id1, id3);

        // Should be SHA-256 prefixed format
        assert_eq!(id1.algorithm(), HashAlgorithm::Sha256);
    }

    #[test]
    fn test_validation() {
        // Too short
        assert!(ID::parse("deadbeef").is_err());

        // Missing algorithm prefix
        assert!(
            ID::parse("deadbeef12345678901234567890123456789012345678901234567890123456").is_err()
        );

        // Invalid hex characters
        assert!(
            ID::parse("sha256:deadbeef123456789012345678901234567890123456789012345678901234567g")
                .is_err()
        );

        // Unknown algorithm
        assert!(
            ID::parse("unknown:deadbeef12345678901234567890123456789012345678901234567890123456")
                .is_err()
        );

        // Valid cases
        assert!(
            ID::parse("sha256:deadbeef12345678901234567890123456789012345678901234567890123456")
                .is_ok()
        );
        assert!(
            ID::parse("blake3:deadbeef12345678901234567890123456789012345678901234567890123456")
                .is_ok()
        );
    }

    #[test]
    fn test_serialization() {
        let id = ID::from_bytes("test_data_serialization");

        // Should serialize/deserialize as string
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: ID = serde_json::from_str(&json).unwrap();

        assert_eq!(id, deserialized);
        assert_eq!(id.algorithm(), deserialized.algorithm());
    }
}
