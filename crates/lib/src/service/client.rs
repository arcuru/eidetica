//! Remote connection client for the Eidetica service.
//!
//! `RemoteConnection` connects to an Eidetica service server and forwards
//! storage operations as RPC calls. It backs the `RemoteBackend` implementation
//! of the `Backend` seam and is not itself a `BackendImpl`.
//!
//! Authentication uses the client-side-signing flow described in the Service
//! Architecture doc § Security Model: `RemoteConnection::trusted_login` drives
//! the daemon's `TrustedLoginUser` / `TrustedLoginProve` challenge-response,
//! decrypts the user's root signing key in-process, and signs the challenge
//! locally. The daemon never sees the password or the plaintext signing key.
//! After login, subsequent backend operations travel inside the `Authenticated`
//! envelope and are dispatched against the user's identity; the daemon gates
//! each one per-tree against the target database's auth settings.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};

use lru::LruCache;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify, mpsc, oneshot};

use crate::auth::crypto::PrivateKey;
use crate::auth::crypto::{PublicKey, create_challenge_response};
use crate::auth::types::SigKey;
use crate::backend::InstanceMetadata;
use crate::entry::{Entry, ID};
use crate::instance::WeakInstance;
use crate::service::error::service_error_to_eidetica_error;
use crate::service::protocol::{
    AuthenticatedDbRequest, DatabaseOp, Handshake, HandshakeAck, MergeState, Notification,
    PROTOCOL_VERSION, ReadScope, ServerFrame, ServiceRequest, ServiceResponse, TransactionContext,
    WireCrdtValue, read_frame, write_frame,
};
use crate::user::UserError;
use crate::user::crypto::{decrypt_private_key, derive_encryption_key};
use crate::user::types::{KeyStorage, UserInfo};

/// Default cap on the client-side CRDT-state LRU. Matches `MAX_FRAME_SIZE`
/// (64 MiB) so a single oversized cached blob can still ride the wire.
const CLIENT_CACHE_CAPACITY_BYTES: usize = 64 * 1024 * 1024;

