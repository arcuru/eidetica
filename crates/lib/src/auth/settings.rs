//! Authentication settings management for Eidetica
//!
//! This module provides a simple wrapper around Map for managing authentication
//! settings. AuthSettings is a view/interface layer over the auth portion of the
//! _settings subtree - it doesn't implement CRDT itself since merging happens at
//! the higher settings level.

use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

use super::errors::AuthError;
use crate::{
    Error, Result,
    auth::{
        types::{AuthKey, DelegatedTreeRef, KeyStatus, Permission, ResolvedAuth, SigKey},
        validation::AuthValidator,
    },
    backend::BackendDB,
    crdt::Doc,
};

/// Authentication settings view/interface over Doc data
///
/// This provides a convenient interface for working with authentication data
/// stored in the _settings.auth subtree. The underlying Doc CRDT handles
/// all merging at the settings level - this is just a view with auth-specific
/// convenience methods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSettings {
    /// Doc data from _settings.auth - this is a view, not the authoritative copy
    inner: Doc,
}

impl AuthSettings {
    /// Create a new empty auth settings view
    pub fn new() -> Self {
        Self { inner: Doc::new() }
    }

    /// Create from existing Doc (e.g., from _settings.auth)
    pub fn from_doc(doc: Doc) -> Self {
        Self { inner: doc }
    }

    /// Get the underlying Doc for direct access
    pub fn as_doc(&self) -> &Doc {
        &self.inner
    }

    /// Get mutable access to the underlying Map
    pub fn as_doc_mut(&mut self) -> &mut Doc {
        &mut self.inner
    }

    /// Add a new authentication key (fails if key already exists)
    ///
    /// This method ensures keys are not accidentally overwritten during operations
    /// like bootstrap sync, preventing key conflicts between devices.
    pub fn add_key(&mut self, key_name: impl Into<String>, key: AuthKey) -> Result<()> {
        let name = key_name.into();

        // Check if key already exists
        if self.get_key(&name).is_ok() {
            return Err(crate::auth::errors::AuthError::KeyAlreadyExists { key_name: name }.into());
        }

        self.inner.set_json(name, key)?;
        Ok(())
    }

    /// Explicitly overwrite an existing authentication key
    ///
    /// Use this method when you intentionally want to replace an existing key.
    /// This provides clear intent and prevents accidental overwrites.
    pub fn overwrite_key(&mut self, key_name: impl Into<String>, key: AuthKey) -> Result<()> {
        let key_name_str = key_name.into();
        self.inner.set_json(key_name_str, key)?;
        Ok(())
    }

    /// Add or update a delegated tree reference
    pub fn add_delegated_tree(
        &mut self,
        key_name: impl Into<String>,
        tree_ref: DelegatedTreeRef,
    ) -> Result<()> {
        let key_name_str = key_name.into();
        self.inner.set_json(key_name_str, tree_ref)?;
        Ok(())
    }

    /// Revoke a key by setting its status to Revoked
    pub fn revoke_key(&mut self, key_name: impl AsRef<str>) -> Result<()> {
        let key_name = key_name.as_ref();
        if self.inner.get(key_name).is_some() {
            match self.inner.get_json::<AuthKey>(key_name) {
                Ok(mut auth_key) => {
                    auth_key.set_status(KeyStatus::Revoked);
                    self.inner.set_json(key_name, auth_key)?;
                    Ok(())
                }
                Err(_) => {
                    // Not an AuthKey, might be a DelegatedTreeRef - for now just error
                    Err(AuthError::CannotRevokeNonKey {
                        key_name: key_name.to_string(),
                    }
                    .into())
                }
            }
        } else {
            Err(AuthError::KeyNotFound {
                key_name: key_name.to_string(),
            }
            .into())
        }
    }

