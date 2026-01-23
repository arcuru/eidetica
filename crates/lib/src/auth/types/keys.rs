//! Key management types for authentication
//!
//! This module defines types related to authentication keys, signatures,
//! and key resolution.

use serde::{Deserialize, Serialize};

use super::permissions::{KeyStatus, Permission};
use crate::entry::ID;

/// Authentication key configuration stored in _settings.auth
///
/// Keys are indexed by pubkey in AuthSettings. The name field is optional
/// metadata that can be used as a hint in signatures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthKey {
    /// Optional human-readable name for this key
    /// Multiple keys can share the same name (aliases)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Permission level for this key
    permissions: Permission,
    /// Current status of the key
    status: KeyStatus,
}

impl AuthKey {
    /// Create a new AuthKey with validation
    ///
    /// # Arguments
    /// * `name` - Optional human-readable name for this key
    /// * `permissions` - Permission level for this key
    /// * `status` - Current status of the key
    ///
    /// # Examples
    /// ```
    /// use eidetica::auth::types::{AuthKey, Permission, KeyStatus};
    ///
    /// let key = AuthKey::new(
    ///     Some("alice_laptop"),
    ///     Permission::Write(10),
    ///     KeyStatus::Active
    /// );
    /// ```
    pub fn new(
        name: Option<impl Into<String>>,
        permissions: Permission,
        status: KeyStatus,
    ) -> Self {
        Self {
            name: name.map(|n| n.into()),
            permissions,
            status,
        }
    }

    /// Create a new active AuthKey (common case)
    ///
    /// # Arguments
    /// * `name` - Optional human-readable name for this key
    /// * `permissions` - Permission level for this key
    ///
    /// # Examples
    /// ```
    /// use eidetica::auth::types::{AuthKey, Permission};
    ///
    /// let key = AuthKey::active(
    ///     Some("alice_laptop"),
    ///     Permission::Admin(1)
    /// );
    /// ```
    pub fn active(name: Option<impl Into<String>>, permissions: Permission) -> Self {
        Self::new(name, permissions, KeyStatus::Active)
    }

    /// Get the optional name
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
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

    /// Set the name
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
}

/// Step in a delegation path
///
/// References a delegated tree by ID. The final signer hint is stored
/// in the parent SigKey, not in the DelegationStep.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegationStep {
    /// Delegated tree ID (content hash)
    pub tree: String,
    /// Tips of the delegated tree at time of signing
    pub tips: Vec<ID>,
}

/// Key hint for resolving the signer
///
/// Contains explicit fields for each hint type. Exactly one hint field
/// should be set. The hint is used to look up the actual public key
/// in AuthSettings for signature verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct KeyHint {
    /// Public key hint: "ed25519:ABC..." or "*:ed25519:ABC..." for global
    /// For global permissions, it must contain "*:" followed by the FULL pubkey
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
    /// Name hint: "alice_laptop" - searches keys where name matches
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    // TODO: Fingerprint hint (future): "7F8A9B3C..." - matches hash of pubkey
    // This is used in other systems and may be a better option than matching/revealing
    // the full pubkey
    // #[serde(skip_serializing_if = "Option::is_none")]
    // pub fingerprint: Option<String>,
}

impl KeyHint {
    /// Create a hint from a public key
    pub fn from_pubkey(pubkey: impl Into<String>) -> Self {
        Self {
            pubkey: Some(pubkey.into()),
            name: None,
        }
    }

    /// Create a hint from a name
    pub fn from_name(name: impl Into<String>) -> Self {
        Self {
            pubkey: None,
            name: Some(name.into()),
        }
    }

    /// Create a global permission hint with actual signer pubkey
    /// Format: "*:ed25519:ABC..."
    pub fn global(actual_pubkey: impl Into<String>) -> Self {
        Self {
            pubkey: Some(format!("*:{}", actual_pubkey.into())),
            name: None,
        }
    }

    /// Check if this is a global permission hint
    pub fn is_global(&self) -> bool {
        self.pubkey.as_ref().is_some_and(|pk| pk.starts_with("*:"))
    }

