//! Height-based sorting shared by every database backend.
//!
//! Traversal order feeds CRDT merge, so all backends must order entries
//! identically: by height ascending, tied on [`ID`]'s `Ord` — the CID tuple.
//! Keeping the comparators in one place is what enforces that; a per-backend
//! copy that drifts silently diverges materialized state.
//!
//! Ordering is applied in-process rather than in SQL. The ID tiebreak must
//! follow `ID`'s `Ord`, and SQL `id` columns hold the base32lower string form,
//! whose ASCII order differs from it: base32lower encodes the values 26-31 as
//! the digits `2`-`7`, which sort before letters in ASCII while standing for
//! larger values.
//!
//! Entry sorts compare through [`Entry::id_ref`], which borrows the entry's
//! memoized ID: the tiebreak costs neither a rehash nor a copy of the digest.

use std::collections::HashSet;

use crate::{
    Result,
    entry::{Entry, ID},
};

/// Sort entries by tree height, with ID as tiebreaker.
pub(crate) fn sort_entries_by_height(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        a.height()
            .cmp(&b.height())
            .then_with(|| a.id_ref().cmp(b.id_ref()))
    });
}

/// Sort entries by store height, with ID as tiebreaker.
///
/// Entries missing a height for `store` sort as height 0.
pub(crate) fn sort_entries_by_store_height(store: &str, entries: &mut [Entry]) {
    let height = |e: &Entry| e.subtree_height(store).unwrap_or(0);
    entries.sort_by(|a, b| {
        height(a)
            .cmp(&height(b))
            .then_with(|| a.id_ref().cmp(b.id_ref()))
    });
}

/// Sort `(id, height)` rows by height, with ID as tiebreaker.
///
/// For traversals that carry the height alongside the ID rather than a full
/// entry.
pub(crate) fn sort_ids_by_height<H: Ord>(rows: &mut [(ID, H)]) {
    rows.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
}

/// Return maximal main-tree entries from a validated ancestry closure.
pub(crate) fn tree_tips_from_entries(entries: &[Entry]) -> Result<Vec<ID>> {
    let mut parents = HashSet::new();
    for entry in entries {
        parents.extend(entry.parents()?);
    }
    Ok(entries
        .iter()
        .filter(|entry| !parents.contains(entry.id_ref()))
        .map(Entry::id)
        .collect())
}