    /// Get a specific key by key name
    pub fn get_key(&self, key_name: impl AsRef<str>) -> Result<AuthKey> {
        match self.inner.get_json::<AuthKey>(key_name.as_ref()) {
            Ok(key) => Ok(key),
            Err(e) if e.is_not_found() => Err(AuthError::KeyNotFound {
                key_name: key_name.as_ref().to_string(),
            }
            .into()),
            Err(e) => Err(AuthError::InvalidKeyFormat {
                reason: e.to_string(),
            }
            .into()),
        }
    }

    /// Get a specific delegated tree reference by key name
    pub fn get_delegated_tree(&self, key_name: impl AsRef<str>) -> Result<DelegatedTreeRef> {
        match self.inner.get_json::<DelegatedTreeRef>(key_name.as_ref()) {
            Ok(tree_ref) => Ok(tree_ref),
            Err(e) if e.is_not_found() => Err(AuthError::KeyNotFound {
                key_name: key_name.as_ref().to_string(),
            }
            .into()),
            Err(e) => Err(AuthError::InvalidAuthConfiguration {
                reason: format!("Invalid delegated tree format: {e}"),
            }
            .into()),
        }
    }

    /// Get all authentication keys
    pub fn get_all_keys(&self) -> Result<HashMap<String, AuthKey>> {
        let mut keys = HashMap::new();
        for (key_name, _) in self.inner.as_hashmap().iter() {
            // Try to parse as AuthKey, skip if it's not one
            if let Ok(auth_key) = self.inner.get_json::<AuthKey>(key_name) {
                keys.insert(key_name.clone(), auth_key);
            }
        }
        Ok(keys)
    }

    /// Get all delegated tree references
    pub fn get_all_delegated_trees(&self) -> Result<HashMap<String, DelegatedTreeRef>> {
        let mut trees = HashMap::new();
        for (tree_id, _) in self.inner.as_hashmap().iter() {
            // Try to parse as DelegatedTreeRef, skip if it's not one
            if let Ok(tree_ref) = self.inner.get_json::<DelegatedTreeRef>(tree_id) {
                trees.insert(tree_id.clone(), tree_ref);
            }
        }
        Ok(trees)
    }

    /// Simple validation for entry creation - checks if auth key name is valid and active
    ///
    /// This is entry-time validation using current settings state only.
    /// No complex merge-time validation is performed.
    pub fn validate_entry_auth(
        &self,
        sig_key: &SigKey,
        backend: Option<&Arc<dyn BackendDB>>,
    ) -> Result<ResolvedAuth> {
        match sig_key {
            SigKey::Direct(key_name) => {
                let auth_key = self.get_key(key_name)?;
                let public_key = crate::auth::crypto::parse_public_key(auth_key.pubkey())?;
                Ok(ResolvedAuth {
                    public_key,
                    effective_permission: auth_key.permissions().clone(),
                    key_status: auth_key.status().clone(),
                })
            }
            SigKey::DelegationPath(_) => {
                // For delegation path entries, validate using the backend
                let backend = backend.ok_or_else(|| {
                    Error::from(AuthError::DatabaseRequired {
                        operation: "delegation path validation".to_string(),
                    })
                })?;

                // Use AuthValidator to resolve the delegation path
                let mut validator = AuthValidator::new();
                validator.resolve_sig_key(sig_key, self, Some(backend))
            }
        }
    }

    /// Check if a signing key can modify an existing target key.
    ///
    /// Only admin keys can modify other keys. Uses the built-in permission ordering
    /// where higher permissions can modify keys with equal or lower permissions.
    /// Returns an error if the target key doesn't exist - use `can_create_key` for creation checks.
    pub fn can_modify_key(
        &self,
        signing_key: &ResolvedAuth,
        target_key_name: impl AsRef<str>,
    ) -> Result<bool> {
        // Must have admin permissions to modify keys
        if !signing_key.effective_permission.can_admin() {
            return Ok(false);
        }

        // Get target key info
        let target_key = self.get_key(target_key_name.as_ref())?;

        // Use the built-in permission ordering: signing key must be >= target key
        Ok(signing_key.effective_permission >= *target_key.permissions())
    }

