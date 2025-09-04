//! Key resolution for authentication
//!
//! This module handles resolving authentication keys, both direct keys
//! and delegation paths.

use crate::Result;
use crate::auth::crypto::parse_public_key;
use crate::auth::errors::AuthError;
use crate::auth::types::{AuthKey, ResolvedAuth, SigKey};
use crate::backend::BackendDB;
use crate::crdt::Doc;
use crate::crdt::doc::Value;
use std::collections::HashMap;
use std::sync::Arc;

use super::delegation::DelegationResolver;

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
    /// * `settings` - Document settings containing auth configuration
    /// * `backend` - Backend for loading delegated trees (required for DelegationPath sig_key)
    pub fn resolve_sig_key(
        &mut self,
        sig_key: &SigKey,
        settings: &Doc,
        backend: Option<&Arc<dyn BackendDB>>,
    ) -> Result<ResolvedAuth> {
        // Note: We don't cache results here because auth settings can change
        // and cached results could become stale (e.g., revoked keys, updated permissions).
        // In a production system, caching would need to be more sophisticated with
        // invalidation strategies based on settings changes.
        self.resolve_sig_key_with_depth(sig_key, settings, backend, 0)
    }

    /// Resolve authentication identifier with recursion depth tracking
    ///
    /// This internal method tracks delegation depth to prevent infinite loops
    /// and ensures that delegation chains don't exceed reasonable limits.
    pub fn resolve_sig_key_with_depth(
        &mut self,
        sig_key: &SigKey,
        settings: &Doc,
        backend: Option<&Arc<dyn BackendDB>>,
        depth: usize,
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
            SigKey::Direct(key_name) => self.resolve_direct_key(key_name, settings),
            SigKey::DelegationPath(steps) => {
                let backend = backend.ok_or_else(|| AuthError::DatabaseRequired {
                    operation: "delegated tree resolution".to_string(),
                })?;
                self.delegation_resolver
                    .resolve_delegation_path_with_depth(steps, settings, backend, depth)
            }
        }
    }

    /// Resolve a direct key reference from the main tree's auth settings
    pub fn resolve_direct_key(&mut self, key_name: &str, settings: &Doc) -> Result<ResolvedAuth> {
        // First get the auth section from settings
        let auth_section = settings
            .get("auth")
            .ok_or_else(|| AuthError::NoAuthConfiguration)?;

        // Extract the auth Node from the Value
        let auth_nested = match auth_section {
            Value::Node(auth_map) => auth_map,
            _ => {
                return Err(AuthError::InvalidAuthConfiguration {
                    reason: "Auth section must be a nested map".to_string(),
                }
                .into());
            }
        };

        // Use get_json to parse AuthKey
        let auth_key = auth_nested.get_json::<AuthKey>(key_name).map_err(|e| {
            AuthError::InvalidAuthConfiguration {
                reason: format!("Invalid auth key format: {e}"),
            }
        })?;

        let public_key = parse_public_key(&auth_key.pubkey)?;

        Ok(ResolvedAuth {
            public_key,
            effective_permission: auth_key.permissions.clone(),
            key_status: auth_key.status,
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