    /// Extract the actual pubkey from a global hint
    /// Returns None if not a global hint or no pubkey set
    pub fn global_actual_pubkey(&self) -> Option<&str> {
        self.pubkey.as_ref().and_then(|pk| pk.strip_prefix("*:"))
    }

    /// Check if any hint field is set
    ///
    /// Returns `true` if at least one of `pubkey` or `name` is `Some`.
    ///
    /// # Unsigned Entry Detection
    ///
    /// This method is primarily used to detect **unsigned entries** during validation.
    /// An entry is considered unsigned when:
    /// - The `SigKey` is `Direct` with an empty hint (`!hint.is_set()`)
    /// - The signature field is `None`
    ///
    /// This allows databases to operate without authentication when no auth keys are
    /// configured, supporting both authenticated and unauthenticated use cases.
    ///
    /// ```
    /// # use eidetica::auth::types::KeyHint;
    /// // Empty hint - represents an unsigned entry when combined with no signature
    /// let empty = KeyHint::default();
    /// assert!(!empty.is_set());
    ///
    /// // Hint with pubkey - this entry requires signature verification
    /// let with_pubkey = KeyHint::from_pubkey("ed25519:ABC...");
    /// assert!(with_pubkey.is_set());
    ///
    /// // Hint with name only - also requires signature verification
    /// let with_name = KeyHint::from_name("alice_laptop");
    /// assert!(with_name.is_set());
    /// ```
    pub fn is_set(&self) -> bool {
        self.pubkey.is_some() || self.name.is_some()
    }

    /// Get the hint type as a string (for error messages)
    pub fn hint_type(&self) -> &'static str {
        if self.pubkey.is_some() {
            "pubkey"
        } else if self.name.is_some() {
            "name"
        } else {
            "none"
        }
    }
}

/// Authentication key identifier for entry signing
///
/// Represents the path to resolve the signing key, either directly or through delegation.
/// Uses explicit hint fields to point to the signer's public key.
///
/// # JSON Format
///
/// Uses untagged serialization for compact JSON:
/// - Direct: `{"pubkey": "ed25519:..."}`
/// - Delegation: `{"path": [...], "pubkey": "ed25519:..."}`
///
/// The `path` field distinguishes `Delegation` from `Direct` during deserialization.
///
/// # Variant Ordering
///
/// `Delegation` must be listed before `Direct` because serde tries variants in order.
/// Since `Direct` contains only optional fields, it would match any object if listed first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SigKey {
    // Note: Delegation must be listed before Direct for correct deserialization.
    // The required `path` field distinguishes it; Direct would match anything if first.
    /// Delegation path through other trees
    Delegation {
        /// Path of delegation steps (tree references)
        path: Vec<DelegationStep>,
        /// Final signer hint (resolved in last delegated tree's auth)
        #[serde(flatten)]
        hint: KeyHint,
    },
    /// Direct reference to a key in the current tree's _settings.auth
    Direct(KeyHint),
}

impl Default for SigKey {
    fn default() -> Self {
        SigKey::Direct(KeyHint::default())
    }
}

impl SigKey {
    /// Create a direct key reference from a pubkey
    pub fn from_pubkey(pubkey: impl Into<String>) -> Self {
        SigKey::Direct(KeyHint::from_pubkey(pubkey))
    }

    /// Create a direct key reference from a name
    pub fn from_name(name: impl Into<String>) -> Self {
        SigKey::Direct(KeyHint::from_name(name))
    }

    /// Create a global permission key with actual signer pubkey
    pub fn global(actual_pubkey: impl Into<String>) -> Self {
        SigKey::Direct(KeyHint::global(actual_pubkey))
    }

    /// Get the key hint (for both Direct and Delegation variants)
    pub fn hint(&self) -> &KeyHint {
        match self {
            SigKey::Direct(hint) => hint,
            SigKey::Delegation { hint, .. } => hint,
        }
    }

    /// Get mutable reference to the key hint
    pub fn hint_mut(&mut self) -> &mut KeyHint {
        match self {
            SigKey::Direct(hint) => hint,
            SigKey::Delegation { hint, .. } => hint,
        }
    }