    /// Check if a signing key can create a new key with the specified permissions.
    ///
    /// Only admin keys can create other keys. The signing key must have permissions
    /// greater than or equal to the new key's permissions to prevent privilege escalation.
    pub fn can_create_key(
        &self,
        signing_key: &ResolvedAuth,
        new_key_permissions: &Permission,
    ) -> Result<bool> {
        // Must have admin permissions to create keys
        if !signing_key.effective_permission.can_admin() {
            return Ok(false);
        }

        // Signing key must be >= new key permissions to prevent privilege escalation
        Ok(signing_key.effective_permission >= *new_key_permissions)
    }

    /// Check if a public key can access the database with the requested permission.
    ///
    /// This method checks both specific key permissions and global '*' permissions
    /// to determine if the given public key has sufficient access.
    ///
    /// FIXME: Needs update to work for delegated keys
    ///
    /// # Arguments
    /// * `pubkey` - The public key to check (e.g., "ed25519:...")
    /// * `requested_permission` - The permission level required
    ///
    /// # Returns
    /// - `true` if the key has sufficient permission (either specific or global)
    /// - `false` if the key lacks sufficient permission
    pub fn can_access(&self, pubkey: &str, requested_permission: &Permission) -> bool {
        // First check if there's a specific key entry that matches this pubkey
        for key_name in self.inner.keys() {
            if let Ok(auth_key) = self.get_key(key_name) {
                // Check if this key matches the requested pubkey and has sufficient permissions
                if auth_key.pubkey() == pubkey
                    && *auth_key.status() == KeyStatus::Active
                    && *auth_key.permissions() >= *requested_permission
                {
                    return true;
                }
            }
        }

        // Check if global '*' permission exists and is sufficient
        self.global_permission_grants_access(requested_permission)
    }

    /// Check if global "*" permission exists and is active.
    ///
    /// # Returns
    /// - `true` if global "*" permission exists with pubkey="*" and status=Active
    /// - `false` otherwise
    #[cfg(test)]
    pub(crate) fn has_active_global_permission(&self) -> bool {
        self.get_global_permission().is_some()
    }

    /// Get global "*" permission level if it exists and is active.
    ///
    /// # Returns
    /// - `Some(Permission)` if global "*" permission is active
    /// - `None` otherwise
    pub(crate) fn get_global_permission(&self) -> Option<Permission> {
        if let Ok(key) = self.get_key("*")
            && key.pubkey() == "*"
            && *key.status() == KeyStatus::Active
        {
            Some(key.permissions().clone())
        } else {
            None
        }
    }

    /// Check if global "*" permission grants sufficient access for the requested permission.
    ///
    /// # Arguments
    /// * `requested_permission` - The permission level required
    ///
    /// # Returns
    /// - `true` if global permission exists and is >= requested_permission
    /// - `false` otherwise
    pub(crate) fn global_permission_grants_access(
        &self,
        requested_permission: &Permission,
    ) -> bool {
        if let Some(global_perm) = self.get_global_permission() {
            global_perm >= *requested_permission
        } else {
            false
        }
    }

    /// Find all SigKeys that a public key can use to access this database.
    ///
    /// Returns all possible SigKey identifiers that match the given public key,
    /// including specific key names and global "*" permission. Results are sorted
    /// by permission level with highest permissions first.
    ///
    /// # Arguments
    /// * `device_pubkey` - The public key of the device (e.g., "Ed25519:abc123...")
    ///
    /// # Returns
    /// A vector of tuples containing (SigKey, Permission), sorted by Permission (highest first)
    /// - Empty vector if no matching keys found
    /// - Multiple entries if the pubkey matches multiple key names or has global access
    pub fn find_all_sigkeys_for_pubkey(&self, device_pubkey: &str) -> Vec<(SigKey, Permission)> {
        let mut results = Vec::new();

        // 1. Search all direct keys for matching pubkey
        for key_name in self.inner.keys() {
            if let Ok(auth_key) = self.get_key(key_name)
                && auth_key.pubkey() == device_pubkey
            {
                results.push((
                    SigKey::Direct(key_name.clone()),
                    auth_key.permissions().clone(),
                ));
            }
        }

        // 2. Check if global "*" permission exists
        if let Some(global_perm) = self.get_global_permission() {
            results.push((SigKey::Direct("*".to_string()), global_perm));
        }

        // FIXME: 3. Check delegation paths
        // This would search for delegation paths that could grant access
        // to device_pubkey. For now, we only support direct keys and global "*".

        // Sort by permission, highest first (reverse sort since Permission Ord has higher > lower)
        results.sort_by(|a, b| b.1.cmp(&a.1));

        results
    }

