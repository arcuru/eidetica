//! Core authentication data structures for Eidetica
//!
//! This module defines the fundamental types for authentication, including permissions,
//! key management, and authentication identifiers used in the system.

use crate::crdt::{Nested, Value};
use serde::{Deserialize, Serialize};

/// Macro to implement Value conversions for types that convert via String
macro_rules! impl_nested_value_string {
    ($type:ty) => {
        impl From<$type> for Value {
            fn from(value: $type) -> Self {
                Value::String(value.into())
            }
        }

        impl TryFrom<Value> for $type {
            type Error = String;

            fn try_from(value: Value) -> Result<Self, Self::Error> {
                match value {
                    Value::String(s) => <$type>::try_from(s),
                    Value::Map(_) => {
                        Err(concat!("Cannot convert map to ", stringify!($type)).to_string())
                    }
                    Value::Array(_) => {
                        Err(concat!("Cannot convert array to ", stringify!($type)).to_string())
                    }
                    Value::Deleted => Err(concat!(
                        "Cannot convert deleted value to ",
                        stringify!($type)
                    )
                    .to_string()),
                }
            }
        }
    };
}

/// Macro to implement Value conversions for types that convert to Map
macro_rules! impl_nested_value_map {
    ($type:ty, {
        $($field:ident : $field_type:ty),* $(,)?
    }) => {
        impl From<$type> for Value {
            fn from(value: $type) -> Self {
                let mut nested = Nested::new();
                $(
                    nested.as_hashmap_mut().insert(stringify!($field).to_string(), Value::from(value.$field));
                )*
                Value::Map(nested)
            }
        }

        impl TryFrom<Value> for $type {
            type Error = String;

            fn try_from(value: Value) -> Result<Self, Self::Error> {
                match value {
                    Value::Map(map) => {
                        $(
                            let $field = map
                                .get(stringify!($field))
                                .ok_or_else(|| format!("Missing '{}' field in {}", stringify!($field), stringify!($type)))?;

                            let $field = <$field_type>::try_from($field.clone())
                                .map_err(|e| format!("Invalid {}: {}", stringify!($field), e))?;
                        )*

                        Ok(Self {
                            $($field,)*
                        })
                    }
                    Value::String(json) => {
                        // Fallback to JSON parsing for backward compatibility
                        serde_json::from_str(&json)
                            .map_err(|e| format!("Failed to parse {} from JSON: {}", stringify!($type), e))
                    }
                    Value::Array(_) => Err(concat!("Cannot convert array to ", stringify!($type)).to_string()),
                    Value::Deleted => Err(concat!("Cannot convert deleted value to ", stringify!($type)).to_string()),
                }
            }
        }
    };
}

/// Permission levels for authenticated operations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Permission {
    /// Full administrative access including settings and key management
    /// Priority determines administrative hierarchy (lower number = higher priority)
    /// Priority only affects who can modify keys, not merge conflict resolution
    Admin(u32),
    /// Read and write access to data (excludes settings modifications)
    /// Priority determines administrative hierarchy (lower number = higher priority)
    /// Priority only affects who can modify keys, not merge conflict resolution
    Write(u32),
    /// Read-only access to data
    Read,
}

impl PartialOrd for Permission {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Permission {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ordering_value().cmp(&other.ordering_value())
    }
}

impl Permission {
    /// Calculate ordering value for mathematical comparison
    /// Read = 0, Write(p) = 1 + u32::MAX - p, Admin(p) = 1 + (2 * u32::MAX) - p
    /// This allows for easy comparison of permissions
    fn ordering_value(&self) -> u64 {
        match self {
            Permission::Read => 0,
            Permission::Write(p) => 1 + (u32::MAX as u64) - (*p as u64),
            Permission::Admin(p) => 1 + (2 * u32::MAX as u64) - (*p as u64),
        }
    }

    /// Get the priority level for permissions that have one
    pub fn priority(&self) -> Option<u32> {
        match self {
            Permission::Read => None,
            Permission::Write(priority) => Some(*priority),
            Permission::Admin(priority) => Some(*priority),
        }
    }

    /// Check if this permission allows writing data
    pub fn can_write(&self) -> bool {
        matches!(self, Permission::Write(_) | Permission::Admin(_))
    }

    /// Check if this permission allows administrative operations
    pub fn can_admin(&self) -> bool {
        matches!(self, Permission::Admin(_))
    }