/// How long an `Idle` per-tree subscription is kept warm before the
/// sweep sends `UnsubscribeWrites`. A re-registration arriving inside
/// this window transitions back to `Subscribed` without a wire call.
///
/// Sized for a "user is briefly between two react renders" case rather
/// than "user closes the app, comes back tomorrow." 60s is plenty for
/// the churn case and small enough that abandoned subscriptions don't
/// linger.
///
/// Tests override this via the `EIDETICA_TEST_IDLE_GRACE_MS` env var
/// (see [`idle_grace_window`]) so the lazy-unsubscribe path is
/// exercisable without making test suites wait the full minute.
const IDLE_GRACE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// How often the sweep task wakes up to check for expired `Idle`
/// entries. Half the grace window so an entry that becomes Idle right
/// after a sweep tick still gets unsubscribed within roughly one grace
/// window's worth of clock time.
///
/// Tests override this via the `EIDETICA_TEST_SWEEP_INTERVAL_MS` env
/// var (see [`sweep_interval`]).
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Test-overridable grace window. Reads `EIDETICA_TEST_IDLE_GRACE_MS`
/// (milliseconds) if set; otherwise [`IDLE_GRACE_WINDOW`].
fn idle_grace_window() -> std::time::Duration {
    std::env::var("EIDETICA_TEST_IDLE_GRACE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(IDLE_GRACE_WINDOW)
}

/// Test-overridable sweep interval. Reads
/// `EIDETICA_TEST_SWEEP_INTERVAL_MS` (milliseconds) if set; otherwise
/// [`SWEEP_INTERVAL`].
fn sweep_interval() -> std::time::Duration {
    std::env::var("EIDETICA_TEST_SWEEP_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(SWEEP_INTERVAL)
}

/// Process-lifetime LRU of materialized CRDT states for this connection.
///
/// Tier 1 of a two-level cache: local hits short-circuit any wire activity;
/// misses fall through to `GetCachedCrdtState` against the daemon. Cleared
/// on connection drop — durability across the daemon's lifetime is the
/// unified [`crate::backend::CacheScope`]-keyed cache in the daemon's
/// `BackendImpl`, not this one.
///
/// Keys are `(root_id, key, store_name)`; values are opaque bytes (cipher-
/// or plaintext depending on the store, decided by the Transaction's
/// `encryptors` map). The cache itself is byte-blind.
struct ClientCrdtCache {
    lru: LruCache<(ID, ID, String), Vec<u8>>,
    current_bytes: usize,
    capacity_bytes: usize,
}

impl ClientCrdtCache {
    fn new(capacity_bytes: usize) -> Self {
        Self {
            lru: LruCache::unbounded(),
            current_bytes: 0,
            capacity_bytes,
        }
    }

    fn get(&mut self, root_id: &ID, key: &ID, store: &str) -> Option<Vec<u8>> {
        // `LruCache::get` promotes the entry to most-recently-used.
        self.lru
            .get(&(root_id.clone(), key.clone(), store.to_string()))
            .cloned()
    }

    fn put(&mut self, root_id: ID, key: ID, store: String, blob: Vec<u8>) {
        let blob_size = blob.len();
        let cache_key = (root_id, key, store);
        if let Some(prev) = self.lru.put(cache_key.clone(), blob) {
            self.current_bytes = self.current_bytes.saturating_sub(prev.len());
        }
        self.current_bytes = self.current_bytes.saturating_add(blob_size);
        // Evict LRU until under cap. Soft cap: a single oversized blob is
        // allowed to exceed the limit alone rather than thrashing.
        while self.current_bytes > self.capacity_bytes {
            let Some((k, v)) = self.lru.pop_lru() else {
                break;
            };
            if k == cache_key {
                self.lru.put(k, v);
                break;
            }
            self.current_bytes = self.current_bytes.saturating_sub(v.len());
        }
    }
}

/// Per-connection session state, populated by `trusted_login` on success.
///
/// Holds only the public key the daemon verified during challenge-response.
/// The plaintext signing key is intentionally **not** stored here — it lives
/// in the `User::key_manager` session that owns this connection. The
/// daemon-side `ConnectionState::Authenticated` is what carries the
/// `user_uuid` (for chunk 6's cache scoping); the client doesn't need it.
#[derive(Clone, Debug)]
struct SessionState {
    session_pubkey: PublicKey,
}

/// Per-tree subscription state for [`RemoteConnectionInner::subscribed_trees`].
///
/// State machine:
/// ```text
///     (absent)
///        │
///        │ first `on_write` for tree
///        ▼
///   InFlight(notify) ──leader wire failure──> (absent)
///        │
///        │ leader wire success
///        ▼
///    Subscribed ───drop last cb───> Idle { since: Instant }
///        ▲                              │
///        │ new `on_write` arrives        │ sweep determines past
///        │ (no wire call)                │ grace window
///        └──────────────────────────────┘
///                                       │
///                                       ▼
///                              UnsubscribeWrites on wire → (absent)
/// ```
///
/// `InFlight` carries a `Notify` whose waiters are released exactly
/// once when the leader finishes the wire round-trip (success or
/// failure). Followers re-check the map after waking — success
/// transitions to `Subscribed` (they return `Ok`), failure removes
/// the entry (one of them becomes the next leader on retry).
/// Defensive against a future shape change: under the current
/// [`RemoteConnectionInner::subscription_locks`] fence,
/// [`RemoteConnection::subscribe_writes`] is per-tree-serialized and
/// no caller can observe `InFlight` for a tree it's about to subscribe
/// to. Kept so a future relaxation of the fence (e.g. per-connection
/// rather than per-tree) doesn't silently re-introduce the
/// concurrent-leader race the `Notify` originally guarded.
///
/// `Idle` records the moment the last local callback for this tree
/// was dropped. The daemon-side subscription is still alive — we
/// haven't sent `UnsubscribeWrites` — so a re-registration before the
/// grace window expires can transition straight back to `Subscribed`
/// without a wire round-trip. A periodic sweep task removes Idle
/// entries that have been quiet long enough, sending
/// `UnsubscribeWrites` to the daemon at that point under the same
/// per-tree fence the subscribe path uses, so a sweep's Unsubscribe
/// is fully acked before any racing re-subscribe can send its
/// Subscribe — no daemon-side `Sub → Unsub` inversion possible.
enum SubState {
    InFlight(Arc<Notify>),
    Subscribed {
        identity: SigKey,
    },
    Idle {
        since: std::time::Instant,
        identity: SigKey,
    },
}

/// Role assignment for one entry into [`RemoteConnection::subscribe_writes`].
/// Decided under the `subscribed_trees` mutex and consumed outside it so the
/// std::Mutex is never held across an `await`.
enum SubRole {
    /// This task owns the wire round-trip and must transition the state +
    /// `notify_waiters` when it finishes (success or failure).
    Leader(Arc<Notify>),
    /// Another task is already subscribing; await its notify and re-check.
    Follower(Arc<Notify>),
}

/// Internal state for a remote connection, wrapped in Arc for Clone.
struct RemoteConnectionInner {
    /// Owns the write half of the socket. `tokio::sync::Mutex` because
    /// `write_frame` is async (held across awaits). Only ever held for the
    /// duration of one frame's write plus the FIFO push into [`Self::pending`];
    /// the await on the response itself happens *after* the lock is released
    /// so concurrent callers don't serialise on read-side latency.
    writer: Mutex<WriteHalf<UnixStream>>,
    /// FIFO of awaiting response slots. `request()` pushes one before
    /// releasing the writer lock; the reader task pops the front on every
    /// `ServerFrame::Response` so request and response order line up. The
    /// VecDeque is guarded by a plain `std::sync::Mutex` — never held
    /// across an await — so it can't deadlock with the writer lock.
    pending: std::sync::Mutex<VecDeque<oneshot::Sender<ServiceResponse>>>,
    /// Set once by [`RemoteConnection::attach_instance`] right after
    /// `Instance::connect` has finished building the Instance. The reader
    /// task reads (cheap clone of the inner `Weak`) on each
    /// [`Notification::DatabaseWrite`] to dispatch into the instance's
    /// callback registry. Stays `None` until attach; notifications can't
    /// arrive in that window because the client subscribes lazily on the
    /// first `Database::on_write` registration, which itself can't run
    /// until the Instance exists.
    weak_instance: std::sync::Mutex<Option<WeakInstance>>,
    /// Set on successful `trusted_login`; read by `backend_request` to populate
    /// the `Authenticated` envelope's identity field. `RwLock` because reads
    /// are far more frequent than the one-shot login write.
    ///
    /// Accessed poison-tolerantly via [`RemoteConnectionInner::session_read`]
    /// and [`RemoteConnectionInner::session_write`]: a panic in one task
    /// while holding the guard must not promote itself to a permanent connection
    /// outage. The worst observable case is a half-written session field, which
    /// the caller already treats as "unauthenticated" (`session_identity`
    /// returns `None` and the per-tree gate rejects the op).
    session: RwLock<Option<SessionState>>,
    /// Pubkeys this client has already proven possession of on this
    /// connection (via `SessionKeyChallenge`/`SessionKeyRegister`), plus the
    /// login pubkey added in `trusted_login`. Lets `register_session_key`
    /// short-circuit when the key has already been registered, avoiding
    /// per-request wire chatter for the common case where a single per-DB
    /// key is reused across many ops.
    registered_keys: Mutex<HashSet<PublicKey>>,
    /// Per-tree subscription state. Entries are inserted on first call to
    /// [`RemoteConnection::subscribe_writes`] and never removed on success
    /// (subscriptions live for the connection's lifetime; the daemon scrubs
    /// them on disconnect).
    ///
    /// The two states coordinate concurrent registrations against the same
    /// tree: exactly one task is the "leader" that drives the wire round-trip;
    /// other tasks observe `InFlight(notify)`, await the notify, and re-check
    /// state. On leader success the state transitions to `Subscribed` and
    /// followers return `Ok`; on leader failure the entry is removed so the
    /// next waker can take leadership and retry.
    subscribed_trees: std::sync::Mutex<HashMap<ID, SubState>>,
    /// Per-tree async mutexes that fence wire-subscription state
    /// transitions on this connection: held across the full
    /// `SubscribeWrites` / `UnsubscribeWrites` request-response by the
    /// leader path of [`RemoteConnection::subscribe_writes`] and by the
    /// lazy-unsubscribe sweep ([`run_sweep_task`]).
    ///
    /// Closes the latent sweep-vs-resubscribe race in the gap between
    /// the sweep removing an `Idle` entry from `subscribed_trees` and
    /// its `UnsubscribeWrites` reaching the daemon: a racing
    /// `subscribe_writes` could observe `None`, send `SubscribeWrites`,
    /// and — if the daemon processed Subscribe before the in-flight
    /// Unsubscribe — end Sub → Unsub (silent broken delivery).
    ///
    /// **Why the race is currently latent**: the daemon's per-connection
    /// request loop is serial today (`server.rs` SubscribeWrites
    /// handler comment), so Subscribe queues behind in-flight
    /// Unsubscribe and gets processed after — end Subscribed. The
    /// fence is structural future-proofing against a daemon shape
    /// change to per-connection parallel dispatch. Cheap to maintain
    /// (one async mutex per active tree, held only across sweep and
    /// subscribe wire RTTs) and removes the dependency on the daemon
    /// invariant entirely. Holding this lock across the daemon's ack
    /// means a `subscribe_writes` arriving while a sweep is in flight
    /// on the same tree blocks until the daemon has fully processed
    /// the unsubscribe, regardless of dispatch shape.
    ///
    /// **Correctness contract this fence depends on**: the daemon
    /// must serialize `SubscribeWrites` / `UnsubscribeWrites` *per
    /// tree* within a single connection. Today this holds trivially
    /// via per-connection serial dispatch. A future shape change to
    /// per-connection+tree-parallel dispatch (the natural next step,
    /// mirroring the client's `tree_workers`) also satisfies the
    /// contract: within tree X the daemon would still order
    /// Unsubscribe → Subscribe, while unrelated work on tree Y
    /// proceeds in parallel. The fence stays correct under that
    /// shape with no further work.
    ///
    /// What would break the fence: a daemon that *parallelizes
    /// requests within a single tree* on one connection, freely
    /// reordering Subscribe/Unsubscribe processing for the same
    /// `root_id`. That shape would also break verify, settled-state
    /// cursor advancement, and other invariants — it's not a
    /// realistic future direction. If it ever becomes one, this
    /// fence is insufficient and the design needs to revisit
    /// ack-then-Subscribe vs. an `Unsubscribing { notify }` sub-state.
    ///
    /// Deliberately a separate lock from [`crate::instance::Instance`]'s
    /// `tree_lock`: that lock serializes local `put_entry`/`verify`
    /// against callback-dispatch coherence; reusing it here would
    /// stall local writes on the same tree for an Unsubscribe RTT
    /// for no correctness benefit.
    ///
    /// Shape mirrors `Instance::tree_lock`: std mutex around a hashmap
    /// of `Arc<tokio::sync::Mutex<()>>` so the per-tree guard can be
    /// cloned out and held across awaits.
    subscription_locks: std::sync::Mutex<HashMap<ID, Arc<Mutex<()>>>>,
    /// Process-lifetime CRDT-state LRU shared across every `Database` handle
    /// (every `RemoteBackend`) on this connection. Tier 1 of the
    /// two-level cache; tier 2 is the daemon's unified scope-keyed cache
    /// (lives in `BackendImpl`), reached via `GetCachedCrdtState` /
    /// `CacheCrdtState` RPCs.
    ///
    /// Accessed poison-tolerantly via [`Self::crdt_cache_lock`]: same
    /// rationale as `session` — a panic in one task must not strand the
    /// rest of the connection, since cache state is rebuildable.
    crdt_cache: std::sync::Mutex<ClientCrdtCache>,
    /// Set to `true` when the reader task exits (clean EOF, socket error,
    /// or deserialization failure). Once set, [`RemoteConnection::request`]
    /// short-circuits with `ConnectionAborted` instead of pushing a fresh
    /// oneshot that would never be matched.
    ///
    /// Required because dropping the user-visible `RemoteConnection` does
    /// not tear down the inner Arc (the reader task holds its own clone);
    /// post-reader-exit calls would otherwise queue a sender into
    /// [`Self::pending`] and `await` indefinitely on a `recv()` that no
    /// one can fulfil. `pending` is cleared on reader exit, but a fresh
    /// request landing *after* the clear would push a new sender into
    /// the now-orphan queue.
    ///
    /// Ordering: the reader task sets this with `Release` ordering before
    /// clearing `pending`, so any `Acquire` load that observes `true` is
    /// guaranteed to also observe the empty queue.
    closed: AtomicBool,
    /// Per-tree dispatch lanes. The reader routes each incoming
    /// `Notification::DatabaseWrite` by `root_id` into the matching
    /// tree's `mpsc<Notification>` (lazily creating one + spawning a
    /// per-tree worker on first notification for the tree). Each
    /// worker pulls from its own channel and `await`s
    /// `Instance::fire_write_callbacks` sequentially — sequential
    /// within a tree (cursor advancement is well-defined), concurrent
    /// across trees (a slow callback on one tree doesn't stall any
    /// other tree's dispatches on this connection).
    ///
    /// **Why per-tree, not per-connection.** User-callback work is
    /// per-tree; cursor advancement is per-tree; the only ordering
    /// constraint we actually need is per-tree. The previous
    /// single-drain-task model serialised across trees and could
    /// stall an entire connection on one slow callback.
    ///
    /// **Why the reader doesn't await inline.** User callbacks may
    /// issue wire calls (e.g. `Database::open` on a connected
    /// instance) whose responses land through *this same reader*.
    /// Awaiting a callback inline would deadlock the reader against
    /// the response it needs to deliver. Routing to a separate worker
    /// task keeps the reader free.
    ///
    /// Each worker holds `Weak<RemoteConnectionInner>` so it doesn't
    /// keep `inner` alive. When `inner` drops (every user-facing
    /// `RemoteConnection` released *and* the reader has exited),
    /// every sender in this map drops, each worker's `recv()` returns
    /// `None`, and workers exit cleanly without prolonging
    /// `inner`'s lifetime.
    tree_workers: std::sync::Mutex<HashMap<ID, mpsc::UnboundedSender<Notification>>>,
}

impl RemoteConnectionInner {
    /// Acquire a read guard on `session`, tolerating poisoning.
    ///
    /// See the field-level doc on [`Self::session`] for the recovery rationale.
    fn session_read(&self) -> std::sync::RwLockReadGuard<'_, Option<SessionState>> {
        self.session
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Acquire a write guard on `session`, tolerating poisoning.
    fn session_write(&self) -> std::sync::RwLockWriteGuard<'_, Option<SessionState>> {
        self.session
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Acquire the CRDT cache lock, tolerating poisoning. See the field-level
    /// doc on [`Self::crdt_cache`] for the recovery rationale.
    fn crdt_cache_lock(&self) -> std::sync::MutexGuard<'_, ClientCrdtCache> {
        self.crdt_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Acquire the pending-queue lock, tolerating poisoning.
    fn pending_lock(&self) -> std::sync::MutexGuard<'_, VecDeque<oneshot::Sender<ServiceResponse>>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Get-or-insert the per-tree subscription mutex. The returned
    /// `Arc` is cheap to clone; the caller takes `lock().await` on it
    /// outside the std mutex guard.
    ///
    /// See the field-level doc on [`Self::subscription_locks`] for the
    /// race this fence closes.
    fn subscription_lock(&self, tree_id: &ID) -> Arc<Mutex<()>> {
        let mut locks = self
            .subscription_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        Arc::clone(
            locks
                .entry(tree_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    /// Mark this connection dead and drop all dependent state. Used by
    /// the reader task on its own exit path and by the sweep when an
    /// `UnsubscribeWrites` times out (a daemon that can't ack a trivial
    /// hashmap removal in seconds is broken; tear down and let the
    /// caller reconnect).
    ///
    /// Same three-step sequence as the reader's exit cleanup:
    /// 1. Mark `closed` with `Release` ordering so future `request()`
    ///    calls short-circuit before pushing senders into an orphan
    ///    queue.
    /// 2. Drain `pending` — every awaiting caller sees `RecvError`
    ///    and surfaces `ConnectionAborted`.
    /// 3. Drop every per-tree worker sender so workers exit cleanly.
    fn mark_dead(&self) {
        self.closed.store(true, Ordering::Release);
        self.pending_lock().clear();
        self.tree_workers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }
}

/// A connection to a remote Eidetica service server over a Unix domain socket.
///
/// `RemoteConnection` backs the `RemoteBackend` implementation of the `Backend`
/// seam. It provides the storage operations as inherent methods, plus additional
/// coordination methods like `notify_entry_written`.
///
/// Cloning is cheap (Arc-backed).
#[derive(Clone)]
pub struct RemoteConnection {
    inner: Arc<RemoteConnectionInner>,
}

impl std::fmt::Debug for RemoteConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteConnection").finish_non_exhaustive()
    }
}

impl RemoteConnection {
    /// Connect to a service server at the given socket path.
    ///
    /// Performs the protocol handshake, then spawns a background reader
    /// task that demuxes [`ServerFrame`]s: `Response` frames pop the next
    /// pending oneshot in FIFO order, `Notification` frames dispatch into
    /// the attached `Instance`'s callback registry (after
    /// [`Self::attach_instance`] has been called).
    pub async fn connect(path: impl AsRef<Path>) -> crate::Result<Self> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        let (mut reader, mut writer) = tokio::io::split(stream);

        // Send handshake
        let handshake = Handshake {
            protocol_version: PROTOCOL_VERSION,
        };
        write_frame(&mut writer, &handshake).await?;

        // Read ack
        let ack: HandshakeAck = read_frame(&mut reader).await?.ok_or_else(|| {
            crate::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "Server closed connection during handshake",
            ))
        })?;

        if ack.protocol_version != PROTOCOL_VERSION {
            return Err(crate::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Protocol version mismatch: client={}, server={}",
                    PROTOCOL_VERSION, ack.protocol_version
                ),
            )));
        }

        let inner = Arc::new(RemoteConnectionInner {
            writer: Mutex::new(writer),
            pending: std::sync::Mutex::new(VecDeque::new()),
            weak_instance: std::sync::Mutex::new(None),
            session: RwLock::new(None),
            registered_keys: Mutex::new(HashSet::new()),
            subscribed_trees: std::sync::Mutex::new(HashMap::new()),
            subscription_locks: std::sync::Mutex::new(HashMap::new()),
            crdt_cache: std::sync::Mutex::new(ClientCrdtCache::new(CLIENT_CACHE_CAPACITY_BYTES)),
            closed: AtomicBool::new(false),
            tree_workers: std::sync::Mutex::new(HashMap::new()),
        });

        // Spawn the reader task. It holds an Arc clone of `inner` so the
        // connection (and its pending queue) stay live as long as any
        // request is in flight, and exits cleanly on EOF / read error /
        // failure-to-deserialize. On exit it drops the remaining oneshot
        // senders (surfaces as `RecvError` on awaiting `request()`s) and
        // also drops every per-tree worker channel, which causes those
        // workers to exit. No separate dispatch task — per-tree workers
        // are spawned lazily by the reader on first notification per
        // tree.
        let inner_for_reader = inner.clone();
        tokio::spawn(run_reader_task(reader, inner_for_reader));

        // Spawn the lazy-unsubscribe sweep. It holds a `Weak<inner>` so
        // it doesn't extend `inner`'s lifetime; exits when `weak.upgrade()`
        // returns `None` (the connection is being torn down).
        let weak_for_sweep = Arc::downgrade(&inner);
        tokio::spawn(run_sweep_task(weak_for_sweep));

        Ok(Self { inner })
    }

    /// Attach an `Instance` to this connection so the reader task can
    /// dispatch incoming [`Notification::DatabaseWrite`]s into the
    /// instance's callback registry. Called exactly once by
    /// `Instance::connect` after the Instance has been constructed.
    /// Subsequent calls overwrite the previous reference, but no caller
    /// does that today.
    pub(crate) fn attach_instance(&self, weak: WeakInstance) {
        *self
            .inner
            .weak_instance
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(weak);
    }

    /// Look up a cached materialized CRDT state in the connection-shared
    /// process-lifetime LRU. Promotes the entry to most-recently-used.
    pub(crate) fn cache_get(&self, root_id: &ID, key: &ID, store: &str) -> Option<Vec<u8>> {
        self.inner.crdt_cache_lock().get(root_id, key, store)
    }

    /// Insert a materialized CRDT state into the connection-shared LRU.
    /// Triggers byte-bounded eviction if over capacity.
    pub(crate) fn cache_put(&self, root_id: ID, key: ID, store: String, blob: Vec<u8>) {
        self.inner.crdt_cache_lock().put(root_id, key, store, blob);
    }

    /// Send a request and await its response.
    ///
    /// Take the writer lock; push a oneshot into the pending FIFO; write
    /// the frame; release the lock; await the oneshot. Pushing the
    /// oneshot *while still holding the writer lock* guarantees the FIFO
    /// order in `pending` lines up with the on-wire order so the reader
    /// task pairs each `ServerFrame::Response` with the right caller.
    /// Concurrent `request()` calls do not serialise on the response
    /// wait — only on the (cheap) frame write.
    ///
    /// Two `closed` checks gate the path: a cheap `Acquire` load before
    /// acquiring the writer lock (the common-case fast path) and a second
    /// re-check inside the lock *after* pushing the oneshot, in case the
    /// reader exited concurrently between the first check and the push.
    /// The reader sets `closed` with `Release` ordering *before* clearing
    /// `pending`, so a load that sees `true` is guaranteed to see the
    /// empty (or about-to-be-empty) queue. Without the post-push check,
    /// a fresh request landing right after the reader clears could push
    /// a sender into the orphan queue and `rx.await` forever.
    async fn request(&self, req: ServiceRequest) -> crate::Result<ServiceResponse> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(connection_aborted());
        }
        let (tx, rx) = oneshot::channel::<ServiceResponse>();
        {
            let mut writer = self.inner.writer.lock().await;
            // Re-check under the writer lock to close the race where the
            // reader exits and clears `pending` between our pre-check and
            // this point. If we observe `closed` now, drop our oneshot on
            // the floor without pushing — no reader means no response.
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(connection_aborted());
            }
            // Push *before* writing the frame so the FIFO is consistent
            // with the order frames hit the wire. If the write fails we
            // pop the just-pushed sender so a future caller doesn't get
            // matched to a response that never comes.
            self.inner.pending_lock().push_back(tx);
            if let Err(e) = write_frame(&mut *writer, &req).await {
                let _ = self.inner.pending_lock().pop_back();
                return Err(e);
            }
        }
        rx.await.map_err(|_| connection_aborted())
    }

    /// Send a request and convert error responses to `crate::Error`.
    pub(crate) async fn request_ok(&self, req: ServiceRequest) -> crate::Result<ServiceResponse> {
        let resp = self.request(req).await?;
        match resp {
            ServiceResponse::Error(e) => Err(service_error_to_eidetica_error(e)),
            other => Ok(other),
        }
    }

    /// Wrap a `DatabaseOp` in the `AuthenticatedDb` envelope and send it.
    ///
    /// `(root_id, identity)` scope and `request_ok` error conversion, carrying
    /// a `DatabaseOp` in an `AuthenticatedDbRequest`.
    async fn db_request(
        &self,
        root_id: ID,
        identity: SigKey,
        op: DatabaseOp,
    ) -> crate::Result<ServiceResponse> {
        self.request_ok(ServiceRequest::AuthenticatedDb(Box::new(
            AuthenticatedDbRequest {
                root_id,
                identity,
                op,
            },
        )))
        .await
    }

    /// Authenticate this connection as `username` by completing the
    /// `TrustedLogin*` handshake against the daemon.
    ///
    /// Flow: send `TrustedLoginUser` → receive challenge + the user's full
    /// `UserInfo` (encrypted credentials, user-database id, status) → derive
    /// the password-encryption key locally (Argon2id) and decrypt the root
    /// signing key in-process (or take it raw for passwordless users) → sign
    /// the challenge → send `TrustedLoginProve` → expect `TrustedLoginOk`.
    ///
    /// The daemon never sees the password or the plaintext signing key; the
    /// trust model for shipping the encrypted blob over the socket is captured
    /// in the Service Architecture doc § Trusted login threat model.
    ///
    /// On success the connection's server-side state is `Authenticated`. The
    /// caller receives the user's record and the decrypted root key so it can
    /// build the `User` session without a second wire read of `_users` —
    /// reads through the wire always travel as the authenticated user, which
    /// with the per-tree gate means a fresh user without permissions on
    /// `_users` would not be able to re-fetch it.
    pub(crate) async fn trusted_login(
        &self,
        username: &str,
        password: Option<&str>,
    ) -> crate::Result<(String, UserInfo, PrivateKey)> {
        // Step 1: name the user, receive challenge + user record.
        let resp = self
            .request_ok(ServiceRequest::TrustedLoginUser {
                username: username.to_string(),
            })
            .await?;
        let (challenge, user_uuid, user_info) = match resp {
            ServiceResponse::TrustedLoginChallenge {
                challenge,
                user_uuid,
                user_info,
            } => (challenge, user_uuid, user_info),
            other => return Err(unexpected_response("TrustedLoginChallenge", &other)),
        };

        // Step 2: decrypt the root signing key locally. Cross-check that the
        // caller's password/no-password matches the credential's salt/no-salt;
        // a mismatch is the same UX-level error as a wrong password.
        let credentials = &user_info.credentials;
        let is_passwordless = credentials.password_salt.is_none();
        let signing_key = match (&credentials.root_key, password, is_passwordless) {
            (KeyStorage::Unencrypted { key }, None, true) => key.clone(),
            (
                KeyStorage::Encrypted {
                    ciphertext, nonce, ..
                },
                Some(pwd),
                false,
            ) => {
                let salt = credentials.password_salt.as_deref().ok_or_else(|| {
                    UserError::PasswordRequired {
                        operation: "decrypt root key for remote login".to_string(),
                    }
                })?;
                let kek = derive_encryption_key(pwd, salt)?;
                decrypt_private_key(ciphertext, nonce, &kek)?
            }
            _ => return Err(UserError::InvalidPassword.into()),
        };

        // Step 3: sign the challenge and send the proof.
        let signature = create_challenge_response(&challenge, &signing_key);
        let resp = self
            .request_ok(ServiceRequest::TrustedLoginProve { signature })
            .await?;
        match resp {
            ServiceResponse::TrustedLoginOk => {
                // Stash the verified session pubkey so subsequent
                // `backend_request` calls can populate the `Authenticated`
                // envelope's identity field.
                *self.inner.session_write() = Some(SessionState {
                    session_pubkey: credentials.root_key_id.clone(),
                });
                // The login pubkey is in the server-side session keyset by
                // construction (the server seeds it there in
                // `handle_trusted_login_prove`). Mirror that here so
                // `register_session_key` short-circuits without a wire
                // round-trip when called for the login key.
                self.inner
                    .registered_keys
                    .lock()
                    .await
                    .insert(credentials.root_key_id.clone());
                Ok((user_uuid, user_info, signing_key))
            }
            other => Err(unexpected_response("TrustedLoginOk", &other)),
        }
    }

    /// Prove possession of `signing_key` and add its public key to the
    /// connection's session keyset.
    ///
    /// Used by every `Database` handle whose `RemoteBackend` carries a
    /// per-database identity (e.g. `Database::create` on a connected
    /// instance, or `user.open_database_with_key` over the wire): the daemon
    /// gates reads against the *acting* pubkey from the identity hint, and
    /// the acting pubkey must be in the keyset, so we register the per-DB
    /// key before the first read.
    ///
    /// Idempotent and cheap on repeated calls: a successful registration
    /// caches the pubkey in `registered_keys`, and a follow-up call with the
    /// same key returns `Ok(())` without touching the wire. The login pubkey
    /// is seeded into the cache by `trusted_login`.
    ///
    /// Cryptographically a two-step proof of possession:
    /// 1. `SessionKeyChallenge { pubkey }` → server returns a single-use,
    ///    pubkey-bound random challenge.
    /// 2. Client signs the challenge with `signing_key`; `SessionKeyRegister
    ///    { pubkey, signature }` → server verifies and inserts the pubkey
    ///    into its `session_keyset`.
    pub(crate) async fn register_session_key(&self, signing_key: &PrivateKey) -> crate::Result<()> {
        let pubkey = signing_key.public_key();
        {
            let cache = self.inner.registered_keys.lock().await;
            if cache.contains(&pubkey) {
                return Ok(());
            }
        }
        // Step 1: ask for a challenge bound to this pubkey.
        let resp = self
            .request_ok(ServiceRequest::SessionKeyChallenge {
                pubkey: pubkey.clone(),
            })
            .await?;
        let challenge = match resp {
            ServiceResponse::SessionKeyChallenge { challenge } => challenge,
            other => return Err(unexpected_response("SessionKeyChallenge", &other)),
        };
        // Step 2: sign and submit. The daemon verifies and joins the pubkey
        // into the connection's keyset on Ok.
        let signature = create_challenge_response(&challenge, signing_key);
        let resp = self
            .request_ok(ServiceRequest::SessionKeyRegister {
                pubkey: pubkey.clone(),
                signature,
            })
            .await?;
        Self::expect_ok(resp)?;
        self.inner.registered_keys.lock().await.insert(pubkey);
        Ok(())
    }

    // === Response extraction helpers ===

    fn expect_ok(resp: ServiceResponse) -> crate::Result<()> {
        match resp {
            ServiceResponse::Ok => Ok(()),
            other => Err(unexpected_response("Ok", &other)),
        }
    }

    // === Instance-level operations ===

    /// Build a `SigKey` from the session pubkey, when logged in.
    pub fn session_identity(&self) -> Option<SigKey> {
        self.inner
            .session_read()
            .as_ref()
            .map(|s| SigKey::from_pubkey(&s.session_pubkey))
    }

    pub async fn get_instance_metadata(&self) -> crate::Result<Option<InstanceMetadata>> {
        let resp = self.request_ok(ServiceRequest::GetInstanceMetadata).await?;
        match resp {
            ServiceResponse::InstanceMetadata(meta) => Ok(meta),
            other => Err(unexpected_response("InstanceMetadata", &other)),
        }
    }

    pub async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> crate::Result<()> {
        // Gated server-side as Admin on `_databases`, not on `root_id`, so the
        // scope's `root_id` is unused for this op (default is fine).
        let identity = self.session_identity().unwrap_or_default();
        let resp = self
            .db_request(
                ID::default(),
                identity,
                DatabaseOp::SetInstanceMetadata {
                    metadata: Box::new(metadata.clone()),
                },
            )
            .await?;
        Self::expect_ok(resp)
    }

    /// Subscribe this connection to write notifications for `tree_id`.
    ///
    /// Safe to call concurrently from multiple tasks for the same
    /// `tree_id`: serialized per-tree via
    /// [`RemoteConnectionInner::subscription_locks`] so only one task
    /// at a time decides state + runs the wire round-trip. The fence
    /// also serializes against the lazy-unsubscribe sweep — if a
    /// sweep is mid-`UnsubscribeWrites` for this tree, this call
    /// blocks until the daemon has fully acked the unsubscribe, then
    /// observes `None` and sends a fresh `SubscribeWrites`. Wire
    /// order Unsubscribe → Subscribe is preserved end-to-end,
    /// independent of the daemon's request-dispatch shape.
    ///
    /// Returns `Ok` only *after* the daemon has registered the
    /// subscription, so an immediately-following commit on this
    /// connection cannot race the subscribe and lose its
    /// notification. On failure the local state is rolled back so a
    /// subsequent call can retry.
    ///
    /// Idempotent across calls: a tree that is already `Subscribed`
    /// returns `Ok` without any wire activity. The server's
    /// `ConnectionRegistry` is also idempotent. Identity is gated
    /// server-side as Read on `tree_id`.
    pub(crate) async fn subscribe_writes(
        &self,
        tree_id: ID,
        identity: SigKey,
        tips: Vec<ID>,
    ) -> crate::Result<()> {
        // Per-tree subscription fence. Held across the entire state
        // decision + (leader path's) wire round-trip so a concurrent
        // sweep that's mid-`UnsubscribeWrites` for this tree can't
        // interleave its frame between our state read and our Subscribe.
        // See `RemoteConnectionInner::subscription_locks` for the race
        // this closes.
        let sub_lock = self.inner.subscription_lock(&tree_id);
        let _sub_guard = sub_lock.lock().await;
        loop {
            // Decide our role under the std::Mutex without holding it across
            // any await: leader inserts `InFlight(notify)` and proceeds to the
            // wire call; followers clone the notify and `await` it below.
            // `Idle` re-entries transition straight to `Subscribed` without
            // a wire round-trip — the daemon-side subscription is still
            // alive (the sweep hasn't unsubscribed it yet).
            let role = {
                let mut subs = self.subscribed_trees_lock();
                match subs.get(&tree_id) {
                    Some(SubState::Subscribed { .. }) => return Ok(()),
                    Some(SubState::Idle {
                        identity: existing_identity,
                        ..
                    }) => {
                        // Daemon-side subscription is still live; re-mark
                        // as Subscribed locally without a wire call. Reuse
                        // the identity the original subscribe succeeded
                        // with — the daemon's subscription is keyed off
                        // that pubkey, not whatever this caller now holds.
                        let existing_identity = existing_identity.clone();
                        subs.insert(
                            tree_id.clone(),
                            SubState::Subscribed {
                                identity: existing_identity,
                            },
                        );
                        return Ok(());
                    }
                    Some(SubState::InFlight(n)) => SubRole::Follower(n.clone()),
                    None => {
                        let n = Arc::new(Notify::new());
                        subs.insert(tree_id.clone(), SubState::InFlight(n.clone()));
                        SubRole::Leader(n)
                    }
                }
            };

            match role {
                SubRole::Follower(notify) => {
                    notify.notified().await;
                    // Re-check: success → return Ok; failure (entry removed)
                    // → loop, where this task may become the next leader.
                    continue;
                }
                SubRole::Leader(notify) => {
                    let result = self
                        .db_request(
                            tree_id.clone(),
                            identity.clone(),
                            DatabaseOp::SubscribeWrites { tips: tips.clone() },
                        )
                        .await
                        .and_then(Self::expect_ok);
                    {
                        let mut subs = self.subscribed_trees_lock();
                        match &result {
                            Ok(()) => {
                                subs.insert(
                                    tree_id.clone(),
                                    SubState::Subscribed {
                                        identity: identity.clone(),
                                    },
                                );
                            }
                            Err(_) => {
                                subs.remove(&tree_id);
                            }
                        }
                    }
                    // Wake everyone exactly once; new waiters that arrive
                    // after this point land in the post-transition state.
                    notify.notify_waiters();
                    return result;
                }
            }
        }
    }

    fn subscribed_trees_lock(&self) -> std::sync::MutexGuard<'_, HashMap<ID, SubState>> {
        self.inner
            .subscribed_trees
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Transition the subscription state for `tree_id` from `Subscribed`
    /// to `Idle`. Called from `WriteCallback::drop` when the last local
    /// callback for a tree on this connection is released.
    ///
    /// No wire call: the daemon-side subscription stays alive through
    /// the Idle grace window. If a new `on_write` registration arrives
    /// before the sweep, [`Self::subscribe_writes`] transitions back to
    /// `Subscribed` without touching the wire.
    ///
    /// If the state isn't `Subscribed` at the moment of the call —
    /// e.g. a concurrent re-registration already raced us, or the
    /// sweep already unsubscribed — this is a no-op.
    pub(crate) fn transition_to_idle(&self, tree_id: &ID) {
        let mut subs = self.subscribed_trees_lock();
        if let Some(SubState::Subscribed { identity }) = subs.get(tree_id) {
            let identity = identity.clone();
            subs.insert(
                tree_id.clone(),
                SubState::Idle {
                    since: std::time::Instant::now(),
                    identity,
                },
            );
        }
    }

    /// Send `UnsubscribeWrites` to the daemon for `tree_id`. Called by
    /// the sweep task when an `Idle` entry's grace window has expired.
    pub(crate) async fn unsubscribe_writes(
        &self,
        tree_id: ID,
        identity: SigKey,
    ) -> crate::Result<()> {
        self.db_request(tree_id, identity, DatabaseOp::UnsubscribeWrites)
            .await
            .and_then(Self::expect_ok)
    }

    // === Database operations (DatabaseOp via AuthenticatedDb envelope) ===

    /// Acquire a [`TransactionContext`] for the given stores and scope.
    ///
    /// The returned context includes main-tree parents with heights,
    /// per-store subtree parents, `_settings` tips, and the merged
    /// `_settings` value — everything needed to build and sign an entry
    /// locally without further round-trips.
    pub async fn begin_transaction(
        &self,
        root_id: ID,
        identity: SigKey,
        stores: Vec<String>,
        scope: ReadScope,
    ) -> crate::Result<TransactionContext> {
        let resp = self
            .db_request(
                root_id,
                identity,
                DatabaseOp::BeginTransaction { stores, scope },
            )
            .await?;
        match resp {
            ServiceResponse::TransactionContext(ctx) => Ok(ctx),
            other => Err(unexpected_response("TransactionContext", &other)),
        }
    }

    /// Fetch the server-materialized merged state of an unencrypted store.
    pub async fn get_store_state(
        &self,
        root_id: ID,
        identity: SigKey,
        store: String,
    ) -> crate::Result<WireCrdtValue> {
        let resp = self
            .db_request(root_id, identity, DatabaseOp::GetStoreState { store })
            .await?;
        match resp {
            ServiceResponse::CrdtValue(v) => Ok(v),
            other => Err(unexpected_response("CrdtValue", &other)),
        }
    }

    /// Fetch ordered, verified, opaque store entries reachable from `tips`.
    ///
    /// Universal primitive — works for encrypted stores (client decrypts
    /// locally) as well as unencrypted ones.
    pub async fn get_store_entries(
        &self,
        root_id: ID,
        identity: SigKey,
        store: String,
        tips: Vec<ID>,
        scope: ReadScope,
    ) -> crate::Result<Vec<Entry>> {
        let resp = self
            .db_request(
                root_id,
                identity,
                DatabaseOp::GetStoreEntries { store, tips, scope },
            )
            .await?;
        match resp {
            ServiceResponse::Entries(entries) => Ok(entries),
            other => Err(unexpected_response("Entries", &other)),
        }
    }

    /// Fetch the database's Verified-frontier tips.
    pub async fn get_verified_tips(&self, root_id: ID, identity: SigKey) -> crate::Result<Vec<ID>> {
        let resp = self
            .db_request(root_id, identity, DatabaseOp::GetVerifiedTips)
            .await?;
        match resp {
            ServiceResponse::Ids(ids) => Ok(ids),
            other => Err(unexpected_response("Ids", &other)),
        }
    }

    /// Submit a client-signed entry to the server.
    ///
    /// The server stores the entry as `Unverified` and runs its own
    /// verification pass — it never trusts a submitted entry's claimed
    /// validity.
    pub async fn submit_signed_entry(
        &self,
        root_id: ID,
        identity: SigKey,
        entry: Entry,
    ) -> crate::Result<()> {
        let resp = self
            .db_request(
                root_id,
                identity,
                DatabaseOp::SubmitSignedEntry {
                    entry: Box::new(entry),
                },
            )
            .await?;
        match resp {
            ServiceResponse::Ok => Ok(()),
            other => Err(unexpected_response("Ok", &other)),
        }
    }

    /// Fetch a single database entry by id.
    ///
    /// Gated post-fetch by the entry's owning tree, so the caller must hold
    /// at least `Read` on the database the entry belongs to.
    pub async fn db_get_entry(
        &self,
        root_id: ID,
        identity: SigKey,
        id: ID,
    ) -> crate::Result<Entry> {
        let resp = self
            .db_request(root_id, identity, DatabaseOp::GetEntry { id })
            .await?;
        match resp {
            ServiceResponse::Entry(entry) => Ok(entry),
            other => Err(unexpected_response("Entry", &other)),
        }
    }

    /// Subtree tips reachable from given main-tree entries.
    pub async fn get_store_tips_up_to_entries(
        &self,
        root_id: ID,
        identity: SigKey,
        store: String,
        up_to: Vec<ID>,
    ) -> crate::Result<Vec<ID>> {
        let resp = self
            .db_request(
                root_id,
                identity,
                DatabaseOp::GetStoreTipsUpToEntries { store, up_to },
            )
            .await?;
        match resp {
            ServiceResponse::Ids(ids) => Ok(ids),
            other => Err(unexpected_response("Ids", &other)),
        }
    }

    /// Compute merge state: lowest common ancestor + path to tip entries.
    pub async fn compute_merge_state(
        &self,
        root_id: ID,
        identity: SigKey,
        store: String,
        entry_ids: Vec<ID>,
    ) -> crate::Result<MergeState> {
        let resp = self
            .db_request(
                root_id,
                identity,
                DatabaseOp::ComputeMergeState { store, entry_ids },
            )
            .await?;
        match resp {
            ServiceResponse::MergeState(state) => Ok(state),
            other => Err(unexpected_response("MergeState", &other)),
        }
    }

    /// Tier 2 cache read: ask the daemon for a previously-stashed CRDT
    /// state blob. `None` on miss; the caller falls back to a full
    /// recompute from store entries.
    pub async fn get_cached_crdt_state_remote(
        &self,
        root_id: ID,
        identity: SigKey,
        store: String,
        key: ID,
    ) -> crate::Result<Option<Vec<u8>>> {
        let resp = self
            .db_request(
                root_id,
                identity,
                DatabaseOp::GetCachedCrdtState { store, key },
            )
            .await?;
        match resp {
            ServiceResponse::CachedCrdtState(blob) => Ok(blob),
            other => Err(unexpected_response("CachedCrdtState", &other)),
        }
    }

    /// Tier 2 cache write: stash a client-computed CRDT state blob in the
    /// daemon's unified cache, scoped to the session user
    /// ([`crate::backend::CacheScope::User`]). Per-user trust; the daemon
    /// stores opaque bytes verbatim.
    pub async fn cache_crdt_state_remote(
        &self,
        root_id: ID,
        identity: SigKey,
        store: String,
        key: ID,
        blob: Vec<u8>,
    ) -> crate::Result<()> {
        let resp = self
            .db_request(
                root_id,
                identity,
                DatabaseOp::CacheCrdtState { store, key, blob },
            )
            .await?;
        match resp {
            ServiceResponse::Ok => Ok(()),
            other => Err(unexpected_response("Ok", &other)),
        }
    }
}

