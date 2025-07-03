//! Core authentication data structures for Eidetica
//!
//! This module defines the fundamental types for authentication, including permissions,
//! key management, and authentication identifiers used in the system.

use crate::entry::ID;
use serde::{Deserialize, Serialize};

/// Permission levels for authenticated operations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Permission {
    /// Full administrative access including settings and key management
    /// Priority may be used for conflict resolution, lower number = higher priority
    /// Admin keys always have priority over Write keys
    Admin(u32),
    /// Read and write access to data (excludes settings modifications)
    /// Priority may be used for conflict resolution, lower number = higher priority
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
    /// Read = 0, Write(p) = 1 + u32::MAX - p, Admin(p) = 2 + (2 * u32::MAX) - p
    /// This ensures Admin permissions are always > Write permissions
    fn ordering_value(&self) -> u64 {
        match self {
            Permission::Read => 0,
            Permission::Write(p) => 1 + (u32::MAX as u64) - (*p as u64),
            Permission::Admin(p) => 2 + (2 * u32::MAX as u64) - (*p as u64),
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
    /// Used for delegated tree delegation to ensure users cannot escalate
    /// their permissions beyond what was granted in the main tree.
    /// Returns the minimum of self and max_permission.
    pub fn clamp_to(&self, max_permission: &Permission) -> Permission {
        use std::cmp::min;
        min(self.clone(), max_permission.clone())
    }

    /// Clamp permissions within bounds (for delegated trees)
    ///
    /// Applies both minimum and maximum bounds from PermissionBounds.
    /// If min is specified and self is below min, raises to min.
    /// If self is above max, lowers to max.
    pub fn clamp_to_bounds(&self, bounds: &PermissionBounds) -> Permission {
        crate::auth::permission::clamp_permission(self.clone(), bounds)
    }
}

/// Status of an authentication key
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum KeyStatus {
    /// Key is active and can create new entries
    Active,
    /// Key is revoked - cannot create new entries, but historical entries are preserved
    /// Content of revoked entries is preserved during merges, but cannot be parents of new entries
    Revoked,
}

/// Authentication key configuration stored in _settings.auth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthKey {
    /// Public key with crypto-agility prefix
    /// Currently only supports ed25519 format: "ed25519:<base64_url_unpadded_key>"
    pub pubkey: String,
    /// Permission level for this key
    pub permissions: Permission,
    /// Current status of the key
    pub status: KeyStatus,
}

/// Reference to a Merkle-DAG tree (for delegated trees)
/// TODO: May standardize on this format across the codebase
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TreeReference {
    /// Root entry ID of the referenced tree
    pub root: ID,
    /// Current tip entry IDs of the referenced tree
    pub tips: Vec<ID>,
}

/// User Authentication Tree reference stored in main tree's _settings.auth
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionBounds {
    /// Maximum permission level (required)
    pub max: Permission,
    /// Minimum permission level (optional)
    pub min: Option<Permission>,
}

/// Delegated tree reference stored in main tree's _settings.auth
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DelegatedTreeRef {
    /// Permission bounds for keys from this delegated tree
    #[serde(rename = "permission-bounds")]
    pub permission_bounds: PermissionBounds,
    /// Reference to the delegated tree
    pub tree: TreeReference,
}

impl Default for PermissionBounds {
    fn default() -> Self {
        Self {
            max: Permission::Read,
            min: None,
        }
    }
}

/// Step in a delegation path
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegationStep {
    /// Delegated tree ID or final key name
    pub key: String,
    /// Tips of the delegated tree at time of signing (None for final step)
    pub tips: Option<Vec<ID>>,
}

/// Authentication key identifier for entry signing
///
/// Represents the path to resolve the signing key, either directly or through delegation.
/// Uses a flat list structure instead of recursive nesting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SigKey {
    /// Direct reference to a key ID in the current tree's _settings.auth
    Direct(String),
    /// Flat delegation path as ordered list
    /// Each step except the last contains {"key": "tree_id", "tips": ["A", "B"]}
    /// The final step contains only {"key": "final_key_name"}
    DelegationPath(Vec<DelegationStep>),
}

impl Default for SigKey {
    fn default() -> Self {
        SigKey::Direct(String::new())
    }
}

/// Signature information embedded in an entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SigInfo {
    /// Authentication signature - base64-encoded signature bytes
    /// Optional to allow for entry creation before signing
    pub sig: Option<String>,
    /// Authentication key reference path
    /// Either a direct key ID defined in this tree's _settings.auth,
    /// or a delegation path as an ordered list of {"key": "delegated_tree_1", "tips": ["A", "B"]}.
    /// The last element in the delegation path must contain only a "key" field.
    /// This represents the path that needs to be traversed to find the public key of the signing key.
    pub key: SigKey,
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

