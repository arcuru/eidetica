//! Bounded canonical Store history. No CRDT implementation or plaintext is
//! required by the transport. Budgets are fixed, not request/configuration API.

use std::{
    collections::{HashMap, HashSet},
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    Result, Snapshot,
    backend::{BackendError, BackendImpl, VerificationStatus},
    constants::INDEX,
    crdt::{CRDT, Codec, Doc},
    entry::{Entry, ID},
    store::{
        StoreError,
        query::{QuerySource, ReadScope, StoreQueryRequest},
    },
};

/// An authenticated, daemon-issued canonical source. Registration is encoded
/// Doc metadata, not merged Store data. The seal fixes all fields and the reader;
/// it permits same-source recovery, never selection of fresh tips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreSource {
    pub database: ID,
    pub store: String,
    pub type_id: String,
    pub source: QuerySource,
    pub snapshot: Snapshot,
    pub index_snapshot: Snapshot,
    pub registration: Vec<u8>,
    pub seal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCursor {
    pub context: String,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawStoreRequest {
    pub source: StoreSource,
    pub cursor: Option<RawCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawStorePage {
    pub source: StoreSource,
    pub offset: usize,
    pub entries: Vec<Entry>,
    pub next: Option<RawCursor>,
}

/// Client-side envelope checks run before any Entry is consumed. Store-owned
/// decoding/decryption follows these checks, not capability-error suppression.
impl RawStorePage {
    pub fn validate(&self, request: &RawStoreRequest) -> Result<()> {
        if (self.entries.is_empty()
            && (request.cursor.is_some() || !self.source.snapshot.is_empty()))
            || (!self.entries.is_empty() && self.source.snapshot.is_empty())
        {
            return Err(BackendError::InvalidRawPage.into());
        }
        let offset = request.cursor.as_ref().map_or(0, |c| c.offset);
        if self.entries.len() > Limits::default().nodes
            || self.source != request.source
            || self.offset != offset
            || self.next.as_ref().is_some_and(|next| {
                self.entries.is_empty()
                    || Some(next.offset) != offset.checked_add(self.entries.len())
                    || request
                        .cursor
                        .as_ref()
                        .is_some_and(|c| c.context != next.context)
            })
        {
            return Err(BackendError::InvalidRawPage.into());
        }
        let mut previous = None;
        let mut ids = HashSet::new();
        for entry in &self.entries {
            let order = (entry.subtree_height(&self.source.store)?, entry.id());
            if !entry.in_tree(&self.source.database)
                || !entry.in_subtree(&self.source.store)
                || !ids.insert(entry.id())
                || previous.as_ref().is_some_and(|p| p >= &order)
            {
                return Err(BackendError::InvalidRawPage.into());
            }
            previous = Some(order);
        }
        page_size(self, Limits::default().page_bytes)?;
        Ok(())
    }
}

// Fixed conservative ceilings, described in internal/service.md. Test code can
// replace these privately; neither the wire nor daemon config accepts budgets.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub nodes: usize,
    pub source_bytes: usize,
    pub page_bytes: usize,
    pub query_bytes: usize,
    pub user_bytes: usize,
    pub global_bytes: usize,
    pub user_jobs: usize,
    pub global_jobs: usize,
    pub user_contexts: usize,
    pub global_contexts: usize,
    pub source_contexts: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            nodes: 32_768,
            source_bytes: 256 * 1024 * 1024,
            page_bytes: 4 * 1024 * 1024,
            query_bytes: 1024 * 1024,
            user_bytes: 768 * 1024 * 1024,
            global_bytes: 1536 * 1024 * 1024,
            user_jobs: 2,
            global_jobs: 4,
            user_contexts: 8,
            global_contexts: 32,
            source_contexts: 2,
        }
    }
}