fn unexpected_response(expected: &str, actual: &ServiceResponse) -> crate::Error {
    crate::Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Expected {expected} response, got {actual:?}"),
    ))
}

/// Canonical "connection torn down" error returned to any caller whose
/// request couldn't reach (or be answered by) the daemon.
fn connection_aborted() -> crate::Error {
    crate::Error::Io(std::io::Error::new(
        std::io::ErrorKind::ConnectionAborted,
        "Server closed connection unexpectedly",
    ))
}

/// Background task driving the read half of the socket.
///
/// Loops on [`read_frame`] and demuxes by [`ServerFrame`] variant:
///
/// - `Response(r)`: pop the front of the pending FIFO and resolve its
///   oneshot. If the queue is empty something has gone badly wrong
///   (server sent more responses than the client issued requests) — log
///   and continue.
/// - `Notification(Notification::DatabaseWrite { … })`: upgrade the
///   attached `WeakInstance` and route the event into its callback
///   registry via [`crate::Instance::fire_write_callbacks`]. If no
///   instance is attached yet or the instance has been dropped, the
///   notification is silently dropped — both are expected end-states,
///   not errors.
///
/// Exit conditions: clean EOF (server closed), any read error, or any
/// deserialisation error. On exit the task drops its `Arc<inner>`, which
/// in turn drops every remaining oneshot sender in `pending`, surfacing
/// as a `RecvError` on each awaiting `request()` (translated to a
/// connection-closed `io::Error` there).
async fn run_reader_task(mut reader: ReadHalf<UnixStream>, inner: Arc<RemoteConnectionInner>) {
    loop {
        let frame_result: crate::Result<Option<ServerFrame>> = read_frame(&mut reader).await;
        let frame = match frame_result {
            Ok(Some(f)) => f,
            Ok(None) => break, // Clean EOF
            Err(e) => {
                tracing::debug!("RemoteConnection reader error: {e}");
                break;
            }
        };

        match frame {
            ServerFrame::Response(resp) => {
                let next = inner.pending_lock().pop_front();
                match next {
                    Some(tx) => {
                        // Receiver dropped → the caller has already given
                        // up. Not an error worth logging.
                        let _ = tx.send(*resp);
                    }
                    None => {
                        tracing::warn!(
                            "RemoteConnection reader: response with no pending request; dropping"
                        );
                    }
                }
            }
            ServerFrame::Notification(notif) => {
                route_notification(&inner, notif);
            }
        }
    }

    // Mark the connection dead with `Release` ordering paired against
    // `request()`'s `Acquire` load: any post-exit caller that observes
    // `true` is guaranteed to also see the drained `pending` queue and
    // bail with `ConnectionAborted` instead of pushing a sender no one
    // will ever pop. The helper also drops every per-tree worker
    // sender so workers exit cleanly without prolonging `inner`'s
    // lifetime.
    inner.mark_dead();
}

