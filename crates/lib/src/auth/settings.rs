//! Authentication settings management for Eidetica
//!
//! This module provides a wrapper around Doc for managing authentication
//! settings. Keys are indexed by pubkey to prevent collision bugs.
//! Names are optional metadata that can be used as hints in signatures.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::errors::AuthError;
use crate::{
    Result,
    auth::{
        crypto::parse_public_key,
        types::{AuthKey, DelegatedTreeRef, KeyHint, KeyStatus, Permission, ResolvedAuth, SigKey},
    },
    crdt::{Doc, doc::Value},
    entry::ID,
};

/// Authentication settings view/interface over Doc data
///
/// Keys are stored by pubkey in the "keys" sub-object.
/// Delegations are stored by root tree ID in the "delegations" sub-object.
/// Global permission uses the special "*" pubkey.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSettings {
    /// Doc data from _settings.auth - this is a view, not the authoritative copy
    inner: Doc,
}

impl From<Doc> for AuthSettings {
    fn from(doc: Doc) -> Self {
        Self { inner: doc }
    }
}

impl From<AuthSettings> for Doc {
    fn from(settings: AuthSettings) -> Doc {
        settings.inner
    }
}

impl AuthSettings {
    /// Create a new empty auth settings view
    pub fn new() -> Self {
        Self { inner: Doc::new() }
    }

    /// Get the underlying Doc for direct access
    pub fn as_doc(&self) -> &Doc {
        &self.inner
    }

    /// Get mutable access to the underlying Doc
    pub fn as_doc_mut(&mut self) -> &mut Doc {
        &mut self.inner
    }

    // ==================== Key Operations ====================

    /// Add a new authentication key by pubkey (fails if key already exists)
    ///
    /// # Arguments
    /// * `pubkey` - The public key string (e.g., "ed25519:ABC...")
    /// * `key` - The AuthKey containing permissions, status, and optional name
    pub fn add_key(&mut self, pubkey: impl Into<String>, key: AuthKey) -> Result<()> {
        let pubkey = pubkey.into();

        // Validate pubkey format (allow "*" for global)
        if pubkey != "*" {
            parse_public_key(&pubkey)?;
        }

        // Check if key already exists
        if self.get_key_by_pubkey(&pubkey).is_ok() {
            return Err(AuthError::KeyAlreadyExists { key_name: pubkey }.into());
        }

        self.inner.set(format!("keys.{pubkey}"), key);
        Ok(())
    }

    /// Explicitly overwrite an existing authentication key
    pub fn overwrite_key(&mut self, pubkey: impl Into<String>, key: AuthKey) -> Result<()> {
        let pubkey = pubkey.into();

        // Validate pubkey format (allow "*" for global)
        if pubkey != "*" {
            parse_public_key(&pubkey)?;
        }

        self.inner.set(format!("keys.{pubkey}"), key);
        Ok(())
    }

    /// Get a key by its public key
    pub fn get_key_by_pubkey(&self, pubkey: &str) -> Result<AuthKey> {
        match self.inner.get(format!("keys.{pubkey}")) {
            Some(Value::Doc(doc)) => AuthKey::try_from(doc).map_err(|e| {
                AuthError::InvalidKeyFormat {
                    reason: e.to_string(),
                }
                .into()
            }),
            Some(_) => Err(AuthError::InvalidKeyFormat {
                reason: format!("key '{pubkey}' is not a Doc"),
            }
            .into()),
            None => Err(AuthError::KeyNotFound {
                key_name: pubkey.to_string(),
            }
            .into()),
        }
    }

    /// Find keys by name (may return multiple if names collide)
    ///
    /// Returns Vec of (pubkey, AuthKey) tuples sorted by pubkey for deterministic ordering.
    pub fn find_keys_by_name(&self, name: &str) -> Vec<(String, AuthKey)> {
        let mut matches = Vec::new();

        // Get all keys and filter by name
        if let Ok(all_keys) = self.get_all_keys() {
            for (pubkey, auth_key) in all_keys {
                if auth_key.name() == Some(name) {
                    matches.push((pubkey, auth_key));
                }
            }
        }

        // Sort by pubkey for deterministic ordering
        matches.sort_by(|a, b| a.0.cmp(&b.0));
        matches
    }