/// Counts actual JSON bytes without allocating an encoded copy. Stops the
/// serializer as soon as the budget is exceeded (including byte-array expansion).
pub(crate) fn encoded_size<T: Serialize>(value: &T, cap: usize) -> Result<usize> {
    struct Counter {
        bytes: usize,
        cap: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.cap.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("source encoding budget exceeded"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, cap };
    serde_json::to_writer(&mut counter, value).map_err(|_| BackendError::SourceTooLarge)?;
    Ok(counter.bytes)
}

fn page_size(page: &RawStorePage, cap: usize) -> Result<usize> {
    // This is the *actual* ServerFrame JSON shape, also in minimal builds where
    // the service module isn't compiled. A golden service test pins the shape.
    #[derive(Serialize)]
    enum Response<'a> {
        RawStore(&'a RawStorePage),
    }
    #[derive(Serialize)]
    enum Frame<'a> {
        Response(Response<'a>),
    }
    encoded_size(&Frame::Response(Response::RawStore(page)), cap)
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Reader {
    pub user: String,
    pub principal: String,
    pub connection: u64,
}
impl Reader {
    pub(crate) fn local() -> Self {
        Self {
            user: "local".into(),
            principal: "local".into(),
            connection: 0,
        }
    }
}

struct Context {
    reader: Reader,
    source: StoreSource,
    key: String,
    ids: Vec<ID>,
    posture: Vec<ID>,
    bytes: usize,
    next: usize,
    // Exact retry of the preceding page is allowed. No caller can skip ahead.
    previous: Option<usize>,
    last_used: Instant,
}
#[derive(Default)]
struct Accounting {
    jobs: HashMap<(String, String), usize>,
    contexts: HashMap<String, Context>,
}

/// Shared by all connections of a daemon (and clones of a local seam).
/// Only IDs/metadata are retained; Entries are fetched under a work reservation.
pub(crate) struct Sources {
    secret: [u8; 32],
    pub limits: Limits,
    ttl: Duration,
    state: Mutex<Accounting>,
    // coding: one source worker; use per-source locks if throughput warrants it.
    // Pending callers are capped by the existing context count and idle lifetime;
    // no request-spawning queue, enlarged work quota or refusal retry is involved.
    serial: Arc<tokio::sync::Mutex<()>>,
    callers: Arc<tokio::sync::Semaphore>,
}
impl Default for Sources {
    fn default() -> Self {
        Self::new(Limits::default(), Duration::from_secs(300))
    }
}
impl Sources {
    pub(crate) fn new(limits: Limits, ttl: Duration) -> Self {
        Self {
            secret: rand::random(),
            limits,
            ttl,
            state: Mutex::new(Accounting::default()),
            serial: Arc::new(tokio::sync::Mutex::new(())),
            callers: Arc::new(tokio::sync::Semaphore::new(
                limits.user_contexts.min(limits.global_contexts),
            )),
        }
    }

    #[cfg(any(test, all(unix, feature = "service")))]
    pub(crate) fn disconnect(&self, connection: u64) {
        self.state
            .lock()
            .unwrap()
            .contexts
            .retain(|_, c| c.reader.connection != connection);
    }

    fn key(tree: &ID, request: &StoreQueryRequest) -> String {
        blake3::hash(&serde_json::to_vec(&(tree, &request.store, &request.source.main)).unwrap())
            .to_hex()
            .to_string()
    }

    pub(crate) fn admit(
        self: &Arc<Self>,
        reader: &Reader,
        tree: &ID,
        request: &StoreQueryRequest,
    ) -> Result<Work> {
        if request.source.main.len() > self.limits.nodes
            || encoded_size(request, self.limits.query_bytes).is_err()
        {
            return Err(BackendError::SourceTooLarge.into());
        }
        let key = Self::key(tree, request);
        let mut state = self.state.lock().unwrap();
        state
            .contexts
            .retain(|_, c| c.last_used.elapsed() <= self.ttl);
        let global_jobs: usize = state.jobs.values().sum();
        let user_jobs: usize = state
            .jobs
            .iter()
            .filter(|((u, _), _)| u == &reader.user)
            .map(|(_, n)| *n)
            .sum();
        let retained: usize = state.contexts.values().map(|c| c.bytes).sum();
        let user_retained: usize = state
            .contexts
            .values()
            .filter(|c| c.reader.user == reader.user)
            .map(|c| c.bytes)
            .sum();
        if global_jobs >= self.limits.global_jobs
            || user_jobs >= self.limits.user_jobs
            || state.jobs.keys().any(|(_, source)| source == &key)
            || (global_jobs + 1) * self.limits.source_bytes + retained > self.limits.global_bytes
            || (user_jobs + 1) * self.limits.source_bytes + user_retained > self.limits.user_bytes
        {
            return Err(BackendError::SourceAdmissionRefused.into());
        }
        state.jobs.insert((reader.user.clone(), key.clone()), 1);
        Ok(Work {
            sources: self.clone(),
            user: reader.user.clone(),
            key,
            _serialization: None,
        })
    }

    pub(crate) async fn admit_serialized(
        self: &Arc<Self>,
        reader: &Reader,
        tree: &ID,
        request: &StoreQueryRequest,
    ) -> Result<Work> {
        if request.source.main.len() > self.limits.nodes
            || encoded_size(request, self.limits.query_bytes).is_err()
        {
            return Err(BackendError::SourceTooLarge.into());
        }
        let caller = self
            .callers
            .clone()
            .try_acquire_owned()
            .map_err(|_| BackendError::SourceAdmissionRefused)?;
        let guard = tokio::time::timeout(self.ttl, self.serial.clone().lock_owned())
            .await
            .map_err(|_| BackendError::SourceAdmissionRefused)?;
        let mut work = self.admit(reader, tree, request)?;
        work._serialization = Some((caller, guard));
        Ok(work)
    }

    fn seal(&self, reader: &Reader, source: &StoreSource) -> String {
        let mut unsealed = source.clone();
        unsealed.seal.clear();
        // Vec metadata retains its exact encoding; no randomized map ordering.
        let bytes = serde_json::to_vec(&(&reader.user, &reader.principal, unsealed)).unwrap();
        blake3::keyed_hash(&self.secret, &bytes)
            .to_hex()
            .to_string()
    }

    pub(crate) fn check_source(
        &self,
        reader: &Reader,
        tree: &ID,
        source: &StoreSource,
    ) -> Result<()> {
        encoded_size(source, self.limits.page_bytes)?;
        let expected = blake3::Hash::from_hex(self.seal(reader, source)).unwrap();
        if &source.database != tree || blake3::Hash::from_hex(&source.seal).ok() != Some(expected) {
            return Err(BackendError::InvalidRawSource.into());
        }
        Ok(())
    }

    pub(crate) async fn resolve(
        &self,
        engine: &dyn BackendImpl,
        reader: &Reader,
        tree: &ID,
        request: &StoreQueryRequest,
    ) -> Result<BoundSource> {
        if request.source.main.is_empty() {
            return Err(crate::transaction::TransactionError::EmptyTipsNotAllowed.into());
        }
        let mut walk = Walk::new(engine, tree, &request.source, self.limits);
        let main_ids = walk.walk(request.source.main.tips(), None).await?;
        if !main_ids.contains(tree) || !walk.entries[tree].is_root() {
            return Err(BackendError::InvalidRawSource.into());
        }
        walk.check_boundary(request.source.main.tips(), &main_ids)
            .await?;
        // Match the established current-frontier fast path atomically, without
        // an unbounded snapshot or recursive backend collection. Otherwise use
        // the already bounded, immutable main closure to derive Store tips.
        let current = engine
            .current_source_frontiers(tree, &request.source.main, &[INDEX, &request.store])
            .await?;
        let (index_snapshot, snapshot) = match current {
            Some(mut frontiers) => (frontiers.remove(0), frontiers.remove(0)),
            None => (
                walk.frontier(&main_ids, INDEX)?,
                walk.frontier(&main_ids, &request.store)?,
            ),
        };
        let index_ids = walk.walk(index_snapshot.tips(), Some(INDEX)).await?;
        walk.check_store_boundary(&index_ids).await?;
        let mut index = Doc::default();
        for id in &index_ids {
            if let Ok(bytes) = walk.entries[id].data(INDEX)
                && !bytes.is_empty()
            {
                index = index.merge(&Doc::decode(bytes)?)?;
            }
        }
        let registration = index
            .get(&request.store)
            .and_then(|v| v.as_doc())
            .ok_or_else(|| StoreError::InvalidConfiguration {
                store: request.store.clone(),
                reason: "Store is not registered at the requested source".into(),
            })?;
        let type_id = registration
            .get("type")
            .and_then(|v| v.as_text())
            .ok_or_else(|| StoreError::InvalidConfiguration {
                store: request.store.clone(),
                reason: "source registration has no Store type".into(),
            })?;
        if type_id != request.expected_type {
            return Err(StoreError::TypeMismatch {
                store: request.store.clone(),
                expected: type_id.into(),
                actual: request.expected_type.clone(),
            }
            .into());
        }
        let ids = walk.walk(snapshot.tips(), Some(&request.store)).await?;
        walk.check_store_boundary(&ids).await?;
        let mut source = StoreSource {
            database: tree.clone(),
            store: request.store.clone(),
            type_id: type_id.into(),
            source: request.source.clone(),
            snapshot,
            index_snapshot,
            registration: serde_json::to_vec(&serde_json::to_value(registration)?)?,
            seal: String::new(),
        };
        source.seal = self.seal(reader, &source);
        encoded_size(&source, self.limits.page_bytes.saturating_sub(128))?;
        let posture = walk.posture(&source);
        let entries = ids
            .iter()
            .map(|id| walk.entries.remove(id).unwrap())
            .collect();
        Ok(BoundSource {
            source,
            ids,
            posture,
            entries,
        })
    }

    async fn recover(
        &self,
        engine: &dyn BackendImpl,
        reader: &Reader,
        source: &StoreSource,
    ) -> Result<BoundSource> {
        self.check_source(reader, &source.database, source)?;
        self.validate_binding(engine, source).await
    }

    /// Only call with daemon-validated persisted metadata, never a replacement
    /// descriptor from a client. Walk the ORIGINAL tips, not current frontiers.
    pub(crate) async fn validate_binding(
        &self,
        engine: &dyn BackendImpl,
        source: &StoreSource,
    ) -> Result<BoundSource> {
        encoded_size(source, self.limits.page_bytes)?;
        let mut walk = Walk::new(engine, &source.database, &source.source, self.limits);
        let main = walk.walk(source.source.main.tips(), None).await?;
        if !main.contains(&source.database) {
            return Err(BackendError::InvalidRawSource.into());
        }
        walk.check_boundary(source.source.main.tips(), &main)
            .await?;
        // These immutable tips were sealed at initial resolution. In particular
        // do NOT call current_source_frontiers again on reconnect/expiry.
        let index_ids = walk.walk(source.index_snapshot.tips(), Some(INDEX)).await?;
        walk.check_store_boundary(&index_ids).await?;
        let mut index = Doc::default();
        for id in index_ids {
            if let Ok(bytes) = walk.entries[&id].data(INDEX)
                && !bytes.is_empty()
            {
                index = index.merge(&Doc::decode(bytes)?)?;
            }
        }
        let registration = index
            .get(&source.store)
            .and_then(|v| v.as_doc())
            .ok_or(BackendError::InvalidRawSource)?;
        if registration.get("type").and_then(|v| v.as_text()) != Some(source.type_id.as_str())
            || serde_json::to_vec(&serde_json::to_value(registration)?)? != source.registration
        {
            return Err(BackendError::InvalidRawSource.into());
        }
        let ids = walk
            .walk(source.snapshot.tips(), Some(&source.store))
            .await?;
        walk.check_store_boundary(&ids).await?;
        let posture = walk.posture(source);
        Ok(BoundSource {
            source: source.clone(),
            ids,
            posture,
            entries: Vec::new(),
        })
    }

    pub(crate) async fn page(
        &self,
        engine: &dyn BackendImpl,
        reader: &Reader,
        request: &RawStoreRequest,
    ) -> Result<RawStorePage> {
        self.check_source(reader, &request.source.database, &request.source)?;
        let cursor = if let Some(cursor) = &request.cursor {
            cursor.clone()
        } else {
            let bound = self.recover(engine, reader, &request.source).await?;
            // Encoded retained identifiers/metadata plus conservative allocation
            // overhead. Work reservations separately cover transient Entry copies.
            let bytes = encoded_size(
                &(&bound.source, &bound.ids, &bound.posture),
                self.limits.source_bytes,
            )? + 512 * (bound.ids.len() + bound.posture.len())
                + 1024;
            let query = StoreQueryRequest {
                store: bound.source.store.clone(),
                expected_type: bound.source.type_id.clone(),
                source: bound.source.source.clone(),
                query: Vec::new(),
            };
            let key = Self::key(&request.source.database, &query);
            let mut state = self.state.lock().unwrap();
            state
                .contexts
                .retain(|_, c| c.last_used.elapsed() <= self.ttl);
            let user_contexts = state
                .contexts
                .values()
                .filter(|c| c.reader.user == reader.user)
                .count();
            let source_contexts = state.contexts.values().filter(|c| c.key == key).count();
            let source_bytes: usize = state
                .contexts
                .values()
                .filter(|c| c.key == key)
                .map(|c| c.bytes)
                .sum();
            let global_jobs: usize = state.jobs.values().sum();
            let user_jobs: usize = state
                .jobs
                .iter()
                .filter(|((u, _), _)| u == &reader.user)
                .map(|(_, n)| *n)
                .sum();
            if source_bytes + bytes > self.limits.source_bytes
                || state.contexts.len() >= self.limits.global_contexts
                || user_contexts >= self.limits.user_contexts
                || source_contexts >= self.limits.source_contexts
                || global_jobs * self.limits.source_bytes
                    + state.contexts.values().map(|c| c.bytes).sum::<usize>()
                    + bytes
                    > self.limits.global_bytes
                || user_jobs * self.limits.source_bytes
                    + state
                        .contexts
                        .values()
                        .filter(|c| c.reader.user == reader.user)
                        .map(|c| c.bytes)
                        .sum::<usize>()
                    + bytes
                    > self.limits.user_bytes
            {
                return Err(BackendError::SourceAdmissionRefused.into());
            }
            let id = uuid::Uuid::new_v4().to_string();
            state.contexts.insert(
                id.clone(),
                Context {
                    reader: reader.clone(),
                    source: bound.source,
                    key,
                    ids: bound.ids,
                    posture: bound.posture,
                    bytes,
                    next: 0,
                    previous: None,
                    last_used: Instant::now(),
                },
            );
            RawCursor {
                context: id,
                offset: 0,
            }
        };
        let (ids, posture) = {
            let state = self.state.lock().unwrap();
            let ctx = state
                .contexts
                .get(&cursor.context)
                .ok_or(BackendError::InvalidRawCursor)?;
            if ctx.last_used.elapsed() > self.ttl
                || ctx.reader != *reader
                || ctx.source != request.source
                || (cursor.offset != ctx.next && Some(cursor.offset) != ctx.previous)
            {
                return Err(BackendError::InvalidRawCursor.into());
            }
            (ctx.ids.clone(), ctx.posture.clone())
        };
        // Permission is rechecked by dispatch; view admission rechecks the
        // strict closure or the original loose tips, never filters page data.
        for id in posture {
            check_posture(engine, &id, request.source.source.scope).await?;
        }
        let mut page = RawStorePage {
            source: request.source.clone(),
            offset: cursor.offset,
            entries: Vec::new(),
            next: Some(cursor.clone()),
        };
        let envelope = page_size(&page, self.limits.page_bytes)?;
        let mut payload_bytes = 0;
        let digits = |n: usize| n.checked_ilog10().unwrap_or(0) as usize + 1;
        for id in ids.iter().skip(cursor.offset) {
            let entry = engine.get_source_entry(id).await?;
            if entry.id_ref() != id
                || !entry.in_tree(&request.source.database)
                || !entry.in_subtree(&request.source.store)
            {
                return Err(BackendError::InvalidRawSource.into());
            }
            let entry_bytes = encoded_size(&entry, self.limits.page_bytes)?;
            let separator = usize::from(!page.entries.is_empty());
            let next_offset = cursor.offset + page.entries.len() + 1;
            let candidate_bytes =
                envelope + payload_bytes + separator + entry_bytes + digits(next_offset)
                    - digits(cursor.offset);
            if candidate_bytes > self.limits.page_bytes {
                if page.entries.is_empty() {
                    self.state.lock().unwrap().contexts.remove(&cursor.context);
                    return Err(BackendError::SourceTooLarge.into());
                }
                break;
            }
            payload_bytes += separator + entry_bytes;
            page.entries.push(entry);
        }
        let next = cursor.offset + page.entries.len();
        page.next = (next < ids.len()).then(|| RawCursor {
            context: cursor.context.clone(),
            offset: next,
        });
        page_size(&page, self.limits.page_bytes)?;
        let mut state = self.state.lock().unwrap();
        let ctx = state
            .contexts
            .get_mut(&cursor.context)
            .ok_or(BackendError::InvalidRawCursor)?;
        ctx.previous = Some(cursor.offset);
        ctx.next = next;
        ctx.last_used = Instant::now();
        // Final page releases retention immediately. A lost final response can
        // recover the sealed source with one bounded replay instead of leaking.
        if page.next.is_none() {
            state.contexts.remove(&cursor.context);
        }
        Ok(page)
    }
}

pub(crate) struct Work {
    sources: Arc<Sources>,
    user: String,
    key: String,
    _serialization: Option<(
        tokio::sync::OwnedSemaphorePermit,
        tokio::sync::OwnedMutexGuard<()>,
    )>,
}
impl Drop for Work {
    fn drop(&mut self) {
        self.sources
            .state
            .lock()
            .unwrap()
            .jobs
            .remove(&(self.user.clone(), self.key.clone()));
    }
}

pub(crate) struct BoundSource {
    pub source: StoreSource,
    pub ids: Vec<ID>,
    pub posture: Vec<ID>,
    pub entries: Vec<Entry>,
}

async fn check_posture(engine: &dyn BackendImpl, id: &ID, scope: ReadScope) -> Result<()> {
    let status = engine.get_verification_status(id).await?;
    if status == VerificationStatus::Failed
        || (scope == ReadScope::Verified && status != VerificationStatus::Verified)
    {
        return Err(StoreError::InvalidOperation {
            store: "source".into(),
            operation: "source read".into(),
            reason: format!("source entry {id} does not permit {scope:?}"),
        }
        .into());
    }
    Ok(())
}

/// One Entry fetch at a time, bounded unique IDs, edges, total JSON bytes and
/// frontier before enqueue/allocation. No whole-history backend RPC/CTE.
struct Walk<'a> {
    engine: &'a dyn BackendImpl,
    tree: &'a ID,
    source: &'a QuerySource,
    limits: Limits,
    entries: HashMap<ID, Entry>,
    bytes: usize,
    edges: usize,
}
impl<'a> Walk<'a> {
    fn new(
        engine: &'a dyn BackendImpl,
        tree: &'a ID,
        source: &'a QuerySource,
        limits: Limits,
    ) -> Self {
        Self {
            engine,
            tree,
            source,
            limits,
            entries: HashMap::new(),
            bytes: 0,
            edges: 0,
        }
    }
    async fn walk(&mut self, tips: &[ID], store: Option<&str>) -> Result<Vec<ID>> {
        if tips.len() > self.limits.nodes {
            return Err(BackendError::SourceTooLarge.into());
        }
        let mut pending = tips.to_vec();
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            if !self.entries.contains_key(&id) {
                if self.entries.len() >= self.limits.nodes {
                    return Err(BackendError::SourceTooLarge.into());
                }
                let entry = self.engine.get_source_entry(&id).await?;
                if entry.id_ref() != &id {
                    return Err(BackendError::InvalidRawSource.into());
                }
                if !entry.in_tree(self.tree) {
                    return Err(BackendError::EntryNotInTree {
                        entry_id: id,
                        tree_id: self.tree.clone(),
                    }
                    .into());
                }
                let bytes = encoded_size(&entry, self.limits.page_bytes)?;
                if bytes > self.limits.source_bytes.saturating_sub(self.bytes) {
                    return Err(BackendError::SourceTooLarge.into());
                }
                self.bytes += bytes;
                self.entries.insert(id.clone(), entry);
            }
            let entry = &self.entries[&id];
            let parents = match store {
                Some(store) => entry.subtree_parents(store)?,
                None => entry.parents()?,
            };
            self.edges += parents.len();
            if self.edges > self.limits.nodes * 8
                || parents.len() + pending.len() > self.limits.nodes
            {
                return Err(BackendError::SourceTooLarge.into());
            }
            pending.extend(parents.into_iter().filter(|p| !visited.contains(p)));
        }
        let mut ids: Vec<_> = visited.into_iter().collect();
        ids.sort_by(|a, b| {
            let height = |id: &ID| match store {
                Some(s) => self.entries[id].subtree_height(s).unwrap_or(0),
                None => self.entries[id].height(),
            };
            height(a).cmp(&height(b)).then_with(|| a.cmp(b))
        });
        Ok(ids)
    }
    // Admission follows the existing views: strict is an ancestor-closed
    // Verified prefix; explicit loose checks selected tips, not a value filter.
    async fn check_boundary(&self, tips: &[ID], closure: &[ID]) -> Result<()> {
        let ids = if self.source.scope == ReadScope::Verified {
            closure
        } else {
            tips
        };
        for id in ids {
            check_posture(self.engine, id, self.source.scope).await?;
        }
        Ok(())
    }

    fn posture(&self, source: &StoreSource) -> Vec<ID> {
        if self.source.scope == ReadScope::Verified {
            self.entries.keys().cloned().collect()
        } else {
            source.source.main.tips().to_vec()
        }
    }

    async fn check_store_boundary(&self, closure: &[ID]) -> Result<()> {
        if self.source.scope == ReadScope::Verified {
            for id in closure {
                check_posture(self.engine, id, ReadScope::Verified).await?;
            }
        }
        // Explicit loose uses the already selected raw main boundary. Store
        // tips derived from it stay immutable, including Failed interior data.
        // Selecting different Store tips here would silently retarget paging.
        Ok(())
    }

    fn frontier(&self, main: &[ID], store: &str) -> Result<Snapshot> {
        let entries: Vec<_> = main
            .iter()
            .filter(|id| self.entries[*id].in_subtree(store))
            .collect();
        let ids: HashSet<_> = entries.iter().map(|id| (*id).clone()).collect();
        let mut parents = HashSet::new();
        for id in entries {
            parents.extend(self.entries[id].subtree_parents(store)?);
        }
        Ok(Snapshot::from(
            ids.into_iter()
                .filter(|id| !parents.contains(id))
                .collect::<Vec<_>>(),
        ))
    }
}

#[cfg(test)]
mod tests;