    /// Clamp permissions to a maximum level
    ///
    /// Used for User Auth Tree delegation to ensure users cannot escalate
    /// their permissions beyond what was granted in the main tree.
    /// Returns the minimum of self and max_permission.
    pub fn clamp_to(&self, max_permission: &Permission) -> Permission {
        use std::cmp::min;
        min(self.clone(), max_permission.clone())
    }
}

/// Status of an authentication key
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum KeyStatus {
    /// Key is active and can create new entries
    Active,
    /// Key is revoked - cannot create new entries, but historical entries are preserved
    /// During merges, content from revoked entries is preserved using standard LWW merge
    Revoked,
}

/// Authentication key configuration stored in _settings.auth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthKey {
    /// Public key with crypto-agility prefix
    /// Currently only supports ed25519 format: "ed25519:<base64_url_unpadded_key>"
    pub key: String,
    /// Permission level for this key
    pub permissions: Permission,
    /// Current status of the key
    pub status: KeyStatus,
}

/// Reference to a Merkle-DAG tree (for User Auth Trees)
/// TODO: May standardize on this format across the codebase
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TreeReference {
    /// Root entry ID of the referenced tree
    pub root: String,
    /// Current tip entry IDs of the referenced tree
    pub tips: Vec<String>,
}

/// User Authentication Tree reference stored in main tree's _settings.auth
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserAuthTreeRef {
    /// Maximum permission level this user can have (subject to clamping)
    pub permissions: Permission,
    /// Reference to the User Auth Tree
    pub tree: TreeReference,
}

impl Default for UserAuthTreeRef {
    fn default() -> Self {
        Self {
            permissions: Permission::Read,
            tree: TreeReference::default(),
        }
    }
}

/// Authentication identifier for entry signing
///
/// Can be either a direct key reference or a nested User Auth Tree delegation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuthId {
    /// Direct reference to a key ID in the main tree's _settings.auth
    Direct(String),
    /// User Auth Tree delegation with optional nesting
    UserTree {
        /// User Auth Tree ID in the main tree's _settings.auth
        id: String,
        /// Tips of the User Auth Tree at time of signing
        tips: Vec<String>,
        /// Key reference within the User Auth Tree (can be nested)
        key: Box<AuthId>,
    },
}

impl Default for AuthId {
    fn default() -> Self {
        AuthId::Direct(String::new())
    }
}

/// Authentication information embedded in an entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AuthInfo {
    /// Authentication identifier (direct key or User Auth Tree delegation)
    pub id: AuthId,
    /// Base64-encoded signature bytes
    /// Optional to allow for entry creation before signing
    pub signature: Option<String>,
}

/// Resolved authentication information after validation
#[derive(Debug, Clone)]
pub struct ResolvedAuth {
    /// The actual public key used for signing
    pub public_key: ed25519_dalek::VerifyingKey,
    /// Effective permission after clamping
    pub effective_permission: Permission,
    /// Current status of the key
    pub key_status: KeyStatus,
}

/// Operation types for permission checking
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Writing data to non-settings subtrees
    WriteData,
    /// Writing to _settings subtree (includes authentication modifications)
    WriteSettings,
}

impl From<Permission> for String {
    fn from(permission: Permission) -> Self {
        match permission {
            Permission::Read => "read".to_string(),
            Permission::Write(priority) => format!("write:{priority}"),
            Permission::Admin(priority) => format!("admin:{priority}"),
        }
    }
}

impl TryFrom<String> for Permission {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let parts = s.split(':').collect::<Vec<&str>>();
        match parts[0] {
            "read" => Ok(Permission::Read),
            "write" => {
                if parts.len() != 2 {
                    return Err("Write permission requires priority".to_string());
                }
                let priority = parts[1]
                    .parse::<u32>()
                    .map_err(|_| "Invalid priority value".to_string())?;
                Ok(Permission::Write(priority))
            }
            "admin" => {
                if parts.len() != 2 {
                    return Err("Admin permission requires priority".to_string());
                }
                let priority = parts[1]
                    .parse::<u32>()
                    .map_err(|_| "Invalid priority value".to_string())?;
                Ok(Permission::Admin(priority))
            }
            _ => Err(format!("Invalid permission string: {s}")),
        }
    }
}

impl From<KeyStatus> for String {
    fn from(status: KeyStatus) -> Self {
        match status {
            KeyStatus::Active => "active".to_string(),
            KeyStatus::Revoked => "revoked".to_string(),
        }
    }
}

