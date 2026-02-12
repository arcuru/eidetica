//! Core entry validation for authentication
//!
//! This module provides the main entry point for validating entries
//! and the AuthValidator struct that coordinates all validation operations.

use std::collections::HashMap;

use tracing::debug;

use super::resolver::KeyResolver;
use crate::{
    Entry, Instance, Result,
    auth::{
        crypto::verify_entry_signature,
        settings::AuthSettings,
        types::{KeyStatus, Operation, ResolvedAuth, SigKey},
    },
    constants::SETTINGS,
};

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

    /// Validate an entry's authentication
    ///
    /// This method answers: "Is this entry valid?" which includes:
    /// 1. Is the signature valid (or is unsigned allowed)?
    /// 2. Does the signing key have permission for what this entry does?
    ///
    /// For entries with name hints that match multiple keys, this method
    /// tries signature verification against each matching key until one succeeds.
    ///
    /// # Returns
    /// - `Ok(true)` - Entry is valid (signature verified with sufficient permissions, or unsigned allowed)
    /// - `Ok(false)` - Entry is invalid (malformed, bad signature, insufficient permissions, etc.)
    /// - `Err(...)` - Actual error (I/O, database failures)
    ///
    /// # Arguments
    /// * `entry` - The entry to validate
    /// * `auth_settings` - Authentication settings for key lookup
    /// * `instance` - Instance for loading delegated trees (optional for direct keys)
    pub async fn validate_entry(
        &mut self,
        entry: &Entry,
        auth_settings: &AuthSettings,
        instance: Option<&Instance>,
    ) -> Result<bool> {
        // Malformed entries fail validation
        if entry.sig.malformed_reason().is_some() {
            debug!("Malformed entry detected");
            return Ok(false);
        }

        // Check if auth is configured
        let has_auth = !auth_settings.get_all_keys()?.is_empty();

        // Handle unsigned entries
        if entry.sig.is_unsigned() {
            if has_auth {
                // Auth is configured but entry is unsigned - invalid
                debug!("Unsigned entry in authenticated database");
                return Ok(false);
            }
            // No auth configured, unsigned is valid
            debug!("Unsigned entry allowed (no auth configured)");
            return Ok(true);
        }

        // Entry is signed but no auth configured - invalid
        if !has_auth {
            debug!("Signed entry but no auth configured");
            return Ok(false);
        }

        // Resolve all matching keys
        let resolved_auths = match self
            .resolver
            .resolve_sig_key(&entry.sig.key, auth_settings, instance)
            .await
        {
            Ok(auths) => auths,
            Err(e) => {
                debug!("Key resolution failed: {:?}", e);
                return Ok(false);
            }
        };

        // Determine operation type from entry content
        let operation = if entry.subtrees().contains(&SETTINGS.to_string()) {
            Operation::WriteSettings
        } else {
            Operation::WriteData
        };

        // Try signature verification + permission check against each candidate
        for resolved_auth in resolved_auths {
            // Skip keys that are not active
            if resolved_auth.key_status != KeyStatus::Active {
                debug!("Skipping inactive key: {:?}", resolved_auth.key_status);
                continue;
            }

            // Try to verify the signature with this key
            if verify_entry_signature(entry, &resolved_auth.public_key).is_ok() {
                debug!("Signature verified, checking permissions");
                // Signature verified - now check permissions
                if self.check_permissions(&resolved_auth, &operation)? {
                    debug!("Entry valid: signature verified with sufficient permissions");
                    return Ok(true);
                }
                debug!("Signature valid but insufficient permissions, trying next key");
                // Continue to try other keys that might have higher permissions
            }
        }

        // No key verified with sufficient permissions
        debug!("Entry invalid: no key verified with sufficient permissions");
        Ok(false)
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
        // Delegate to the resolver
        self.resolver
            .resolve_sig_key(sig_key, auth_settings, instance)
            .await
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
