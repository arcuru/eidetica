//! Authentication settings management for Eidetica
//!
//! This module provides a simple wrapper around Nested for managing authentication
//! settings. AuthSettings is a view/interface layer over the auth portion of the
//! _settings subtree - it doesn't implement CRDT itself since merging happens at
//! the higher settings level.

use crate::auth::types::{AuthId, AuthKey, DelegatedTreeRef, KeyStatus, ResolvedAuth};
use crate::crdt::{Nested, Value};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Authentication settings view/interface over Nested data
///
/// This provides a convenient interface for working with authentication data
/// stored in the _settings.auth subtree. The underlying Nested CRDT handles
/// all merging at the settings level - this is just a view with auth-specific
/// convenience methods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSettings {
    /// Nested data from _settings.auth - this is a view, not the authoritative copy
    inner: Nested,
}

impl AuthSettings {
    /// Create a new empty auth settings view
    pub fn new() -> Self {
        Self {
            inner: Nested::new(),
        }
    }

    /// Create from existing Nested (e.g., from _settings.auth)
    pub fn from_kvnested(kvnested: Nested) -> Self {
        Self { inner: kvnested }
    }

    /// Get the underlying Nested for direct access
    pub fn as_kvnested(&self) -> &Nested {
        &self.inner
    }

    /// Get mutable access to the underlying Nested
    pub fn as_kvnested_mut(&mut self) -> &mut Nested {
        &mut self.inner
    }

    /// Add or update an authentication key
    pub fn add_key(&mut self, id: impl Into<String>, key: AuthKey) -> Result<()> {
        self.inner
            .as_hashmap_mut()
            .insert(id.into(), Value::from(key));
        Ok(())
    }

    /// Add or update a delegated tree reference
    pub fn add_delegated_tree(
        &mut self,
        id: impl Into<String>,
        tree_ref: DelegatedTreeRef,
    ) -> Result<()> {
        self.inner
            .as_hashmap_mut()
            .insert(id.into(), Value::from(tree_ref));
        Ok(())
    }

    /// Revoke a key by setting its status to Revoked
    pub fn revoke_key(&mut self, id: impl AsRef<str>) -> Result<()> {
        let id = id.as_ref();
        if let Some(value) = self.inner.get(id) {
            match AuthKey::try_from(value.clone()) {
                Ok(mut auth_key) => {
                    auth_key.status = KeyStatus::Revoked;
                    self.inner
                        .as_hashmap_mut()
                        .insert(id.to_string(), Value::from(auth_key));
                    Ok(())
                }
                Err(_) => {
                    // Not an AuthKey, might be a DelegatedTreeRef - for now just error
                    Err(Error::Authentication(format!(
                        "Cannot revoke non-key entry: {id}"
                    )))
                }
            }
        } else {
            Err(Error::Authentication(format!("Key not found: {id}")))
        }
    }

    /// Get a specific key by ID
    pub fn get_key(&self, id: impl AsRef<str>) -> Option<Result<AuthKey>> {
        self.inner.get(id.as_ref()).map(|value| {
            AuthKey::try_from(value.clone())
                .map_err(|e| Error::Authentication(format!("Invalid auth key format: {e}")))
        })
    }

    /// Get a specific delegated tree reference by ID
    pub fn get_delegated_tree(&self, id: impl AsRef<str>) -> Option<Result<DelegatedTreeRef>> {
        self.inner.get(id.as_ref()).map(|value| {
            DelegatedTreeRef::try_from(value.clone())
                .map_err(|e| Error::Authentication(format!("Invalid delegated tree format: {e}")))
        })
    }

    /// Get all authentication keys
    pub fn get_all_keys(&self) -> Result<HashMap<String, AuthKey>> {
        let mut keys = HashMap::new();
        for (key_id, value) in self.inner.as_hashmap().iter() {
            // Try to parse as AuthKey, skip if it's not one
            if let Ok(auth_key) = AuthKey::try_from(value.clone()) {
                keys.insert(key_id.clone(), auth_key);
            }
        }
        Ok(keys)
    }

