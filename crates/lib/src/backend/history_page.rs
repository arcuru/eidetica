//! Canonical, byte-bounded Store history pages.
//!
//! Entries are emitted in the same order as [`super::BackendImpl::store_at`]:
//! Store height ascending, tied on [`ID`]'s `Ord` (the CID tuple, not SQL text).
//! A backend streams reachable `(ID, height)` metadata once per page, keeps only
//! the next K candidates after the exclusive cursor in a bounded heap, and then
//! fetches those Entry bodies one at a time. This rescans O(history) metadata per
//! page by design. Reachable metadata uses a finite accounted workspace; bodies
//! are fetched only for selected candidates, not a whole-history Vec.

use std::{collections::BinaryHeap, sync::OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    Result,
    encoding::json_encoded_len,
    entry::{Entry, ID},
    snapshot::Snapshot,
};

use super::{BackendError, record_page::PageBudget};

/// Hard Entry-count ceiling for one history page.
pub const MAX_HISTORY_PAGE_ENTRIES: usize = 1024;
/// Hard encoded page ceiling. Transport envelopes must fit independently.
pub const MAX_HISTORY_PAGE_ENCODING: usize = 63 * 1024 * 1024;
/// Accounted per-operation metadata workspace (candidate heap, InMemory
/// visited set and work queue, SQL owned IDs and finite driver-channel slack).
/// Independent of any cache budget, so disabling
/// caching does not disable history reads. Not a database-engine or RSS bound.
pub const MAX_HISTORY_WORKSPACE_BYTES: usize = 16 * 1024 * 1024;
/// Reject a stored Entry body larger than this before fetching it.
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) const MAX_ENTRY_RAW_BYTES: usize = 64 * 1024 * 1024;
/// Concurrent history-page operations admitted per process. Excess callers
/// receive an explicit resource error rather than joining an unbounded queue.
pub(crate) const MAX_CONCURRENT_HISTORY_PAGES: usize = 8;
const CURSOR_VERSION: u8 = 1;
/// Request deadline for one SQL operation. Native statements drain through the
/// ordinary pool cleanup path; this is not a hard OS-I/O deadline.
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) const MAX_HISTORY_QUERY_DURATION: std::time::Duration =
    std::time::Duration::from_secs(120);
/// Maximum Snapshot tips accepted by one query; each is bound into SQL.
pub(crate) const MAX_HISTORY_TIPS: usize = 4096;

/// Bounds for one canonical history page.
///
/// `max_encoded_bytes` covers compact JSON of the entire [`HistoryPage`],
/// including its continuation. Counts and byte budgets are clamped to the hard
/// caps. These are application payload limits, not database or RSS quotas.
#[derive(Debug, Clone, Copy)]
pub struct HistoryPageLimits {
    pub max_entries: usize,
    pub max_encoded_bytes: usize,
    /// Permit a first Entry exceeding the preferred budget as a singleton,
    /// provided the complete page fits [`MAX_HISTORY_PAGE_ENCODING`].
    pub allow_large_singleton: bool,
}

/// Opaque exclusive continuation for [`HistoryPage`].
///
/// Versioned and bound to the exact tree, Store and Snapshot it was minted
/// for. It carries no authority: a cursor presented with any other query is
/// rejected with [`BackendError::InvalidHistoryCursor`], never reinterpreted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    v: u8,
    binding: [u8; 32],
    height: u64,
    id: ID,
}

/// One ordered page of Store history. `next` is `None` only when history after
/// the request cursor is exhausted; a non-final page is never empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub entries: Vec<Entry>,
    pub next: Option<HistoryCursor>,
}

/// A validated history query: exact Snapshot binding and decoded cursor key.
pub(crate) struct HistoryQuery<'a> {
    pub tree: &'a ID,
    pub store: &'a str,
    pub tips: &'a [ID],
    binding: [u8; 32],
    after: Option<(u64, ID)>,
}

