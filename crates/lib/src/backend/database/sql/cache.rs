//! Height-based sorting for SQL backends.
//!
//! Heights are stored directly in entries, so sorting is trivial.
//! This module provides convenience functions for sorting entries by height.

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
