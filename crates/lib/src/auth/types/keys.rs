//! Key management types for authentication
//!
//! This module defines types related to authentication keys, signatures,
//! and key resolution.

use serde::{Deserialize, Serialize};

use super::permissions::{KeyStatus, Permission};
use crate::{Result, auth::crypto::parse_public_key, entry::ID};

/// Authentication key configuration stored in _settings.auth
///
/// All fields are private to ensure validation through constructors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthKey {
    /// Public key with crypto-agility prefix
    /// Currently only supports ed25519 format: "ed25519:<base64_url_unpadded_key>"
    pubkey: String,
    /// Permission level for this key
    permissions: Permission,
    /// Current status of the key
    status: KeyStatus,
}

impl AuthKey {
    /// Create a new AuthKey with validation
    ///
    /// This validates the public key format and ensures all fields are valid.
    /// Prefer this over direct struct construction for better error handling.
    ///
    /// # Arguments
    /// * `pubkey` - Ed25519 public key in format "ed25519:<base64_key>"
    /// * `permissions` - Permission level for this key
    /// * `status` - Current status of the key
    ///
    /// # Returns
    /// Result containing the AuthKey or an AuthError if validation fails
    ///
    /// # Examples
    /// ```
    /// use eidetica::auth::types::{AuthKey, Permission, KeyStatus};
    /// use eidetica::auth::crypto::{generate_keypair, format_public_key};
    ///
    /// // Generate a valid key for the example
    /// let (_, verifying_key) = generate_keypair();
    /// let pubkey = format_public_key(&verifying_key);
    ///
    /// let key = AuthKey::new(
    ///     &pubkey,
    ///     Permission::Write(10),
    ///     KeyStatus::Active
    /// )?;
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn new(
        pubkey: impl Into<String>,
        permissions: Permission,
        status: KeyStatus,
    ) -> Result<Self> {
        let pubkey = pubkey.into();

        // Validate public key format (allow wildcard "*")
        if pubkey != "*" {
            parse_public_key(&pubkey)?;
        }

        Ok(Self {
            pubkey,
            permissions,
            status,
        })
    }

    /// Create a new active AuthKey (common case)
    ///
    /// This is a convenience constructor for creating active keys.
    ///
    /// # Arguments
    /// * `pubkey` - Ed25519 public key in format "ed25519:<base64_key>"
    /// * `permissions` - Permission level for this key
    ///
    /// # Returns
    /// Result containing the active AuthKey or an AuthError if validation fails
    ///
    /// # Examples
    /// ```
    /// use eidetica::auth::types::{AuthKey, Permission};
    /// use eidetica::auth::crypto::{generate_keypair, format_public_key};
    ///
    /// // Generate a valid key for the example
    /// let (_, verifying_key) = generate_keypair();
    /// let pubkey = format_public_key(&verifying_key);
    ///
    /// let key = AuthKey::active(
    ///     &pubkey,
    ///     Permission::Admin(1)
    /// )?;
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn active(pubkey: impl Into<String>, permissions: Permission) -> Result<Self> {
        Self::new(pubkey, permissions, KeyStatus::Active)
    }

    /// Validate the format of this AuthKey
    ///
    /// This can be called on existing AuthKey instances to ensure they're valid.
    /// Useful for validating keys that were created through direct construction
    /// or deserialized from storage.
    ///
    /// # Returns
    /// Result indicating success or an AuthError if validation fails
    pub fn validate(&self) -> Result<()> {
        parse_public_key(&self.pubkey)?;
        Ok(())
    }

    /// Get the public key
    pub fn pubkey(&self) -> &str {
        &self.pubkey
    }

    /// Get the permissions
    pub fn permissions(&self) -> &Permission {
        &self.permissions
    }

    /// Get the status
    pub fn status(&self) -> &KeyStatus {
        &self.status
    }

    /// Set the status (e.g., for revocation)
    pub fn set_status(&mut self, status: KeyStatus) {
        self.status = status;
    }

    /// Set the permissions (e.g., for updates)
    pub fn set_permissions(&mut self, permissions: Permission) {
        self.permissions = permissions;
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
    /// Direct reference to a key name in the current tree's _settings.auth
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
    /// Check if this SigKey ultimately resolves to a specific key name
    pub fn is_signed_by(&self, key_name: &str) -> bool {
        match self {
            SigKey::Direct(id) => id == key_name,
            SigKey::DelegationPath(steps) => {
                // Check the final step in the delegation path
                if let Some(last_step) = steps.last() {
                    last_step.key == key_name
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
    /// Either a direct key name defined in this tree's _settings.auth,
    /// or a delegation path as an ordered list of {"key": "delegated_tree_1", "tips": ["A", "B"]}.
    /// The last element in the delegation path must contain only a "key" field.
    /// This represents the path that needs to be traversed to find the public key of the signing key.
    pub key: SigKey,
    /// Actual signer's public key for wildcard permissions
    /// When using SigKey::Direct("*"), this field MUST contain the actual public key
    /// of the signer since the "*" auth setting has pubkey="*" which is not a real key.
    /// Optional for regular keys where the public key is stored in auth settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
}

impl SigInfo {
    /// Check if this SigInfo was signed by a specific key name
    ///
    /// For direct keys, this checks if the key name matches.
    /// For delegated trees, this checks if the final key in the delegation path matches the given key name.
    pub fn is_signed_by(&self, key_name: &str) -> bool {
        self.key.is_signed_by(key_name)
    }

    /// Create a new SigInfoBuilder for constructing SigInfo instances
    pub fn builder() -> SigInfoBuilder {
        SigInfoBuilder::new()
    }
}

/// Builder for constructing SigInfo instances
///
/// This builder provides a fluent interface for creating SigInfo objects,
/// making it easier to set optional fields like pubkey for global permissions.
#[derive(Debug, Clone, Default)]
pub struct SigInfoBuilder {
    sig: Option<String>,
    key: Option<SigKey>,
    pubkey: Option<String>,
}

impl SigInfoBuilder {
    /// Create a new empty SigInfoBuilder
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the signature (base64-encoded signature bytes)
    pub fn sig(mut self, sig: impl Into<String>) -> Self {
        self.sig = Some(sig.into());
        self
    }

    /// Set the authentication key reference path
    pub fn key(mut self, key: SigKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Set the full public key (for global permissions)
    ///
    /// This is only necessary when using global permissions '*' where the public key
    /// needs to be embedded directly rather than resolved through key references.
    pub fn pubkey(mut self, pubkey: impl Into<String>) -> Self {
        self.pubkey = Some(pubkey.into());
        self
    }

    /// Build the final SigInfo instance
    ///
    /// # Panics
    /// Panics if key is not set, as it's a required field.
    pub fn build(self) -> SigInfo {
        SigInfo {
            sig: self.sig,
            key: self.key.expect("key is required for SigInfo"),
            pubkey: self.pubkey,
        }
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