    /// Resolve which SigKey should be used for an operation based on the device's public key.
    ///
    /// Given a device's public key, searches auth settings to determine the appropriate SigKey
    /// to use in entry signatures. When multiple SigKeys are available for the same public key,
    /// this method returns the one with the highest permission level.
    ///
    /// This method does NOT check key status - it only resolves which key reference should be
    /// used in the SigInfo. Status validation happens later during entry validation.
    ///
    /// Resolution order:
    /// 1. Search all direct keys in auth settings for a matching pubkey
    /// 2. Check if global "*" permission exists
    /// 3. FIXME: Check delegation paths (not yet implemented)
    /// 4. Return the match with highest permission (if multiple matches exist)
    ///
    /// # Arguments
    /// * `device_pubkey` - The public key of the device (e.g., "Ed25519:abc123...")
    ///
    /// # Returns
    /// A tuple of (SigKey to use, granted Permission level) - always returns highest permission when multiple options exist
    pub fn resolve_sig_key_for_operation(
        &self,
        device_pubkey: &str,
    ) -> Result<(SigKey, Permission)> {
        // Use find_all_sigkeys_for_pubkey (returns sorted by highest permission first) and return the first match
        let matches = self.find_all_sigkeys_for_pubkey(device_pubkey);

        matches.into_iter().next().ok_or_else(|| {
            AuthError::PermissionDenied {
                reason: format!("No active key found for pubkey: {device_pubkey}"),
            }
            .into()
        })
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
        auth::{
            generate_public_key,
            types::{KeyStatus, Permission},
        },
        crdt::CRDT,
    };

    #[test]
    fn test_auth_settings_basic_operations() {
        let mut settings = AuthSettings::new();

        // Add a key
        let auth_key = AuthKey::active(generate_public_key(), Permission::Write(10)).unwrap();

        settings.add_key("KEY_LAPTOP", auth_key.clone()).unwrap();

        // Retrieve the key
        let retrieved = settings.get_key("KEY_LAPTOP").unwrap();
        assert_eq!(retrieved.pubkey(), auth_key.pubkey());
        assert_eq!(retrieved.permissions(), auth_key.permissions());
        assert_eq!(retrieved.status(), auth_key.status());
    }

    #[test]
    fn test_revoke_key() {
        let mut settings = AuthSettings::new();

        let auth_key = AuthKey::active(generate_public_key(), Permission::Admin(5)).unwrap();

        settings.add_key("KEY_LAPTOP", auth_key).unwrap();

        // Revoke the key
        settings.revoke_key("KEY_LAPTOP").unwrap();

        // Check that it's revoked
        let retrieved = settings.get_key("KEY_LAPTOP").unwrap();
        assert_eq!(retrieved.status(), &KeyStatus::Revoked);
    }

    #[test]
    fn test_auth_settings_view_operations() {
        let mut settings1 = AuthSettings::new();
        let mut settings2 = AuthSettings::new();

        let key1 = AuthKey::new(
            generate_public_key(),
            Permission::Write(10),
            KeyStatus::Active,
        )
        .unwrap();

        let key2 = AuthKey::new(
            generate_public_key(),
            Permission::Admin(5),
            KeyStatus::Active,
        )
        .unwrap();

        settings1.add_key("KEY_1", key1).unwrap();
        settings2.add_key("KEY_2", key2).unwrap();

        // Test that we can access the underlying Doc for merging at higher level
        let map1 = settings1.as_doc().clone();
        let map2 = settings2.as_doc().clone();

        // This would be done at the higher settings level, not here
        let merged_map = map1.merge(&map2).unwrap();
        let merged_settings = AuthSettings::from_doc(merged_map);

        // Both keys should be present in the merged view
        assert!(merged_settings.get_key("KEY_1").is_ok());
        assert!(merged_settings.get_key("KEY_2").is_ok());
    }

