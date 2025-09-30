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
}
