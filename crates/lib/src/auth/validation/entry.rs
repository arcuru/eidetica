//! Core entry validation for authentication
//!
//! This module provides the main entry point for validating entries
//! and the AuthValidator struct that coordinates all validation operations.

use std::collections::HashMap;

use tracing::debug;

use super::resolver::KeyResolver;
use crate::{
    Entry, Result,
    auth::{
        crypto::verify_entry_signature,
        settings::AuthSettings,
        types::{KeyStatus, Operation, ResolvedAuth, SigKey},
    },
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

    /// Validate authentication information for an entry
    ///
    /// # Arguments
    /// * `entry` - The entry to validate
    /// * `auth_settings` - Authentication settings for key lookup
    /// * `instance` - Instance for loading delegated trees (optional for direct keys)
    pub async fn validate_entry(
        &mut self,
        entry: &Entry,
        auth_settings: &AuthSettings,
        instance: Option<&crate::Instance>,
    ) -> Result<bool> {
        // Handle unsigned entries (for backward compatibility)
        // An entry is considered unsigned if it has an empty Direct key name and no signature
        if let SigKey::Direct(key_name) = &entry.sig.key
            && key_name.is_empty()
            && entry.sig.sig.is_none()
        {
            debug!("Unsigned entry detected: {:?}", entry);
            // This is an unsigned entry - allow it to pass without authentication
            return Ok(true);
        }

        // If auth settings has no keys configured, allow unsigned entries
        if auth_settings.get_all_keys()?.is_empty() {
            debug!(
                "No keys configured in auth settings, allowing all access: {:?}",
                entry
            );
            return Ok(true);
        }

        // For all other entries, proceed with normal authentication validation
        // Resolve the authentication information
        let resolved_auth = self
            .resolver
            .resolve_sig_key_with_pubkey(
                &entry.sig.key,
                auth_settings,
                instance,
                entry.sig.pubkey.as_deref(),
            )
            .await?;

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
    /// * `auth_settings` - Authentication settings containing auth configuration
    /// * `instance` - Instance for loading delegated trees (required for DelegationPath sig_key)
    pub async fn resolve_sig_key(
        &mut self,
        sig_key: &SigKey,
        auth_settings: &AuthSettings,
        instance: Option<&crate::Instance>,
    ) -> Result<ResolvedAuth> {
        // Delegate to the resolver
        self.resolver
            .resolve_sig_key(sig_key, auth_settings, instance)
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
        // Delegate to the resolver
        self.resolver
            .resolve_sig_key_with_pubkey(sig_key, auth_settings, instance, pubkey_override)
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
