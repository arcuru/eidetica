//! Shared local/received Entry parent validation. Raw storage is not acceptance.
use std::collections::{HashMap, HashSet};

use super::Database;
use crate::{
    Entry, Error, Result, Snapshot,
    auth::{errors::AuthError, validation::floors::causal_settings_frontier},
    backend::VerificationStatus,
    constants::SETTINGS,
    entry::ID,
};

pub(super) enum ParentSettings {
    Complete(Snapshot),
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
        match self.validate_parent_settings(entry).await? {
            ParentSettings::Complete(_) => Ok(true),
            ParentSettings::Invalid => Ok(false),
            ParentSettings::Incomplete(missing) => Err(AuthError::DelegatedTreeUnsynced {
                tree_id: self.root_id().clone(),
                missing,
            }
            .into()),
        }
    }

    pub(super) async fn validate_parent_settings(&self, entry: &Entry) -> Result<ParentSettings> {
        let instance = self.instance()?;
        let backend = instance.require_local_engine()?;
        let parents = entry.parents()?;

        // Check conclusive Store-edge evidence even when some main ancestry is
        // absent. Never fetch a Store edge as part of its own reachability proof.
        for store in entry.subtrees() {
            let store_parents = entry.subtree_parents(&store)?;
            for id in store_parents {
                let parent = match backend.get(&id).await {
                    Ok(parent) => parent,
                    Err(e) if e.is_not_found() => continue,
                    Err(e) => return Err(e),
                };
                if !parent.in_tree(self.root_id()) || !parent.in_subtree(&store) {
                    return Ok(ParentSettings::Invalid);
                }
                match backend.get_verification_status(&id).await? {
                    VerificationStatus::Failed => return Ok(ParentSettings::Invalid),
                    VerificationStatus::Verified
                        if entry.subtree_height(&store)? <= parent.subtree_height(&store)? =>
                    {
                        return Ok(ParentSettings::Invalid);
                    }
                    _ => {}
                }
            }
        }

        // Main links alone prove membership for named Store parents and
        // supply the causal settings pin. Ordinary Store links need not consume
        // every contribution observed through this ancestry.
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
                return Ok(ParentSettings::Invalid);
            }
            match backend.get_verification_status(&id).await? {
                VerificationStatus::Failed => return Ok(ParentSettings::Invalid),
                VerificationStatus::Unverified => pending.push(id.clone()),
                // Only _settings must consume the entire causal frontier.
                // A known settings contribution conclusively rules out an
                // empty settings write, even if another main branch is missing.
                VerificationStatus::Verified
                    if (parents.contains(&id) && entry.height() <= ancestor.height())
                        || (entry.in_subtree(SETTINGS)
                            && entry.subtree_parents(SETTINGS)?.is_empty()
                            && ancestor.in_subtree(SETTINGS)) =>
                {
                    return Ok(ParentSettings::Invalid);
                }
                _ => {}
            }
            stack.push((id.clone(), true));
            stack.extend(ancestor.parents()?.into_iter().map(|id| (id, false)));
            nodes.insert(id, ancestor);
        }
        if !pending.is_empty() {
            return Ok(ParentSettings::Incomplete(pending));
        }

        for store in entry.subtrees() {
            if entry
                .subtree_parents(&store)?
                .iter()
                .any(|id| !seen.contains(id))
            {
                // A complete main cut conclusively excludes this reference,
                // whether or not its bytes have arrived over another path.
                return Ok(ParentSettings::Invalid);
            }
        }
        let settings = match causal_settings_frontier(&ancestry, &parents) {
            Ok(settings) => settings,
            Err(Error::Auth(e)) if matches!(*e, AuthError::InvalidAuthConfiguration { .. }) => {
                return Ok(ParentSettings::Invalid);
            }
            Err(e) => return Err(e),
        };
        if entry.in_subtree(SETTINGS)
            && Snapshot::from(entry.subtree_parents(SETTINGS)?) != settings
        {
            return Ok(ParentSettings::Invalid);
        }
        Ok(ParentSettings::Complete(settings))
    }
}