    #[test]
    fn test_priority_based_key_modification() {
        let mut settings = AuthSettings::new();

        // Add high-priority admin key
        let high_priority_key = AuthKey::new(
            generate_public_key(),
            Permission::Admin(1), // High priority
            KeyStatus::Active,
        )
        .unwrap();

        settings
            .add_key("ADMIN_KEY", high_priority_key.clone())
            .unwrap();

        // Create resolved auth for the admin key
        let admin_resolved = ResolvedAuth {
            public_key: crate::auth::crypto::generate_keypair().1,
            effective_permission: high_priority_key.permissions().clone(),
            key_status: high_priority_key.status().clone(),
        };

        // Should be able to create new keys with lower permissions
        assert!(
            settings
                .can_create_key(&admin_resolved, &Permission::Write(20))
                .unwrap()
        );

        // Test with write key (lower privileges)
        let write_resolved = ResolvedAuth {
            public_key: crate::auth::crypto::generate_keypair().1,
            effective_permission: Permission::Write(10),
            key_status: KeyStatus::Active,
        };

        // Write key should not be able to create other keys
        assert!(
            !settings
                .can_create_key(&write_resolved, &Permission::Write(20))
                .unwrap()
        );
    }

    #[test]
    fn test_can_access_comprehensive() {
        use crate::auth::generate_public_key;

        let mut settings = AuthSettings::new();

        // Generate valid keys for testing
        let specific_pubkey = generate_public_key();
        let revoked_pubkey = generate_public_key();
        let test_pubkey = generate_public_key();

        // Test without any permissions
        assert!(!settings.can_access(&test_pubkey, &Permission::Read));
        assert!(!settings.can_access(&test_pubkey, &Permission::Write(10)));

        // Add a specific key with Write(5) permission
        let specific_key = AuthKey::active(&specific_pubkey, Permission::Write(5)).unwrap();
        settings.add_key("laptop_key", specific_key).unwrap();

        // Test specific key access
        assert!(settings.can_access(&specific_pubkey, &Permission::Read));
        assert!(settings.can_access(&specific_pubkey, &Permission::Write(5)));
        assert!(settings.can_access(&specific_pubkey, &Permission::Write(10)));
        assert!(!settings.can_access(&specific_pubkey, &Permission::Write(1))); // Higher permission
        assert!(!settings.can_access(&specific_pubkey, &Permission::Admin(10)));

        // Test that other keys don't have access
        assert!(!settings.can_access(&test_pubkey, &Permission::Read));

        // Add a revoked key
        let revoked_key =
            AuthKey::new(&revoked_pubkey, Permission::Admin(1), KeyStatus::Revoked).unwrap();
        settings.add_key("revoked_key", revoked_key).unwrap();

        // Test revoked key cannot access
        assert!(!settings.can_access(&revoked_pubkey, &Permission::Read));
        assert!(!settings.can_access(&revoked_pubkey, &Permission::Admin(10)));

        // Add global '*' permission with Write(10)
        let global_key = AuthKey::active("*", Permission::Write(10)).unwrap();
        settings.add_key("*", global_key).unwrap();

        // Test global permission allows appropriate access for any key
        let random_key = generate_public_key();
        assert!(settings.can_access(&random_key, &Permission::Read));
        assert!(settings.can_access(&random_key, &Permission::Write(10)));
        assert!(settings.can_access(&random_key, &Permission::Write(15)));

        // Test global permission denies higher privileges
        assert!(!settings.can_access(&random_key, &Permission::Write(5)));
        assert!(!settings.can_access(&random_key, &Permission::Admin(10)));

        // Test that specific key still works and takes precedence when it has higher permission
        assert!(settings.can_access(&specific_pubkey, &Permission::Write(5))); // Specific key has Write(5)
        assert!(!settings.can_access(&random_key, &Permission::Write(5))); // Global only has Write(10)

        // Add revoked global permission
        let revoked_global_key =
            AuthKey::new("*", Permission::Admin(1), KeyStatus::Revoked).unwrap();
        settings.overwrite_key("*", revoked_global_key).unwrap();

        // Test revoked global permission doesn't grant access
        let new_random_key = generate_public_key();
        assert!(!settings.can_access(&new_random_key, &Permission::Read));

        // But specific key should still work
        assert!(settings.can_access(&specific_pubkey, &Permission::Read));

        // Add active global Admin permission
        let admin_global_key = AuthKey::active("*", Permission::Admin(1)).unwrap();
        settings.overwrite_key("*", admin_global_key).unwrap();

        // Test global Admin permission allows everything
        let any_key = generate_public_key();
        assert!(settings.can_access(&any_key, &Permission::Read));
        assert!(settings.can_access(&any_key, &Permission::Write(1)));
        assert!(settings.can_access(&any_key, &Permission::Admin(5)));
    }