impl TryFrom<String> for KeyStatus {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.as_str() {
            "active" => Ok(KeyStatus::Active),
            "revoked" => Ok(KeyStatus::Revoked),
            _ => Err(format!("Invalid key status: {s}")),
        }
    }
}

// Use macros for Value conversions
impl_nested_value_string!(Permission);
impl_nested_value_string!(KeyStatus);

// Add TryFrom<Value> for String to support the macro
impl TryFrom<Value> for String {
    type Error = String;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(s) => Ok(s),
            Value::Map(_) => Err("Cannot convert map to String".to_string()),
            Value::Array(_) => Err("Cannot convert array to String".to_string()),
            Value::Deleted => Err("Cannot convert deleted value to String".to_string()),
        }
    }
}

// Use the map macro for struct types
impl_nested_value_map!(AuthKey, {
    key: String,
    permissions: Permission,
    status: KeyStatus
});

impl_nested_value_map!(TreeReference, {
    root: String,
    tips: Vec<String>
});

impl_nested_value_map!(UserAuthTreeRef, {
    permissions: Permission,
    tree: TreeReference
});

impl From<Vec<String>> for Value {
    fn from(vec: Vec<String>) -> Self {
        // Convert Vec<String> to a JSON array string
        Value::String(serde_json::to_string(&vec).unwrap_or_else(|_| "[]".to_string()))
    }
}

impl TryFrom<Value> for Vec<String> {
    type Error = String;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(s) => serde_json::from_str(&s)
                .map_err(|e| format!("Failed to parse Vec<String> from JSON: {e}")),
            Value::Map(_) => Err("Cannot convert map to Vec<String>".to_string()),
            Value::Array(array) => {
                let mut result = Vec::new();
                for (_, nested_value) in array.iter() {
                    match nested_value {
                        Value::String(s) => result.push(s.clone()),
                        _ => return Err("Array contains non-string values".to_string()),
                    }
                }
                Ok(result)
            }
            Value::Deleted => Err("Cannot convert deleted value to Vec<String>".to_string()),
        }
    }
}

impl From<AuthId> for Value {
    fn from(auth_id: AuthId) -> Self {
        let mut nested = Nested::new();
        match auth_id {
            AuthId::Direct(key_id) => {
                nested.set("type", "direct".to_string());
                nested.set("key_id", key_id);
            }
            AuthId::UserTree { id, tips, key } => {
                nested.set("type", "user_tree".to_string());
                nested.set("id", id);
                nested.set("tips", serde_json::to_string(&tips).unwrap_or_default());
                nested.set("key", *key);
            }
        }
        Value::Map(nested)
    }
}

impl TryFrom<Value> for AuthId {
    type Error = String;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Map(map) => {
                let auth_type = map
                    .get("type")
                    .ok_or_else(|| "Missing 'type' field in AuthId".to_string())?;

                let type_str = match auth_type {
                    Value::String(s) => s,
                    _ => return Err("AuthId 'type' field must be a string".to_string()),
                };

                match type_str.as_str() {
                    "direct" => {
                        let key_id = map
                            .get("key_id")
                            .ok_or_else(|| "Missing 'key_id' field in Direct AuthId".to_string())?;

                        let key_id_str = match key_id {
                            Value::String(s) => s.clone(),
                            _ => return Err("AuthId 'key_id' field must be a string".to_string()),
                        };

                        Ok(AuthId::Direct(key_id_str))
                    }
                    "user_tree" => {
                        let id = map
                            .get("id")
                            .ok_or_else(|| "Missing 'id' field in UserTree AuthId".to_string())?;
                        let tips = map
                            .get("tips")
                            .ok_or_else(|| "Missing 'tips' field in UserTree AuthId".to_string())?;
                        let key = map
                            .get("key")
                            .ok_or_else(|| "Missing 'key' field in UserTree AuthId".to_string())?;

                        let id_str = match id {
                            Value::String(s) => s.clone(),
                            _ => return Err("AuthId 'id' field must be a string".to_string()),
                        };

                        let tips_vec = Vec::<String>::try_from(tips.clone())
                            .map_err(|e| format!("Invalid tips: {e}"))?;

                        let key_parsed = AuthId::try_from(key.clone())
                            .map_err(|e| format!("Invalid nested key: {e}"))?;

                        Ok(AuthId::UserTree {
                            id: id_str,
                            tips: tips_vec,
                            key: Box::new(key_parsed),
                        })
                    }
                    _ => Err(format!("Unknown AuthId type: {type_str}")),
                }
            }
            Value::String(json) => {
                // Fallback to JSON parsing for backward compatibility
                serde_json::from_str(&json)
                    .map_err(|e| format!("Failed to parse AuthId from JSON: {e}"))
            }
            Value::Array(_) => Err("Cannot convert array to AuthId".to_string()),
            Value::Deleted => Err("Cannot convert deleted value to AuthId".to_string()),
        }
    }
}