/// Route a notification to its per-tree worker, spawning one if this is
/// the first notification for the tree on this connection.
///
/// Worker spawn is lazy: we don't create a worker for a tree until the
/// daemon actually pushes a notification for it. The map of per-tree
/// senders lives in `inner.tree_workers` (std mutex; the map is touched
/// for at most a single insert + clone per notification).
///
/// Sends are best-effort: if the worker has already exited (e.g. the
/// connection is winding down and `inner` is mid-drop), the send fails
/// and we silently drop. Same posture as the previous single-dispatch
/// shape.
///
/// TODO(dispatch-bound): per-tree channels are `unbounded`. Under
/// sustained write load on one tree, a slow user callback lets that
/// worker's queue grow without limit, holding all queued notifications
/// in client memory.
///
/// Under the cursor-only `Notification::DatabaseWrite` shape, drops are
/// *recoverable*: a worker that drops event N still receives event N+1
/// whose `post_tips` reflects the daemon's latest frontier, and the user
/// callback's next fire's `previous_tips = post_tips_of_N+1` lets
/// `ids_added` pick up any skipped IDs. So drop-oldest via
/// `Mutex<VecDeque<Notification>> + Notify` is the right v2 shape —
/// roughly a 30-line primitive isolated to this file. Bound size is the
/// tunable; `~256` is a reasonable starting point.
///
/// Deferred for its own PR (alongside server-side
/// [`TODO(backpressure)`]) so the drop-semantics tests get focused
/// review.
fn route_notification(inner: &Arc<RemoteConnectionInner>, notif: Notification) {
    let tree_id = match &notif {
        Notification::DatabaseWrite { root_id, .. } => root_id.clone(),
    };
    let tx = {
        let mut workers = inner
            .tree_workers
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        workers
            .entry(tree_id)
            .or_insert_with(|| {
                let (tx, rx) = mpsc::unbounded_channel::<Notification>();
                let weak = Arc::downgrade(inner);
                tokio::spawn(run_tree_worker(rx, weak));
                tx
            })
            .clone()
    };
    let _ = tx.send(notif);
}

