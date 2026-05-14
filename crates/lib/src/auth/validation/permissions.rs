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
            auth_settings.get_global_permission().ok_or_else(|| {
                Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                    reason: "Global '*' permission not configured".to_string(),
                }))
            })
        }
        SigKey::Direct { hint } => match (&hint.pubkey, &hint.name) {
            (Some(claimed_pubkey), _) => {
                if *claimed_pubkey != *pubkey {
                    return Err(Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                        reason: format!("pubkey '{pubkey}' but identity claims '{claimed_pubkey}'"),
                    })));
                }
                let auth_key = auth_settings.get_key_by_pubkey(pubkey)?;
                Ok(*auth_key.permissions())
            }
            (_, Some(name)) => {
                let matches = auth_settings.find_keys_by_name(name);
                if matches.is_empty() {
                    return Err(Error::Auth(Box::new(AuthError::KeyNotFound {
                        key_name: name.clone(),
                    })));
                }
                let pubkey_str = pubkey.to_string();
                let (_, auth_key) = matches
                    .iter()
                    .find(|(pk, _)| *pk == pubkey_str)
                    .ok_or_else(|| {
                        Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                            reason: format!(
                                "pubkey '{pubkey}' but no key named '{name}' has that pubkey"
                            ),
                        }))
                    })?;
                Ok(*auth_key.permissions())
            }
            _ => Err(Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                reason: "identity has empty hint".to_string(),
            }))),
        },
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

            resolved_auths
                .into_iter()
                .find(|ra| ra.public_key == *pubkey)
                .map(|ra| ra.effective_permission)
                .ok_or_else(|| {
                    Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                        reason: format!("no resolved delegation key matches pubkey '{pubkey}'"),
                    }))
                })
        }
    }
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