    #[test]
    fn test_global_permission_helpers() {
        let mut settings = AuthSettings::new();

        // No global permission initially
        assert!(!settings.has_active_global_permission());
        assert_eq!(settings.get_global_permission(), None);
        assert!(!settings.global_permission_grants_access(&Permission::Read));

        // Add global Write(10) permission
        let global_key = AuthKey::active("*", Permission::Write(10)).unwrap();
        settings.add_key("*", global_key).unwrap();

        // Global permission should now be detected
        assert!(settings.has_active_global_permission());
        assert_eq!(
            settings.get_global_permission(),
            Some(Permission::Write(10))
        );

        // Test permission granting
        assert!(settings.global_permission_grants_access(&Permission::Read));
        assert!(settings.global_permission_grants_access(&Permission::Write(10)));
        assert!(settings.global_permission_grants_access(&Permission::Write(15)));
        assert!(!settings.global_permission_grants_access(&Permission::Write(5)));
        assert!(!settings.global_permission_grants_access(&Permission::Admin(10)));

        // Revoke global permission
        let revoked_global = AuthKey::new("*", Permission::Write(10), KeyStatus::Revoked).unwrap();
        settings.overwrite_key("*", revoked_global).unwrap();

        // Should no longer be detected as active
        assert!(!settings.has_active_global_permission());
        assert_eq!(settings.get_global_permission(), None);
        assert!(!settings.global_permission_grants_access(&Permission::Read));
    }

    #[test]
    fn test_resolve_sig_key_for_operation() {
        use crate::auth::generate_public_key;

        let mut settings = AuthSettings::new();

        let device_pubkey = generate_public_key();
        let device_key_name = "my_device";

        // No keys configured - should fail
        let result = settings.resolve_sig_key_for_operation(&device_pubkey);
        assert!(result.is_err());

        // Add device key with Write(5) permission
        let device_key = AuthKey::active(&device_pubkey, Permission::Write(5)).unwrap();
        settings.add_key(device_key_name, device_key).unwrap();

        // Should resolve to specific device key by searching for matching pubkey
        let (sig_key, granted_perm) = settings
            .resolve_sig_key_for_operation(&device_pubkey)
            .unwrap();
        assert_eq!(sig_key, SigKey::Direct(device_key_name.to_string()));
        assert_eq!(granted_perm, Permission::Write(5));
    }

