//! Authentication settings management for Eidetica
//!
//! This module provides a simple wrapper around Map for managing authentication
//! settings. AuthSettings is a view/interface layer over the auth portion of the
//! _settings subtree - it doesn't implement CRDT itself since merging happens at
//! the higher settings level.

use super::errors::AuthError;
use crate::auth::types::{AuthKey, DelegatedTreeRef, KeyStatus, Permission, ResolvedAuth, SigKey};
use crate::auth::validation::AuthValidator;
use crate::backend::Database;
use crate::crdt::Map;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Authentication settings view/interface over Map data
///
/// This provides a convenient interface for working with authentication data
/// stored in the _settings.auth subtree. The underlying Map CRDT handles
/// all merging at the settings level - this is just a view with auth-specific
/// convenience methods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSettings {
    /// Map data from _settings.auth - this is a view, not the authoritative copy
    inner: Map,
}

impl AuthSettings {
    /// Create a new empty auth settings view
    pub fn new() -> Self {
        Self { inner: Map::new() }
    }

    /// Create from existing Map (e.g., from _settings.auth)
    pub fn from_kvnested(kvnested: Map) -> Self {
        Self { inner: kvnested }
    }

    /// Get the underlying Map for direct access
    pub fn as_kvnested(&self) -> &Map {
        &self.inner
    }

    /// Get mutable access to the underlying Map
    pub fn as_kvnested_mut(&mut self) -> &mut Map {
        &mut self.inner
    }

    /// Add or update an authentication key
    pub fn add_key(&mut self, id: impl Into<String>, key: AuthKey) -> Result<()> {
        self.inner.set_json(id, key)?;
        Ok(())
    }

    /// Add or update a delegated tree reference
    pub fn add_delegated_tree(
        &mut self,
        id: impl Into<String>,
        tree_ref: DelegatedTreeRef,
    ) -> Result<()> {
        self.inner.set_json(id, tree_ref)?;
        Ok(())
    }

    /// Revoke a key by setting its status to Revoked
    pub fn revoke_key(&mut self, id: impl AsRef<str>) -> Result<()> {
        let id = id.as_ref();
        if self.inner.get(id).is_some() {
            match self.inner.get_json::<AuthKey>(id) {
                Ok(mut auth_key) => {
                    auth_key.status = KeyStatus::Revoked;
                    self.inner.set_json(id, auth_key)?;
                    Ok(())
                }
                Err(_) => {
                    // Not an AuthKey, might be a DelegatedTreeRef - for now just error
                    Err(AuthError::CannotRevokeNonKey { id: id.to_string() }.into())
                }
            }
        } else {
            Err(AuthError::KeyNotFound { id: id.to_string() }.into())
        }
    }

    /// Get a specific key by ID
    pub fn get_key(&self, id: impl AsRef<str>) -> Option<Result<AuthKey>> {
        match self.inner.get_json::<AuthKey>(id.as_ref()) {
            Ok(key) => Some(Ok(key)),
            Err(e) if e.is_not_found() => None,
            Err(e) => Some(Err(AuthError::InvalidKeyFormat {
                reason: e.to_string(),
            }
            .into())),
        }
    }

    /// Get a specific delegated tree reference by ID
    pub fn get_delegated_tree(&self, id: impl AsRef<str>) -> Option<Result<DelegatedTreeRef>> {
        match self.inner.get_json::<DelegatedTreeRef>(id.as_ref()) {
            Ok(tree_ref) => Some(Ok(tree_ref)),
            Err(e) if e.is_not_found() => None,
            Err(e) => Some(Err(AuthError::InvalidAuthConfiguration {
                reason: format!("Invalid delegated tree format: {e}"),
            }
            .into())),
        }
    }