    /// Check if this is a global permission key
    pub fn is_global(&self) -> bool {
        self.hint().is_global()
    }

    /// Check if this SigKey uses a specific pubkey hint
    pub fn has_pubkey_hint(&self, pubkey: &str) -> bool {
        self.hint().pubkey.as_deref() == Some(pubkey)
    }

    /// Check if this SigKey uses a specific name hint
    pub fn has_name_hint(&self, name: &str) -> bool {
        self.hint().name.as_deref() == Some(name)
    }
}

/// Signature information embedded in an entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SigInfo {
    /// Authentication signature - base64-encoded signature bytes
    /// Optional to allow for entry creation before signing
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// Key lookup hint
    pub key: SigKey,
}

impl SigInfo {
    /// Create a new SigInfo with a pubkey hint
    pub fn from_pubkey(pubkey: impl Into<String>) -> Self {
        Self {
            sig: None,
            key: SigKey::from_pubkey(pubkey),
        }
    }

    /// Create a new SigInfo with a name hint
    pub fn from_name(name: impl Into<String>) -> Self {
        Self {
            sig: None,
            key: SigKey::from_name(name),
        }
    }

    /// Create a new SigInfo for global permission
    pub fn global(actual_pubkey: impl Into<String>) -> Self {
        Self {
            sig: None,
            key: SigKey::global(actual_pubkey),
        }
    }

    /// Check if this is a global permission signature
    pub fn is_global(&self) -> bool {
        self.key.is_global()
    }

    /// Get the key hint
    pub fn hint(&self) -> &KeyHint {
        self.key.hint()
    }

    /// Create a new SigInfoBuilder for constructing SigInfo instances
    pub fn builder() -> SigInfoBuilder {
        SigInfoBuilder::new()
    }

    /// Check if this represents an unsigned/unauthenticated entry.
    ///
    /// An entry is unsigned when `SigInfo` is in its default state:
    /// - Direct SigKey (not Delegation)
    /// - Empty KeyHint (no pubkey, no name)
    /// - No signature
    pub fn is_unsigned(&self) -> bool {
        matches!(self.key, SigKey::Direct(ref hint) if !hint.is_set()) && self.sig.is_none()
    }

    /// Check if this represents a malformed/inconsistent signature state.
    ///
    /// Returns `Some(reason)` if malformed, `None` if valid.
    ///
    /// Malformed states:
    /// - Direct with hint but no signature (can't verify without signature)
    /// - Direct with signature but no hint (can't verify without knowing which key)
    /// - Delegation with no signature (delegation always requires signature)
    pub fn malformed_reason(&self) -> Option<&'static str> {
        match &self.key {
            SigKey::Direct(hint) => {
                if hint.is_set() && self.sig.is_none() {
                    Some("entry has key hint but no signature")
                } else if !hint.is_set() && self.sig.is_some() {
                    Some("entry has signature but no key hint")
                } else {
                    None
                }
            }
            SigKey::Delegation { .. } => {
                if self.sig.is_none() {
                    Some("delegation entry requires a signature")
                } else {
                    None
                }
            }
        }
    }
}

/// Builder for constructing SigInfo instances
///
/// This builder provides a fluent interface for creating SigInfo objects.
#[derive(Debug, Clone, Default)]
pub struct SigInfoBuilder {
    sig: Option<String>,
    key: Option<SigKey>,
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

    /// Set the authentication key reference
    pub fn key(mut self, key: SigKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Set a pubkey hint
    pub fn pubkey_hint(mut self, pubkey: impl Into<String>) -> Self {
        self.key = Some(SigKey::from_pubkey(pubkey));
        self
    }

    /// Set a name hint
    pub fn name_hint(mut self, name: impl Into<String>) -> Self {
        self.key = Some(SigKey::from_name(name));
        self
    }

    /// Set a global permission hint with actual signer pubkey
    pub fn global_hint(mut self, actual_pubkey: impl Into<String>) -> Self {
        self.key = Some(SigKey::global(actual_pubkey));
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
