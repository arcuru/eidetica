//! Content-addressable identifier type used throughout Eidetica.
//!
//! The `ID` type represents a hex-encoded SHA-256 hash string.

use serde::{Deserialize, Serialize};

/// A content-addressable identifier for an `Entry` or other database object.
///
/// Represents a hex-encoded SHA-256 hash string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ID(String);

impl ID {
    /// Creates a new ID from any string-like input.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Returns the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns true if the ID is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for ID {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ID {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<&ID> for ID {
    fn from(id: &ID) -> Self {
        id.clone()
    }
}

impl AsRef<str> for ID {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.0)
    }
}

impl std::ops::Deref for ID {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq<str> for ID {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ID {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for ID {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<ID> for str {
    fn eq(&self, other: &ID) -> bool {
        self == other.0
    }
}

impl PartialEq<ID> for &str {
    fn eq(&self, other: &ID) -> bool {
        *self == other.0
    }
}

impl PartialEq<ID> for String {
    fn eq(&self, other: &ID) -> bool {
        self == &other.0
    }
}

impl From<ID> for String {
    fn from(id: ID) -> Self {
        id.0
    }
}

impl PartialEq<&ID> for ID {
    fn eq(&self, other: &&ID) -> bool {
        self == *other
    }
}

impl From<&ID> for String {
    fn from(id: &ID) -> Self {
        id.0.clone()
    }
}

// Manual Serialize/Deserialize implementations for String
impl Serialize for ID {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ID {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(ID(s))
    }
}