    /// Get all authentication keys
    pub fn get_all_keys(&self) -> Result<HashMap<String, AuthKey>> {
        let mut result: HashMap<String, AuthKey> = HashMap::new();

        // Get the "keys" sub-doc
        if let Some(Value::Doc(keys_doc)) = self.inner.get("keys") {
            for (pubkey, value) in keys_doc.iter() {
                if let Value::Doc(key_doc) = value
                    && let Ok(auth_key) = AuthKey::try_from(key_doc)
                {
                    result.insert(pubkey.clone(), auth_key);
                }
            }
        }

        Ok(result)
    }

    /// Revoke a key by pubkey
    pub fn revoke_key(&mut self, pubkey: &str) -> Result<()> {
        let mut auth_key = self.get_key_by_pubkey(pubkey)?;
        auth_key.set_status(KeyStatus::Revoked);
        self.inner.set(format!("keys.{pubkey}"), auth_key);
        Ok(())
    }

    // ==================== Delegation Operations ====================

    /// Add or update a delegated tree reference
    ///
    /// The delegation is stored by root tree ID, extracted from `tree_ref.tree.root`.
    /// This ensures collision-resistant storage similar to key storage by pubkey.
    pub fn add_delegated_tree(&mut self, tree_ref: DelegatedTreeRef) -> Result<()> {
        let root_id = tree_ref.tree.root.as_str().to_string();
        self.inner.set(format!("delegations.{root_id}"), tree_ref);
        Ok(())
    }

    /// Get a delegated tree reference by root tree ID
    pub fn get_delegated_tree(&self, root_id: &ID) -> Result<DelegatedTreeRef> {
        self.get_delegated_tree_by_str(root_id.as_str())
    }

    /// Get a delegated tree reference by root tree ID string
    ///
    /// This variant accepts a string directly, useful when the ID comes from
    /// a `DelegationStep.tree` field which stores the root ID as a string.
    pub fn get_delegated_tree_by_str(&self, root_id: &str) -> Result<DelegatedTreeRef> {
        match self.inner.get(format!("delegations.{root_id}")) {
            Some(Value::Doc(doc)) => DelegatedTreeRef::try_from(doc).map_err(|e| {
                AuthError::InvalidAuthConfiguration {
                    reason: format!("Invalid delegated tree format: {e}"),
                }
                .into()
            }),
            Some(_) => Err(AuthError::InvalidAuthConfiguration {
                reason: format!("delegation '{root_id}' is not a Doc"),
            }
            .into()),
            None => Err(AuthError::DelegationNotFound {
                tree_id: root_id.to_string(),
            }
            .into()),
        }
    }

    /// Get all delegated tree references
    ///
    /// Returns a map from root tree ID to the delegation reference.
    pub fn get_all_delegated_trees(&self) -> Result<HashMap<ID, DelegatedTreeRef>> {
        let mut result: HashMap<ID, DelegatedTreeRef> = HashMap::new();

        // Get the "delegations" sub-doc
        if let Some(Value::Doc(delegations_doc)) = self.inner.get("delegations") {
            for (root_id_str, value) in delegations_doc.iter() {
                if let Value::Doc(doc) = value
                    && let Ok(tree_ref) = DelegatedTreeRef::try_from(doc)
                {
                    let root_id = ID::new(root_id_str);
                    result.insert(root_id, tree_ref);
                }
            }
        }

        Ok(result)
    }

    // ==================== Key Hint Resolution ====================

