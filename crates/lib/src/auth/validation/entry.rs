//! Core entry validation for authentication
//!
//! This module provides the main entry point for validating entries
//! and the AuthValidator struct that coordinates all validation operations.

use crate::Result;
use crate::auth::crypto::verify_entry_signature;
use crate::auth::types::{KeyStatus, Operation, ResolvedAuth, SigKey};
use crate::backend::Database;
use crate::crdt::Map;
use crate::crdt::map::Value;
use crate::entry::Entry;
use std::collections::HashMap;
use std::sync::Arc;

use super::resolver::KeyResolver;

/// Authentication validator for validating entries and resolving auth information
pub struct AuthValidator {
    /// Cache for resolved authentication data to improve performance
    auth_cache: HashMap<String, ResolvedAuth>,
    /// Key resolver for handling key resolution
    pub(crate) resolver: KeyResolver,
}

impl AuthValidator {
    /// Create a new authentication validator
    pub fn new() -> Self {
        Self {
            auth_cache: HashMap::new(),
            resolver: KeyResolver::new(),
        }
    }

    /// Validate authentication information for an entry
    ///
    /// # Arguments
    /// * `entry` - The entry to validate
    /// * `settings_state` - Current state of the _settings subtree for key lookup
    /// * `backend` - Backend for loading delegated trees (optional for direct keys)
    pub fn validate_entry(
        &mut self,
        entry: &Entry,
        settings_state: &Map,
        backend: Option<&Arc<dyn Database>>,
    ) -> Result<bool> {
        // Handle unsigned entries (for backward compatibility)
        // An entry is considered unsigned if it has an empty Direct key name and no signature
        if let SigKey::Direct(key_name) = &entry.sig.key
            && key_name.is_empty()
            && entry.sig.sig.is_none()
        {
            // This is an unsigned entry - allow it to pass without authentication
            return Ok(true);
        }

        // If the settings state has no 'auth' section or an empty 'auth' map, allow unsigned entries.
        match settings_state.get("auth") {
            Some(Value::Node(auth_map)) => {
                // If 'auth' section exists and is a map, check if it's empty
                if auth_map.as_hashmap().is_empty() {
                    return Ok(true);
                }
            }
            None => {
                // If 'auth' section does not exist at all, it means no keys are configured
                return Ok(true);
            }
            _ => {
                // If 'auth' section exists but is not a map (e.g., a string or deleted),
                // or if it's a non-empty map, then proceed with normal validation.
            }
        }

        // For all other entries, proceed with normal authentication validation
        // Resolve the authentication information
        let resolved_auth = self.resolve_sig_key(&entry.sig.key, settings_state, backend)?;

        // Check if the key is in an active state
        if resolved_auth.key_status != KeyStatus::Active {
            return Ok(false);
        }

        // Verify the signature using the entry-based verification
        verify_entry_signature(entry, &resolved_auth.public_key).map_err(|e| e.into())
    }

    /// Resolve authentication identifier to concrete authentication information
    ///
    /// # Arguments
    /// * `sig_key` - The signature key identifier to resolve
    /// * `settings` - Map settings containing auth configuration
    /// * `backend` - Backend for loading delegated trees (required for DelegationPath sig_key)
    pub fn resolve_sig_key(
        &mut self,
        sig_key: &SigKey,
        settings: &Map,
        backend: Option<&Arc<dyn Database>>,
    ) -> Result<ResolvedAuth> {
        // Delegate to the resolver
        self.resolver.resolve_sig_key(sig_key, settings, backend)
    }

    /// Check if a resolved authentication has sufficient permissions for an operation
    pub fn check_permissions(
        &self,
        resolved: &ResolvedAuth,
        operation: &Operation,
    ) -> Result<bool> {
        super::permissions::check_permissions(resolved, operation)
    }

    /// Clear the authentication cache
    pub fn clear_cache(&mut self) {
        self.auth_cache.clear();
        self.resolver.clear_cache();
    }
}

impl Default for AuthValidator {
    fn default() -> Self {
        Self::new()
    }
}
