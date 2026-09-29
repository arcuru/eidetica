//! Ancestor-completeness checks shared by the database backends.
//!
//! A CRDT state is only correct if it is folded over the *complete* ancestor
//! closure of the tips it is computed from. Under partial sync a node can hold
//! a child whose parents have not arrived yet, so a traversal that follows
//! parent pointers can run off the end of what is stored. Folding the
//! truncated set produces a state that is wrong and — because materialized
//! states are cached per entry — stays wrong after the gap closes.
//!
//! Traversals therefore report the gap instead of returning a short answer.
//! Both backends route their check through this module so the two agree on
//! what "complete" means, entry-for-entry.

use std::collections::BTreeSet;

use crate::{
    Error,
    backend::errors::BackendError,
    entry::{Entry, ID},
};

/// Parent pointers in `store`'s DAG that `entries` references but does not contain.
///
/// `entries` is the full result of a traversal, so a parent missing from it is
/// a parent the traversal could not visit — whether it is absent locally, held
/// under a different tree, or not a member of the store.
pub(crate) fn missing_store_ancestors(store: &str, entries: &[Entry]) -> Vec<ID> {
    missing_ancestors(entries, |entry| {
        entry.subtree_parents(store).unwrap_or_default()
    })
}

/// Parent pointers in the main tree DAG that `entries` references but does not contain.
pub(crate) fn missing_tree_ancestors(entries: &[Entry]) -> Vec<ID> {
    missing_ancestors(entries, |entry| entry.parents().unwrap_or_default())
}

fn missing_ancestors(entries: &[Entry], parents_of: impl Fn(&Entry) -> Vec<ID>) -> Vec<ID> {
    let present: BTreeSet<ID> = entries.iter().map(|entry| entry.id()).collect();
    let mut missing: BTreeSet<ID> = BTreeSet::new();
    for entry in entries {
        for parent in parents_of(entry) {
            if !present.contains(&parent) {
                missing.insert(parent);
            }
        }
    }
    missing.into_iter().collect()
}

/// The error a traversal returns when it could not reach the full ancestry of
/// a store's tips.
pub(crate) fn incomplete_store_history(tree: &ID, store: &str, missing: Vec<ID>) -> Error {
    BackendError::IncompleteHistory {
        context: format!("store '{store}' of tree {tree}"),
        missing,
    }
    .into()
}

/// The error a traversal returns when it could not reach the full ancestry of
/// a tree's tips.
pub(crate) fn incomplete_tree_history(tree: &ID, missing: Vec<ID>) -> Error {
    BackendError::IncompleteHistory {
        context: format!("tree {tree}"),
        missing,
    }
    .into()
}
