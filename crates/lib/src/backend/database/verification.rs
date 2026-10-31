//! Cold, conservative verification repair shared by durable and in-memory backends.
//! The plan is computed before publishing any mutation; callers supply one serialized
//! tree view and apply the plan under the same transaction/write lock.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::{
    Result,
    backend::{VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
    snapshot::Snapshot,
};

pub(crate) struct RepairPlan {
    pub(crate) demoted: Vec<ID>,
    pub(crate) snapshot: Snapshot,
}

/// Reconstruct from immutable main ancestry, never heights or retained tips.
/// Missing/foreign parents exclude their dependent region. Malformed entries and
/// cycles are storage errors: callers must preserve all data and roll back.
pub(crate) fn repair_plan(
    tree: &ID,
    entries: &[(Entry, VerificationStatus)],
) -> Result<RepairPlan> {
    let held: HashSet<ID> = entries.iter().map(|(entry, _)| entry.id()).collect();
    let mut parents = HashMap::new();
    let mut children: HashMap<ID, Vec<ID>> = HashMap::new();
    let mut degrees = HashMap::new();
    let mut statuses = HashMap::new();
    for (entry, status) in entries {
        entry.validate()?;
        let id = entry.id();
        if !entry.in_tree(tree) {
            return Err(BackendError::EntryNotInTree {
                entry_id: id,
                tree_id: tree.clone(),
            }
            .into());
        }
        let entry_parents = entry.parents()?;
        let mut degree = 0;
        for parent in &entry_parents {
            children.entry(parent.clone()).or_default().push(id.clone());
            if held.contains(parent) {
                degree += 1;
            }
        }
        degrees.insert(id.clone(), degree);
        parents.insert(id.clone(), entry_parents);
        statuses.insert(id, *status);
    }
    let mut ready: BTreeSet<ID> = degrees
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut accepted = HashSet::new();
    let mut frontier = HashSet::new();
    let mut visited = 0;
    while let Some(id) = ready.pop_first() {
        visited += 1;
        if statuses[&id] == VerificationStatus::Verified
            && parents[&id].iter().all(|parent| accepted.contains(parent))
        {
            accepted.insert(id.clone());
            for parent in &parents[&id] {
                frontier.remove(parent);
            }
            frontier.insert(id.clone());
        }
        if let Some(kids) = children.get(&id) {
            for kid in kids {
                let degree = degrees.get_mut(kid).expect("held child");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(kid.clone());
                }
            }
        }
    }
    if visited != entries.len() {
        let entry_id = degrees
            .into_iter()
            .find(|(_, degree)| *degree != 0)
            .expect("unvisited cycle")
            .0;
        return Err(BackendError::CycleDetected { entry_id }.into());
    }
    let mut demoted: Vec<ID> = statuses
        .into_iter()
        .filter(|(id, status)| *status == VerificationStatus::Verified && !accepted.contains(id))
        .map(|(id, _)| id)
        .collect();
    demoted.sort();
    Ok(RepairPlan {
        demoted,
        snapshot: Snapshot::new(frontier.into_iter().collect()),
    })
}