/// Drain one tree's notification queue, dispatching to the attached
/// `Instance`'s callback registry in arrival order.
///
/// **Ordering guarantee within the tree.** Notifications are processed
/// strictly one at a time — the next `recv()` doesn't run until the
/// previous callback's `fire_write_callbacks().await` has returned.
/// The reader pushes in the order frames hit the socket, so user
/// callbacks for this tree observe events in the daemon's canonical
/// order.
///
/// **No ordering guarantee across trees.** Different trees have their
/// own worker tasks; a slow callback on tree A doesn't stall tree B's
/// dispatches on the same connection. This is the load-bearing
/// difference from the previous single-drain-task shape.
///
/// **Why this can't be inline in the reader.** User callbacks may
/// issue wire ops (e.g. `Database::open` over the connected instance)
/// whose responses land through the same reader. Awaiting a callback
/// inline would deadlock the reader against the response it is
/// supposed to deliver. Per-tree workers keep the reader free.
///
/// **Lifecycle.** Holds `Weak<RemoteConnectionInner>` so it does not
/// extend `inner`'s lifetime. When the reader exits it clears
/// `tree_workers`, dropping every sender; `recv()` returns `None`;
/// this worker exits.
async fn run_tree_worker(
    mut rx: mpsc::UnboundedReceiver<Notification>,
    weak_inner: Weak<RemoteConnectionInner>,
) {
    while let Some(notif) = rx.recv().await {
        // Snapshot the attached `WeakInstance` per-notification under
        // the std mutex — never held across an await. Worst case is
        // `None`, which we treat as "instance not attached yet" (the
        // attach-vs-first-notification race is impossible in practice
        // because attach happens before `Instance::connect` returns,
        // and subscriptions only start after that).
        let weak_instance = {
            let Some(inner) = weak_inner.upgrade() else {
                tracing::debug!(
                    "RemoteConnection tree worker: inner gone; exiting"
                );
                return;
            };
            let guard = inner
                .weak_instance
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.clone()
        };
        let Some(weak) = weak_instance else {
            tracing::debug!(
                "RemoteConnection tree worker: notification before attach_instance; dropping"
            );
            continue;
        };
        let Some(instance) = weak.upgrade() else {
            tracing::debug!(
                "RemoteConnection tree worker: instance dropped; ignoring notification"
            );
            continue;
        };

        match notif {
            Notification::DatabaseWrite {
                root_id,
                previous_tips,
                post_tips,
                source,
            } => {
                instance
                    .fire_write_callbacks(&root_id, &previous_tips, &post_tips, source)
                    .await;
            }
        }
    }
}