    #[test]
    fn test_resolve_sig_key_falls_back_to_global() {
        use crate::auth::generate_public_key;

        let mut settings = AuthSettings::new();

        // Add global Write(10) permission
        let global_key = AuthKey::active("*", Permission::Write(10)).unwrap();
        settings.add_key("*", global_key).unwrap();

        // Random device not in auth settings
        let random_pubkey = generate_public_key();

        // Should fall back to global since pubkey not found in auth settings
        let (sig_key, granted_perm) = settings
            .resolve_sig_key_for_operation(&random_pubkey)
            .unwrap();
        assert_eq!(sig_key, SigKey::Direct("*".to_string()));
        assert_eq!(granted_perm, Permission::Write(10));
    }

    #[test]
    fn test_resolve_sig_key_revoked_key_is_resolved() {
        use crate::auth::generate_public_key;

        let mut settings = AuthSettings::new();

        let device_pubkey = generate_public_key();
        let device_key_name = "my_device";

        // Add revoked device key
        let revoked_key =
            AuthKey::new(&device_pubkey, Permission::Admin(1), KeyStatus::Revoked).unwrap();
        settings.add_key(device_key_name, revoked_key).unwrap();

        // Revoked key should still be resolved by pubkey (validation will reject it later)
        let (sig_key, granted_perm) = settings
            .resolve_sig_key_for_operation(&device_pubkey)
            .unwrap();
        assert_eq!(sig_key, SigKey::Direct(device_key_name.to_string()));
        assert_eq!(granted_perm, Permission::Admin(1));
    }

    #[test]
    fn test_resolve_sig_key_with_multiple_keys() {
        use crate::auth::generate_public_key;

        let mut settings = AuthSettings::new();

        let pubkey1 = generate_public_key();
        let pubkey2 = generate_public_key();
        let pubkey3 = generate_public_key();

        // Add multiple device keys
        settings
            .add_key(
                "device1",
                AuthKey::active(&pubkey1, Permission::Write(5)).unwrap(),
            )
            .unwrap();
        settings
            .add_key(
                "device2",
                AuthKey::active(&pubkey2, Permission::Admin(1)).unwrap(),
            )
            .unwrap();

        // Should resolve correct key for each pubkey
        let (sig_key1, perm1) = settings.resolve_sig_key_for_operation(&pubkey1).unwrap();
        assert_eq!(sig_key1, SigKey::Direct("device1".to_string()));
        assert_eq!(perm1, Permission::Write(5));

        let (sig_key2, perm2) = settings.resolve_sig_key_for_operation(&pubkey2).unwrap();
        assert_eq!(sig_key2, SigKey::Direct("device2".to_string()));
        assert_eq!(perm2, Permission::Admin(1));

        // Unknown pubkey should fail (no global fallback)
        let result = settings.resolve_sig_key_for_operation(&pubkey3);
        assert!(result.is_err());

        // Add global permission
        let global_key = AuthKey::active("*", Permission::Write(10)).unwrap();
        settings.add_key("*", global_key).unwrap();

        // Unknown pubkey should now fall back to global
        let (sig_key3, perm3) = settings.resolve_sig_key_for_operation(&pubkey3).unwrap();
        assert_eq!(sig_key3, SigKey::Direct("*".to_string()));
        assert_eq!(perm3, Permission::Write(10));

        // Known pubkeys should still resolve to their specific keys (not global)
        let (sig_key1_after, _) = settings.resolve_sig_key_for_operation(&pubkey1).unwrap();
        assert_eq!(sig_key1_after, SigKey::Direct("device1".to_string()));
    }

    #[test]
    fn test_find_all_sigkeys_for_pubkey() {
        use crate::auth::generate_public_key;

        let mut settings = AuthSettings::new();

        let pubkey = generate_public_key();

        // No keys - should return empty vec
        let results = settings.find_all_sigkeys_for_pubkey(&pubkey);
        assert_eq!(results.len(), 0);

        // Add one specific key
        settings
            .add_key(
                "device1",
                AuthKey::active(&pubkey, Permission::Write(5)).unwrap(),
            )
            .unwrap();

        let results = settings.find_all_sigkeys_for_pubkey(&pubkey);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, SigKey::Direct("device1".to_string()));
        assert_eq!(results[0].1, Permission::Write(5));

