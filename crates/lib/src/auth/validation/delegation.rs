//! Delegation path resolution for authentication
//!
//! This module handles the complex logic of resolving delegation paths,
//! including multi-tree traversal and permission clamping.

use std::collections::HashSet;

use crate::{
    Database, Instance, Result, Snapshot,
    auth::{
        errors::AuthError,
        permission::clamp_permission,
        settings::AuthSettings,
        types::{DelegationStep, KeyHint, PermissionBounds, ResolvedAuth},
    },
    entry::ID,
};

/// Maximum number of steps in a single delegation path.
///
/// The path is wire-supplied and processed as a flat list, so its length is the
/// delegation-chain depth. Bounding it caps the backend work an unauthenticated
/// signature key can force before the authorization gate decides (mirrors the
/// `MAX_DELEGATION_DEPTH` recursion guard in the resolver).
const MAX_DELEGATION_STEPS: usize = 10;

/// Maximum number of claimed tips per delegation step.
///
/// Tips are wire-supplied and each drives DAG traversal; bound the per-step
/// fan-out. A legitimate tree frontier is small (concurrent heads only).
const MAX_DELEGATION_TIPS: usize = 64;

/// Delegation resolver for handling complex delegation paths
pub struct DelegationResolver;

impl DelegationResolver {
    /// Create a new delegation resolver
    pub fn new() -> Self {
        Self
    }