impl<'a> HistoryQuery<'a> {
    pub(crate) fn new(
        tree: &'a ID,
        store: &'a str,
        snapshot: &'a Snapshot,
        after: Option<&HistoryCursor>,
    ) -> Result<Self> {
        let tips = snapshot.tips();
        if tips.len() > MAX_HISTORY_TIPS {
            return Err(resource("history Snapshot tips", MAX_HISTORY_TIPS));
        }
        let binding = binding(tree, store, snapshot);
        let after = match after {
            None => None,
            Some(cursor) if cursor.v == CURSOR_VERSION && cursor.binding == binding => {
                Some((cursor.height, cursor.id.clone()))
            }
            Some(_) => return Err(BackendError::InvalidHistoryCursor.into()),
        };
        Ok(Self {
            tree,
            store,
            tips,
            binding,
            after,
        })
    }

    pub(crate) fn cursor(&self, height: u64, id: &ID) -> HistoryCursor {
        HistoryCursor {
            v: CURSOR_VERSION,
            binding: self.binding,
            height,
            id: id.clone(),
        }
    }
}

/// Validate wire order, query binding and the exclusive continuation. A cursor
/// is not authority; callers must still authorize each page independently.
#[cfg(all(unix, feature = "service"))]
pub(crate) fn validate_page(
    query: &HistoryQuery<'_>,
    page: &HistoryPage,
    previous: &mut Option<(u64, ID)>,
) -> Result<()> {
    if page.entries.is_empty() && page.next.is_some() {
        return Err(BackendError::InvalidHistoryCursor.into());
    }
    for entry in &page.entries {
        let key = (entry.subtree_height(query.store).unwrap_or(0), entry.id());
        if previous.as_ref().is_some_and(|last| key <= *last) {
            return Err(BackendError::InvalidHistoryCursor.into());
        }
        *previous = Some(key);
    }
    if let Some(next) = &page.next {
        let (height, id) = previous
            .as_ref()
            .ok_or(BackendError::InvalidHistoryCursor)?;
        if next != &query.cursor(*height, id)
            || query
                .after
                .as_ref()
                .is_some_and(|after| (*height, id) <= (after.0, &after.1))
        {
            return Err(BackendError::InvalidHistoryCursor.into());
        }
    }
    Ok(())
}

fn binding(tree: &ID, store: &str, snapshot: &Snapshot) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("eidetica history cursor v1");
    let mut field = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    field(tree.to_string().as_bytes());
    field(store.as_bytes());
    for tip in snapshot.tips() {
        field(tip.to_string().as_bytes());
    }
    *hasher.finalize().as_bytes()
}

pub(crate) fn resource(resource: &str, limit: usize) -> crate::Error {
    BackendError::HistoryResourceLimit {
        resource: resource.to_string(),
        limit,
    }
    .into()
}

/// Admit one history operation without waiting.
pub(crate) fn admit() -> Result<OwnedSemaphorePermit> {
    static PERMITS: OnceLock<std::sync::Arc<Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| std::sync::Arc::new(Semaphore::new(MAX_CONCURRENT_HISTORY_PAGES)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| resource("concurrent history pages", MAX_CONCURRENT_HISTORY_PAGES))
}

/// Accounted metadata workspace. Charges conservative container costs rather
/// than wire lengths.
pub(crate) struct Workspace {
    pub(crate) used: usize,
    limit: usize,
}

impl Workspace {
    pub(crate) fn new() -> Self {
        Self {
            used: 0,
            limit: MAX_HISTORY_WORKSPACE_BYTES,
        }
    }

    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    #[cfg(test)]
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    pub(crate) fn charge(&mut self, bytes: usize) -> Result<()> {
        match self.used.checked_add(bytes) {
            Some(used) if used <= self.limit => {
                self.used = used;
                Ok(())
            }
            _ => Err(resource(
                "history metadata workspace",
                MAX_HISTORY_WORKSPACE_BYTES,
            )),
        }
    }
}

