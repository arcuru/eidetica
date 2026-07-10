//! Permission checking for authentication operations
//!
//! This module provides utilities for checking if resolved authentication
//! has sufficient permissions for specific operations.

use crate::{
    Error, Result,
    auth::{
        crypto::PublicKey,
        errors::AuthError,
        settings::AuthSettings,
        types::{Operation, Permission, ResolvedAuth, SigKey},
        validation::AuthValidator,
    },
};

/// Resolve the permission level for a pubkey + identity against auth settings.
///
/// Shared validation logic used by both the local path (`Database::validate_key`,
/// which holds a `DatabaseKey` that bundles signing key + identity) and the
/// remote path (the service server, which has the pubkey from the session
/// challenge-response and the identity from the request's authenticated scope).
///
/// # Arguments
/// * `pubkey` - The public key to validate
/// * `identity` - The `SigKey` identity claiming access
/// * `auth_settings` - The database's auth configuration
/// * `instance` - Optional `Instance` for delegation resolution; required when
///   `identity` is a `SigKey::Delegation`
pub async fn resolve_identity_permission(
    pubkey: &PublicKey,
    identity: &SigKey,
    auth_settings: &AuthSettings,
    instance: Option<&crate::Instance>,
) -> Result<Permission> {
    match identity {
        SigKey::Direct { hint } if hint.is_global() => {
            if let Some(embedded_pubkey) = &hint.pubkey
                && *embedded_pubkey != *pubkey
            {
                return Err(Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                    reason: format!(
                        "pubkey '{pubkey}' but global identity claims '{embedded_pubkey}'"
                    ),
                })));
            }
            let global = auth_settings.get_global_key().map_err(|_| {
                Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                    reason: "Global '*' permission not configured".to_string(),
                }))
            })?;
            if !global.is_active() {
                return Err(Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                    reason: "Global '*' permission is not active".to_string(),
                })));
            }
            Ok(*global.permissions())
        }
        SigKey::Direct { hint } => {
            // Anti-spoof: a pubkey-bearing hint must claim the proven key.
            if let Some(claimed_pubkey) = &hint.pubkey
                && *claimed_pubkey != *pubkey
            {
                return Err(Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                    reason: format!("pubkey '{pubkey}' but identity claims '{claimed_pubkey}'"),
                })));
            }
            if hint.pubkey.is_none() && hint.name.is_none() {
                return Err(Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                    reason: "identity has empty hint".to_string(),
                })));
            }
            // Resolve the hint through the shared resolver, then take the
            // highest active grant that belongs to the proven pubkey. Direct
            // membership wins; otherwise fall back to the wildcard ('*') slot —
            // the tree's grant to "any key not otherwise listed". The caller
            // already proved possession of `pubkey` (session keyset check on
            // the wire path, signature verification locally), so accepting the
            // wildcard level is the structural intent of a global grant.
            if let Ok(candidates) = auth_settings.resolve_hint(hint)
                && let Some(permission) = select_effective_permission(&candidates, pubkey)
            {
                return Ok(permission);
            }
            if let Ok(global) = auth_settings.get_global_key()
                && global.is_active()
            {
                return Ok(*global.permissions());
            }
            Err(Error::Auth(Box::new(AuthError::KeyNotFound {
                key_name: hint.name.clone().unwrap_or_else(|| pubkey.to_string()),
            })))
        }
        SigKey::Delegation { .. } => {
            let mut validator = AuthValidator::new();
            let resolved_auths = validator
                .resolve_sig_key(identity, auth_settings, instance)
                .await
                .map_err(|e| {
                    Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                        reason: format!("Delegation resolution failed: {e}"),
                    }))
                })?;

            select_effective_permission(&resolved_auths, pubkey).ok_or_else(|| {
                Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                    reason: format!("no active resolved delegation key matches pubkey '{pubkey}'"),
                }))
            })
        }
    }
}

/// Highest permission among resolved candidates that belong to `pubkey` and
/// currently grant access.
///
/// The single place auth paths turn resolved candidates into an effective
/// permission: resolvers return candidates regardless of key status, so this is
/// where revocation is honoured — identically for direct, wildcard, and
/// delegated authority. Returns `None` when no active candidate matches.
pub(crate) fn select_effective_permission(
    candidates: &[ResolvedAuth],
    pubkey: &PublicKey,
) -> Option<Permission> {
    candidates
        .iter()
        .filter(|ra| ra.public_key == *pubkey && ra.grants_access())
        .map(|ra| ra.effective_permission)
        .max()
}

/// Check if a resolved authentication has sufficient permissions for an operation
pub fn check_permissions(resolved: &ResolvedAuth, operation: &Operation) -> Result<bool> {
    match operation {
        Operation::WriteData => {
            Ok(resolved.effective_permission.can_write()
                || resolved.effective_permission.can_admin())
        }
        Operation::WriteSettings => Ok(resolved.effective_permission.can_admin()),
    }
}