    /// Resolve delegation path using flat list structure
    ///
    /// This iteratively processes each step in the delegation path,
    /// applying permission clamping at each level. The final hint
    /// is resolved in the last delegated tree's auth settings.
    ///
    /// Returns all matching ResolvedAuth entries. For name hints that match
    /// multiple keys at the final step, all matches are returned with the
    /// same permission clamping applied to each.
    pub async fn resolve_delegation_path_with_depth(
        &mut self,
        steps: &[DelegationStep],
        final_hint: &KeyHint,
        auth_settings: &AuthSettings,
        instance: &Instance,
        _depth: usize,
    ) -> Result<Vec<ResolvedAuth>> {
        if steps.is_empty() {
            return Err(AuthError::EmptyDelegationPath.into());
        }

        // Bound the wire-supplied path length before doing any backend work.
        if steps.len() > MAX_DELEGATION_STEPS {
            return Err(AuthError::DelegationPathTooLong {
                len: steps.len(),
                max: MAX_DELEGATION_STEPS,
            }
            .into());
        }

        // Validate no global hints in delegation (must resolve to concrete key)
        if final_hint.is_global() {
            return Err(AuthError::InvalidDelegationStep {
                reason: "Delegation paths cannot use global '*' hint".to_string(),
            }
            .into());
        }

        // Iterate through delegation steps
        let mut current_auth_settings = auth_settings.clone();
        let current_backend = instance
            .backend()
            .local_engine()
            .expect("delegation validation requires local backend");
        let mut cumulative_bounds: Option<PermissionBounds> = None;

        // Process all delegation steps (tree traversal)
        for step in steps {
            // Bound the wire-supplied claimed tips before any backend traversal.
            if step.tips.len() > MAX_DELEGATION_TIPS {
                return Err(AuthError::DelegationTipsTooMany {
                    tree_id: step.tree.clone(),
                    len: step.tips.len(),
                    max: MAX_DELEGATION_TIPS,
                }
                .into());
            }

            // Look up the delegation declaration in the *parent's* settings. The
            // declaration carries `tree.tips` — the snapshot the parent tree has
            // committed for this delegation — which is the monotonicity floor
            // enforced below. Because the parent's auth settings here are taken
            // at the validating entry's own settings snapshot, the floor is the
            // historically-correct one, not a global "now".
            let delegated_tree_ref = current_auth_settings.get_delegated_tree(&step.tree)?;

            let root_id = delegated_tree_ref.tree.root.clone();
            let delegated_tree = Database::open(instance, &root_id).await.map_err(|e| {
                AuthError::DelegatedTreeLoadFailed {
                    tree_id: root_id.clone(),
                    source: Box::new(e),
                }
            })?;

            // Tree-scoped validation of the claimed tips: every claimed tip must
            // be a real entry belonging to *this* delegated tree. `get_tree_from_tips`
            // returns the ancestor set reachable from the claimed tips and errors
            // (EntryNotInTree / EntryNotFound) on foreign or fabricated tips — this
            // replaces the prior check that merely confirmed an entry existed
            // somewhere in the backend.
            let reachable = current_backend
                .get_tree_from_tips(&root_id, &step.tips)
                .await
                .map_err(|_| AuthError::InvalidDelegationTips {
                    tree_id: root_id.clone(),
                    claimed_tips: step.tips.clone(),
                })?;
            let reachable_ids: HashSet<ID> = reachable.into_iter().map(|e| e.id()).collect();

            // Monotonicity floor: the claimed snapshot may not regress below the
            // snapshot the parent tree committed for this delegation. Equivalently,
            // every floor tip must be an ancestor-or-equal of the claimed tips
            // (i.e. reachable from them). This stops an entry from time-travelling
            // the delegated tree backwards to resurrect auth state that the parent
            // has already advanced past (e.g. a since-revoked key). Advancing the
            // floor is an admin-gated `_settings` write on the parent tree.
            //
            // FIXME(security): this floor is the only monotonicity guarantee today
            // and is a known partial fix. It does not enforce strict per-entry
            // non-regression (sibling entries above the floor may still differ), nor
            // does it gate settings writes to keep the committed pointer itself
            // moving only forward. Both remain to be done.
            let floor = &delegated_tree_ref.tree.tips;
            if !floor.iter().all(|tip| reachable_ids.contains(tip)) {
                return Err(AuthError::InvalidDelegationTips {
                    tree_id: root_id.clone(),
                    claimed_tips: step.tips.clone(),
                }
                .into());
            }

            // Resolve the delegated tree's auth settings AS OF the claimed tips,
            // not its live head: permissions are evaluated at the state the signer
            // actually observed. This is safe now that the snapshot cannot regress
            // below the committed floor. `new_transaction_at` re-validates the tips
            // are in-tree (defence in depth) and is never committed — it is used
            // purely as a read anchor at the pinned snapshot.
            let pinned_txn = delegated_tree
                .new_transaction_at(&Snapshot::from(step.tips.clone()))
                .await
                .map_err(|_| AuthError::InvalidDelegationTips {
                    tree_id: root_id.clone(),
                    claimed_tips: step.tips.clone(),
                })?;
            current_auth_settings =
                pinned_txn
                    .get_settings()?
                    .auth_snapshot()
                    .await
                    .map_err(|e| AuthError::InvalidAuthConfiguration {
                        reason: format!(
                            "Failed to read delegated tree auth settings at claimed tips: {e}"
                        ),
                    })?;

            // Accumulate permission bounds
            cumulative_bounds = Some(match cumulative_bounds {
                Some(existing_bounds) => {
                    // Combine bounds by taking the minimum of max permissions
                    let new_max = std::cmp::min(
                        existing_bounds.max,
                        delegated_tree_ref.permission_bounds.max,
                    );
                    let new_min = match (
                        existing_bounds.min,
                        delegated_tree_ref.permission_bounds.min,
                    ) {
                        (Some(existing_min), Some(new_min)) => {
                            Some(std::cmp::max(existing_min, new_min))
                        }
                        (Some(existing_min), None) => Some(existing_min),
                        (None, Some(new_min)) => Some(new_min),
                        (None, None) => None,
                    };
                    PermissionBounds {
                        max: new_max,
                        min: new_min,
                    }
                }
                None => delegated_tree_ref.permission_bounds.clone(),
            });
        }

        // After traversing all steps, resolve the final hint in the last tree's auth settings
        let mut matches = current_auth_settings.resolve_hint(final_hint)?;
        if matches.is_empty() {
            return Err(AuthError::KeyNotFound {
                key_name: format!("hint({:?})", final_hint.hint_type()),
            }
            .into());
        }

        // Apply accumulated permission bounds to all matches
        if let Some(bounds) = cumulative_bounds {
            for resolved in &mut matches {
                resolved.effective_permission =
                    clamp_permission(resolved.effective_permission, &bounds);
            }
        }

        Ok(matches)
    }
}

impl Default for DelegationResolver {
    fn default() -> Self {
        Self::new()
    }
}
