//! Key resolution for authentication
//!
//! This module handles resolving authentication keys, both direct keys
//! and delegation paths.

use std::collections::HashMap;

use super::delegation::DelegationResolver;
use crate::{
    Instance, Result,
    auth::{
        errors::AuthError,
        settings::AuthSettings,
        types::{KeyHint, ResolvedAuth, SigKey},
    },
};

/// Key resolver for handling both direct and delegated key resolution
pub struct KeyResolver {
    /// Cache for resolved authentication data to improve performance
    auth_cache: HashMap<String, ResolvedAuth>,
    /// Delegation resolver for handling complex delegation paths
    delegation_resolver: DelegationResolver,
}

impl KeyResolver {
    /// Create a new key resolver
    pub fn new() -> Self {
        Self {
            auth_cache: HashMap::new(),
            delegation_resolver: DelegationResolver::new(),
        }
    }

    /// Resolve authentication identifier to concrete authentication information
    ///
    /// Returns all matching ResolvedAuth entries. For name hints that match
    /// multiple keys, all matches are returned so the caller can try signature
    /// verification against each.
    ///
    /// # Arguments
    /// * `sig_key` - The signature key identifier to resolve
    /// * `auth_settings` - Authentication settings containing auth configuration
    /// * `instance` - Instance for loading delegated trees (required for Delegation sig_key)
    pub async fn resolve_sig_key(
        &mut self,
        sig_key: &SigKey,
        auth_settings: &AuthSettings,
        instance: Option<&Instance>,
    ) -> Result<Vec<ResolvedAuth>> {
        match sig_key {
            SigKey::Direct { hint } => self.resolve_direct_key(hint, auth_settings),
            SigKey::Delegation { path, hint } => {
                let instance = instance.ok_or_else(|| AuthError::DatabaseRequired {
                    operation: "delegated tree resolution".to_string(),
                })?;
                self.delegation_resolver
                    .resolve_delegation_path(path, hint, auth_settings, instance)
                    .await
            }
        }
    }

    /// Resolve a direct key reference from the main tree's auth settings
    ///
    /// Returns all matching ResolvedAuth entries. For name hints that match
    /// multiple keys, all matches are returned so the caller can try signature
    /// verification against each.
    pub fn resolve_direct_key(
        &mut self,
        hint: &KeyHint,
        auth_settings: &AuthSettings,
    ) -> Result<Vec<ResolvedAuth>> {
        // Use AuthSettings.resolve_hint which handles:
        // - Global permission (returns single match with actual pubkey)
        // - Direct pubkey lookup (returns single match)
        // - Name lookup (may return multiple matches)
        let matches = auth_settings.resolve_hint(hint)?;
        if matches.is_empty() {
            return Err(AuthError::KeyNotFound {
                key_name: format!("hint({:?})", hint.hint_type()),
            }
            .into());
        }

        Ok(matches)
    }

    /// Clear the authentication cache
    pub fn clear_cache(&mut self) {
        self.auth_cache.clear();
    }
}

impl Default for KeyResolver {
    fn default() -> Self {
        Self::new()
    }
}