/// Monotone admission before allocation, shared by the native walks. The
/// charge covers owned IDs (or borrowed pointers), hash buckets/control bytes,
/// queue copies, worst-case capacity rounding and old/new growth overlap.
/// This is conservative accounting on the pinned containers, not allocator RSS.
pub(crate) struct Visits<T> {
    pub(crate) seen: std::collections::HashSet<T>,
    pub(crate) queue: std::collections::VecDeque<T>,
}
impl<T: Clone + Eq + std::hash::Hash> Visits<T> {
    pub(crate) const COST: usize = 16
        * (std::mem::size_of::<T>() + std::mem::size_of::<usize>() + 1)
        + 8 * std::mem::size_of::<T>();
    pub(crate) fn new() -> Self {
        Self {
            seen: Default::default(),
            queue: Default::default(),
        }
    }
    pub(crate) fn insert(&mut self, id: T, workspace: &mut Workspace) -> Result<()> {
        if self.seen.contains(&id) {
            return Ok(());
        }
        workspace.charge(Self::COST)?;
        self.seen
            .try_reserve(1)
            .map_err(|_| resource("history allocation", MAX_HISTORY_WORKSPACE_BYTES))?;
        self.queue
            .try_reserve(1)
            .map_err(|_| resource("history allocation", MAX_HISTORY_WORKSPACE_BYTES))?;
        self.seen.insert(id.clone());
        self.queue.push_back(id);
        Ok(())
    }
}

/// Bounded selection of the next K `(height, ID)` keys after the cursor,
/// using the canonical Rust comparator.
pub(crate) struct TopK {
    k: usize,
    after: Option<(u64, ID)>,
    heap: BinaryHeap<(u64, ID)>,
    more: bool,
    cursor_found: bool,
}

impl TopK {
    pub(crate) fn new(
        query: &HistoryQuery<'_>,
        limits: &HistoryPageLimits,
        workspace: &mut Workspace,
    ) -> Result<Self> {
        let k = limits.max_entries.clamp(1, MAX_HISTORY_PAGE_ENTRIES);
        // Heap storage plus one in-flight candidate, doubled for growth slack.
        workspace.charge(
            (k + 1)
                .saturating_mul(std::mem::size_of::<(u64, ID)>())
                .saturating_mul(2),
        )?;
        Ok(Self {
            k,
            after: query.after.clone(),
            heap: BinaryHeap::with_capacity(k + 1),
            more: false,
            cursor_found: query.after.is_none(),
        })
    }

    /// Offer one reachable key. Duplicates must already be removed.
    pub(crate) fn offer(&mut self, height: u64, id: &ID) {
        if let Some((after_height, after_id)) = &self.after {
            if (height, id) == (*after_height, after_id) {
                self.cursor_found = true;
            }
            if (height, id) <= (*after_height, after_id) {
                return;
            }
        }
        if self.heap.len() < self.k {
            self.heap.push((height, id.clone()));
            return;
        }
        self.more = true;
        if let Some(top) = self.heap.peek()
            && (height, id) < (top.0, &top.1)
        {
            self.heap.pop();
            self.heap.push((height, id.clone()));
        }
    }

    /// Ascending selected keys, and whether any key beyond them exists.
    pub(crate) fn finish(self) -> Result<(Vec<(u64, ID)>, bool)> {
        if !self.cursor_found {
            return Err(BackendError::InvalidHistoryCursor.into());
        }
        Ok((self.heap.into_sorted_vec(), self.more))
    }
}

/// Accumulates fetched Entries against the exact encoded page budget.
pub(crate) struct HistoryPageBuilder<'q, 'a> {
    query: &'q HistoryQuery<'a>,
    budget: PageBudget,
    page: HistoryPage,
}

impl<'q, 'a> HistoryPageBuilder<'q, 'a> {
    pub(crate) fn new(query: &'q HistoryQuery<'a>, limits: &HistoryPageLimits) -> Result<Self> {
        Ok(Self {
            query,
            budget: PageBudget::new(
                &HistoryPage::default(),
                limits.max_entries.clamp(1, MAX_HISTORY_PAGE_ENTRIES),
                limits.max_encoded_bytes,
                MAX_HISTORY_PAGE_ENCODING,
                limits.allow_large_singleton,
            )?,
            page: HistoryPage::default(),
        })
    }

    pub(crate) fn is_full(&self) -> bool {
        self.budget.is_full()
    }

    /// A stored body exceeded the raw ceiling before it was fetched.
    /// Return already-complete Entries first.
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub(crate) fn raw_too_large(&self, id: &ID) -> Result<()> {
        if self.budget.is_empty() {
            Err(BackendError::EntryTooLarge {
                id: id.clone(),
                encoded_bytes: MAX_ENTRY_RAW_BYTES + 1,
            }
            .into())
        } else {
            Ok(())
        }
    }

