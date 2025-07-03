//! Key management types for authentication
//!
//! This module defines types related to authentication keys, signatures,
//! and key resolution.

use crate::entry::ID;
use serde::{Deserialize, Serialize};

use super::permissions::{KeyStatus, Permission};

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

impl SigInfo {
    /// Check if this SigInfo was signed by a specific key ID
    ///
    /// For direct keys, this checks if the key ID matches.
    /// For delegated trees, this checks if the final key in the delegation path matches the given key ID.
    pub fn is_signed_by(&self, key_id: &str) -> bool {
        self.key.is_signed_by(key_id)
    }
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
