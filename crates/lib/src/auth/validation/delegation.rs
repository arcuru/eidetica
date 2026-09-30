//! Delegation path resolution for authentication
//!
//! This module handles the complex logic of resolving delegation paths,
//! including multi-tree traversal and permission clamping.

use super::floors::DerivedFloors;
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
pub(crate) const MAX_DELEGATION_STEPS: usize = 10;

/// Maximum number of claimed tips per delegation step.
///
/// Tips are wire-supplied and each drives DAG traversal; bound the per-step
/// fan-out. A legitimate tree frontier is small (concurrent heads only).
pub(crate) const MAX_DELEGATION_TIPS: usize = 64;

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

enum AuthorityState<'a> {
    ClaimedSnapshot(Option<&'a mut DerivedFloors>),
    CurrentAccepted,
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
    /// This low-level resolver needs local verification evidence. Connected
    /// callers use [`crate::Database::verify_entry_signature`] for the full
    /// historical check; signature hints alone do not authenticate a signer.
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
        self.resolve_delegation_path_inner(
            steps,
            final_hint,
            auth_settings,
            instance,
            AuthorityState::ClaimedSnapshot(None),
        )
        .await
    }

    /// Resolve live access at each delegated database's accepted frontier.
    ///
    /// Claimed tips are bounded hints, not authority. The caller must also
    /// supply the target's current accepted auth settings.
    pub(crate) async fn resolve_live_delegation_path(
        &mut self,
        steps: &[DelegationStep],
        final_hint: &KeyHint,
        auth_settings: &AuthSettings,
        instance: &Instance,
    ) -> Result<Vec<ResolvedAuth>> {
        self.resolve_delegation_path_inner(
            steps,
            final_hint,
            auth_settings,
            instance,
            AuthorityState::CurrentAccepted,
        )
        .await
    }

    /// [`resolve_delegation_path`](Self::resolve_delegation_path) for a
    /// concrete entry: each step's claimed snapshot must also ancestry-cover
    /// every snapshot of the same delegated tree pinned by the entry's
    /// ancestors (see [`DerivedFloors`]).
    pub(crate) async fn resolve_delegation_path_for_entry(
        &mut self,
        steps: &[DelegationStep],
        final_hint: &KeyHint,
        auth_settings: &AuthSettings,
        instance: &Instance,
        floors: &mut DerivedFloors,
    ) -> Result<Vec<ResolvedAuth>> {
        self.resolve_delegation_path_inner(
            steps,
            final_hint,
            auth_settings,
            instance,
            AuthorityState::ClaimedSnapshot(Some(floors)),
        )
        .await
    }

    async fn resolve_delegation_path_inner(
        &mut self,
        steps: &[DelegationStep],
        final_hint: &KeyHint,
        auth_settings: &AuthSettings,
        instance: &Instance,
        mut authority: AuthorityState<'_>,
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
        let current_backend = instance.require_local_engine()?;
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
            // declaration carries `tree.tips`, a floor for this path. Parent
            // settings are pinned for historical validation and current accepted
            // state for live access.
            let delegated_tree_ref = current_auth_settings.get_delegated_tree(&step.tree)?;

            let root_id = delegated_tree_ref.tree.root.clone();
            if step.tree != root_id {
                return Err(AuthError::InvalidAuthConfiguration {
                    reason: "delegation key differs from its tree root".to_string(),
                }
                .into());
            }
            // Read the retained accepted frontier directly: Database::snapshot
            // may start cross-tree verification or expose raw tips while a
            // verification pass is on the stack.
            let snapshot = match &authority {
                AuthorityState::ClaimedSnapshot(_) => Snapshot::from(&step.tips),
                AuthorityState::CurrentAccepted => {
                    current_backend.verified_snapshot(&root_id).await?
                }
            };
            if snapshot.len() > MAX_DELEGATION_TIPS {
                return Err(AuthError::DelegationTipsTooMany {
                    tree_id: root_id.clone(),
                    len: snapshot.len(),
                    max: MAX_DELEGATION_TIPS,
                }
                .into());
            }
            if snapshot.is_empty() {
                return Err(AuthError::InvalidDelegationTips {
                    tree_id: root_id,
                    claimed_tips: Vec::new(),
                }
                .into());
            }
            // The selected snapshot must cover its parent's configured pointer.
            // Main-tree validation also checks the per-entry parent-joined
            // derived floor (including same-entry first-hop pointer writes).
            // Membership and complete locally Verified ancestry are checked
            // below. Missing and present-but-Unverified history is retryable;
            // a proven regression or foreign-tree tip is not.
            let floor = Snapshot::from(delegated_tree_ref.tree.tips.to_vec());
            if floor.is_empty() {
                return Err(AuthError::InvalidDelegationTips {
                    tree_id: root_id.clone(),
                    claimed_tips: vec![],
                }
                .into());
            }
            let directly_referenced = Snapshot::from(
                std::iter::once(root_id.clone())
                    .chain(snapshot.iter().cloned())
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
                        claimed_tips: snapshot.tips().to_vec(),
                    }
                    .into());
                }
                Err(error) => return Err(error),
            }
            match current_backend
                .check_targets_reachable_from(&root_id, &snapshot, &floor)
                .await
            {
                Err(Error::Backend(error))
                    if matches!(*error, BackendError::EntryNotInTree { .. }) =>
                {
                    return Err(AuthError::InvalidDelegationTips {
                        tree_id: root_id.clone(),
                        claimed_tips: snapshot.tips().to_vec(),
                    }
                    .into());
                }
                Err(error) => return Err(error),
                Ok(Reachability::Reachable) => {}
                Ok(Reachability::Unreachable) => {
                    return Err(AuthError::InvalidDelegationTips {
                        tree_id: root_id.clone(),
                        claimed_tips: snapshot.tips().to_vec(),
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
            // snapshot cannot be resurrected below a still-active inherited
            // floor. Keyed by tree root, so it is unaffected by which key
            // signs or by intervening direct-key / other-tree signatures. The
            // committed pointer above is checked first, so the claimed tips are
            // already known to be members of this tree.
            if let AuthorityState::ClaimedSnapshot(Some(floors)) = &mut authority {
                let targets = floors.floor_for(&root_id);
                if !targets.is_empty() {
                    match current_backend
                        .check_targets_reachable_from(&root_id, &snapshot, &targets)
                        .await
                    {
                        Err(Error::Backend(error))
                            if matches!(*error, BackendError::EntryNotInTree { .. }) =>
                        {
                            return Err(AuthError::InvalidDelegationTips {
                                tree_id: root_id.clone(),
                                claimed_tips: snapshot.tips().to_vec(),
                            }
                            .into());
                        }
                        Err(error) => return Err(error),
                        Ok(Reachability::Reachable) => {}
                        Ok(Reachability::Unreachable) => {
                            return Err(AuthError::DelegationSnapshotRegressed {
                                tree_id: Box::new(root_id.clone()),
                                claimed_tips: snapshot.tips().to_vec(),
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

            DerivedFloors::proof_on(current_backend.as_ref(), &root_id, &snapshot).await?;
            // Each parent tree's configured pointer needs a complete locally
            // verified proof, including the middle steps of a nested path.
            if floor != snapshot {
                DerivedFloors::proof_on(current_backend.as_ref(), &root_id, &floor).await?;
            }

            // Materialize the same immutable boundary proved above: the signed
            // claim for history, or the accepted frontier for live access.
            // Non-NotFound backend failures remain operational errors.
            current_auth_settings = match current_backend
                .get_tree_from_tips(&root_id, &snapshot)
                .await
            {
                Ok(entries) => {
                    if !entries.iter().any(|entry| entry.id() == root_id) {
                        return Err(AuthError::InvalidDelegationTips {
                            tree_id: root_id.clone(),
                            claimed_tips: snapshot.tips().to_vec(),
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