        // Add another alias for the same pubkey
        settings
            .add_key(
                "device1_alias",
                AuthKey::active(&pubkey, Permission::Read).unwrap(),
            )
            .unwrap();

        let results = settings.find_all_sigkeys_for_pubkey(&pubkey);
        assert_eq!(results.len(), 2);
        // Both keys should be returned
        let key_names: Vec<String> = results
            .iter()
            .map(|(sk, _)| match sk {
                SigKey::Direct(name) => name.clone(),
                _ => panic!("Expected Direct SigKey"),
            })
            .collect();
        assert!(key_names.contains(&"device1".to_string()));
        assert!(key_names.contains(&"device1_alias".to_string()));

        // Add global permission
        let global_key = AuthKey::active("*", Permission::Write(10)).unwrap();
        settings.add_key("*", global_key).unwrap();

        let results = settings.find_all_sigkeys_for_pubkey(&pubkey);
        assert_eq!(results.len(), 3); // Two specific keys + global

        // Global should be in the results
        let has_global = results
            .iter()
            .any(|(sk, _)| *sk == SigKey::Direct("*".to_string()));
        assert!(has_global);
    }

    #[test]
    fn test_find_all_sigkeys_sorted_by_permission() {
        use crate::auth::generate_public_key;

        let mut settings = AuthSettings::new();
        let pubkey = generate_public_key();

        // Add keys with different permissions (intentionally in non-sorted order)
        settings
            .add_key(
                "key_write",
                AuthKey::active(&pubkey, Permission::Write(10)).unwrap(),
            )
            .unwrap();
        settings
            .add_key(
                "key_admin",
                AuthKey::active(&pubkey, Permission::Admin(5)).unwrap(),
            )
            .unwrap();
        settings
            .add_key(
                "key_read",
                AuthKey::active(&pubkey, Permission::Read).unwrap(),
            )
            .unwrap();
        settings
            .add_key(
                "key_write_high",
                AuthKey::active(&pubkey, Permission::Write(2)).unwrap(),
            )
            .unwrap();

        let results = settings.find_all_sigkeys_for_pubkey(&pubkey);
        assert_eq!(results.len(), 4);

        // Verify sorted by highest permission first
        // Admin(5) > Write(2) > Write(10) > Read
        assert_eq!(results[0].1, Permission::Admin(5));
        assert_eq!(results[1].1, Permission::Write(2));
        assert_eq!(results[2].1, Permission::Write(10));
        assert_eq!(results[3].1, Permission::Read);

        // Verify key names match permissions
        assert_eq!(results[0].0, SigKey::Direct("key_admin".to_string()));
        assert_eq!(results[1].0, SigKey::Direct("key_write_high".to_string()));
        assert_eq!(results[2].0, SigKey::Direct("key_write".to_string()));
        assert_eq!(results[3].0, SigKey::Direct("key_read".to_string()));
    }

    #[test]
    fn test_resolve_sig_key_returns_highest_permission() {
        use crate::auth::generate_public_key;

        let mut settings = AuthSettings::new();
        let pubkey = generate_public_key();

        // Add multiple keys with different permissions
        settings
            .add_key(
                "key_write",
                AuthKey::active(&pubkey, Permission::Write(10)).unwrap(),
            )
            .unwrap();
        settings
            .add_key(
                "key_admin",
                AuthKey::active(&pubkey, Permission::Admin(5)).unwrap(),
            )
            .unwrap();
        settings
            .add_key(
                "key_read",
                AuthKey::active(&pubkey, Permission::Read).unwrap(),
            )
            .unwrap();

        // resolve_sig_key_for_operation should return the highest permission (Admin)
        let (sig_key, perm) = settings.resolve_sig_key_for_operation(&pubkey).unwrap();
        assert_eq!(sig_key, SigKey::Direct("key_admin".to_string()));
        assert_eq!(perm, Permission::Admin(5));
    }
}
