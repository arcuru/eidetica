//! [`RemoteBackend`]: the seam backed by a service connection.

use async_trait::async_trait;

use super::{Backend, MergeSlice};
use crate::{
    Result,
    auth::SigKey,
    backend::{InstanceMetadata, VerificationStatus},
    entry::{Entry, ID},
    instance::WriteSource,
    service::{client::RemoteConnection, protocol::ReadScope},
    snapshot::Snapshot,
};

/// A [`Backend`] that translates every storage operation to a wire RPC over a
/// shared [`RemoteConnection`].
///
/// The only per-handle state is the acting identity: `None` means "use the
/// connection's current session identity" (the instance-level backend), and
/// `Some(k)` means "act as `k`" (a `Database` handle opened with key `k`).
/// Every clone shares the same socket and session — additional keys are
/// proof-of-possession registered into the connection's keyset by the handle
/// constructors, not by holding a separate connection.
///
/// Tree-scoped methods use the `tree` argument the caller already supplies
/// (`Transaction` passes the owning database's root), so no root is bound here.
/// `get` derives its gating tree server-side from the fetched entry, so it
/// passes `ID::default()` as the (waved-through) request root.
///
/// CRDT-state caching is two-tiered: a connection-scoped process-lifetime LRU
/// (tier 1) backed by the daemon's unified scope-keyed cache (tier 2) reached
/// via `GetCachedCrdtState` / `CacheCrdtState` RPCs.
#[derive(Debug, Clone)]
pub struct RemoteBackend {
    conn: RemoteConnection,
    identity: Option<SigKey>,
}

/// Entries per `GetEntries` request.
///
/// A batched response travels as a single frame, and frames are capped at
/// [`MAX_FRAME_SIZE`](crate::service::protocol::MAX_FRAME_SIZE) (64 MiB), so
/// an unwindowed fetch of a long merge path can exceed the cap and fail where
/// a per-entry loop would have succeeded. Windowing bounds each response by
/// `chunk × entry size` instead of by the length of the path, at the cost of
/// one round-trip per window.
///
/// The bound is a count rather than a byte budget because the client cannot
/// know the encoded size of entries it has not fetched yet. 512 leaves ~128
/// KiB of frame budget per entry, well above an ordinary entry, while still
/// collapsing a thousand-entry path into two round-trips instead of a
/// thousand.
///
/// Tests override this via `EIDETICA_TEST_GET_ENTRIES_CHUNK` (see
/// [`get_entries_chunk`]).
const GET_ENTRIES_CHUNK: usize = 512;

