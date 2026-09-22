//! Delegation path resolution for authentication
//!
//! This module handles the complex logic of resolving delegation paths,
//! including multi-tree traversal and permission clamping.

use super::floors::FloorWalker;
use crate::{
    Error, Instance, Result, Snapshot,
    auth::{
        errors::AuthError,
        permission::clamp_permission,
        settings::AuthSettings,
        types::{DelegationStep, KeyHint, PermissionBounds, ResolvedAuth},
    },
    backend::{BackendError, BackendImpl, Reachability},
};

/// Maximum number of steps in a single delegation path.
///
/// The path is wire-supplied and processed as a flat list, so its length is the
/// delegation-chain depth, and checking it here bounds that depth before any
/// backend work happens. That caps what an unauthenticated signature key can
/// force the resolver to do before the authorization gate decides.
const MAX_DELEGATION_STEPS: usize = 10;

/// Maximum number of claimed tips per delegation step.
///
/// Tips are wire-supplied and each drives DAG traversal; bound the per-step
/// fan-out. A legitimate tree frontier is small (concurrent heads only).
const MAX_DELEGATION_TIPS: usize = 64;

/// Check the entries named directly by a delegation step before traversing
/// between them. This preserves the useful initial dependency set (including
/// an absent database root) without forcing the reachability check to use the
/// root as its floor and walk the whole history a second time.
async fn missing_delegation_entries(
    backend: &dyn BackendImpl,
    tree: &crate::ID,
    ids: &Snapshot,
) -> Result<Vec<crate::ID>> {
    let mut missing = Vec::new();
    for id in ids {
        match backend.get(id).await {
            Ok(entry) if entry.in_tree(tree) => {}
            Ok(_) => {
                return Err(BackendError::EntryNotInTree {
                    entry_id: id.clone(),
                    tree_id: tree.clone(),
                }
                .into());
            }
            Err(e) if e.is_not_found() => missing.push(id.clone()),
            Err(e) => return Err(e),
        }
    }
    Ok(missing)
}

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
    /// Resolution is a flat loop, not recursion: the path arrives as a list, so
    /// chain depth is `steps.len()` and is bounded up front by
    /// [`MAX_DELEGATION_STEPS`]. Nothing here re-enters the resolver — the final
    /// hint is looked up directly in the last tree's settings.
    ///
    /// Returns all matching ResolvedAuth entries. For name hints that match
    /// multiple keys at the final step, all matches are returned with the
    /// same permission clamping applied to each.
    ///
    /// This entry-less form enforces the committed settings-pointer floor
    /// only. Entry validation goes through
    /// [`resolve_delegation_path_for_entry`](Self::resolve_delegation_path_for_entry),
    /// which additionally enforces the floor inherited from the entry's
    /// ancestors.
    pub async fn resolve_delegation_path(
        &mut self,
        steps: &[DelegationStep],
        final_hint: &KeyHint,
        auth_settings: &AuthSettings,
        instance: &Instance,
    ) -> Result<Vec<ResolvedAuth>> {
        self.resolve_delegation_path_inner(steps, final_hint, auth_settings, instance, None)
            .await
    }

    /// [`resolve_delegation_path`](Self::resolve_delegation_path) for a
    /// concrete entry: each step's claimed snapshot must also ancestry-cover
    /// every snapshot of the same delegated tree pinned by the entry's
    /// ancestors (see [`FloorWalker`]).
    pub(crate) async fn resolve_delegation_path_for_entry(
        &mut self,
        steps: &[DelegationStep],
        final_hint: &KeyHint,
        auth_settings: &AuthSettings,
        instance: &Instance,
        floors: &mut FloorWalker<'_>,
    ) -> Result<Vec<ResolvedAuth>> {
        self.resolve_delegation_path_inner(steps, final_hint, auth_settings, instance, Some(floors))
            .await
    }

    async fn resolve_delegation_path_inner(
        &mut self,
        steps: &[DelegationStep],
        final_hint: &KeyHint,
        auth_settings: &AuthSettings,
        instance: &Instance,
        mut floors: Option<&mut FloorWalker<'_>>,
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
            if step.tips.is_empty() {
                return Err(AuthError::InvalidDelegationTips {
                    tree_id: root_id,
                    claimed_tips: Vec::new(),
                }
                .into());
            }
            // Tree-scoped membership + monotonicity floor. The claimed snapshot
            // may not regress below the snapshot the parent committed for this
            // delegation (`delegated_tree_ref.tree.tips`, the "floor"): every floor
            // tip must be an ancestor-or-equal of the claimed tips.
            // `check_targets_reachable_from` answers exactly that, and in doing so
            // validates that each claimed tip is a real entry of this delegated
            // tree (rejecting foreign or fabricated tips). It is bounded by the
            // target floor height, so the cost tracks the floor distance rather
            // than the whole tree on both the reachable and unreachable paths —
            // which matters because this runs on every delegated-entry validation
            // (and re-validation). Membership here is *presence in the tree*, not
            // `VerificationStatus::Verified`: a delegation can legitimately resolve
            // against a delegated tree whose entries are still unverified locally
            // (e.g. just arrived over sync and not yet re-verified).
            //
            // The floor stops an entry time-travelling the delegated tree backwards
            // to resurrect auth state the parent has already advanced past (e.g. a
            // since-revoked key). Advancing the floor is an admin-gated `_settings`
            // write on the parent tree.
            //
            // A three-state verdict keeps a *proven* regression (Unreachable →
            // reject) distinct from "the delegated tree hasn't synced far enough
            // to decide" (Indeterminate → surface a retriable error so the entry
            // stays unverified and is re-checked once `missing` arrives, instead
            // of being rejected as a forgery).
            //
            // This committed pointer is one of two floors. The other — the
            // snapshots the entry's own ancestors pinned for this tree — is
            // checked just below when an entry is being validated. The pointer
            // itself may only move forward (`AuthValidator` gates `_settings`
            // writes), so together they pin the snapshot per entry.
            let floor = Snapshot::from(delegated_tree_ref.tree.tips.to_vec());
            let directly_referenced = Snapshot::from(
                std::iter::once(root_id.clone())
                    .chain(step.tips.iter().cloned())
                    .chain(floor.iter().cloned())
                    .collect::<Vec<_>>(),
            );
            match missing_delegation_entries(
                current_backend.as_ref(),
                &root_id,
                &directly_referenced,
            )
            .await
            {
                Ok(missing) if !missing.is_empty() => {
                    return Err(AuthError::DelegatedTreeUnsynced {
                        tree_id: root_id.clone(),
                        missing,
                    }
                    .into());
                }
                Ok(_) => {}
                Err(Error::Backend(error))
                    if matches!(*error, BackendError::EntryNotInTree { .. }) =>
                {
                    return Err(AuthError::InvalidDelegationTips {
                        tree_id: root_id.clone(),
                        claimed_tips: step.tips.clone(),
                    }
                    .into());
                }
                Err(error) => return Err(error),
            }
            match current_backend
                .check_targets_reachable_from(&root_id, &step.tips, &floor)
                .await
            {
                Err(Error::Backend(error))
                    if matches!(*error, BackendError::EntryNotInTree { .. }) =>
                {
                    return Err(AuthError::InvalidDelegationTips {
                        tree_id: root_id.clone(),
                        claimed_tips: step.tips.clone(),
                    }
                    .into());
                }
                Err(error) => return Err(error),
                Ok(Reachability::Reachable) => {}
                Ok(Reachability::Unreachable) => {
                    return Err(AuthError::InvalidDelegationTips {
                        tree_id: root_id.clone(),
                        claimed_tips: step.tips.clone(),
                    }
                    .into());
                }
                Ok(Reachability::Indeterminate { missing }) => {
                    return Err(AuthError::DelegatedTreeUnsynced {
                        tree_id: root_id.clone(),
                        missing,
                    }
                    .into());
                }
            }

            // Inherited floor: the claimed snapshot must ancestry-cover every
            // snapshot of this same delegated tree that the entry's ancestors
            // pinned, joined across all parents. Equality is allowed (an old
            // branch may keep using an old snapshot); regression is not, so a
            // snapshot in which an identity member has since been removed cannot
            // be resurrected below a parent that already acknowledged the
            // removal. Keyed by tree root, so it is unaffected by which key
            // signs or by intervening direct-key / other-tree signatures. The
            // committed pointer above is checked first, so the claimed tips are
            // already known to be members of this tree.
            if let Some(floors) = floors.as_deref_mut() {
                let targets = Snapshot::from(
                    floors
                        .floor_for(&root_id)
                        .await?
                        .iter()
                        .flat_map(|snapshot| snapshot.iter())
                        .cloned()
                        .collect::<Vec<_>>(),
                );
                if !targets.is_empty() {
                    match current_backend
                        .check_targets_reachable_from(&root_id, &step.tips, &targets)
                        .await
                    {
                        Err(Error::Backend(error))
                            if matches!(*error, BackendError::EntryNotInTree { .. }) =>
                        {
                            return Err(AuthError::InvalidDelegationTips {
                                tree_id: root_id.clone(),
                                claimed_tips: step.tips.clone(),
                            }
                            .into());
                        }
                        Err(error) => return Err(error),
                        Ok(Reachability::Reachable) => {}
                        Ok(Reachability::Unreachable) => {
                            return Err(AuthError::DelegationSnapshotRegressed {
                                tree_id: Box::new(root_id.clone()),
                                claimed_tips: step.tips.clone(),
                            }
                            .into());
                        }
                        Ok(Reachability::Indeterminate { missing }) => {
                            return Err(AuthError::DelegatedTreeUnsynced {
                                tree_id: root_id.clone(),
                                missing,
                            }
                            .into());
                        }
                    }
                }
            }

            // Resolve the delegated tree's auth settings AS OF the claimed tips,
            // not its live head: permissions are evaluated at the state the signer
            // actually observed. This is safe now that the snapshot cannot regress
            // below the committed floor. Fetching the tree snapshot is both the
            // completeness check and the materialization input, so this replaces
            // the old extra full-history probe. Backends may satisfy it in one
            // traversal/query; non-NotFound failures propagate unchanged.
            let snapshot = Snapshot::from(&step.tips);
            current_auth_settings = match current_backend
                .get_tree_from_tips(&root_id, &snapshot)
                .await
            {
                Ok(entries) => {
                    if !entries.iter().any(|entry| entry.id() == root_id) {
                        return Err(AuthError::InvalidDelegationTips {
                            tree_id: root_id.clone(),
                            claimed_tips: step.tips.clone(),
                        }
                        .into());
                    }
                    crate::database::fold_settings_entries(&entries)?
                }
                Err(e) if e.is_not_found() => {
                    let missing = e
                        .entry_id()
                        .cloned()
                        .map_or_else(|| snapshot.tips().to_vec(), |id| vec![id]);
                    return Err(AuthError::DelegatedTreeUnsynced {
                        tree_id: root_id.clone(),
                        missing,
                    }
                    .into());
                }
                Err(e) => return Err(e),
            };

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
