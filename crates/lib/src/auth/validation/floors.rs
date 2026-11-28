//! Disposable, per-entry authorization frontiers. Only locally Verified parents
//! can contribute to a child's floor; the cache is never an authority.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    Database, Entry, Error, Result, Snapshot,
    auth::{
        errors::AuthError,
        settings::AuthSettings,
        types::{DelegatedTreeRef, SigKey},
    },
    backend::{
        BackendError, BackendImpl, CacheScope, ProjectionDescriptor, RecordMutations,
        StoreStateLifecycle, StoreStateRequest, VerificationStatus,
    },
    constants::SETTINGS,
    crdt::doc::Value,
    entry::ID,
};

#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    // Direct declarations/claims are reset on effective removal. Observations
    // through deeper paths survive even when the same root is also direct.
    direct: HashMap<ID, Snapshot>,
    nested: HashMap<ID, Snapshot>,
}

pub(crate) struct DerivedFloors {
    backend: std::sync::Arc<dyn BackendImpl>,
    tree: ID,
    state: State,
    // Effective removal resets the output, not the removing entry's authority.
    // Retain these inherited observations only for its signature coverage gate.
    removed_direct: HashMap<ID, Snapshot>,
}

enum VerifiedHistory<'a> {
    Local(&'a DerivedFloors),
    Remote(&'a Database),
}

/// Derive the complete canonical settings frontier at the specified main tips.
/// `entries` must be a complete, root-first main ancestry for those tips.
/// A signed settings write must consume exactly its main parents' frontier.
pub(crate) fn causal_settings_frontier(entries: &[Entry], tips: &[ID]) -> Result<Snapshot> {
    fn prune(candidates: Vec<ID>, nodes: &HashMap<ID, &Entry>) -> Result<Snapshot> {
        let candidates = Snapshot::from(candidates);
        if candidates.len() <= 1 {
            return Ok(candidates);
        }
        let mut dominated = HashSet::new();
        for tip in &candidates {
            let mut stack = vec![tip.clone()];
            let mut seen = HashSet::new();
            while let Some(id) = stack.pop() {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if id != *tip && candidates.tips().contains(&id) {
                    dominated.insert(id.clone());
                }
                if let Some(ancestor) = nodes.get(&id) {
                    stack.extend(ancestor.subtree_parents(SETTINGS)?);
                }
            }
        }
        Ok(Snapshot::from(
            candidates
                .iter()
                .filter(|id| !dominated.contains(*id))
                .cloned()
                .collect::<Vec<_>>(),
        ))
    }

    let mut frontiers: HashMap<ID, Snapshot> = HashMap::new();
    let mut settings_nodes: HashMap<ID, &Entry> = HashMap::new();
    for node in entries {
        let mut candidates = Vec::new();
        for parent in node.parents()? {
            let frontier =
                frontiers
                    .get(&parent)
                    .ok_or_else(|| AuthError::InvalidAuthConfiguration {
                        reason: "incomplete main ancestry for settings frontier".to_string(),
                    })?;
            candidates.extend(frontier.iter().cloned());
        }
        let expected = prune(candidates, &settings_nodes)?;
        let frontier = if node.in_subtree(SETTINGS) {
            if Snapshot::from(node.subtree_parents(SETTINGS)?) != expected {
                return Err(AuthError::InvalidAuthConfiguration {
                    reason: "settings parents differ from main-parent causal frontier".to_string(),
                }
                .into());
            }
            settings_nodes.insert(node.id(), node);
            Snapshot::from(vec![node.id()])
        } else {
            expected
        };
        frontiers.insert(node.id(), frontier);
    }
    let mut candidates = Vec::new();
    for tip in tips {
        let frontier = frontiers
            .get(tip)
            .ok_or_else(|| AuthError::InvalidAuthConfiguration {
                reason: "missing main tip for settings frontier".to_string(),
            })?;
        candidates.extend(frontier.iter().cloned());
    }
    prune(candidates, &settings_nodes)
}

impl DerivedFloors {
    pub(crate) fn for_entry(backend: std::sync::Arc<dyn BackendImpl>, entry: &Entry) -> Self {
        Self::new(backend, entry.root().unwrap_or_else(|| entry.id()))
    }

    fn new(backend: std::sync::Arc<dyn BackendImpl>, tree: ID) -> Self {
        Self {
            backend,
            tree,
            state: State::default(),
            removed_direct: HashMap::new(),
        }
    }

    /// Floor for a data write at the supplied **Verified** main frontier.
    /// The caller has read the direct declaration at that same frontier.
    pub(crate) async fn at_verified_frontier(
        db: &Database,
        frontier: &Snapshot,
        declaration: &DelegatedTreeRef,
    ) -> Result<Snapshot> {
        let state = if let Some(engine) = db.ops().local_engine() {
            let floors = Self::new(engine, db.root_id().clone());
            VerifiedHistory::Local(&floors)
                .state_at(frontier.tips())
                .await?
        } else {
            VerifiedHistory::Remote(db)
                .state_at(frontier.tips())
                .await?
        };
        let root = &declaration.tree.root;
        Ok(Snapshot::from(
            state
                .direct
                .get(root)
                .into_iter()
                .chain(state.nested.get(root))
                .flat_map(|tips| tips.iter())
                .chain(&declaration.tree.tips)
                .cloned()
                .collect::<Vec<_>>(),
        ))
    }

    fn request(&self, id: &ID) -> StoreStateRequest {
        StoreStateRequest {
            database: self.tree.clone(),
            store: "_auth_floors".to_string(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: ProjectionDescriptor {
                name: "auth-frontier".to_string(),
                version: 2,
            },
            source_key: Snapshot::from([id.clone()]).cache_key_bytes(),
        }
    }

    async fn cached(&self, id: &ID) -> Result<Option<State>> {
        for _ in 0..2 {
            let result: Result<Option<State>> = async {
                let Some(view) = self.backend.resolve_store_state(&self.request(id)).await? else {
                    return Ok(None);
                };
                let bytes = self
                    .backend
                    .store_state_record_get(&view, b"state")
                    .await?
                    .ok_or(BackendError::InvalidStoreStateView)?;
                Ok(Some(serde_json::from_slice(&bytes)?))
            }
            .await;
            match result {
                Err(error) if error.is_invalid_store_state_view() => continue,
                Err(error) if error.is_optional_store_state_unavailable() => return Ok(None),
                result => return result,
            }
        }
        Ok(None)
    }

    async fn publish(&self, id: &ID, state: &State) -> Result<()> {
        let token = match self
            .backend
            .begin_store_state_staging(self.request(id))
            .await
        {
            Ok(token) => token,
            Err(e) if e.is_optional_store_state_unavailable() => {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut records = RecordMutations::new();
        records.insert(b"state".to_vec(), Some(serde_json::to_vec(state)?));
        let result: Result<()> = async {
            self.backend
                .stage_store_state_records(&token, records)
                .await?;
            self.backend.publish_store_state(token.clone()).await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = self.backend.abort_store_state(token).await;
        }
        match result {
            Err(error) if error.is_optional_store_state_unavailable() => Ok(()),
            result => result,
        }
    }

    fn unsynced(&self, missing: Vec<ID>) -> Error {
        AuthError::DelegatedTreeUnsynced {
            tree_id: self.tree.clone(),
            missing,
        }
        .into()
    }

    async fn verified(&self, id: &ID) -> Result<()> {
        match self.backend.get_verification_status(id).await {
            Ok(VerificationStatus::Verified) => Ok(()),
            Ok(VerificationStatus::Unverified) => Err(self.unsynced(vec![id.clone()])),
            Err(e) if e.is_not_found() => Err(self.unsynced(vec![id.clone()])),
            Err(e) => Err(e),
            Ok(VerificationStatus::Failed) => Err(AuthError::InvalidDelegationTips {
                tree_id: self.tree.clone(),
                claimed_tips: vec![id.clone()],
            }
            .into()),
        }
    }

    /// Complete locally verified ancestry, not just tip membership. A bad
    /// entry is a verdict; an existing Unverified one is a retry dependency.
    pub(crate) async fn proof_on(
        backend: &dyn BackendImpl,
        root: &ID,
        tips: &Snapshot,
    ) -> Result<()> {
        if tips.is_empty() {
            return Err(AuthError::InvalidDelegationTips {
                tree_id: root.clone(),
                claimed_tips: vec![],
            }
            .into());
        }
        let entries = match backend.get_tree_from_tips(root, tips).await {
            Ok(entries) => entries,
            Err(e) if e.is_not_found() => {
                return Err(AuthError::DelegatedTreeUnsynced {
                    tree_id: root.clone(),
                    missing: e
                        .entry_id()
                        .cloned()
                        .map_or_else(|| tips.tips().to_vec(), |id| vec![id]),
                }
                .into());
            }
            Err(Error::Backend(e)) if matches!(*e, BackendError::EntryNotInTree { .. }) => {
                return Err(AuthError::InvalidDelegationTips {
                    tree_id: root.clone(),
                    claimed_tips: tips.tips().to_vec(),
                }
                .into());
            }
            Err(e) => return Err(e),
        };
        if !entries.iter().any(|entry| entry.id() == *root) {
            return Err(AuthError::InvalidDelegationTips {
                tree_id: root.clone(),
                claimed_tips: tips.tips().to_vec(),
            }
            .into());
        }
        let mut pending = Vec::new();
        for entry in &entries {
            // A backend must not turn a foreign ancestor into a complete
            // delegated proof, even if that ancestor is locally Verified.
            if !entry.in_tree(root) {
                return Err(AuthError::InvalidDelegationTips {
                    tree_id: root.clone(),
                    claimed_tips: tips.tips().to_vec(),
                }
                .into());
            }
            match backend.get_verification_status(&entry.id()).await? {
                VerificationStatus::Verified => {}
                VerificationStatus::Unverified => pending.push(entry.id()),
                VerificationStatus::Failed => {
                    return Err(AuthError::InvalidDelegationTips {
                        tree_id: root.clone(),
                        claimed_tips: tips.tips().to_vec(),
                    }
                    .into());
                }
            }
        }
        if !pending.is_empty() {
            return Err(AuthError::DelegatedTreeUnsynced {
                tree_id: root.clone(),
                missing: pending,
            }
            .into());
        }
        Ok(())
    }

    // A complete main ancestry scan supplies the expected causal settings
    // frontier. Check signed subtree edges before trusting the post-entry
    // settings state; then fold only the actual settings DAG, not main ancestry.
    async fn settings_at(&self, entry: &Entry) -> Result<AuthSettings> {
        let parents = entry.parents()?;
        let mut entries = if parents.is_empty() {
            vec![]
        } else {
            match self
                .backend
                .get_tree_from_tips(&self.tree, &Snapshot::from(parents))
                .await
            {
                Ok(entries) => entries,
                Err(e) if e.is_not_found() => {
                    return Err(self.unsynced(e.entry_id().cloned().into_iter().collect()));
                }
                Err(e) => return Err(e),
            }
        };
        entries.push(entry.clone());

        let frontier = causal_settings_frontier(&entries, &[entry.id()])?;
        let settings_nodes: HashMap<ID, &Entry> = entries
            .iter()
            .filter(|node| node.in_subtree(SETTINGS))
            .map(|node| (node.id(), node))
            .collect();

        let mut reachable = HashSet::new();
        let mut stack = frontier.into_tips();
        while let Some(id) = stack.pop() {
            if reachable.insert(id.clone()) {
                let node = settings_nodes
                    .get(&id)
                    .ok_or_else(|| self.unsynced(vec![id]))?;
                stack.extend(node.subtree_parents(SETTINGS)?);
            }
        }
        crate::database::fold_settings_entries(
            &entries
                .into_iter()
                .filter(|e| reachable.contains(&e.id()))
                .collect::<Vec<_>>(),
        )
    }

    fn claim(entry: &Entry) -> impl Iterator<Item = (ID, Snapshot)> + '_ {
        let steps = match &entry.auth().key {
            SigKey::Delegation { path, .. } => path.as_slice(),
            _ => &[],
        };
        steps
            .iter()
            .map(|step| (step.tree.clone(), Snapshot::from(&step.tips)))
    }

    fn active(settings: &AuthSettings) -> Result<HashMap<ID, DelegatedTreeRef>> {
        let mut active = HashMap::new();
        let Some(value) = settings.as_doc().get("delegations") else {
            return Ok(active);
        };
        let Value::Doc(delegations) = value else {
            return Err(AuthError::InvalidAuthConfiguration {
                reason: "delegations must be a document".to_string(),
            }
            .into());
        };
        for (key, value) in delegations.iter() {
            let root = ID::parse(key).map_err(|_| AuthError::InvalidAuthConfiguration {
                reason: "delegation key must be a tree root ID".to_string(),
            })?;
            let Value::Doc(doc) = value else {
                return Err(AuthError::InvalidAuthConfiguration {
                    reason: "delegation must be a document".to_string(),
                }
                .into());
            };
            let reference = DelegatedTreeRef::try_from(doc).map_err(|_| {
                AuthError::InvalidAuthConfiguration {
                    reason: "invalid delegation reference".to_string(),
                }
            })?;
            if root != reference.tree.root {
                return Err(AuthError::InvalidAuthConfiguration {
                    reason: "delegation key differs from its tree root".to_string(),
                }
                .into());
            }
            active.insert(root, reference);
        }
        Ok(active)
    }

    fn apply_claims(entry: &Entry, state: &mut State) {
        let mut direct: HashMap<ID, Vec<ID>> = HashMap::new();
        let mut nested: HashMap<ID, Vec<ID>> = HashMap::new();
        for (index, (root, tips)) in Self::claim(entry).enumerate() {
            let claims = if index == 0 { &mut direct } else { &mut nested };
            claims.entry(root).or_default().extend(tips.into_tips());
        }
        for (root, tips) in direct {
            if state.direct.contains_key(&root) {
                state.direct.insert(root, Snapshot::from(tips));
            }
        }
        for (root, tips) in nested {
            state.nested.insert(root, Snapshot::from(tips));
        }
    }

    async fn transition(
        &self,
        entry: &Entry,
        mut state: State,
        check_pointer: bool,
    ) -> Result<State> {
        let settings = self.settings_at(entry).await?;
        let active = Self::active(&settings)?;
        // Only the direct component is reset by effective absence. A nested
        // observation of that same root is independent of the direct grant.
        let records_pointers = entry.in_subtree(SETTINGS) || entry.parents()?.is_empty();
        if records_pointers && check_pointer {
            for (root, reference) in &active {
                let pointer = Snapshot::from(reference.tree.tips.clone());
                if pointer.len() > super::delegation::MAX_DELEGATION_TIPS {
                    return Err(AuthError::DelegationTipsTooMany {
                        tree_id: root.clone(),
                        len: pointer.len(),
                        max: super::delegation::MAX_DELEGATION_TIPS,
                    }
                    .into());
                }
                Self::proof_on(self.backend.as_ref(), root, &pointer).await?;
            }
        }
        state.apply_settings(active, records_pointers);
        // Cold Verified parents already passed their signature gate. For the
        // entry being validated, keep inherited tips until its claim is checked.
        if !check_pointer {
            Self::apply_claims(entry, &mut state);
        }
        Ok(state)
    }

    /// Load all immediate-parent states, then account for this entry's
    /// resulting settings before checking its signed claims.
    pub(crate) async fn prepare(&mut self, entry: &Entry) -> Result<()> {
        let inherited = VerifiedHistory::Local(self)
            .state_at(&entry.parents()?)
            .await?;
        let inherited_direct = inherited.direct.clone();
        self.state = self.transition(entry, inherited, true).await?;
        self.removed_direct = inherited_direct
            .into_iter()
            .filter(|(root, _)| !self.state.direct.contains_key(root))
            .collect();
        Ok(())
    }

    pub(crate) fn floor_for(&self, root: &ID) -> Snapshot {
        Snapshot::from(
            self.state
                .direct
                .get(root)
                .into_iter()
                .chain(self.removed_direct.get(root))
                .chain(self.state.nested.get(root))
                .flat_map(|snapshot| snapshot.iter())
                .cloned()
                .collect::<Vec<_>>(),
        )
    }

    /// Called only after the entry passed signature and permission checks.
    pub(crate) async fn finish(&mut self, entry: &Entry) -> Result<()> {
        Self::apply_claims(entry, &mut self.state);
        self.publish(&entry.id(), &self.state).await
    }
}

impl State {
    fn apply_settings(&mut self, active: HashMap<ID, DelegatedTreeRef>, records_pointers: bool) {
        self.direct.retain(|root, _| active.contains_key(root));
        if records_pointers {
            for (root, reference) in active {
                merge_tips(&mut self.direct, &root, &reference.tree.tips);
            }
        }
    }

    fn join<'a>(parents: impl Iterator<Item = &'a State>) -> Self {
        let mut state = Self::default();
        for parent in parents {
            for (ours, theirs) in [
                (&mut state.direct, &parent.direct),
                (&mut state.nested, &parent.nested),
            ] {
                for (root, tips) in theirs {
                    merge_tips(ours, root, tips.tips());
                }
            }
        }
        state
    }
}

fn merge_tips(states: &mut HashMap<ID, Snapshot>, root: &ID, tips: &[ID]) {
    let old = states.remove(root).unwrap_or_default();
    let mut joined = old.into_tips();
    joined.extend_from_slice(tips);
    states.insert(root.clone(), Snapshot::from(joined));
}

impl VerifiedHistory<'_> {
    /// Rebuild cold states in parent order before joining the requested tips.
    /// The remote path starts only from server-selected Verified tips; their
    /// immutable parents are fetched through the ordinary Read-gated API.
    async fn state_at(&self, tips: &[ID]) -> Result<State> {
        let tree = match self {
            Self::Local(floors) => &floors.tree,
            Self::Remote(db) => db.root_id(),
        };
        let mut built = HashMap::new();
        let mut stack: Vec<_> = tips.iter().rev().map(|id| (id.clone(), false)).collect();
        let mut seen = HashSet::new();
        while let Some((id, ready)) = stack.pop() {
            if built.contains_key(&id) {
                continue;
            }
            if !ready && !seen.insert(id.clone()) {
                continue;
            }
            let entry = match self {
                Self::Local(floors) => {
                    floors.verified(&id).await?;
                    if let Some(state) = floors.cached(&id).await? {
                        built.insert(id, state);
                        continue;
                    }
                    match floors.backend.get(&id).await {
                        Err(e) if e.is_not_found() => return Err(floors.unsynced(vec![id])),
                        result => result?,
                    }
                }
                Self::Remote(db) => db.get_entry(&id).await?,
            };
            if !entry.in_tree(tree) {
                return Err(BackendError::EntryNotInTree {
                    entry_id: id,
                    tree_id: tree.clone(),
                }
                .into());
            }
            let parents = entry.parents()?;
            if !ready {
                // Keep payloads off the walk stack. The immutable entry can be
                // fetched again once its parents' states are ready.
                stack.push((id, true));
                stack.extend(parents.into_iter().map(|parent| (parent, false)));
                continue;
            }
            let mut state = join_built(tree, &built, &parents)?;
            match self {
                Self::Local(floors) => {
                    state = floors.transition(&entry, state, false).await?;
                    floors.publish(&id, &state).await?;
                }
                Self::Remote(db) => {
                    let records_pointers = entry.in_subtree(SETTINGS) || parents.is_empty();
                    // A single parent's effective settings survive an ordinary data write.
                    if records_pointers || parents.len() != 1 {
                        // Keep remote transaction reconstruction out of the
                        // local validator's async frame.
                        let cut = Snapshot::from([id.clone()]);
                        let settings = Box::pin(db.auth_settings_at(&cut)).await?;
                        state.apply_settings(DerivedFloors::active(&settings)?, records_pointers);
                    }
                    DerivedFloors::apply_claims(&entry, &mut state);
                }
            }
            built.insert(id, state);
        }
        join_built(tree, &built, tips)
    }
}

fn join_built(tree: &ID, built: &HashMap<ID, State>, tips: &[ID]) -> Result<State> {
    let states = tips
        .iter()
        .map(|tip| built.get(tip))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| AuthError::DelegatedTreeUnsynced {
            tree_id: tree.clone(),
            missing: tips
                .iter()
                .filter(|tip| !built.contains_key(*tip))
                .cloned()
                .collect(),
        })?;
    Ok(State::join(states.into_iter()))
}