impl From<AuthInfo> for Value {
    fn from(auth_info: AuthInfo) -> Self {
        let mut nested = Nested::new();
        nested
            .as_hashmap_mut()
            .insert("id".to_string(), Value::from(auth_info.id));
        if let Some(signature) = auth_info.signature {
            nested.set("signature", signature);
        }
        Value::Map(nested)
    }
}

impl TryFrom<Value> for AuthInfo {
    type Error = String;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Map(map) => {
                let id = map
                    .get("id")
                    .ok_or_else(|| "Missing 'id' field in AuthInfo".to_string())?;
                let signature = map.get("signature").and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                });

                let id_parsed =
                    AuthId::try_from(id.clone()).map_err(|e| format!("Invalid id: {e}"))?;

                Ok(AuthInfo {
                    id: id_parsed,
                    signature,
                })
            }
            Value::String(s) => Err(format!("Cannot convert string to AuthInfo: {s}")),
            Value::Array(_) => Err("Cannot convert array to AuthInfo".to_string()),
            Value::Deleted => Err("Cannot convert deleted value to AuthInfo".to_string()),
        }
    }
}

impl AuthInfo {
    /// Check if this entry is signed by a specific key ID.
    ///
    /// Returns `true` if the entry is signed by the given key ID and has a signature,
    /// `false` otherwise.
    ///
    /// # Arguments
    /// * `key_id` - The key ID to check against
    ///
    /// # Returns
    /// `true` if signed by the key and has a signature, `false` otherwise
    pub fn is_signed_by(&self, key_id: impl AsRef<str>) -> bool {
        self.id == AuthId::Direct(key_id.as_ref().to_string()) && self.signature.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_clamping() {
        assert_eq!(
            Permission::Admin(5).clamp_to(&Permission::Write(10)),
            Permission::Write(10)
        );
        assert_eq!(
            Permission::Admin(5).clamp_to(&Permission::Read),
            Permission::Read
        );
        assert_eq!(
            Permission::Write(5).clamp_to(&Permission::Read),
            Permission::Read
        );
        assert_eq!(
            Permission::Write(5).clamp_to(&Permission::Admin(10)),
            Permission::Write(5)
        );
        assert_eq!(
            Permission::Read.clamp_to(&Permission::Admin(10)),
            Permission::Read
        );
        assert_eq!(
            Permission::Read.clamp_to(&Permission::Read),
            Permission::Read
        );
        assert_eq!(
            Permission::Write(3).clamp_to(&Permission::Write(7)),
            Permission::Write(7)
        );
        assert_eq!(
            Permission::Admin(2).clamp_to(&Permission::Admin(1)),
            Permission::Admin(2)
        );
    }

    #[test]
    fn test_permission_ordering() {
        // Test permission level ordering (Read < Write < Admin)
        assert!(Permission::Read < Permission::Write(1));
        assert!(Permission::Read < Permission::Admin(1));
        assert!(Permission::Write(1) < Permission::Admin(1));

        // Test priority ordering within same level
        assert!(Permission::Write(1) > Permission::Write(5));
        assert!(Permission::Admin(1) > Permission::Admin(5));

        // Test that permission level takes precedence over priority
        assert!(Permission::Write(100) < Permission::Admin(1));
        assert!(Permission::Read < Permission::Write(0));
        assert!(Permission::Read < Permission::Admin(0));

        // Test equality
        assert_eq!(Permission::Read, Permission::Read);
        assert_eq!(Permission::Write(5), Permission::Write(5));
        assert_eq!(Permission::Admin(10), Permission::Admin(10));

        // Test that different priorities make permissions different
        assert_ne!(Permission::Write(1), Permission::Write(2));
        assert_ne!(Permission::Admin(1), Permission::Admin(2));
    }

    #[test]
    fn test_permission_min_max() {
        use std::cmp::{max, min};

        // Test min/max with different permission levels
        assert_eq!(
            min(Permission::Admin(5), Permission::Write(10)),
            Permission::Write(10)
        );
        assert_eq!(
            max(Permission::Read, Permission::Write(1)),
            Permission::Write(1)
        );

        assert_eq!(
            min(Permission::Write(1), Permission::Write(5)),
            Permission::Write(5)
        );
        assert_eq!(
            max(Permission::Admin(1), Permission::Admin(5)),
            Permission::Admin(1)
        );
    }

    #[test]
    fn test_auth_key_serialization() {
        let key = AuthKey {
            key: "ed25519:PExACKOW0L7bKAM9mK_mH3L5EDwszC437uRzTqAbxpk".to_string(),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let serialized = serde_json::to_string(&key).unwrap();
        let deserialized: AuthKey = serde_json::from_str(&serialized).unwrap();

        assert_eq!(key.key, deserialized.key);
        assert_eq!(key.permissions, deserialized.permissions);
        assert_eq!(key.status, deserialized.status);
    }

    #[test]
    fn test_auth_info_serialization() {
        let auth_info = AuthInfo {
            id: AuthId::Direct("KEY_LAPTOP".to_string()),
            signature: Some("signature_base64_encoded_string_here".to_string()),
        };

        let json = serde_json::to_string(&auth_info).unwrap();
        let deserialized: AuthInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(
            serde_json::to_string(&auth_info.id).unwrap(),
            serde_json::to_string(&deserialized.id).unwrap()
        );
        assert_eq!(auth_info.signature, deserialized.signature);
    }

    #[test]
    fn test_user_tree_auth_id() {
        let auth_id = AuthId::UserTree {
            id: "example@eidetica.dev".to_string(),
            tips: vec!["abc123".to_string()],
            key: Box::new(AuthId::Direct("KEY_LAPTOP".to_string())),
        };

        let json = serde_json::to_string(&auth_id).unwrap();
        let deserialized: AuthId = serde_json::from_str(&json).unwrap();

        assert_eq!(
            serde_json::to_string(&auth_id).unwrap(),
            serde_json::to_string(&deserialized).unwrap()
        );
    }

    #[test]
    fn test_auth_key_to_nested_value() {
        let key = AuthKey {
            key: "ed25519:test_key".to_string(),
            permissions: Permission::Read,
            status: KeyStatus::Active,
        };

        let nested_value: Value = key.clone().into();
        if let Value::Map(map) = nested_value {
            // Check that the map contains the expected keys
            assert!(map.get("key").is_some());
            assert!(map.get("permissions").is_some());
            assert!(map.get("status").is_some());

            // Verify the values
            if let Some(Value::String(key_val)) = map.get("key") {
                assert_eq!(key_val, "ed25519:test_key");
            } else {
                panic!("Expected key to be a string");
            }

            if let Some(Value::String(perm_val)) = map.get("permissions") {
                assert_eq!(perm_val, "read");
            } else {
                panic!("Expected permissions to be a string");
            }

            if let Some(Value::String(status_val)) = map.get("status") {
                assert_eq!(status_val, "active");
            } else {
                panic!("Expected status to be a string");
            }
        } else {
            panic!("Expected Value::Map");
        }
    }

    #[test]
    fn test_permission_nested_value_roundtrip() {
        let original = Permission::Write(42);
        let nested: Value = original.clone().into();
        let parsed = Permission::try_from(nested).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_key_status_nested_value_roundtrip() {
        let original = KeyStatus::Revoked;
        let nested: Value = original.clone().into();
        let parsed = KeyStatus::try_from(nested).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_vec_string_nested_value_roundtrip() {
        let original = vec!["tip1".to_string(), "tip2".to_string(), "tip3".to_string()];
        let nested: Value = original.clone().into();
        let parsed = Vec::<String>::try_from(nested).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_auth_id_nested_value_roundtrip() {
        let original = AuthId::Direct("KEY_LAPTOP".to_string());
        let nested: Value = original.clone().into();
        let parsed = AuthId::try_from(nested).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_auth_id_direct_structured_format() {
        let auth_id = AuthId::Direct("KEY_LAPTOP".to_string());
        let nested: Value = auth_id.into();

        if let Value::Map(map) = nested {
            assert_eq!(map.get("type"), Some(&Value::String("direct".to_string())));
            assert_eq!(
                map.get("key_id"),
                Some(&Value::String("KEY_LAPTOP".to_string()))
            );
        } else {
            panic!("Expected Value::Map for Direct AuthId");
        }
    }

    #[test]
    fn test_auth_id_user_tree_structured_format() {
        let auth_id = AuthId::UserTree {
            id: "user@example.com".to_string(),
            tips: vec!["tip1".to_string(), "tip2".to_string()],
            key: Box::new(AuthId::Direct("KEY_LAPTOP".to_string())),
        };

        let nested: Value = auth_id.clone().into();

        if let Value::Map(map) = nested {
            assert_eq!(
                map.get("type"),
                Some(&Value::String("user_tree".to_string()))
            );
            assert_eq!(
                map.get("id"),
                Some(&Value::String("user@example.com".to_string()))
            );

            // Check tips
            if let Some(Value::String(tips_json)) = map.get("tips") {
                let tips: Vec<String> = serde_json::from_str(tips_json).unwrap();
                assert_eq!(tips, vec!["tip1".to_string(), "tip2".to_string()]);
            } else {
                panic!("Expected tips to be a JSON string");
            }

            // Check nested key
            if let Some(nested_key) = map.get("key") {
                if let Value::Map(key_map) = nested_key {
                    assert_eq!(
                        key_map.get("type"),
                        Some(&Value::String("direct".to_string()))
                    );
                    assert_eq!(
                        key_map.get("key_id"),
                        Some(&Value::String("KEY_LAPTOP".to_string()))
                    );
                } else {
                    panic!("Expected nested key to be a map");
                }
            } else {
                panic!("Expected nested key to be present");
            }
        } else {
            panic!("Expected Value::Map for UserTree AuthId");
        }
    }

    #[test]
    fn test_auth_id_user_tree_roundtrip() {
        let original = AuthId::UserTree {
            id: "user@example.com".to_string(),
            tips: vec!["tip1".to_string(), "tip2".to_string()],
            key: Box::new(AuthId::Direct("KEY_LAPTOP".to_string())),
        };

        let nested: Value = original.clone().into();
        let parsed = AuthId::try_from(nested).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_auth_info_nested_value_roundtrip() {
        let original = AuthInfo {
            id: AuthId::Direct("KEY_LAPTOP".to_string()),
            signature: Some("signature_here".to_string()),
        };
        let nested: Value = original.clone().into();
        let parsed = AuthInfo::try_from(nested).unwrap();
        assert_eq!(original.id, parsed.id);
        assert_eq!(original.signature, parsed.signature);
    }

    #[test]
    fn test_auth_info_is_signed_by() {
        // Test signed entry with matching key
        let auth_info_signed = AuthInfo {
            id: AuthId::Direct("TEST_KEY".to_string()),
            signature: Some("signature_data".to_string()),
        };
        assert!(auth_info_signed.is_signed_by("TEST_KEY"));
        assert!(!auth_info_signed.is_signed_by("OTHER_KEY"));

        // Test signed entry without signature
        let auth_info_no_sig = AuthInfo {
            id: AuthId::Direct("TEST_KEY".to_string()),
            signature: None,
        };
        assert!(!auth_info_no_sig.is_signed_by("TEST_KEY"));

        // Test default (unsigned) auth info
        let auth_info_default = AuthInfo::default();
        assert!(!auth_info_default.is_signed_by("ANY_KEY"));

        // Test User Tree auth (should return false since it's not Direct)
        let auth_info_user_tree = AuthInfo {
            id: AuthId::UserTree {
                id: "user_tree".to_string(),
                tips: vec!["tip1".to_string()],
                key: Box::new(AuthId::Direct("nested_key".to_string())),
            },
            signature: Some("signature".to_string()),
        };
        assert!(!auth_info_user_tree.is_signed_by("nested_key"));
    }

    #[test]
    fn test_tree_reference_nested_value_content() {
        let tree_ref = TreeReference {
            root: "root123".to_string(),
            tips: vec!["tip1".to_string(), "tip2".to_string()],
        };

        let nested: Value = tree_ref.into();
        if let Value::Map(map) = nested {
            assert!(map.get("root").is_some());
            assert!(map.get("tips").is_some());
        } else {
            panic!("Expected Value::Map");
        }
    }

    #[test]
    fn test_auth_key_nested_value_roundtrip() {
        let original = AuthKey {
            key: "ed25519:test_key".to_string(),
            permissions: Permission::Write(42),
            status: KeyStatus::Revoked,
        };

        let nested: Value = original.clone().into();
        let parsed = AuthKey::try_from(nested).unwrap();

        assert_eq!(original.key, parsed.key);
        assert_eq!(original.permissions, parsed.permissions);
        assert_eq!(original.status, parsed.status);
    }
}
