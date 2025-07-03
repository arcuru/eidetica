//! Key resolution for authentication
//!
//! This module handles resolving authentication keys, both direct keys
//! and delegation paths.

use crate::auth::crypto::parse_public_key;
use crate::auth::types::{AuthKey, ResolvedAuth, SigKey};
use crate::backend::Backend;
use crate::crdt::{Nested, Value};
use crate::{Error, Result};
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
    /// * `settings` - Nested settings containing auth configuration
    /// * `backend` - Backend for loading delegated trees (required for DelegationPath sig_key)
    pub fn resolve_sig_key(
        &mut self,
        sig_key: &SigKey,
        settings: &Nested,
        backend: Option<&Arc<dyn Backend>>,
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
        settings: &Nested,
        backend: Option<&Arc<dyn Backend>>,
        depth: usize,
    ) -> Result<ResolvedAuth> {
        // Prevent infinite recursion and overly deep delegation chains
        const MAX_DELEGATION_DEPTH: usize = 10;
        if depth >= MAX_DELEGATION_DEPTH {
            return Err(Error::Authentication(format!(
                "Maximum delegation depth ({MAX_DELEGATION_DEPTH}) exceeded - possible circular delegation"
            )));
        }

        match sig_key {
            SigKey::Direct(key_id) => self.resolve_direct_key(key_id, settings),
            SigKey::DelegationPath(steps) => {
                let backend = backend.ok_or_else(|| {
                    Error::Authentication(
                        "Backend required for delegated tree resolution".to_string(),
                    )
                })?;
                self.delegation_resolver
                    .resolve_delegation_path_with_depth(steps, settings, backend, depth)
            }
        }
    }

    /// Resolve a direct key reference from the main tree's auth settings
    pub fn resolve_direct_key(&mut self, key_id: &str, settings: &Nested) -> Result<ResolvedAuth> {
        // First get the auth section from settings
        let auth_section = settings
            .get("auth")
            .ok_or_else(|| Error::Authentication("No auth configuration found".to_string()))?;

        // Extract the auth Nested from the Value
        let auth_nested = match auth_section {
            Value::Map(auth_map) => auth_map,
            _ => {
                return Err(Error::Authentication(
                    "Auth section must be a nested map".to_string(),
                ));
            }
        };

        // Use get_json to parse AuthKey
        let auth_key = auth_nested
            .get_json::<AuthKey>(key_id)
            .map_err(|e| Error::Authentication(format!("Invalid auth key format: {e}")))?;

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