    /// Resolve a key hint to matching authentication info
    ///
    /// Returns Vec of ResolvedAuth. For pubkey hints, returns at most one.
    /// For name hints, may return multiple if names collide. Caller should try each
    /// until signature verifies.
    ///
    /// # Name Collision Handling
    ///
    /// When multiple keys share the same name, all matching keys are returned.
    /// The caller (typically `validate_entry`) should iterate through the matches
    /// and attempt signature verification with each until one succeeds.
    pub fn resolve_hint(&self, hint: &KeyHint) -> Result<Vec<ResolvedAuth>> {
        // Handle global permission
        if let Some(actual_pubkey) = hint.global_actual_pubkey() {
            // Global hint - check that global permission exists
            let global_key =
                self.get_key_by_pubkey("*")
                    .map_err(|_| AuthError::InvalidAuthConfiguration {
                        reason: "Global '*' hint used but no global permission configured"
                            .to_string(),
                    })?;

            // Return ResolvedAuth with actual pubkey and global permission
            // There is only 1 global, no need to look for others
            return Ok(vec![ResolvedAuth {
                public_key: parse_public_key(actual_pubkey)?,
                effective_permission: global_key.permissions().clone(),
                key_status: global_key.status().clone(),
            }]);
        }

        // Direct pubkey lookup
        if let Some(pubkey) = &hint.pubkey {
            return match self.get_key_by_pubkey(pubkey) {
                Ok(key) => Ok(vec![ResolvedAuth {
                    public_key: parse_public_key(pubkey)?,
                    effective_permission: key.permissions().clone(),
                    key_status: key.status().clone(),
                }]),
                Err(e) => Err(e),
            };
        }

        // Name lookup - may return multiple matches
        if let Some(name) = &hint.name {
            let matches = self.find_keys_by_name(name);
            if matches.is_empty() {
                return Err(AuthError::KeyNotFound {
                    key_name: name.clone(),
                }
                .into());
            }
            // Convert all matches to ResolvedAuth
            let mut results = Vec::with_capacity(matches.len());
            for (pubkey, auth_key) in matches {
                results.push(ResolvedAuth {
                    public_key: parse_public_key(&pubkey)?,
                    effective_permission: auth_key.permissions().clone(),
                    key_status: auth_key.status().clone(),
                });
            }
            return Ok(results);
        }

        // No hint set - empty/unsigned
        Ok(vec![])
    }

    // ==================== Permission Helpers ====================

    /// Check if global "*" permission exists and is active
    pub fn has_global_permission(&self) -> bool {
        self.get_global_permission().is_some()
    }

    /// Get global "*" permission level if it exists and is active
    pub fn get_global_permission(&self) -> Option<Permission> {
        if let Ok(key) = self.get_key_by_pubkey("*")
            && *key.status() == KeyStatus::Active
        {
            Some(key.permissions().clone())
        } else {
            None
        }
    }

    /// Check if global "*" permission grants sufficient access
    pub fn global_permission_grants_access(&self, requested_permission: &Permission) -> bool {
        if let Some(global_perm) = self.get_global_permission() {
            global_perm >= *requested_permission
        } else {
            false
        }
    }

    // ==================== Access Control ====================

    /// Check if a public key can access the database with the requested permission
    pub fn can_access(&self, pubkey: &str, requested_permission: &Permission) -> bool {
        // First check if there's a specific key entry for this pubkey
        if let Ok(auth_key) = self.get_key_by_pubkey(pubkey)
            && *auth_key.status() == KeyStatus::Active
            && *auth_key.permissions() >= *requested_permission
        {
            return true;
        }

        // Check global permission
        self.global_permission_grants_access(requested_permission)
    }

    /// Find all SigKeys that a public key can use to access this database
    ///
    /// Returns (SigKey, Permission) tuples sorted by permission (highest first)
    pub fn find_all_sigkeys_for_pubkey(&self, pubkey: &str) -> Vec<(SigKey, Permission)> {
        let mut results = Vec::new();

        // Check if this pubkey has a direct key entry
        if let Ok(auth_key) = self.get_key_by_pubkey(pubkey) {
            results.push((SigKey::from_pubkey(pubkey), auth_key.permissions().clone()));
        }

        // Check if global "*" permission exists
        if let Some(global_perm) = self.get_global_permission() {
            results.push((SigKey::global(pubkey), global_perm));
        }

        // FIXME: Check delegation paths
        // This would search for delegation paths that could grant access
        // to device_pubkey. For now, we only support searching direct keys and global "*".

        // Sort by permission, highest first (reverse sort since Permission Ord has higher > lower)
        results.sort_by(|a, b| b.1.cmp(&a.1));
        results
    }

    /// Resolve which SigKey should be used for an operation
    ///
    /// Returns the SigKey with highest permission for the given pubkey.
    pub fn resolve_sig_key_for_operation(&self, pubkey: &str) -> Result<(SigKey, Permission)> {
        let matches = self.find_all_sigkeys_for_pubkey(pubkey);

        matches.into_iter().next().ok_or_else(|| {
            AuthError::PermissionDenied {
                reason: format!("No active key found for pubkey: {pubkey}"),
            }
            .into()
        })
    }

    // ==================== Key Modification Authorization ====================