    /// Get all authentication keys
    pub fn get_all_keys(&self) -> Result<HashMap<String, AuthKey>> {
        let mut keys = HashMap::new();
        for (key_id, _) in self.inner.as_hashmap().iter() {
            // Try to parse as AuthKey, skip if it's not one
            if let Ok(auth_key) = self.inner.get_json::<AuthKey>(key_id) {
                keys.insert(key_id.clone(), auth_key);
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

    /// Simple validation for entry creation - checks if auth ID is valid and active
    ///
    /// This is entry-time validation using current settings state only.
    /// No complex merge-time validation is performed.
    pub fn validate_entry_auth(
        &self,
        sig_key: &SigKey,
        backend: Option<&Arc<dyn Database>>,
    ) -> Result<ResolvedAuth> {
        match sig_key {
            SigKey::Direct(key_id) => {
                if let Some(key_result) = self.get_key(key_id) {
                    let auth_key = key_result?;
                    let public_key = crate::auth::crypto::parse_public_key(&auth_key.pubkey)?;
                    Ok(ResolvedAuth {
                        public_key,
                        effective_permission: auth_key.permissions.clone(),
                        key_status: auth_key.status,
                    })
                } else {
                    Err(AuthError::KeyNotFound {
                        id: key_id.to_string(),
                    }
                    .into())
                }
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
                validator.resolve_sig_key(sig_key, &self.inner, Some(backend))
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
        target_key_id: impl AsRef<str>,
    ) -> Result<bool> {
        // Must have admin permissions to modify keys
        if !signing_key.effective_permission.can_admin() {
            return Ok(false);
        }

        // Get target key info
        if let Some(target_result) = self.get_key(target_key_id.as_ref()) {
            let target_key = target_result?;

            // Use the built-in permission ordering: signing key must be >= target key
            Ok(signing_key.effective_permission >= target_key.permissions)
        } else {
            // Target key doesn't exist - this is an error for modification
            Err(AuthError::KeyNotFound {
                id: target_key_id.as_ref().to_string(),
            }
            .into())
        }
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
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::{KeyStatus, Permission};
    use crate::crdt::CRDT;

    #[test]
    fn test_auth_settings_basic_operations() {
        let mut settings = AuthSettings::new();

        // Add a key
        let auth_key = AuthKey {
            pubkey: "ed25519:test_key".to_string(),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        settings.add_key("KEY_LAPTOP", auth_key.clone()).unwrap();

        // Retrieve the key
        let retrieved = settings.get_key("KEY_LAPTOP").unwrap().unwrap();
        assert_eq!(retrieved.pubkey, auth_key.pubkey);
        assert_eq!(retrieved.permissions, auth_key.permissions);
        assert_eq!(retrieved.status, auth_key.status);
    }

    #[test]
    fn test_revoke_key() {
        let mut settings = AuthSettings::new();

        let auth_key = AuthKey {
            pubkey: "ed25519:test_key".to_string(),
            permissions: Permission::Admin(5),
            status: KeyStatus::Active,
        };

        settings.add_key("KEY_LAPTOP", auth_key).unwrap();

        // Revoke the key
        settings.revoke_key("KEY_LAPTOP").unwrap();

        // Check that it's revoked
        let retrieved = settings.get_key("KEY_LAPTOP").unwrap().unwrap();
        assert_eq!(retrieved.status, KeyStatus::Revoked);
    }

    #[test]
    fn test_auth_settings_view_operations() {
        let mut settings1 = AuthSettings::new();
        let mut settings2 = AuthSettings::new();

        let key1 = AuthKey {
            pubkey: "ed25519:key1".to_string(),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let key2 = AuthKey {
            pubkey: "ed25519:key2".to_string(),
            permissions: Permission::Admin(5),
            status: KeyStatus::Active,
        };

        settings1.add_key("KEY_1", key1).unwrap();
        settings2.add_key("KEY_2", key2).unwrap();

        // Test that we can access the underlying Map for merging at higher level
        let kvnested1 = settings1.as_kvnested().clone();
        let kvnested2 = settings2.as_kvnested().clone();

        // This would be done at the higher settings level, not here
        let merged_kvnested = kvnested1.merge(&kvnested2).unwrap();
        let merged_settings = AuthSettings::from_kvnested(merged_kvnested);

        // Both keys should be present in the merged view
        assert!(merged_settings.get_key("KEY_1").is_some());
        assert!(merged_settings.get_key("KEY_2").is_some());
    }

    #[test]
    fn test_priority_based_key_modification() {
        let mut settings = AuthSettings::new();

        // Add high-priority admin key
        let high_priority_key = AuthKey {
            pubkey: "ed25519:admin".to_string(),
            permissions: Permission::Admin(1), // High priority
            status: KeyStatus::Active,
        };

        settings
            .add_key("ADMIN_KEY", high_priority_key.clone())
            .unwrap();

        // Create resolved auth for the admin key
        let admin_resolved = ResolvedAuth {
            public_key: crate::auth::crypto::generate_keypair().1,
            effective_permission: high_priority_key.permissions,
            key_status: high_priority_key.status,
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
}
