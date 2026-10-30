//! Shared local/received Entry parent validation. Raw storage is not acceptance.
use std::collections::{HashMap, HashSet};

use super::Database;
use crate::{
    Entry, Error, Result, Snapshot,
    auth::{errors::AuthError, validation::floors::causal_store_frontiers},
    backend::VerificationStatus,
    constants::SETTINGS,
    entry::ID,
};

pub(super) enum CausalFrontiers {
    Complete(HashMap<String, Snapshot>),
    Incomplete(Vec<ID>),
    Invalid,
}

impl Database {
    pub(crate) async fn validate_entry_parents(&self, entry: &Entry) -> Result<bool> {
        // Remote transactions submit Unverified bytes; the server runs this
        // validation before promotion, using its own local status evidence.
        if self.ops().local_engine().is_none() {
            return Ok(true);
        }
        match self.causal_frontiers(entry).await? {
            CausalFrontiers::Complete(_) => Ok(true),
            CausalFrontiers::Invalid => Ok(false),
            CausalFrontiers::Incomplete(missing) => Err(AuthError::DelegatedTreeUnsynced {
                tree_id: self.root_id().clone(),
                missing,
            }
            .into()),
        }
    }

    pub(super) async fn causal_frontiers(&self, entry: &Entry) -> Result<CausalFrontiers> {
        let instance = self.instance()?;
        let backend = instance.require_local_engine()?;
        let parents = entry.parents()?;

        // Check conclusive Store-edge evidence even when some main ancestry is
        // absent. Never fetch a Store edge as part of its own reachability proof.
        for store in entry.subtrees() {
            for id in entry.subtree_parents(&store)? {
                let parent = match backend.get(&id).await {
                    Ok(parent) => parent,
                    Err(e) if e.is_not_found() => continue,
                    Err(e) => return Err(e),
                };
                if !parent.in_tree(self.root_id()) || !parent.in_subtree(&store) {
                    return Ok(CausalFrontiers::Invalid);
                }
                match backend.get_verification_status(&id).await? {
                    VerificationStatus::Failed => return Ok(CausalFrontiers::Invalid),
                    VerificationStatus::Verified
                        if entry.subtree_height(&store)? <= parent.subtree_height(&store)? =>
                    {
                        return Ok(CausalFrontiers::Invalid);
                    }
                    _ => {}
                }
            }
        }

        // One main-only walk supplies every touched Store and the auth pin.
        // Postorder, not writer-supplied height labels, establishes topology.
        let mut stack: Vec<_> = parents.iter().cloned().map(|id| (id, false)).collect();
        let mut seen = HashSet::new();
        let mut nodes: HashMap<ID, Entry> = HashMap::new();
        let mut ancestry = Vec::new();
        let mut pending = Vec::new();
        while let Some((id, ready)) = stack.pop() {
            if ready {
                ancestry.push(nodes.remove(&id).expect("loaded main ancestor"));
                continue;
            }
            if !seen.insert(id.clone()) {
                continue;
            }
            let ancestor = match backend.get(&id).await {
                Ok(ancestor) => ancestor,
                Err(e) if e.is_not_found() => {
                    pending.push(id);
                    continue;
                }
                Err(e) => return Err(e),
            };
            if !ancestor.in_tree(self.root_id()) {
                return Ok(CausalFrontiers::Invalid);
            }
            match backend.get_verification_status(&id).await? {
                VerificationStatus::Failed => return Ok(CausalFrontiers::Invalid),
                VerificationStatus::Unverified => pending.push(id.clone()),
                VerificationStatus::Verified
                    if parents.contains(&id) && entry.height() <= ancestor.height() =>
                {
                    return Ok(CausalFrontiers::Invalid);
                }
                _ => {}
            }
            stack.push((id.clone(), true));
            stack.extend(ancestor.parents()?.into_iter().map(|id| (id, false)));
            nodes.insert(id, ancestor);
        }
        if !pending.is_empty() {
            return Ok(CausalFrontiers::Incomplete(pending));
        }

        let mut stores = entry.subtrees();
        if !stores.iter().any(|s| s == SETTINGS) {
            stores.push(SETTINGS.to_string());
        }
        let frontiers = match causal_store_frontiers(&ancestry, &parents, &stores) {
            Ok(frontiers) => frontiers,
            Err(Error::Auth(e)) if matches!(*e, AuthError::InvalidAuthConfiguration { .. }) => {
                return Ok(CausalFrontiers::Invalid);
            }
            Err(e) => return Err(e),
        };
        for store in entry.subtrees() {
            if Snapshot::from(entry.subtree_parents(&store)?) != frontiers[&store] {
                // A complete cut also conclusively rejects references whose
                // bytes are absent: they cannot belong to its frontier.
                return Ok(CausalFrontiers::Invalid);
            }
        }
        Ok(CausalFrontiers::Complete(frontiers))
    }
}