    /// Check if a signing key can modify an existing target key
    pub fn can_modify_key(&self, signing_key: &ResolvedAuth, target_pubkey: &str) -> Result<bool> {
        // Must have admin permissions to modify keys
        if !signing_key.effective_permission.can_admin() {
            return Ok(false);
        }

        // Get target key info
        let target_key = self.get_key_by_pubkey(target_pubkey)?;

        // Signing key must be >= target key permissions
        Ok(signing_key.effective_permission >= *target_key.permissions())
    }

    /// Check if a signing key can create a new key with the specified permissions
    pub fn can_create_key(
        &self,
        signing_key: &ResolvedAuth,
        new_key_permissions: &Permission,
    ) -> Result<bool> {
        // Must have admin permissions to create keys
        if !signing_key.effective_permission.can_admin() {
            return Ok(false);
        }

        // Signing key must be >= new key permissions
        Ok(signing_key.effective_permission >= *new_key_permissions)
    }
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{crypto::format_public_key, generate_keypair},
        crdt::CRDT,
    };

    fn generate_public_key() -> String {
        let (_, verifying_key) = generate_keypair();
        format_public_key(&verifying_key)
    }

    #[test]
    fn test_auth_settings_basic_operations() {
        let mut settings = AuthSettings::new();

        let pubkey = generate_public_key();
        let auth_key = AuthKey::active(Some("laptop"), Permission::Write(10));

        settings.add_key(&pubkey, auth_key.clone()).unwrap();

        // Retrieve the key
        let retrieved = settings.get_key_by_pubkey(&pubkey).unwrap();
        assert_eq!(retrieved.name(), Some("laptop"));
        assert_eq!(retrieved.permissions(), auth_key.permissions());
        assert_eq!(retrieved.status(), auth_key.status());
    }

    #[test]
    fn test_find_keys_by_name() {
        let mut settings = AuthSettings::new();

        let pubkey1 = generate_public_key();
        let pubkey2 = generate_public_key();

        // Add two keys with same name
        settings
            .add_key(
                &pubkey1,
                AuthKey::active(Some("device"), Permission::Write(10)),
            )
            .unwrap();
        settings
            .add_key(
                &pubkey2,
                AuthKey::active(Some("device"), Permission::Admin(1)),
            )
            .unwrap();

        // Find by name should return both
        let matches = settings.find_keys_by_name("device");
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_revoke_key() {
        let mut settings = AuthSettings::new();

        let pubkey = generate_public_key();
        let auth_key = AuthKey::active(Some("laptop"), Permission::Admin(5));

        settings.add_key(&pubkey, auth_key).unwrap();

        // Revoke the key
        settings.revoke_key(&pubkey).unwrap();

        // Check that it's revoked
        let retrieved = settings.get_key_by_pubkey(&pubkey).unwrap();
        assert_eq!(retrieved.status(), &KeyStatus::Revoked);
    }

    #[test]
    fn test_global_permission() {
        let mut settings = AuthSettings::new();

        // No global permission initially
        assert!(!settings.has_global_permission());
        assert_eq!(settings.get_global_permission(), None);

        // Add global Write(10) permission
        let global_key = AuthKey::active(None::<String>, Permission::Write(10));
        settings.add_key("*", global_key).unwrap();

        // Global permission should now be detected
        assert!(settings.has_global_permission());
        assert_eq!(
            settings.get_global_permission(),
            Some(Permission::Write(10))
        );

        // Test permission granting
        assert!(settings.global_permission_grants_access(&Permission::Read));
        assert!(settings.global_permission_grants_access(&Permission::Write(10)));
        assert!(!settings.global_permission_grants_access(&Permission::Write(5)));
        assert!(!settings.global_permission_grants_access(&Permission::Admin(10)));
    }

    #[test]
    fn test_resolve_hint_pubkey() {
        let mut settings = AuthSettings::new();

        let pubkey = generate_public_key();
        settings
            .add_key(
                &pubkey,
                AuthKey::active(Some("laptop"), Permission::Write(10)),
            )
            .unwrap();

        // Resolve by pubkey hint
        let hint = KeyHint::from_pubkey(&pubkey);
        let matches = settings.resolve_hint(&hint).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(format_public_key(&matches[0].public_key), pubkey);
        assert_eq!(matches[0].effective_permission, Permission::Write(10));
    }

    #[test]
    fn test_resolve_hint_name() {
        let mut settings = AuthSettings::new();

        let pubkey = generate_public_key();
        settings
            .add_key(
                &pubkey,
                AuthKey::active(Some("laptop"), Permission::Write(10)),
            )
            .unwrap();

        // Resolve by name hint
        let hint = KeyHint::from_name("laptop");
        let matches = settings.resolve_hint(&hint).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(format_public_key(&matches[0].public_key), pubkey);
        assert_eq!(matches[0].effective_permission, Permission::Write(10));
    }

    #[test]
    fn test_resolve_hint_global() {
        let mut settings = AuthSettings::new();

        // Add global permission
        settings
            .add_key("*", AuthKey::active(None::<String>, Permission::Write(10)))
            .unwrap();

        let actual_pubkey = generate_public_key();
        let hint = KeyHint::global(&actual_pubkey);
        let matches = settings.resolve_hint(&hint).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(format_public_key(&matches[0].public_key), actual_pubkey);
        assert_eq!(matches[0].effective_permission, Permission::Write(10));
    }

    #[test]
    fn test_find_all_sigkeys_for_pubkey() {
        let mut settings = AuthSettings::new();

        let pubkey = generate_public_key();

        // No keys - should return empty vec
        let results = settings.find_all_sigkeys_for_pubkey(&pubkey);
        assert_eq!(results.len(), 0);

        // Add direct key
        settings
            .add_key(
                &pubkey,
                AuthKey::active(Some("device1"), Permission::Write(5)),
            )
            .unwrap();

        let results = settings.find_all_sigkeys_for_pubkey(&pubkey);
        assert_eq!(results.len(), 1);

        // Add global permission
        settings
            .add_key("*", AuthKey::active(None::<String>, Permission::Write(10)))
            .unwrap();

        let results = settings.find_all_sigkeys_for_pubkey(&pubkey);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_resolve_sig_key_for_operation() {
        let mut settings = AuthSettings::new();

        let pubkey = generate_public_key();

        // No keys configured - should fail
        let result = settings.resolve_sig_key_for_operation(&pubkey);
        assert!(result.is_err());

        // Add device key
        settings
            .add_key(
                &pubkey,
                AuthKey::active(Some("device"), Permission::Write(5)),
            )
            .unwrap();

        // Should resolve to direct pubkey
        let (sig_key, granted_perm) = settings.resolve_sig_key_for_operation(&pubkey).unwrap();
        assert!(sig_key.has_pubkey_hint(&pubkey));
        assert_eq!(granted_perm, Permission::Write(5));
    }

    #[test]
    fn test_can_access() {
        let mut settings = AuthSettings::new();

        let pubkey = generate_public_key();
        let other_pubkey = generate_public_key();

        // No access without keys
        assert!(!settings.can_access(&pubkey, &Permission::Read));

        // Add specific key
        settings
            .add_key(
                &pubkey,
                AuthKey::active(Some("device"), Permission::Write(5)),
            )
            .unwrap();

        // Specific key should have access
        assert!(settings.can_access(&pubkey, &Permission::Read));
        assert!(settings.can_access(&pubkey, &Permission::Write(5)));
        assert!(!settings.can_access(&pubkey, &Permission::Admin(1)));

        // Other key should not have access
        assert!(!settings.can_access(&other_pubkey, &Permission::Read));

        // Add global permission
        settings
            .add_key("*", AuthKey::active(None::<String>, Permission::Read))
            .unwrap();

        // Other key should now have read access via global
        assert!(settings.can_access(&other_pubkey, &Permission::Read));
        assert!(!settings.can_access(&other_pubkey, &Permission::Write(10)));
    }

    #[test]
    fn test_auth_settings_merge() {
        let mut settings1 = AuthSettings::new();
        let mut settings2 = AuthSettings::new();

        let pubkey1 = generate_public_key();
        let pubkey2 = generate_public_key();

        settings1
            .add_key(
                &pubkey1,
                AuthKey::active(Some("key1"), Permission::Write(10)),
            )
            .unwrap();
        settings2
            .add_key(
                &pubkey2,
                AuthKey::active(Some("key2"), Permission::Admin(5)),
            )
            .unwrap();

        // Merge at Doc level
        let merged_doc = settings1.as_doc().merge(settings2.as_doc()).unwrap();
        let merged_settings: AuthSettings = merged_doc.into();

        // Both keys should be present
        assert!(merged_settings.get_key_by_pubkey(&pubkey1).is_ok());
        assert!(merged_settings.get_key_by_pubkey(&pubkey2).is_ok());
    }
}
