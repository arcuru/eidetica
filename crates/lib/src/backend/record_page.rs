//! Shared exact compact-JSON page budgeting.

use super::BackendError;
use crate::{Result, encoding::json_encoded_len};

pub(crate) fn too_large(limit: usize) -> BackendError {
    BackendError::RecordTooLarge {
        encoded_bytes: limit + 1,
    }
}

/// Exact compact-JSON budget shared by native record and history pages.
///
/// `overhead` is the empty page with a `null` continuation; each admitted item
/// adds its own encoding plus a separating comma, and the continuation replaces
/// `null` with the exact cursor encoding. A first item may exceed the preferred
/// budget only as an explicit singleton within `hard_cap`.
pub(crate) struct PageBudget {
    preferred: usize,
    hard_cap: usize,
    allow_large_singleton: bool,
    max_items: usize,
    overhead: usize,
    items_size: usize,
    items: usize,
    singleton: bool,
}

impl PageBudget {
    /// Refuse metadata that alone cannot fit; no empty non-progress page.
    pub(crate) fn new<P: serde::Serialize>(
        empty_page: &P,
        max_items: usize,
        preferred: usize,
        hard_cap: usize,
        allow_large_singleton: bool,
    ) -> Result<Self> {
        let preferred = preferred.min(hard_cap);
        let overhead =
            json_encoded_len(empty_page, preferred)?.ok_or_else(|| too_large(preferred))?;
        debug_assert!(overhead >= 4, "empty page must encode a null continuation");
        Ok(Self {
            preferred,
            hard_cap,
            allow_large_singleton,
            max_items,
            overhead,
            items_size: 0,
            items: 0,
            singleton: false,
        })
    }

    pub(crate) fn is_full(&self) -> bool {
        self.singleton || self.items >= self.max_items
    }

    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub(crate) fn is_empty(&self) -> bool {
        self.items == 0
    }

    /// Largest encoding the next item could use, for capped size probes.
    pub(crate) fn ceiling(&self) -> usize {
        if self.items == 0 && self.allow_large_singleton {
            self.hard_cap
        } else {
            self.preferred
        }
    }

    /// Admit one item whose capped size probes were taken against [`Self::ceiling`].
    /// `None` sizes mean the probe exceeded it. `cursor` is `None` with
    /// `more == false` for the terminal item (encoded as `null`).
    /// A first item that cannot fit is an error; later ones end the page.
    pub(crate) fn admit(
        &mut self,
        item: Option<usize>,
        cursor: Option<usize>,
        more: bool,
    ) -> Result<bool> {
        debug_assert!(!self.is_full());
        let ceiling = self.ceiling();
        let cursor = if more { cursor } else { Some(4) };
        // Each term is capped; checked arithmetic also protects future cap changes.
        let size = item.and_then(|item| {
            self.items_size
                .checked_add(usize::from(self.items != 0))
                .and_then(|size| size.checked_add(item))
        });
        let total = size.and_then(|size| {
            size.checked_add(self.overhead - 4)
                .and_then(|total| total.checked_add(cursor?))
        });
        if total.is_none_or(|total| total > ceiling) {
            return if self.items == 0 {
                Err(too_large(ceiling).into())
            } else {
                Ok(false)
            };
        }
        self.singleton = total.is_some_and(|total| total > self.preferred);
        self.items_size = size.unwrap_or_default();
        self.items += 1;
        Ok(true)
    }
}
