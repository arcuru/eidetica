//! Content-addressable identifier type used throughout Eidetica.
//!
//! The `ID` type wraps a CID (Content Identifier) from the IPLD/multiformats spec.

use crate::Result;
use cid::Cid;
use multihash_codetable::{Code, MultihashDigest};
use serde::{Deserialize, Serialize};

// Codec values are taken from https://github.com/multiformats/multicodec

/// DAG-CBOR codec identifier (0x71) for CIDs over DAG-CBOR encoded content.
const DAG_CBOR_CODEC: u64 = 0x71;

/// Raw codec identifier (0x55) for CIDs over opaque/raw bytes.
const RAW_CODEC: u64 = 0x55;

/// Error types for ID parsing and validation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// Invalid format - not a valid CID string
    InvalidFormat(String),
    /// Invalid length for the hash algorithm
    InvalidLength { expected: usize, got: usize },
    /// Unknown hash algorithm
    UnknownAlgorithm(String),
    /// Invalid encoding
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
            IdError::InvalidHex(s) => write!(f, "Invalid encoding: {s}"),
        }
    }
}

impl std::error::Error for IdError {}

/// A content-addressable identifier for an `Entry` or other database object.
///
/// Wraps a CID v1 with the DAG-CBOR codec (0x71). Supports SHA-256 and Blake3
/// hash algorithms via the multihash specification.
///
/// String format uses multibase base32lower encoding, producing strings like
/// `bafyrei...` for dag-cbor + sha256 CIDs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ID(Option<Cid>);

impl PartialOrd for ID {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Deterministic ordering: empty IDs sort before non-empty, then by CID fields.
impl Ord for ID {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (&self.0, &other.0) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

impl ID {
    /// Creates an ID by hashing DAG-CBOR encoded bytes with SHA-256.
    ///
    /// This is the primary way to create an ID from serialized entry content.
    pub fn from_dagcbor_bytes(data: impl AsRef<[u8]>) -> Self {
        Self::from_dagcbor_bytes_with(data, Code::Sha2_256)
    }

    /// Creates an ID by hashing DAG-CBOR encoded bytes with the specified algorithm.
    pub fn from_dagcbor_bytes_with(data: impl AsRef<[u8]>, code: Code) -> Self {
        let mh = code.digest(data.as_ref());
        Self(Some(Cid::new_v1(DAG_CBOR_CODEC, mh)))
    }

    /// Creates an ID by hashing the given bytes with SHA-256.
    ///
    /// Uses the raw codec (0x55) since the bytes are not DAG-CBOR encoded content.
    pub fn from_bytes(data: impl AsRef<[u8]>) -> Self {
        // FIXME: ID::from_bytes is only really used in tests
        Self::from_bytes_with(data, Code::Sha2_256)
    }

    /// Creates an ID by hashing the given bytes with the specified algorithm.
    ///
    /// Uses the raw codec (0x55) since the bytes are not DAG-CBOR encoded content.
    pub fn from_bytes_with(data: impl AsRef<[u8]>, code: Code) -> Self {
        let mh = code.digest(data.as_ref());
        Self(Some(Cid::new_v1(RAW_CODEC, mh)))
    }

    /// Parses an ID from its string representation.
    ///
    /// Accepts multibase-encoded CID strings (e.g., base32lower `bafyrei...`).
    /// An empty string produces the default (empty) ID.
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() {
            return Ok(Self::default());
        }

        let cid = Cid::try_from(s).map_err(|e| IdError::InvalidFormat(e.to_string()))?;
        Ok(Self(Some(cid)))
    }

    /// Returns true if the ID is empty (no CID).
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// Get the underlying CID, if present.
    pub fn as_cid(&self) -> Option<&Cid> {
        self.0.as_ref()
    }

