//! Height-based sorting for SQL backends.
//!
//! Heights are stored directly in entries, so sorting is trivial.
//! This module provides convenience functions for sorting entries by height.

use crate::Result;
use crate::entry::Entry;

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

/// Sort entries by subtree height, with ID as tiebreaker.
///
/// Heights are stored in each entry's subtree data, so this just reads the
/// embedded heights and sorts accordingly.
pub fn sort_entries_by_subtree_height(entries: &mut [Entry], subtree: &str) -> Result<()> {
    entries.sort_by(|a, b| {
        let a_height = a.subtree_height(subtree).unwrap_or(0);
        let b_height = b.subtree_height(subtree).unwrap_or(0);
        a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
    });
    Ok(())
}