    /// Get all delegated tree references
    pub fn get_all_delegated_trees(&self) -> Result<HashMap<String, DelegatedTreeRef>> {
        let mut trees = HashMap::new();
        for (tree_id, value) in self.inner.as_hashmap().iter() {
            // Try to parse as DelegatedTreeRef, skip if it's not one
            if let Ok(tree_ref) = DelegatedTreeRef::try_from(value.clone()) {
                trees.insert(tree_id.clone(), tree_ref);
            }
        }
        Ok(trees)
    }

    /// Simple validation for entry creation - checks if auth ID is valid and active
    ///
    /// This is entry-time validation using current settings state only.
    /// No complex merge-time validation is performed.
    pub fn validate_entry_auth(&self, auth_id: &AuthId) -> Result<ResolvedAuth> {
        match auth_id {
            AuthId::Direct(key_id) => {
                if let Some(key_result) = self.get_key(key_id) {
                    let auth_key = key_result?;
                    let public_key = crate::auth::crypto::parse_public_key(&auth_key.key)?;
                    Ok(ResolvedAuth {
                        public_key,
                        effective_permission: auth_key.permissions.clone(),
                        key_status: auth_key.status,
                    })
                } else {
                    Err(Error::Authentication(format!("Key not found: {key_id}")))
                }
            }
            AuthId::DelegatedTree { .. } => {
                // Phase 1: Delegated trees not implemented yet
                Err(Error::Authentication(
                    "Delegated trees not yet implemented in Phase 1".to_string(),
                ))
            }
        }
    }

    /// Check if a signing key can modify a target key based on priority rules
    ///
    /// Priority rules apply only to administrative operations:
    /// - Keys can modify keys with equal or lower priority (equal or higher numbers)
    /// - Admin keys can always modify Write keys regardless of priority
    pub fn can_modify_key(
        &self,
        signing_key: &ResolvedAuth,
        target_key_id: impl AsRef<str>,
    ) -> Result<bool> {
        // Must have admin permissions to modify keys
        if !signing_key.effective_permission.can_admin() {
            return Ok(false);
        }

        // Get signing key priority
        let signing_priority = signing_key
            .effective_permission
            .priority()
            .unwrap_or(u32::MAX); // Default to lowest priority if None

        // Get target key info
        if let Some(target_result) = self.get_key(target_key_id.as_ref()) {
            let target_key = target_result?;
            let target_priority = target_key.permissions.priority().unwrap_or(u32::MAX);

            // Admin keys can always modify Write keys
            if signing_key.effective_permission.can_admin() && target_key.permissions.can_write() {
                return Ok(true);
            }

            // Otherwise, check priority hierarchy (lower number = higher priority)
            Ok(signing_priority <= target_priority)
        } else {
            // Target key doesn't exist, allow creation
            Ok(true)
        }
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
            key: "ed25519:test_key".to_string(),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        settings.add_key("KEY_LAPTOP", auth_key.clone()).unwrap();

        // Retrieve the key
        let retrieved = settings.get_key("KEY_LAPTOP").unwrap().unwrap();
        assert_eq!(retrieved.key, auth_key.key);
        assert_eq!(retrieved.permissions, auth_key.permissions);
        assert_eq!(retrieved.status, auth_key.status);
    }

    #[test]
    fn test_revoke_key() {
        let mut settings = AuthSettings::new();

        let auth_key = AuthKey {
            key: "ed25519:test_key".to_string(),
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
            key: "ed25519:key1".to_string(),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let key2 = AuthKey {
            key: "ed25519:key2".to_string(),
            permissions: Permission::Admin(5),
            status: KeyStatus::Active,
        };

        settings1.add_key("KEY_1", key1).unwrap();
        settings2.add_key("KEY_2", key2).unwrap();

        // Test that we can access the underlying Nested for merging at higher level
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
            key: "ed25519:admin".to_string(),
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

        // Should be able to modify lower priority keys
        assert!(settings.can_modify_key(&admin_resolved, "NEW_KEY").unwrap());

        // Test with write key (lower privileges)
        let write_resolved = ResolvedAuth {
            public_key: crate::auth::crypto::generate_keypair().1,
            effective_permission: Permission::Write(10),
            key_status: KeyStatus::Active,
        };

        // Write key should not be able to modify other keys
        assert!(!settings.can_modify_key(&write_resolved, "NEW_KEY").unwrap());
    }
}