/// Test-overridable window size. Reads `EIDETICA_TEST_GET_ENTRIES_CHUNK` if it
/// parses to a positive count; otherwise [`GET_ENTRIES_CHUNK`].
fn get_entries_chunk() -> usize {
    std::env::var("EIDETICA_TEST_GET_ENTRIES_CHUNK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(GET_ENTRIES_CHUNK)
}

/// Fetch every id in `ids` through `fetch`, in windows of at most `chunk`.
///
/// Windows are issued in input order and their results concatenated, so the
/// output order matches `ids` exactly as long as `fetch` preserves the order
/// of the window it is given. Callers depend on that: the merge path is the
/// canonical CRDT replay order, and a reorder silently corrupts merged state.
async fn fetch_windowed<F, Fut>(ids: &[ID], chunk: usize, fetch: F) -> Result<Vec<Entry>>
where
    F: Fn(Vec<ID>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Entry>>>,
{
    let mut entries = Vec::with_capacity(ids.len());
    for window in ids.chunks(chunk) {
        entries.extend(fetch(window.to_vec()).await?);
    }
    Ok(entries)
}

impl RemoteBackend {
    pub fn new(conn: RemoteConnection, identity: Option<SigKey>) -> Self {
        Self { conn, identity }
    }

    /// The acting identity for authenticated RPCs: the bound per-handle
    /// identity, else the connection's current session identity.
    fn identity(&self) -> SigKey {
        self.identity
            .clone()
            .or_else(|| self.conn.session_identity())
            .unwrap_or_default()
    }
}

#[async_trait]
impl Backend for RemoteBackend {
    async fn get(&self, id: &ID) -> Result<Entry> {
        // `ID::default()` is never a real database, so the pre-dispatch gate
        // waves it through; the server then gates post-fetch against the
        // fetched entry's owning tree using our identity.
        self.conn
            .db_get_entry(ID::default(), self.identity(), id.clone())
            .await
    }

    async fn get_entries(&self, ids: &[ID]) -> Result<Vec<Entry>> {
        // Same waved-through root as `get`: the server gates each entry
        // post-fetch by its owning tree. Order is preserved, so callers that
        // rely on the input order (e.g. a canonical CRDT replay path) get the
        // entries back in that order.
        //
        // Windowed so a long path cannot produce a response frame over the
        // protocol cap; see `GET_ENTRIES_CHUNK`.
        fetch_windowed(ids, get_entries_chunk(), |window| async move {
            self.conn
                .db_get_entries(ID::default(), self.identity(), window)
                .await
        })
        .await
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        match self
            .conn
            .get_verified_tips(tree.clone(), self.identity())
            .await
        {
            Ok(snapshot) => Ok(snapshot),
            Err(e) if e.is_not_found() => Ok(Snapshot::EMPTY),
            Err(e) => Err(e),
        }
    }

    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        let tree_tips = match self
            .conn
            .get_verified_tips(tree.clone(), self.identity())
            .await
        {
            Ok(tips) => tips,
            Err(e) if e.is_not_found() => return Ok(Snapshot::EMPTY),
            Err(e) => return Err(e),
        };
        if tree_tips.is_empty() {
            return Ok(Snapshot::EMPTY);
        }
        match self
            .conn
            .store_snapshot_at(
                tree.clone(),
                self.identity(),
                store.to_string(),
                tree_tips.into_tips(),
            )
            .await
        {
            Ok(snapshot) => Ok(snapshot),
            Err(e) if e.is_not_found() => Ok(Snapshot::EMPTY),
            Err(e) => Err(e),
        }
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        match self
            .conn
            .store_snapshot_at(
                tree.clone(),
                self.identity(),
                store.to_string(),
                main_snapshot.tips().to_vec(),
            )
            .await
        {
            Ok(snapshot) => Ok(snapshot),
            Err(e) if e.is_not_found() => Ok(Snapshot::EMPTY),
            Err(e) => Err(e),
        }
    }

    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.conn
            .get_store_entries(
                tree.clone(),
                self.identity(),
                store.to_string(),
                snapshot.tips().to_vec(),
                ReadScope::Verified,
            )
            .await
    }

    async fn compute_merge_state(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<MergeSlice> {
        // One RPC resolves base and path against a single server-side view;
        // see the trait doc for why they must not be two round-trips.
        let state = self
            .conn
            .compute_merge_state(
                tree.clone(),
                self.identity(),
                store.to_string(),
                entry_ids.to_vec(),
            )
            .await?;
        Ok(MergeSlice {
            merge_base: state.merge_base,
            path: state.path,
        })
    }

    async fn get_cached_crdt_state(
        &self,
        tree: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>> {
        // Tier 1: connection-shared process-lifetime LRU.
        if let Some(blob) = self.conn.cache_get(tree, entry_id, store) {
            return Ok(Some(blob));
        }
        // Tier 2: daemon-side unified cache, durable across sessions.
        let blob = self
            .conn
            .get_cached_crdt_state_remote(
                tree.clone(),
                self.identity(),
                store.to_string(),
                entry_id.clone(),
            )
            .await?;
        if let Some(b) = &blob {
            self.conn
                .cache_put(tree.clone(), entry_id.clone(), store.to_string(), b.clone());
        }
        Ok(blob)
    }

    async fn cache_crdt_state(
        &self,
        tree: &ID,
        entry_id: &ID,
        store: &str,
        state: Vec<u8>,
    ) -> Result<()> {
        // Tier 1: stash locally first so a same-session re-read hits even if
        // the tier-2 write later fails.
        self.conn.cache_put(
            tree.clone(),
            entry_id.clone(),
            store.to_string(),
            state.clone(),
        );
        // Tier 2: propagate to the daemon. Awaited so wire errors surface.
        self.conn
            .cache_crdt_state_remote(
                tree.clone(),
                self.identity(),
                store.to_string(),
                entry_id.clone(),
                state,
            )
            .await
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        let tree_root = entry.root().unwrap_or_else(|| entry.id());
        self.conn
            .submit_signed_entry(tree_root, self.identity(), entry)
            .await
    }

    async fn write_entry(
        &self,
        _verification: VerificationStatus,
        entry: Entry,
        _source: WriteSource,
    ) -> Result<()> {
        // The server stores the submitted entry `Unverified` and runs its own
        // verification pass; a client-asserted status is never trusted.
        let tree_root = entry.root().unwrap_or_else(|| entry.id());
        self.conn
            .submit_signed_entry(tree_root, self.identity(), entry)
            .await
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.conn.get_instance_metadata().await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.conn.set_instance_metadata(metadata).await
    }

    fn remote_connection(&self) -> Option<RemoteConnection> {
        Some(self.conn.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A distinct entry per index, so IDs are unique and order is checkable.
    fn test_entry(n: usize) -> Entry {
        Entry::builder(ID::from_bytes("windowed-root"))
            .set_parents(vec![ID::from_bytes("windowed-root")])
            .set_subtree_data("data", format!("{{\"n\":{n}}}").as_bytes())
            .build()
            .unwrap()
    }

    /// Run `fetch_windowed` over `count` entries with the given window size,
    /// returning the windows the fetcher saw and the IDs it produced.
    async fn run(count: usize, chunk: usize) -> (Vec<Vec<ID>>, Vec<ID>) {
        let entries: Vec<Entry> = (0..count).map(test_entry).collect();
        let ids: Vec<ID> = entries.iter().map(|e| e.id()).collect();
        let windows: Mutex<Vec<Vec<ID>>> = Mutex::new(Vec::new());

        let fetched = {
            let entries = &entries;
            let windows = &windows;
            fetch_windowed(&ids, chunk, move |window: Vec<ID>| {
                windows.lock().unwrap().push(window.clone());
                async move {
                    Ok(window
                        .iter()
                        .map(|id| {
                            entries
                                .iter()
                                .find(|e| e.id() == *id)
                                .expect("window holds only requested ids")
                                .clone()
                        })
                        .collect())
                }
            })
            .await
            .unwrap()
        };

        (
            windows.into_inner().unwrap(),
            fetched.iter().map(|e| e.id()).collect(),
        )
    }

    /// Every window stays within the bound, the windows partition the input in
    /// order, and the concatenated result is in input order. Order is the
    /// invariant that matters most: the merge path is the canonical CRDT
    /// replay order, and a reorder silently corrupts merged state.
    #[tokio::test]
    async fn windows_are_bounded_and_order_is_preserved() {
        let entries: Vec<Entry> = (0..7).map(test_entry).collect();
        let expected: Vec<ID> = entries.iter().map(|e| e.id()).collect();

        let (windows, fetched) = run(7, 3).await;

        assert_eq!(windows.len(), 3, "7 ids in windows of 3 is 3 requests");
        assert!(
            windows.iter().all(|w| w.len() <= 3),
            "no window may exceed the bound: {:?}",
            windows.iter().map(Vec::len).collect::<Vec<_>>()
        );
        let flattened: Vec<ID> = windows.concat();
        assert_eq!(flattened, expected, "windows must partition ids in order");
        assert_eq!(fetched, expected, "results must concatenate in input order");
    }

    /// A window size at or above the input length degenerates to the single
    /// batched request the windowing replaced.
    #[tokio::test]
    async fn a_short_input_takes_one_window() {
        let (windows, fetched) = run(4, 512).await;
        assert_eq!(windows.len(), 1);
        assert_eq!(fetched.len(), 4);
    }

    /// An empty fetch issues no request at all rather than an empty one.
    #[tokio::test]
    async fn an_empty_input_issues_no_request() {
        let (windows, fetched) = run(0, 3).await;
        assert!(windows.is_empty(), "no ids means no round-trip");
        assert!(fetched.is_empty());
    }

    /// A failing window aborts the fetch instead of returning a short result
    /// that would fold into a silently truncated CRDT state.
    #[tokio::test]
    async fn a_failing_window_aborts_the_fetch() {
        let ids: Vec<ID> = (0..9).map(|n| test_entry(n).id()).collect();
        let calls = Mutex::new(0usize);

        let result = {
            let calls = &calls;
            fetch_windowed(&ids, 3, move |window: Vec<ID>| {
                let mut n = calls.lock().unwrap();
                *n += 1;
                let fail = *n == 2;
                async move {
                    if fail {
                        Err(crate::Error::Io(std::io::Error::other("window failed")))
                    } else {
                        Ok(window.iter().map(|_| test_entry(0)).collect())
                    }
                }
            })
            .await
        };

        assert!(result.is_err(), "the window error must propagate");
        assert_eq!(
            *calls.lock().unwrap(),
            2,
            "the fetch must stop at the failing window"
        );
    }

    /// The window size falls back to the constant unless the test knob names a
    /// positive count.
    #[test]
    fn the_window_size_defaults_to_the_constant() {
        assert_eq!(get_entries_chunk(), GET_ENTRIES_CHUNK);
    }
}
