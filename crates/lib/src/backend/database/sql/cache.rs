//! Height-based sorting for SQL backends.
//!
//! Heights are stored directly in entries, so sorting is trivial.
//! This module provides convenience functions for sorting entries by height.
//!
//! Ordering is applied in-process rather than in SQL. The ID tiebreak must
//! follow [`ID`]'s `Ord` — the CID tuple — and the `id` columns hold the
//! base32lower string form, whose ASCII order differs from it: base32lower
//! encodes the values 26-31 as the digits `2`-`7`, which sort before letters
//! in ASCII while standing for larger values. Ordering by those columns would
//! disagree with the in-memory backend, and traversal order feeds CRDT merge.

use crate::entry::{Entry, ID};

/// Sort entries by tree height, with ID as tiebreaker.
///
/// Heights are stored in each entry, so this just reads the embedded heights
/// and sorts accordingly.
pub fn sort_entries_by_height(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        a.height()
            .cmp(&b.height())
            .then_with(|| a.id().cmp(&b.id()))
    });
}

/// Sort entries by store height, with ID as tiebreaker.
///
/// Entries missing a height for `store` sort as height 0, matching the
/// in-memory backend.
pub fn sort_entries_by_store_height(store: &str, entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let a_height = a.subtree_height(store).unwrap_or(0);
        let b_height = b.subtree_height(store).unwrap_or(0);
        a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
    });
}

/// Sort `(id, height)` rows by height, with ID as tiebreaker.
///
/// For queries that carry the height alongside the ID rather than a full entry.
pub fn sort_ids_by_height(rows: &mut [(ID, i64)]) {
    rows.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
}