#[cfg(test)]
mod cache_bypass_tests {
    use super::*;
    use crate::backend::{DerivedCachePolicy, database::InMemory};

    #[tokio::test]
    async fn disabled_floor_cache_preserves_proof_requirements() {
        let backend = std::sync::Arc::new(InMemory::new_with_cache_policy(DerivedCachePolicy {
            max_namespaces: 0,
            max_bytes: 0,
        }));
        let root = Entry::root_builder().build().unwrap();
        backend.put(root.clone()).await.unwrap();
        let floors = DerivedFloors::for_entry(backend.clone(), &root);
        floors.publish(&root.id(), &State::default()).await.unwrap();
        assert!(floors.cached(&root.id()).await.unwrap().is_none());
        let boundary = Snapshot::from([root.id()]);
        let error = DerivedFloors::proof_on(backend.as_ref(), &root.id(), &boundary)
            .await
            .unwrap_err();
        assert!(!error.is_optional_store_state_unavailable());
        assert!(
            matches!(error, Error::Auth(ref e) if matches!(**e, AuthError::DelegatedTreeUnsynced { .. }))
        );
        backend
            .update_verification_status(&root.id(), VerificationStatus::Verified)
            .await
            .unwrap();
        DerivedFloors::proof_on(backend.as_ref(), &root.id(), &boundary)
            .await
            .unwrap();
        let foreign = Entry::root_builder()
            .set_subtree_data("foreign", b"other")
            .build()
            .unwrap();
        backend.put(foreign.clone()).await.unwrap();
        backend
            .update_verification_status(&foreign.id(), VerificationStatus::Verified)
            .await
            .unwrap();
        assert!(
            DerivedFloors::proof_on(
                backend.as_ref(),
                &root.id(),
                &Snapshot::from([foreign.id()])
            )
            .await
            .is_err()
        );
    }
}