impl SigInfo {
    /// Check if this SigInfo was signed by a specific key ID
    ///
    /// For direct keys, this checks if the key ID matches.
    /// For delegated trees, this checks if the final key in the delegation path matches the given key ID.
    pub fn is_signed_by(&self, key_id: &str) -> bool {
        self.key.is_signed_by(key_id)
    }
}

impl SigKey {
    /// Check if this SigKey ultimately resolves to a specific key ID
    pub fn is_signed_by(&self, key_id: &str) -> bool {
        match self {
            SigKey::Direct(id) => id == key_id,
            SigKey::DelegationPath(steps) => {
                // Check the final step in the delegation path
                if let Some(last_step) = steps.last() {
                    last_step.key == key_id
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crdt::Nested;

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
            pubkey: "ed25519:PExACKOW0L7bKAM9mK_mH3L5EDwszC437uRzTqAbxpk".to_string(),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let serialized = serde_json::to_string(&key).unwrap();
        let deserialized: AuthKey = serde_json::from_str(&serialized).unwrap();

        assert_eq!(key.pubkey, deserialized.pubkey);
        assert_eq!(key.permissions, deserialized.permissions);
        assert_eq!(key.status, deserialized.status);
    }

    #[test]
    fn test_sig_info_serialization() {
        let sig_info = SigInfo {
            key: SigKey::Direct("KEY_LAPTOP".to_string()),
            sig: Some("signature_base64_encoded_string_here".to_string()),
        };

        let json = serde_json::to_string(&sig_info).unwrap();
        let deserialized: SigInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(
            serde_json::to_string(&sig_info.key).unwrap(),
            serde_json::to_string(&deserialized.key).unwrap()
        );
        assert_eq!(sig_info.sig, deserialized.sig);
    }

    #[test]
    fn test_delegation_path_sig_key() {
        let sig_key = SigKey::DelegationPath(vec![
            DelegationStep {
                key: "example@eidetica.dev".to_string(),
                tips: Some(vec![ID::new("abc123")]),
            },
            DelegationStep {
                key: "KEY_LAPTOP".to_string(),
                tips: None,
            },
        ]);

        let json = serde_json::to_string(&sig_key).unwrap();
        let deserialized: SigKey = serde_json::from_str(&json).unwrap();

        assert_eq!(
            serde_json::to_string(&sig_key).unwrap(),
            serde_json::to_string(&deserialized).unwrap()
        );
    }

    #[test]
    fn test_auth_key_to_nested_value() {
        let key = AuthKey {
            pubkey: "ed25519:test_key".to_string(),
            permissions: Permission::Read,
            status: KeyStatus::Active,
        };

        let mut nested = Nested::new();
        nested.set_json("test_key", &key).unwrap();

        // Test that we can retrieve it back
        let retrieved: AuthKey = nested.get_json("test_key").unwrap();
        assert_eq!(retrieved.pubkey, key.pubkey);
        assert_eq!(retrieved.permissions, key.permissions);
        assert_eq!(retrieved.status, key.status);
    }

    #[test]
    fn test_permission_nested_value_roundtrip() {
        let original = Permission::Write(42);
        let mut nested = Nested::new();
        nested.set_json("perm", &original).unwrap();
        let parsed: Permission = nested.get_json("perm").unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_key_status_nested_value_roundtrip() {
        let original = KeyStatus::Revoked;
        let mut nested = Nested::new();
        nested.set_json("status", &original).unwrap();
        let parsed: KeyStatus = nested.get_json("status").unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_vec_string_nested_value_roundtrip() {
        let original = vec!["tip1".to_string(), "tip2".to_string(), "tip3".to_string()];
        let mut nested = Nested::new();
        nested.set_json("vec", &original).unwrap();
        let parsed: Vec<String> = nested.get_json("vec").unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_sig_key_nested_value_roundtrip() {
        let original = SigKey::Direct("KEY_LAPTOP".to_string());
        let mut nested = Nested::new();
        nested.set_json("sig_key", &original).unwrap();
        let parsed: SigKey = nested.get_json("sig_key").unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_sig_key_direct_format() {
        let sig_key = SigKey::Direct("KEY_LAPTOP".to_string());
        let mut nested = Nested::new();
        nested.set_json("sig_key", &sig_key).unwrap();

        // Test that we can retrieve it back correctly
        let retrieved: SigKey = nested.get_json("sig_key").unwrap();
        assert_eq!(retrieved, sig_key);
    }

    #[test]
    fn test_sig_key_delegation_path_format() {
        let sig_key = SigKey::DelegationPath(vec![
            DelegationStep {
                key: "user@example.com".to_string(),
                tips: Some(vec![ID::new("tip1"), ID::new("tip2")]),
            },
            DelegationStep {
                key: "KEY_LAPTOP".to_string(),
                tips: None,
            },
        ]);

        let mut nested = Nested::new();
        nested.set_json("sig_key", &sig_key).unwrap();

        // Test that we can retrieve it back correctly
        let retrieved: SigKey = nested.get_json("sig_key").unwrap();
        assert_eq!(retrieved, sig_key);
    }

    #[test]
    fn test_sig_key_delegation_path_roundtrip() {
        let original = SigKey::DelegationPath(vec![
            DelegationStep {
                key: "user@example.com".to_string(),
                tips: Some(vec![ID::new("tip1"), ID::new("tip2")]),
            },
            DelegationStep {
                key: "KEY_LAPTOP".to_string(),
                tips: None,
            },
        ]);

        let mut nested = Nested::new();
        nested.set_json("sig_key", &original).unwrap();
        let parsed: SigKey = nested.get_json("sig_key").unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_sig_info_nested_value_roundtrip() {
        let original = SigInfo {
            key: SigKey::Direct("KEY_LAPTOP".to_string()),
            sig: Some("signature_here".to_string()),
        };
        let mut nested = Nested::new();
        nested.set_json("sig_info", &original).unwrap();
        let parsed: SigInfo = nested.get_json("sig_info").unwrap();
        assert_eq!(original.key, parsed.key);
        assert_eq!(original.sig, parsed.sig);
    }

    #[test]
    fn test_tree_reference_nested_value_content() {
        let tree_ref = TreeReference {
            root: ID::new("root123"),
            tips: vec![ID::new("tip1"), ID::new("tip2")],
        };

        let mut nested = Nested::new();
        nested.set_json("tree_ref", &tree_ref).unwrap();

        // Test that we can retrieve it back correctly
        let retrieved: TreeReference = nested.get_json("tree_ref").unwrap();
        assert_eq!(retrieved.root, tree_ref.root);
        assert_eq!(retrieved.tips, tree_ref.tips);
    }

    #[test]
    fn test_permission_bounds_clamping() {
        // Test permission clamping with bounds
        let bounds = PermissionBounds {
            max: Permission::Write(10),
            min: Some(Permission::Read),
        };

        // Test clamping admin to write (max bound)
        let admin_perm = Permission::Admin(5);
        assert_eq!(admin_perm.clamp_to_bounds(&bounds), Permission::Write(10));

        // Test clamping Write(5) to Write(10) (Write(5) exceeds max)
        let write_perm = Permission::Write(5);
        assert_eq!(write_perm.clamp_to_bounds(&bounds), Permission::Write(10));

        // Test minimum bound enforcement when no minimum specified
        let bounds_no_min = PermissionBounds {
            max: Permission::Admin(5),
            min: None,
        };

        let read_perm = Permission::Read;
        assert_eq!(read_perm.clamp_to_bounds(&bounds_no_min), Permission::Read);
    }

    #[test]
    fn test_delegated_tree_ref_serialization() {
        let bounds = PermissionBounds {
            max: Permission::Write(10),
            min: Some(Permission::Read),
        };

        let tree_ref = DelegatedTreeRef {
            permission_bounds: bounds,
            tree: TreeReference {
                root: ID::new("root123"),
                tips: vec![ID::new("tip1")],
            },
        };

        let mut nested = Nested::new();
        nested.set_json("tree_ref", &tree_ref).unwrap();
        let parsed: DelegatedTreeRef = nested.get_json("tree_ref").unwrap();

        assert_eq!(tree_ref.permission_bounds.max, parsed.permission_bounds.max);
        assert_eq!(tree_ref.permission_bounds.min, parsed.permission_bounds.min);
        assert_eq!(tree_ref.tree.root, parsed.tree.root);
    }

    #[test]
    fn test_option_permission_nested_value_roundtrip() {
        // Test Some(permission)
        let some_perm = Some(Permission::Write(42));
        let mut nested = Nested::new();
        nested.set_json("perm", &some_perm).unwrap();
        let parsed: Option<Permission> = nested.get_json("perm").unwrap();
        assert_eq!(some_perm, parsed);

        // Test None
        let none_perm: Option<Permission> = None;
        let mut nested2 = Nested::new();
        nested2.set_json("perm", &none_perm).unwrap();
        let parsed2: Option<Permission> = nested2.get_json("perm").unwrap();
        assert_eq!(none_perm, parsed2);
    }

    #[test]
    fn test_option_u32_nested_value_roundtrip() {
        // Test Some(u32)
        let some_num = Some(42u32);
        let mut nested = Nested::new();
        nested.set_json("num", some_num).unwrap();
        let parsed: Option<u32> = nested.get_json("num").unwrap();
        assert_eq!(some_num, parsed);

        // Test None
        let none_num: Option<u32> = None;
        let mut nested2 = Nested::new();
        nested2.set_json("num", none_num).unwrap();
        let parsed2: Option<u32> = nested2.get_json("num").unwrap();
        assert_eq!(none_num, parsed2);
    }

    #[test]
    fn test_permission_bounds_nested_value_roundtrip() {
        // Test with both min and max
        let bounds = PermissionBounds {
            max: Permission::Admin(5),
            min: Some(Permission::Read),
        };

        let mut nested = Nested::new();
        nested.set_json("bounds", &bounds).unwrap();
        let parsed: PermissionBounds = nested.get_json("bounds").unwrap();
        assert_eq!(bounds.max, parsed.max);
        assert_eq!(bounds.min, parsed.min);

        // Test with only max
        let bounds_no_min = PermissionBounds {
            max: Permission::Write(10),
            min: None,
        };

        let mut nested2 = Nested::new();
        nested2.set_json("bounds", &bounds_no_min).unwrap();
        let parsed2: PermissionBounds = nested2.get_json("bounds").unwrap();
        assert_eq!(bounds_no_min.max, parsed2.max);
        assert_eq!(bounds_no_min.min, parsed2.min);
    }

    #[test]
    fn test_delegated_tree_ref_complete_roundtrip() {
        let tree_ref = DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: Some(Permission::Read),
            },
            tree: TreeReference {
                root: ID::new("root123"),
                tips: vec![ID::new("tip1"), ID::new("tip2")],
            },
        };

        let mut nested = Nested::new();
        nested.set_json("tree_ref", &tree_ref).unwrap();
        let parsed: DelegatedTreeRef = nested.get_json("tree_ref").unwrap();

        assert_eq!(tree_ref.permission_bounds.max, parsed.permission_bounds.max);
        assert_eq!(tree_ref.permission_bounds.min, parsed.permission_bounds.min);
        assert_eq!(tree_ref.tree.root, parsed.tree.root);
        assert_eq!(tree_ref.tree.tips, parsed.tree.tips);
    }

    #[test]
    fn test_auth_key_nested_value_roundtrip() {
        let original = AuthKey {
            pubkey: "ed25519:test_key".to_string(),
            permissions: Permission::Write(42),
            status: KeyStatus::Revoked,
        };

        let mut nested = Nested::new();
        nested.set_json("auth_key", &original).unwrap();
        let parsed: AuthKey = nested.get_json("auth_key").unwrap();

        assert_eq!(original.pubkey, parsed.pubkey);
        assert_eq!(original.permissions, parsed.permissions);
        assert_eq!(original.status, parsed.status);
    }

    #[test]
    fn test_sig_key_is_signed_by() {
        // Test direct key
        let direct_key = SigKey::Direct("KEY_LAPTOP".to_string());
        assert!(direct_key.is_signed_by("KEY_LAPTOP"));
        assert!(!direct_key.is_signed_by("KEY_DESKTOP"));

        // Test delegation path
        let delegation_path = SigKey::DelegationPath(vec![
            DelegationStep {
                key: "user@example.com".to_string(),
                tips: Some(vec![ID::new("tip1")]),
            },
            DelegationStep {
                key: "KEY_LAPTOP".to_string(),
                tips: None,
            },
        ]);
        assert!(delegation_path.is_signed_by("KEY_LAPTOP"));
        assert!(!delegation_path.is_signed_by("user@example.com"));
        assert!(!delegation_path.is_signed_by("KEY_DESKTOP"));

        // Test empty delegation path
        let empty_path = SigKey::DelegationPath(vec![]);
        assert!(!empty_path.is_signed_by("KEY_LAPTOP"));
    }

    #[test]
    fn test_delegation_step_serialization() {
        let step = DelegationStep {
            key: "user@example.com".to_string(),
            tips: Some(vec![ID::new("tip1"), ID::new("tip2")]),
        };

        let json = serde_json::to_string(&step).unwrap();
        let deserialized: DelegationStep = serde_json::from_str(&json).unwrap();

        assert_eq!(step.key, deserialized.key);
        assert_eq!(step.tips, deserialized.tips);

        // Test final step (no tips)
        let final_step = DelegationStep {
            key: "KEY_LAPTOP".to_string(),
            tips: None,
        };

        let json = serde_json::to_string(&final_step).unwrap();
        let deserialized: DelegationStep = serde_json::from_str(&json).unwrap();

        assert_eq!(final_step.key, deserialized.key);
        assert_eq!(final_step.tips, deserialized.tips);
    }
}
