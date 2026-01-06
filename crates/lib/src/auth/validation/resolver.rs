//! Key resolution for authentication
//!
//! This module handles resolving authentication keys, both direct keys
//! and delegation paths.

use std::collections::HashMap;

use super::delegation::DelegationResolver;
use crate::{
    Result,
    auth::{
        crypto::parse_public_key,
        errors::AuthError,
        settings::AuthSettings,
        types::{ResolvedAuth, SigKey},
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
    /// # Arguments
    /// * `sig_key` - The signature key identifier to resolve
    /// * `auth_settings` - Authentication settings containing auth configuration
    /// * `instance` - Instance for loading delegated trees (required for DelegationPath sig_key)
    pub async fn resolve_sig_key(
        &mut self,
        sig_key: &SigKey,
        auth_settings: &AuthSettings,
        instance: Option<&crate::Instance>,
    ) -> Result<ResolvedAuth> {
        // Note: We don't cache results here because auth settings can change
        // and cached results could become stale (e.g., revoked keys, updated permissions).
        // In a production system, caching would need to be more sophisticated with
        // invalidation strategies based on settings changes.
        self.resolve_sig_key_with_depth(sig_key, auth_settings, instance, 0)
            .await
    }

    /// Resolve authentication identifier with pubkey override for global permissions
    ///
    /// # Arguments
    /// * `sig_key` - The signature key identifier to resolve
    /// * `auth_settings` - Authentication settings containing auth configuration
    /// * `instance` - Instance for loading delegated trees (required for DelegationPath sig_key)
    /// * `pubkey_override` - Optional pubkey for global "*" permission resolution
    pub async fn resolve_sig_key_with_pubkey(
        &mut self,
        sig_key: &SigKey,
        auth_settings: &AuthSettings,
        instance: Option<&crate::Instance>,
        pubkey_override: Option<&str>,
    ) -> Result<ResolvedAuth> {
        self.resolve_sig_key_with_depth_and_pubkey(
            sig_key,
            auth_settings,
            instance,
            0,
            pubkey_override,
        )
        .await
    }

    /// Resolve authentication identifier with recursion depth tracking
    ///
    /// This internal method tracks delegation depth to prevent infinite loops
    /// and ensures that delegation chains don't exceed reasonable limits.
    pub async fn resolve_sig_key_with_depth(
        &mut self,
        sig_key: &SigKey,
        auth_settings: &AuthSettings,
        instance: Option<&crate::Instance>,
        depth: usize,
    ) -> Result<ResolvedAuth> {
        self.resolve_sig_key_with_depth_and_pubkey(sig_key, auth_settings, instance, depth, None)
            .await
    }

    /// Resolve authentication identifier with recursion depth tracking and pubkey override
    ///
    /// This internal method tracks delegation depth to prevent infinite loops
    /// and ensures that delegation chains don't exceed reasonable limits.
    pub async fn resolve_sig_key_with_depth_and_pubkey(
        &mut self,
        sig_key: &SigKey,
        auth_settings: &AuthSettings,
        instance: Option<&crate::Instance>,
        depth: usize,
        pubkey_override: Option<&str>,
    ) -> Result<ResolvedAuth> {
        // Prevent infinite recursion and overly deep delegation chains
        const MAX_DELEGATION_DEPTH: usize = 10;
        if depth >= MAX_DELEGATION_DEPTH {
            return Err(AuthError::DelegationDepthExceeded {
                depth: MAX_DELEGATION_DEPTH,
            }
            .into());
        }

        match sig_key {
            SigKey::Direct(key_name) => {
                self.resolve_direct_key_with_pubkey(key_name, auth_settings, pubkey_override)
            }
            SigKey::DelegationPath(steps) => {
                // Validate no wildcards in delegation path (before checking instance)
                if steps.iter().any(|s| s.key == "*") {
                    return Err(AuthError::InvalidDelegationStep {
                        reason: "Delegation steps cannot use wildcard '*' key".to_string(),
                    }
                    .into());
                }
                let instance = instance.ok_or_else(|| AuthError::DatabaseRequired {
                    operation: "delegated tree resolution".to_string(),
                })?;
                self.delegation_resolver
                    .resolve_delegation_path_with_depth(steps, auth_settings, instance, depth)
                    .await
            }
        }
    }

    /// Resolve a direct key reference from the main tree's auth settings
    pub fn resolve_direct_key(
        &mut self,
        key_name: &str,
        auth_settings: &AuthSettings,
    ) -> Result<ResolvedAuth> {
        self.resolve_direct_key_with_pubkey(key_name, auth_settings, None)
    }

    /// Resolve a direct key reference with optional pubkey override for global permissions
    ///
    /// Two modes are supported:
    /// - `key_name == "*"`: Explicit global permission - requires pubkey_override
    /// - Otherwise: Key must exist in auth_settings
    pub fn resolve_direct_key_with_pubkey(
        &mut self,
        key_name: &str,
        auth_settings: &AuthSettings,
        pubkey_override: Option<&str>,
    ) -> Result<ResolvedAuth> {
        // Handle explicit global "*" permission
        if key_name == "*" {
            let global_perm = auth_settings.get_global_permission().ok_or_else(|| {
                AuthError::InvalidAuthConfiguration {
                    reason: "Global '*' sigkey used but no global permission configured"
                        .to_string(),
                }
            })?;
            let pubkey_str =
                pubkey_override.ok_or_else(|| AuthError::InvalidAuthConfiguration {
                    reason: "Global '*' permission requires pubkey in SigInfo".to_string(),
                })?;
            return Ok(ResolvedAuth {
                public_key: parse_public_key(pubkey_str)?,
                effective_permission: global_perm,
                key_status: crate::auth::types::KeyStatus::Active,
            });
        }

        // Non-"*" key - must exist in auth settings
        let auth_key =
            auth_settings
                .get_key(key_name)
                .map_err(|_| AuthError::InvalidAuthConfiguration {
                    reason: format!("Key '{}' not found in auth settings", key_name),
                })?;

        // Use pubkey from auth settings
        let public_key = parse_public_key(auth_key.pubkey())?;
        Ok(ResolvedAuth {
            public_key,
            effective_permission: auth_key.permissions().clone(),
            key_status: auth_key.status().clone(),
        })
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