/// Bound on how long the sweep waits for the daemon's
/// `UnsubscribeWrites` ack before declaring the connection broken.
///
/// The daemon's handler is a single hashmap removal — well under a
/// millisecond on local transport, single-digit ms over loopback TCP.
/// Five seconds is a generous "is the daemon alive at all" bound; on
/// expiry we mark the connection dead via
/// [`RemoteConnectionInner::mark_dead`] and let callers reconnect.
const UNSUBSCRIBE_RTT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Periodically tear down `Idle` per-tree subscriptions whose grace
/// window has elapsed.
///
/// Wakes every [`SWEEP_INTERVAL`]; for each entry in
/// `subscribed_trees` that *appears* to be `Idle` past
/// [`IDLE_GRACE_WINDOW`], the sweep processes that tree under its
/// per-tree subscription fence (see
/// `RemoteConnectionInner::subscription_locks`):
///
/// 1. Acquire `subscription_lock(tree_id)`. This serializes with the
///    leader path in [`RemoteConnection::subscribe_writes`], which
///    must take the same lock before any mutation of
///    `subscribed_trees`. A racing re-registration that arrived after
///    the candidate snapshot blocks here until our unsubscribe is
///    fully acked.
/// 2. Re-check state under the `subscribed_trees` std mutex. If still
///    `Idle` past grace, remove the entry and capture its identity
///    for the wire call. If the state changed (re-registered, swept
///    by another path) skip.
/// 3. Send `UnsubscribeWrites` via [`RemoteConnection::unsubscribe_writes`]
///    wrapped in [`tokio::time::timeout`] of [`UNSUBSCRIBE_RTT_TIMEOUT`].
///    On timeout the daemon is wedged on a trivial op — mark the
///    connection dead and exit.
/// 4. Release `subscription_lock`. A pending `subscribe_writes` on
///    this tree can now proceed: it observes `None` in
///    `subscribed_trees`, takes the leader path, and sends a fresh
///    `SubscribeWrites` — guaranteed to land on the daemon *after*
///    our `UnsubscribeWrites` because we held the fence across the
///    ack, independent of the daemon's request-dispatch shape.
///
/// Holds `Weak<RemoteConnectionInner>` so it doesn't extend `inner`'s
/// lifetime. Exits when the upgrade fails (last `Arc<inner>` dropped →
/// connection torn down) or when the timeout path marks the connection
/// dead.
async fn run_sweep_task(weak_inner: Weak<RemoteConnectionInner>) {
    let grace = idle_grace_window();
    let mut ticker = tokio::time::interval(sweep_interval());
    // Skip the initial immediate tick so we don't sweep before any
    // subscription has had a chance to register.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let Some(inner) = weak_inner.upgrade() else {
            tracing::debug!("RemoteConnection sweep: inner gone; exiting");
            return;
        };

        // Cheap snapshot: collect tree IDs that *appear* to be Idle
        // past grace. The per-tree fence below re-checks under the
        // std mutex so a re-registration that lands between snapshot
        // and fence acquisition is honored.
        let candidates: Vec<ID> = {
            let subs = inner
                .subscribed_trees
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let now = std::time::Instant::now();
            subs.iter()
                .filter_map(|(id, state)| match state {
                    SubState::Idle { since, .. } if now.duration_since(*since) >= grace => {
                        Some(id.clone())
                    }
                    _ => None,
                })
                .collect()
        };

        // Resolve the connection handle once for the batch. Moving
        // `inner` into the `RemoteConnection` releases the local
        // upgrade `Arc` at the same time, so the sweep task does not
        // hold a strong reference to `inner` across the wire calls.
        let conn = RemoteConnection { inner };

        for tree_id in candidates {
            // Per-tree fence: hold across the full Unsubscribe RTT so
            // a concurrent `subscribe_writes` on the same tree must
            // wait for the daemon's ack before sending its Subscribe.
            // See `RemoteConnectionInner::subscription_locks`.
            let sub_lock = conn.inner.subscription_lock(&tree_id);
            let _sub_guard = sub_lock.lock().await;

            // Re-check under the std mutex now that we hold the
            // fence. If a re-registration won the race before we got
            // the fence, state will be `Subscribed` (or `InFlight` /
            // absent on an unrelated race) and we skip — the next
            // sweep tick will pick it up if it goes Idle again.
            let identity = {
                let mut subs = conn
                    .inner
                    .subscribed_trees
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let now = std::time::Instant::now();
                match subs.get(&tree_id) {
                    Some(SubState::Idle { since, .. }) if now.duration_since(*since) >= grace => {
                        // Still due — pull the entry out so a racing
                        // `subscribe_writes` (queued behind our
                        // subscription_lock) will observe `None` when
                        // it finally proceeds and take the leader path.
                        match subs.remove(&tree_id) {
                            Some(SubState::Idle { identity, .. }) => Some(identity),
                            _ => unreachable!("just observed Idle under the same lock"),
                        }
                    }
                    _ => None,
                }
            };

            let Some(identity) = identity else {
                continue;
            };

            // Wire round-trip under the fence. Timeout-then-teardown
            // on hang: a daemon that can't ack a hashmap removal in
            // five seconds is broken; mark dead and let the next
            // caller reconnect. Don't release the fence and continue
            // — that re-opens the race we're fencing against.
            match tokio::time::timeout(
                UNSUBSCRIBE_RTT_TIMEOUT,
                conn.unsubscribe_writes(tree_id.clone(), identity),
            )
            .await
            {
                Ok(Ok(())) => tracing::debug!(?tree_id, "lazy unsubscribe complete"),
                Ok(Err(e)) => tracing::debug!(
                    ?tree_id,
                    "lazy unsubscribe failed (connection likely closing): {e}"
                ),
                Err(_elapsed) => {
                    tracing::error!(
                        ?tree_id,
                        "lazy unsubscribe timed out after {:?}; tearing down connection",
                        UNSUBSCRIBE_RTT_TIMEOUT,
                    );
                    conn.inner.mark_dead();
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eid(s: &str) -> ID {
        ID::from_bytes(s)
    }

    fn root() -> ID {
        eid("root")
    }

    #[test]
    fn client_cache_round_trip() {
        let mut c = ClientCrdtCache::new(1024);
        c.put(root(), eid("e1"), "store1".into(), b"hello".to_vec());
        assert_eq!(
            c.get(&root(), &eid("e1"), "store1"),
            Some(b"hello".to_vec())
        );
    }

    #[test]
    fn client_cache_evicts_under_byte_pressure() {
        let mut c = ClientCrdtCache::new(100);
        c.put(root(), eid("e1"), "s".into(), vec![1u8; 50]);
        c.put(root(), eid("e2"), "s".into(), vec![2u8; 50]);
        assert_eq!(c.current_bytes, 100);
        c.put(root(), eid("e3"), "s".into(), vec![3u8; 50]);
        assert!(
            c.get(&root(), &eid("e1"), "s").is_none(),
            "least-recently-used entry must be evicted"
        );
        assert_eq!(c.get(&root(), &eid("e2"), "s"), Some(vec![2u8; 50]));
        assert_eq!(c.get(&root(), &eid("e3"), "s"), Some(vec![3u8; 50]));
    }

    #[test]
    fn client_cache_get_promotes_to_most_recent() {
        let mut c = ClientCrdtCache::new(100);
        c.put(root(), eid("e1"), "s".into(), vec![1u8; 50]);
        c.put(root(), eid("e2"), "s".into(), vec![2u8; 50]);
        let _ = c.get(&root(), &eid("e1"), "s"); // promote e1
        c.put(root(), eid("e3"), "s".into(), vec![3u8; 50]);
        assert!(
            c.get(&root(), &eid("e1"), "s").is_some(),
            "promoted entry must survive eviction"
        );
        assert!(
            c.get(&root(), &eid("e2"), "s").is_none(),
            "older un-touched entry must be evicted"
        );
    }

    #[test]
    fn client_cache_replaces_in_place() {
        let mut c = ClientCrdtCache::new(1024);
        c.put(root(), eid("e1"), "s".into(), b"v1".to_vec());
        c.put(root(), eid("e1"), "s".into(), b"v2-different-len".to_vec());
        assert_eq!(
            c.get(&root(), &eid("e1"), "s"),
            Some(b"v2-different-len".to_vec())
        );
        // current_bytes should reflect only the replacement, not the sum.
        assert_eq!(c.current_bytes, b"v2-different-len".len());
    }
}