    /// Get the multihash code used for this ID.
    pub fn hash_code(&self) -> Option<u64> {
        self.0.as_ref().map(|cid| cid.hash().code())
    }
}

impl From<Cid> for ID {
    fn from(cid: Cid) -> Self {
        Self(Some(cid))
    }
}

impl From<&ID> for ID {
    fn from(id: &ID) -> Self {
        id.clone()
    }
}

impl std::fmt::Display for ID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(cid) => {
                // CID's default Display uses base32lower for v1 CIDs
                // We want to use the defaults for consistency
                write!(f, "{cid}")
            }
            None => Ok(()),
        }
    }
}

impl From<ID> for String {
    fn from(id: ID) -> Self {
        id.to_string()
    }
}

impl From<&ID> for String {
    fn from(id: &ID) -> Self {
        id.to_string()
    }
}

// Serialize as a CID link (CBOR tag 42) in binary formats, or as a string in
// human-readable formats (JSON). This allows IDs to be used as map keys in JSON
// while still being proper IPLD links in DAG-CBOR.
impl Serialize for ID {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // For human-readable formats, serialize as a string (multibase CID)
        if serializer.is_human_readable() {
            self.to_string().serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for ID {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let s = String::deserialize(deserializer)?;
            if s.is_empty() {
                Ok(Self(None))
            } else {
                Cid::try_from(s.as_str())
                    .map(|cid| Self(Some(cid)))
                    .map_err(serde::de::Error::custom)
            }
        } else {
            Ok(Self(Option::<Cid>::deserialize(deserializer)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_bytes_produces_cid() {
        let data = b"hello world";
        let id = ID::from_bytes(data);

        assert!(!id.is_empty());
        let cid = id.as_cid().unwrap();
        assert_eq!(cid.version(), cid::Version::V1);
        assert_eq!(cid.codec(), RAW_CODEC);
        // SHA-256 multihash code is 0x12
        assert_eq!(cid.hash().code(), 0x12);
    }

    #[test]
    fn test_from_bytes_blake3() {
        let data = b"hello world";
        let id = ID::from_bytes_with(data, Code::Blake3_256);

        assert!(!id.is_empty());
        let cid = id.as_cid().unwrap();
        assert_eq!(cid.version(), cid::Version::V1);
        assert_eq!(cid.codec(), RAW_CODEC);
        // Blake3-256 multihash code is 0x1e
        assert_eq!(cid.hash().code(), 0x1e);
    }

    #[test]
    fn test_parse_roundtrip() {
        let id = ID::from_bytes(b"test data");
        let s = id.to_string();

        let parsed = ID::parse(&s).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_parse_empty() {
        let id = ID::parse("").unwrap();
        assert!(id.is_empty());
        assert_eq!(id, ID::default());
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
    }

    #[test]
    fn test_empty_id() {
        let id = ID::default();
        assert!(id.is_empty());
        assert_eq!(id.to_string(), "");
    }

    #[test]
    fn test_serialization_json() {
        let id = ID::from_bytes("test_data_serialization");

        // Should serialize/deserialize via JSON
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: ID = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn test_serialization_dagcbor() {
        let id = ID::from_bytes("test_data_cbor");

        // Should serialize/deserialize via DAG-CBOR
        let bytes = serde_ipld_dagcbor::to_vec(&id).unwrap();
        let deserialized: ID = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn test_empty_id_serialization() {
        let id = ID::default();

        // JSON roundtrip
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: ID = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
        assert!(deserialized.is_empty());

        // DAG-CBOR roundtrip
        let bytes = serde_ipld_dagcbor::to_vec(&id).unwrap();
        let deserialized: ID = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
        assert_eq!(id, deserialized);
        assert!(deserialized.is_empty());
    }

    #[test]
    fn test_ordering() {
        let id1 = ID::from_bytes("aaa");
        let id2 = ID::from_bytes("bbb");
        let empty = ID::default();

        // Empty is less than non-empty
        assert!(empty < id1);
        assert!(empty < id2);
        assert!(id1 != id2);
    }

    #[test]
    fn test_parse_invalid() {
        // Invalid CID string
        assert!(ID::parse("not-a-valid-cid").is_err());
    }
}
