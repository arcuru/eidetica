//! Core entry validation for authentication
//!
//! This module provides the main entry point for validating entries
//! and the AuthValidator struct that coordinates all validation operations.

use std::collections::HashMap;

use tracing::debug;

use super::{floors::FloorWalker, resolver::KeyResolver};
use crate::{
    Entry, Error, Instance, Result,
    auth::{
        crypto::verify_entry_signature,
        errors::AuthError,
        settings::AuthSettings,
        types::{Operation, ResolvedAuth, SigKey},
    },
    backend::{BackendError, BackendImpl, Reachability},
    constants::SETTINGS,
    crdt::{Doc, doc::Value},
    entry::ID,
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
        if entry.auth().malformed_reason().is_some() {
            debug!("Malformed entry detected");
            return Ok(false);
        }

        // Check if auth is configured (keys or global permission)
        let has_auth =
            !auth_settings.get_all_keys()?.is_empty() || auth_settings.has_global_permission();

        // Handle unsigned entries
        if entry.auth().is_unsigned() {
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

        // A `_settings` write may only move a committed delegation pointer
        // forward. This is independent of which key signs, so it is decided
        // before key resolution; it touches the backend only when the write
        // actually changes a pointer.
        if !self
            .check_delegation_pointers_forward(entry, auth_settings, instance)
            .await?
        {
            return Ok(false);
        }

        // Resolve all matching keys. The claimed tips in a delegation SigKey pin
        // resolution: the delegated tree's auth settings are read as of those
        // tips (not its live head), and the tips may regress below neither the
        // snapshot the parent tree committed for the delegation nor any snapshot
        // the entry's ancestors pinned for that tree (see
        // DelegationResolver::resolve_delegation_path_for_entry).
        let resolution = match (&entry.auth().key, instance) {
            (SigKey::Delegation { .. }, Some(inst)) => {
                let engine = inst.require_local_engine()?;
                let mut floors = FloorWalker::for_entry(engine.as_ref(), entry);
                self.resolver
                    .resolve_sig_key_for_entry(
                        &entry.auth().key,
                        auth_settings,
                        instance,
                        &mut floors,
                    )
                    .await
            }
            _ => {
                self.resolver
                    .resolve_sig_key(&entry.auth().key, auth_settings, instance)
                    .await
            }
        };
        let resolved_auths = match resolution {
            Ok(auths) => auths,
            // Not a verdict: the history needed to decide is not held locally.
            // Surface it so the caller can keep the entry unverified and retry.
            Err(Error::Auth(e)) if e.is_delegated_tree_unsynced() => {
                return Err(Error::Auth(e));
            }
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
            // Skip keys that do not currently grant access
            if !resolved_auth.grants_access() {
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

    /// Gate `_settings` writes so a committed delegation pointer
    /// (`auth.delegations.<root>.tree.tips`) only ever moves forward.
    ///
    /// The committed pointer is the floor every signature through that
    /// delegation must cover, so moving it backwards would re-open the
    /// snapshot regression it exists to close. For every delegation pointer
    /// this entry writes, the new tips must ancestry-cover the tips committed
    /// for the same delegated tree in the settings the entry builds on
    /// (`auth_settings`, the pinned pre-state). Equality is allowed and
    /// decided without touching the backend. The pointer is matched by the
    /// delegated tree's root, not by the settings key it is stored under, so
    /// re-spelling the key does not reset it. A write that removes a
    /// delegation, or that only touches its bounds, is not a pointer move.
    ///
    /// Reads the raw `_settings` data this entry carries, so it covers raw
    /// settings writes and remotely ingested entries, not only
    /// `add_delegated_tree`.
    ///
    /// Returns `Ok(false)` on a proven regression or an unusable pointer, and
    /// `Err(DelegatedTreeUnsynced)` when the delegated tree is not held
    /// locally far enough to decide.
    async fn check_delegation_pointers_forward(
        &self,
        entry: &Entry,
        auth_settings: &AuthSettings,
        instance: Option<&Instance>,
    ) -> Result<bool> {
        if !entry.in_subtree(SETTINGS) {
            return Ok(true);
        }
        let Ok(data) = entry.data(SETTINGS) else {
            return Ok(true);
        };
        let Ok(settings) = serde_json::from_slice::<Doc>(data) else {
            debug!("Malformed _settings data");
            return Ok(false);
        };
        if settings.is_tombstone("auth.delegations") {
            return Ok(true);
        }
        let Some(delegations) = settings.get("auth.delegations") else {
            return Ok(true);
        };
        let Value::Doc(delegations) = delegations else {
            debug!("Malformed auth.delegations write");
            return Ok(false);
        };

        let previous = auth_settings.get_all_delegated_trees()?;
        let mut engine: Option<std::sync::Arc<dyn BackendImpl>> = None;

        for (key, value) in delegations.iter() {
            if matches!(value, Value::Deleted) {
                continue;
            }
            let Value::Doc(delegation) = value else {
                debug!("Malformed delegation write for {key}");
                return Ok(false);
            };
            if delegation.is_tombstone("tree") {
                continue;
            }
            let Some(tree) = delegation.get("tree") else {
                continue;
            };
            let Value::Doc(tree) = tree else {
                debug!("Malformed delegation tree write for {key}");
                return Ok(false);
            };
            let Some(new_tips) = delegation_pointer_tips(tree)? else {
                continue;
            };
            let root = match tree.get("root") {
                Some(Value::Text(root)) => ID::parse(root)?,
                Some(_) => {
                    debug!("Malformed delegation root write for {key}");
                    return Ok(false);
                }
                None => match ID::parse(key) {
                    Ok(root) => root,
                    // Neither a root nor an ID key: nothing resolves through it.
                    Err(_) => continue,
                },
            };

            let mut previous_tips: Vec<ID> = previous
                .values()
                .filter(|tree_ref| tree_ref.tree.root == root)
                .flat_map(|tree_ref| tree_ref.tree.tips.iter().cloned())
                .collect();
            previous_tips.sort();
            previous_tips.dedup();
            if previous_tips.is_empty() {
                continue;
            }
            let mut sorted_new = new_tips.clone();
            sorted_new.sort();
            sorted_new.dedup();
            if sorted_new == previous_tips {
                continue;
            }

            let engine = match &engine {
                Some(engine) => engine,
                None => {
                    let inst = instance.ok_or_else(|| AuthError::DatabaseRequired {
                        operation: "delegation pointer validation".to_string(),
                    })?;
                    engine.insert(inst.require_local_engine()?)
                }
            };
            let targets = crate::Snapshot::from(
                previous_tips
                    .iter()
                    .cloned()
                    .chain(std::iter::once(root.clone()))
                    .collect::<Vec<_>>(),
            );
            match engine
                .check_targets_reachable_from(&root, &new_tips, &targets)
                .await
            {
                Ok(Reachability::Reachable) => {}
                Ok(Reachability::Unreachable) => {
                    debug!(
                        "{}",
                        AuthError::DelegationPointerRegressed {
                            tree_id: Box::new(root),
                            previous_tips: previous_tips.into_boxed_slice(),
                            new_tips: new_tips.into_boxed_slice(),
                        }
                    );
                    return Ok(false);
                }
                Ok(Reachability::Indeterminate { missing }) => {
                    return Err(AuthError::DelegatedTreeUnsynced {
                        tree_id: root,
                        missing,
                    }
                    .into());
                }
                // A pointer naming an entry of some other tree is unusable.
                Err(Error::Backend(e)) if matches!(*e, BackendError::EntryNotInTree { .. }) => {
                    debug!("Delegation pointer for {root} names a tip outside the tree");
                    return Ok(false);
                }
                Err(e) => return Err(e),
            }
        }

        Ok(true)
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

/// The `tips` a `_settings` delegation write commits, read leniently from
/// its `tree` doc: `Ok(None)` when the write does not touch the pointer, an empty
/// list when it sets or deletes the pointer. Mirrors `TreeReference`'s wire shape
/// (`tips` is a doc keyed by index) without requiring the rest of the
/// reference to be present, so a partial raw write is still gated.
fn delegation_pointer_tips(tree: &Doc) -> Result<Option<Vec<ID>>> {
    if tree.is_tombstone("tips") {
        return Ok(Some(Vec::new()));
    }
    let Some(tips) = tree.get("tips") else {
        return Ok(None);
    };
    let Value::Doc(tips) = tips else {
        return Err(AuthError::InvalidAuthConfiguration {
            reason: "delegation tree tips are not a document".to_string(),
        }
        .into());
    };
    let mut entries = Vec::with_capacity(tips.keys().count());
    for (key, value) in tips.iter() {
        if matches!(value, Value::Deleted) {
            continue;
        }
        let index = key
            .parse::<usize>()
            .map_err(|_| AuthError::InvalidAuthConfiguration {
                reason: format!("delegation tip index '{key}' is not numeric"),
            })?;
        let text = value
            .as_text()
            .ok_or_else(|| AuthError::InvalidAuthConfiguration {
                reason: format!("delegation tip at index {key} is not text"),
            })?;
        entries.push((index, ID::parse(text)?));
    }
    entries.sort_by_key(|(index, _)| *index);
    Ok(Some(entries.into_iter().map(|(_, id)| id).collect()))
}

impl Default for AuthValidator {
    fn default() -> Self {
        Self::new()
    }
}
