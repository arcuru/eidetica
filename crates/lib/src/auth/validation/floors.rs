//! Inherited delegation snapshot floors
//!
//! A delegated signature pins a snapshot (a set of tips) of the delegated
//! tree it resolves through. Those snapshots must not regress along the
//! signed tree's history: for a given delegated tree, every signature on an
//! entry must ancestry-cover every snapshot pinned by the entry's ancestors
//! for that same tree. Equality is allowed — this is non-regression, not
//! freshness.
//!
//! The floor is keyed by delegated tree root, never by signer or by how the
//! delegation path spells the step, and it survives intervening entries that
//! were signed by a direct key or through some other delegated tree. This
//! module derives it from the entry DAG rather than serializing anything new
//! into entries.
//!
//! Derivation. Because every valid delegated entry already covers its own
//! inherited floor, the floor an entry inherits for tree `R` is exactly the
//! set of snapshots pinned by its *nearest* ancestors that used `R` — the
//! first `R`-using entry reached along each ancestry path. Anything further
//! back is covered transitively by those. The walk therefore starts at the
//! entry's parents, stops on each path at the first entry whose delegation
//! path names `R`, and otherwise continues through the parents. Its cost is
//! the distance back to the previous `R`-using entries on each branch, and it
//! only runs for entries that themselves carry a delegated signature.

use std::collections::{HashMap, HashSet};

use crate::{
    Entry, Result, Snapshot,
    auth::{errors::AuthError, types::SigKey},
    backend::BackendImpl,
    entry::ID,
};

/// Lazily derives, per delegated tree root, the snapshot floor an entry
/// inherits from its ancestors.
///
/// One walker serves one entry: it caches loaded ancestors and resolved tip
/// roots across the (usually single) delegated tree the entry's path names.
pub(crate) struct FloorWalker<'a> {
    backend: &'a dyn BackendImpl,
    /// Root of the tree the entry being validated belongs to.
    tree: ID,
    /// Main-tree parents of the entry being validated.
    parents: Vec<ID>,
    /// Loaded ancestors, shared across per-root walks.
    entries: HashMap<ID, Entry>,
    /// Claimed snapshot → root of the tree it belongs to.
    snapshot_roots: HashMap<Snapshot, ID>,
    /// Memoized floors per delegated tree root.
    floors: HashMap<ID, Vec<Snapshot>>,
}

impl<'a> FloorWalker<'a> {
    /// Prepare a walker for `entry`. Does no backend work until
    /// [`floor_for`](Self::floor_for) is called.
    pub(crate) fn for_entry(backend: &'a dyn BackendImpl, entry: &Entry) -> Self {
        // A root entry is its own tree and has no ancestors.
        let tree = entry.root().unwrap_or_else(|| entry.id());
        Self {
            backend,
            tree,
            parents: entry.parents().unwrap_or_default(),
            entries: HashMap::new(),
            snapshot_roots: HashMap::new(),
            floors: HashMap::new(),
        }
    }

    /// The snapshots pinned for delegated tree `root` by the entry's nearest
    /// `root`-using ancestors. Every returned snapshot must be ancestry-covered
    /// by a new signature through `root`. Empty when no ancestor used `root`.
    ///
    /// Fails closed with [`AuthError::DelegatedTreeUnsynced`] (naming the
    /// signed tree and the absent entries) when an ancestor, or a tip an
    /// ancestor claimed, is not held locally: the floor cannot be established
    /// from partial history, and that is a retriable condition, not a verdict.
    /// On the normal verification paths this cannot trigger — an entry is only
    /// validated once its parents are `Verified`, and verification is
    /// prefix-closed — but a direct caller may ask about an entry whose history
    /// it does not hold.
    pub(crate) async fn floor_for(&mut self, root: &ID) -> Result<&[Snapshot]> {
        if !self.floors.contains_key(root) {
            let floor = self.walk(root).await?;
            self.floors.insert(root.clone(), floor);
        }
        Ok(self.floors.get(root).map(Vec::as_slice).unwrap_or(&[]))
    }

    async fn walk(&mut self, root: &ID) -> Result<Vec<Snapshot>> {
        let mut snapshots = Vec::new();
        let mut seen_snapshots = HashSet::new();
        let mut visited: HashSet<ID> = HashSet::new();
        let mut missing: Vec<ID> = Vec::new();
        let mut stack: Vec<ID> = self.parents.clone();

        while let Some(id) = stack.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let Some(entry) = self.load(&id).await? else {
                missing.push(id);
                continue;
            };

            let mut named_root = false;
            if let SigKey::Delegation { path, .. } = &entry.auth().key {
                for step in path {
                    let snapshot = Snapshot::from(&step.tips);
                    if snapshot.is_empty() {
                        continue;
                    }
                    let Some(step_root) = self.snapshot_root(&snapshot).await? else {
                        missing.extend(snapshot.iter().cloned());
                        continue;
                    };
                    if &step_root != root {
                        continue;
                    }
                    named_root = true;
                    if seen_snapshots.insert(snapshot.clone()) {
                        snapshots.push(snapshot);
                    }
                }
            }

            // An ancestor that pinned `root` covers everything behind it on
            // this path; stop here. Otherwise the floor carries through.
            if !named_root {
                stack.extend(entry.parents()?);
            }
        }

        if !missing.is_empty() {
            missing.sort();
            missing.dedup();
            return Err(AuthError::DelegatedTreeUnsynced {
                tree_id: self.tree.clone(),
                missing,
            }
            .into());
        }

        Ok(snapshots)
    }

    /// Load an ancestor, caching it. `Ok(None)` when it is not held locally.
    async fn load(&mut self, id: &ID) -> Result<Option<Entry>> {
        if let Some(entry) = self.entries.get(id) {
            return Ok(Some(entry.clone()));
        }
        match self.backend.get(id).await {
            Ok(entry) => {
                self.entries.insert(id.clone(), entry.clone());
                Ok(Some(entry))
            }
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The root shared by every tip in a claimed snapshot, read from the tips
    /// themselves so the answer does not depend on how the ancestor's path
    /// spelled the step. `Ok(None)` when any tip is not held locally. A mixed-
    /// root snapshot is invalid and fails closed.
    async fn snapshot_root(&mut self, snapshot: &Snapshot) -> Result<Option<ID>> {
        if let Some(root) = self.snapshot_roots.get(snapshot) {
            return Ok(Some(root.clone()));
        }
        let mut root: Option<ID> = None;
        for tip in snapshot {
            let entry = match self.backend.get(tip).await {
                Ok(entry) => entry,
                Err(e) if e.is_not_found() => return Ok(None),
                Err(e) => return Err(e),
            };
            // A tree's root entry has no `root` of its own; it *is* the tree.
            let tip_root = entry.root().unwrap_or_else(|| entry.id());
            if root.as_ref().is_some_and(|root| root != &tip_root) {
                return Err(AuthError::InvalidDelegationTips {
                    tree_id: tip_root,
                    claimed_tips: snapshot.tips().to_vec(),
                }
                .into());
            }
            root = Some(tip_root);
        }
        if let Some(root) = root {
            self.snapshot_roots.insert(snapshot.clone(), root.clone());
            Ok(Some(root))
        } else {
            Ok(None)
        }
    }
}
