//! Disposable, per-entry authorization frontiers. Only locally Verified parents
//! can contribute to a child's floor; the cache is never an authority.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    Entry, Error, Result, Snapshot,
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
}

impl DerivedFloors {
    pub(crate) fn for_entry(backend: std::sync::Arc<dyn BackendImpl>, entry: &Entry) -> Self {
        Self {
            backend,
            tree: entry.root().unwrap_or_else(|| entry.id()),
            state: State::default(),
        }
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
            source_key: id.to_string().into_bytes(),
        }
    }

    async fn cached(&self, id: &ID) -> Result<Option<State>> {
        match self.backend.resolve_store_state(&self.request(id)).await {
            Ok(Some(view)) => {
                let bytes = self
                    .backend
                    .store_state_record_get(&view, b"state")
                    .await?
                    .ok_or(BackendError::InvalidStoreStateView)?;
                Ok(Some(serde_json::from_slice(&bytes)?))
            }
            Ok(None) => Ok(None),
            Err(Error::Backend(e)) if matches!(*e, BackendError::StoreStateStorageUnsupported) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn publish(&self, id: &ID, state: &State) -> Result<()> {
        let token = match self
            .backend
            .begin_store_state_staging(self.request(id))
            .await
        {
            Ok(token) => token,
            Err(Error::Backend(e)) if matches!(*e, BackendError::StoreStateStorageUnsupported) => {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut records = RecordMutations::new();
        records.insert(b"state".to_vec(), Some(serde_json::to_vec(state)?));
        let result = async {
            self.backend
                .stage_store_state_records(&token, records)
                .await?;
            self.backend.publish_store_state(token.clone()).await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            self.backend.abort_store_state(token).await?;
        }
        result
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

        // One pass over the complete main DAG. Each node carries the nearest
        // settings frontier of its parents; a merge prunes settings ancestors
        // already superseded by another candidate (without backend rescans).
        let mut frontiers: HashMap<ID, Snapshot> = HashMap::new();
        let mut settings_nodes: HashMap<ID, &Entry> = HashMap::new();
        for node in &entries {
            let mut candidates = Vec::new();
            for parent in node.parents()? {
                let frontier = frontiers
                    .get(&parent)
                    .ok_or_else(|| self.unsynced(vec![parent]))?;
                candidates.extend(frontier.iter().cloned());
            }
            let candidates = Snapshot::from(candidates);
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
                    if let Some(ancestor) = settings_nodes.get(&id) {
                        stack.extend(ancestor.subtree_parents(SETTINGS)?);
                    }
                }
            }
            let expected = Snapshot::from(
                candidates
                    .iter()
                    .filter(|id| !dominated.contains(*id))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            let frontier = if node.in_subtree(SETTINGS) {
                if Snapshot::from(node.subtree_parents(SETTINGS)?) != expected {
                    return Err(AuthError::InvalidAuthConfiguration {
                        reason: "settings parents differ from main-parent causal frontier"
                            .to_string(),
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

        let mut reachable = HashSet::new();
        let mut stack = frontiers
            .get(&entry.id())
            .into_iter()
            .flat_map(|snapshot| snapshot.iter())
            .cloned()
            .collect::<Vec<_>>();
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
        state.direct.retain(|root, _| active.contains_key(root));
        if entry.in_subtree(SETTINGS) || entry.parents()?.is_empty() {
            for (root, reference) in &active {
                let pointer = Snapshot::from(reference.tree.tips.clone());
                if check_pointer {
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
                let old = state.direct.remove(root).unwrap_or_default();
                state.direct.insert(
                    root.clone(),
                    Snapshot::from(
                        old.iter()
                            .chain(pointer.iter())
                            .cloned()
                            .collect::<Vec<_>>(),
                    ),
                );
            }
        }
        // Cold Verified parents already passed their signature gate. For the
        // entry being validated, keep inherited tips until its claim is checked.
        if !check_pointer {
            Self::apply_claims(entry, &mut state);
        }
        Ok(state)
    }

    /// Rebuild a cold Verified parent's state from its own Verified parents,
    /// never from an empty default at a non-root. No partial state is published.
    async fn parent_state(&self, id: &ID) -> Result<State> {
        let mut built: HashMap<ID, State> = HashMap::new();
        let mut stack = vec![(id.clone(), false)];
        let mut seen = HashSet::new();
        while let Some((current, ready)) = stack.pop() {
            if built.contains_key(&current) {
                continue;
            }
            self.verified(&current).await?;
            if let Some(state) = self.cached(&current).await? {
                built.insert(current, state);
                continue;
            }
            let entry = match self.backend.get(&current).await {
                Ok(entry) => entry,
                Err(e) if e.is_not_found() => return Err(self.unsynced(vec![current])),
                Err(e) => return Err(e),
            };
            if !entry.in_tree(&self.tree) {
                return Err(BackendError::EntryNotInTree {
                    entry_id: current,
                    tree_id: self.tree.clone(),
                }
                .into());
            }
            let parents = entry.parents()?;
            if !ready {
                if !seen.insert(current.clone()) {
                    continue;
                }
                stack.push((current, true));
                for parent in parents {
                    stack.push((parent, false));
                }
                continue;
            }
            let state = self.join(
                parents
                    .iter()
                    .map(|p| built.get(p).expect("verified parent was built")),
            );
            let state = self.transition(&entry, state, false).await?;
            self.publish(&current, &state).await?;
            built.insert(current, state);
        }
        built
            .remove(id)
            .ok_or_else(|| self.unsynced(vec![id.clone()]))
    }

    fn join<'b>(&self, parents: impl Iterator<Item = &'b State>) -> State {
        let mut state = State::default();
        for parent in parents {
            for (ours, theirs) in [
                (&mut state.direct, &parent.direct),
                (&mut state.nested, &parent.nested),
            ] {
                for (root, tips) in theirs {
                    let old = ours.remove(root).unwrap_or_default();
                    ours.insert(
                        root.clone(),
                        Snapshot::from(old.iter().chain(tips.iter()).cloned().collect::<Vec<_>>()),
                    );
                }
            }
        }
        state
    }

    /// Load all immediate-parent states, then account for this entry's
    /// resulting settings before checking its signed claims.
    pub(crate) async fn prepare(&mut self, entry: &Entry) -> Result<()> {
        let mut parents = Vec::new();
        for parent in entry.parents()? {
            parents.push(self.parent_state(&parent).await?);
        }
        self.state = self
            .transition(entry, self.join(parents.iter()), true)
            .await?;
        Ok(())
    }

    pub(crate) fn floor_for(&self, root: &ID) -> Snapshot {
        Snapshot::from(
            self.state
                .direct
                .get(root)
                .into_iter()
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