    /// Size a borrowed Entry before retaining it. `more` means some reachable
    /// Entry follows this one. The continuation names the last emitted Entry.
    pub(crate) fn push(&mut self, height: u64, entry: &Entry, more: bool) -> Result<bool> {
        let ceiling = self.budget.ceiling();
        let cursor = self.query.cursor(height, entry.id_ref());
        let item = json_encoded_len(entry, ceiling)?;
        let cursor_size = if more {
            json_encoded_len(&cursor, ceiling)?
        } else {
            None
        };
        match self.budget.admit(item, cursor_size, more) {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(_) => {
                return Err(BackendError::EntryTooLarge {
                    id: entry.id(),
                    encoded_bytes: ceiling + 1,
                }
                .into());
            }
        }
        self.page.entries.push(entry.clone());
        self.page.next = more.then_some(cursor);
        Ok(true)
    }

    pub(crate) fn finish(self) -> HistoryPage {
        self.page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ID {
        ID::from_bytes([n])
    }

    #[test]
    fn top_k_selects_canonical_keys_after_cursor() {
        let tree = id(0);
        let snapshot = Snapshot::new(vec![id(9)]);
        let limits = HistoryPageLimits {
            max_entries: 3,
            max_encoded_bytes: 1 << 20,
            allow_large_singleton: false,
        };
        let mut keys: Vec<(u64, ID)> = (1..=20u8).map(|n| (u64::from(n % 4), id(n))).collect();
        keys.sort();
        let query = HistoryQuery::new(&tree, "s", &snapshot, None).unwrap();
        let mut top = TopK::new(&query, &limits, &mut Workspace::new()).unwrap();
        for (height, key) in keys.iter().rev() {
            top.offer(*height, key);
        }
        let (selected, more) = top.finish().unwrap();
        assert_eq!(selected, keys[..3]);
        assert!(more);

        let cursor = query.cursor(keys[17].0, &keys[17].1);
        let query = HistoryQuery::new(&tree, "s", &snapshot, Some(&cursor)).unwrap();
        let mut top = TopK::new(&query, &limits, &mut Workspace::new()).unwrap();
        for (height, key) in &keys {
            top.offer(*height, key);
        }
        let (selected, more) = top.finish().unwrap();
        assert_eq!(selected, keys[18..]);
        assert!(!more);
    }

    #[test]
    fn cursor_is_bound_to_exact_query() {
        let tree = id(0);
        let snapshot = Snapshot::new(vec![id(9)]);
        let query = HistoryQuery::new(&tree, "s", &snapshot, None).unwrap();
        let cursor = query.cursor(1, &id(1));
        assert!(HistoryQuery::new(&tree, "s", &snapshot, Some(&cursor)).is_ok());
        let other = Snapshot::new(vec![id(8)]);
        for result in [
            HistoryQuery::new(&tree, "t", &snapshot, Some(&cursor)),
            HistoryQuery::new(&id(1), "s", &snapshot, Some(&cursor)),
            HistoryQuery::new(&tree, "s", &other, Some(&cursor)),
        ] {
            let err = result.err().expect("mismatched cursor must be rejected");
            assert!(matches!(
                err,
                crate::Error::Backend(ref e) if matches!(**e, BackendError::InvalidHistoryCursor)
            ));
        }
        let mut stale = cursor.clone();
        stale.v = CURSOR_VERSION + 1;
        assert!(HistoryQuery::new(&tree, "s", &snapshot, Some(&stale)).is_err());
    }

    #[test]
    fn workspace_limit_is_explicit() {
        let mut workspace = Workspace::new();
        workspace.charge(MAX_HISTORY_WORKSPACE_BYTES).unwrap();
        assert!(workspace.charge(1).is_err());
        assert!(Workspace::new().charge(usize::MAX).is_err());
    }
    #[test]
    fn history_capacity_admission_covers_owned_and_borrowed_containers() {
        fn check<T: Clone + Eq + std::hash::Hash>(ids: impl Iterator<Item = T>) {
            let mut visits = Visits::<T>::new();
            let mut workspace = Workspace::new();
            for id in ids {
                let before = workspace.used;
                visits.insert(id.clone(), &mut workspace).unwrap();
                let charged = workspace.used;
                visits.insert(id, &mut workspace).unwrap();
                assert_eq!(workspace.used, charged, "duplicate recharged");
                assert!(charged >= before);
                // Hash capacity is usable slots; round up for control/bucket
                // load and include a second complete allocation during growth.
                let actual = (visits.seen.capacity() * (std::mem::size_of::<T>() + 1) * 8 / 7
                    + 16
                    + visits.queue.capacity() * std::mem::size_of::<T>())
                    * 2;
                assert!(
                    charged >= actual,
                    "charged={charged} capacities={}/{} conservative actual={actual}",
                    visits.seen.capacity(),
                    visits.queue.capacity()
                );
            }
        }
        let ids: Vec<_> = (0u32..4096)
            .map(|n| ID::from_bytes(n.to_le_bytes()))
            .collect();
        check(ids.iter().cloned());
        check(ids.iter());
        for budget in [
            Visits::<ID>::COST - 1,
            Visits::<ID>::COST,
            Visits::<ID>::COST + 1,
        ] {
            let mut visits = Visits::<ID>::new();
            let mut workspace = Workspace::with_limit(budget);
            assert_eq!(
                visits.insert(id(3), &mut workspace).is_ok(),
                budget >= Visits::<ID>::COST
            );
            if budget < Visits::<ID>::COST {
                assert_eq!(visits.seen.capacity(), 0);
                assert_eq!(visits.queue.capacity(), 0);
            }
        }
    }

    #[test]
    fn history_canonical_id_text_fits_native_guard() {
        use cid::{Cid, multihash::Multihash};
        let hash = Multihash::<64>::wrap(u64::MAX, &[255; 64]).unwrap();
        for cid in [
            Cid::new_v1(u64::MAX, hash),
            Cid::new_v0(Multihash::wrap(0x12, &[255; 32]).unwrap()).unwrap(),
        ] {
            let canonical = cid.to_string();
            assert!(canonical.len() <= 256, "{canonical}");
            assert_eq!(ID::parse(&canonical).unwrap().to_string(), canonical);
        }
    }
    #[test]
    fn history_hard_singleton_cap_minus_one_exact_plus_one() {
        let one = Entry::root_builder()
            .set_subtree_data("s", vec![255u8])
            .set_metadata(b"boundary".to_vec())
            .build()
            .unwrap();
        let overhead = json_encoded_len(&HistoryPage::default(), usize::MAX)
            .unwrap()
            .unwrap();
        let one_size = json_encoded_len(&one, usize::MAX).unwrap().unwrap() + overhead;
        for target in [
            MAX_HISTORY_PAGE_ENCODING - 1,
            MAX_HISTORY_PAGE_ENCODING,
            MAX_HISTORY_PAGE_ENCODING + 1,
        ] {
            // Each additional 255 adds four JSON bytes. A 99 subtracts one,
            // letting the real immutable Entry envelope hit all three sizes.
            let n = (target - one_size).div_ceil(4) + 1;
            let excess = one_size + 4 * (n - 1) - target;
            let mut data = vec![255u8; n];
            for byte in data.iter_mut().take(excess) {
                *byte = 99;
            }
            let entry = Entry::root_builder()
                .set_subtree_data("s", data)
                .set_metadata(b"boundary".to_vec())
                .build()
                .unwrap();
            assert_eq!(
                json_encoded_len(&entry, usize::MAX).unwrap().unwrap() + overhead,
                target
            );
            let tree = entry.id();
            let snapshot = Snapshot::new(vec![tree.clone()]);
            let query = HistoryQuery::new(&tree, "s", &snapshot, None).unwrap();
            let limits = HistoryPageLimits {
                max_entries: 1024,
                max_encoded_bytes: 4 * 1024 * 1024,
                allow_large_singleton: true,
            };
            let mut page = HistoryPageBuilder::new(&query, &limits).unwrap();
            assert_eq!(
                page.push(0, &entry, false).is_ok(),
                target <= MAX_HISTORY_PAGE_ENCODING
            );
            if target <= MAX_HISTORY_PAGE_ENCODING {
                assert_eq!(
                    json_encoded_len(&page.finish(), usize::MAX)
                        .unwrap()
                        .unwrap(),
                    target
                );
            }
        }
    }
}
