//! In-process multi-instance test harness (Tier 0).
//!
//! Standing up several real `Instance`s that sync with one another takes a pile
//! of identical boilerplate: create a user and key, enable sync, register a
//! transport, start serving, resolve the bound address. [`Cluster`] does *that*
//! plumbing and nothing else — it hands back wired peers and leaves every policy
//! decision (auth, what to write, how to drive sync) to the test.
//!
//! That split is deliberate. A correctness harness for authenticated CRDT sync
//! must keep the two things it exists to exercise — the **transport** and the
//! **auth** — under the test's control, not baked into the setup:
//!
//! - **Transport is a seam.** [`ClusterBuilder::transport`] takes any
//!   [`TestTransport`]; the default is [`HttpLoopback`]. A controllable in-memory
//!   transport (deliver / reorder / drop / single-step — Tier 1) is a drop-in
//!   here, which is the point: Tier 1 extends this, it doesn't replace it.
//! - **Auth is the test's.** The harness never grants keys or permissions for
//!   you. A peer exposes its `User`, key id, and key name; the test creates its
//!   database with whatever auth posture it's exercising. [`add_auth_keys`] and
//!   [`set_global_auth_key`] are policy-neutral *tools* the test composes — they
//!   apply the keys you pass, they don't choose them.
//!
//! What the harness owns is plumbing only: wiring peers, marking a tree
//! sync-enabled ([`Peer::serve`]), driving an exchange ([`Cluster::exchange`]),
//! and observing convergence ([`Cluster::converged`]). It does not hold your
//! databases — the test opens and keeps those itself.
//!
//! This is **topology A** (multi-peer sync): N independent `Instance`s, each
//! owning an in-memory backend. Sync is driven explicitly (no background timers),
//! so a test fully orders the exchange. The multi-client / single-service
//! topology is a separate harness that lands with the `service` feature.
//!
//! Gated behind `cfg(any(test, feature = "testing"))` alongside [`FixedClock`]
//! and [`Instance::create_backend_with_clock`]; never compiled into a release build.
//!
//! ```no_run
//! # async fn ex() -> eidetica::Result<()> {
//! use eidetica::{
//!     auth::{Permission, types::AuthKey},
//!     crdt::Doc,
//!     testing::{Cluster, set_global_auth_key},
//!     user::types::SyncSettings,
//! };
//!
//! let mut net = Cluster::builder().peers(2).build().await?;
//!
//! // Peer 0 creates a database with auth the *test* chooses, then serves it.
//! let key0 = net.peer(0).key_id().clone();
//! let mut settings = Doc::new();
//! settings.set("name", "chat");
//! let db = net.peer_mut(0).user_mut().create_database(settings, &key0).await?;
//! let room = db.root_id().clone();
//! set_global_auth_key(&db, AuthKey::active(None, Permission::Write(10))).await?;
//! net.peer_mut(0).serve(&room).await?;
//!
//! // Peer 1 bootstraps with its own key, then converges against peer 0.
//! let key1 = net.peer(1).key_id().clone();
//! let name1 = net.peer(1).key_name().to_string();
//! let addr0 = net.peer(0).address().clone();
//! net.peer(1)
//!     .sync()
//!     .sync_with_peer_for_bootstrap_with_key(&addr0, &room, &key1, &name1, Permission::Write(10))
//!     .await?;
//! net.peer_mut(1)
//!     .user_mut()
//!     .track_database(room.clone(), &key1, SyncSettings::disabled())
//!     .await?;
//!
//! net.exchange(1, 0, &room).await?;
//! assert!(net.converged(&[0, 1], &room).await?);
//! # Ok(()) }
//! ```

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    Database, Entry, Instance, NewUser, Result,
    auth::{Permission, crypto::PublicKey, types::AuthKey},
    backend::{BackendImpl, VerificationStatus, database::InMemory},
    clock::{Clock, FixedClock},
    crdt::Doc,
    entry::ID,
    sync::{
        Address, Sync,
        error::SyncError,
        handler::SyncHandler,
        protocol::{RequestContext, SyncRequest, SyncResponse},
        transports::{SyncTransport, TransportBuilder, http::HttpTransport},
    },
    user::{User, types::SyncSettings},
};

/// Display name given to every peer's signing key. Exposed per peer via
/// [`Peer::key_name`] so a test can name it in a bootstrap request.
const KEY_NAME: &str = "test-key";

/// How a peer makes itself reachable to other peers. The one seam Tier 1 swaps:
/// implement this over an in-memory, controllable network and the rest of the
/// harness is unchanged.
#[async_trait]
pub trait TestTransport: Send + std::marker::Sync {
    /// Register this transport on `sync`, start serving, and return the address
    /// other peers use to reach it. Called once per peer at build time.
    async fn serve(&self, sync: &Sync) -> Result<Address>;
}

/// Default [`TestTransport`]: HTTP over an OS-assigned loopback port.
#[derive(Debug, Default, Clone)]
pub struct HttpLoopback;

#[async_trait]
impl TestTransport for HttpLoopback {
    async fn serve(&self, sync: &Sync) -> Result<Address> {
        sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
            .await?;
        sync.accept_connections().await?;
        Ok(Address::http(sync.get_server_address().await?))
    }
}

/// Builder for a [`Cluster`]. Obtain via [`Cluster::builder`].
pub struct ClusterBuilder {
    peers: usize,
    clock: Option<Arc<dyn Clock>>,
    transport: Arc<dyn TestTransport>,
}

impl ClusterBuilder {
    /// Number of peers (independent `Instance`s) to start. Defaults to 2.
    pub fn peers(mut self, n: usize) -> Self {
        self.peers = n;
        self
    }

    /// Share a single clock across every peer (e.g. a [`FixedClock`] the test
    /// drives by hand). When unset, each peer gets its own fresh `FixedClock`,
    /// mirroring the standard `test_instance()` setup.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Use a custom [`TestTransport`] for every peer. Defaults to [`HttpLoopback`].
    pub fn transport(mut self, transport: Arc<dyn TestTransport>) -> Self {
        self.transport = transport;
        self
    }

    /// Build the cluster: for each peer, open an instance, create a user and
    /// signing key, enable sync, and serve over the configured transport.
    pub async fn build(self) -> Result<Cluster> {
        let mut peers = Vec::with_capacity(self.peers);
        for i in 0..self.peers {
            let clock: Arc<dyn Clock> = match &self.clock {
                Some(shared) => shared.clone(),
                None => Arc::new(FixedClock::default()),
            };
            // Create the backend and bootstrap its admin user in one step; each
            // peer is a fresh in-memory instance, so its only user is this one.
            let (instance, mut user) = Instance::create_backend_with_clock(
                Box::new(InMemory::new()),
                clock,
                NewUser::passwordless(format!("peer{i}")),
            )
            .await?;
            let key_id = user.add_private_key(Some(KEY_NAME)).await?;

            instance.enable_sync().await?;
            let sync = instance
                .sync()
                .expect("sync handle present immediately after enable_sync");
            let address = self.transport.serve(&sync).await?;

            peers.push(Peer {
                instance,
                user,
                key_id,
                sync,
                address,
            });
        }
        Ok(Cluster { peers })
    }
}

/// A set of in-process eidetica peers wired for multi-peer sync. Each peer is a
/// full `Instance` with its own backend. The cluster owns the wiring; the test
/// owns the databases, the auth, and the order of operations.
pub struct Cluster {
    peers: Vec<Peer>,
}

impl Cluster {
    /// Start building a cluster. Defaults: 2 peers, per-peer `FixedClock`,
    /// [`HttpLoopback`] transport.
    pub fn builder() -> ClusterBuilder {
        ClusterBuilder {
            peers: 2,
            clock: None,
            transport: Arc::new(HttpLoopback),
        }
    }

    /// Number of peers in the cluster.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Whether the cluster has no peers.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Shared access to a peer (instance, user, key, address).
    pub fn peer(&self, i: usize) -> &Peer {
        &self.peers[i]
    }

    /// Mutable access to a peer, for `create_database` / `open_database` on its
    /// `User`.
    pub fn peer_mut(&mut self, i: usize) -> &mut Peer {
        &mut self.peers[i]
    }

    /// Have peer `to` bootstrap `tree` from peer `from`: request the tree with
    /// `to`'s own key (asking for `permission`), flush, and track it locally
    /// (sync disabled — the test turns on [`serve`]/[`auto_sync`] if it wants
    /// more). The joining-peer dance as one call.
    ///
    /// `permission` is the access level `to` requests; the harness does not pick
    /// it — auth posture stays the test's. `from` must already be serving `tree`
    /// (see [`Peer::serve`]) with a policy that admits this request.
    ///
    /// [`serve`]: Peer::serve
    /// [`auto_sync`]: Cluster::auto_sync
    pub async fn bootstrap(
        &mut self,
        from: usize,
        to: usize,
        tree: &ID,
        permission: Permission,
    ) -> Result<()> {
        let from_addr = self.peers[from].address.clone();
        let to_key = self.peers[to].key_id.clone();
        self.peers[to]
            .sync
            .sync_with_peer_for_bootstrap_with_key(&from_addr, tree, &to_key, KEY_NAME, permission)
            .await?;
        self.peers[to].sync.flush().await?;
        self.peers[to]
            .user
            .track_database(tree.clone(), &to_key, SyncSettings::disabled())
            .await?;
        Ok(())
    }

    /// Drive a sync exchange for `tree`, initiated by peer `from` against peer
    /// `to`. eidetica's `sync_with_peer` exchanges in *both* directions, so after
    /// this both peers hold each other's entries for the tree. Both sync queues
    /// are flushed; flush errors propagate (this is a bug-finding tool — it does
    /// not swallow them).
    pub async fn exchange(&self, from: usize, to: usize, tree: &ID) -> Result<()> {
        let to_addr = self.peers[to].address.clone();
        self.peers[from]
            .sync
            .sync_with_peer(&to_addr, Some(tree))
            .await?;
        self.peers[from].sync.flush().await?;
        self.peers[to].sync.flush().await?;
        Ok(())
    }

    /// Turn on **background, automatic** sync of `tree` between peers `a` and `b`,
    /// in both directions. After this a commit on either peer is queued for the
    /// other automatically (sync-on-commit) — no per-write [`exchange`] call. Use
    /// [`flush`] to push the queue immediately, or let the background interval
    /// carry it.
    ///
    /// Both peers must already hold `tree` (e.g. one [`Peer::serve`]d it and the
    /// other bootstrapped it). This wires the peer relationship both ways
    /// (register peer + dial-back address + per-tree sync target) and re-tracks
    /// the tree as `on_commit` on each side.
    ///
    /// [`exchange`]: Cluster::exchange
    /// [`flush`]: Cluster::flush
    pub async fn auto_sync(&mut self, a: usize, b: usize, tree: &ID) -> Result<()> {
        let a_pub = self.peers[a].sync.get_device_pubkey()?;
        let b_pub = self.peers[b].sync.get_device_pubkey()?;
        let a_addr = self.peers[a].address.clone();
        let b_addr = self.peers[b].address.clone();
        let a_key = self.peers[a].key_id.clone();
        let b_key = self.peers[b].key_id.clone();

        // Each peer learns how to reach the other. A prior bootstrap may already
        // have registered the peer, so registration is idempotent here.
        register_peer_idempotent(&self.peers[a].sync, &b_pub).await?;
        self.peers[a].sync.add_peer_address(&b_pub, b_addr).await?;
        register_peer_idempotent(&self.peers[b].sync, &a_pub).await?;
        self.peers[b].sync.add_peer_address(&a_pub, a_addr).await?;

        // Each peer tracks the tree on-commit and targets the other for it.
        self.peers[a]
            .user
            .track_database(tree.clone(), &a_key, SyncSettings::on_commit())
            .await?;
        self.peers[a].sync.add_tree_sync(&b_pub, tree).await?;
        self.peers[b]
            .user
            .track_database(tree.clone(), &b_key, SyncSettings::on_commit())
            .await?;
        self.peers[b].sync.add_tree_sync(&a_pub, tree).await?;
        Ok(())
    }

    /// Push peer `peer`'s pending auto-sync queue to its targets now, instead of
    /// waiting for the background interval. The deterministic barrier for
    /// auto-sync tests: commit, `flush`, assert.
    pub async fn flush(&self, peer: usize) -> Result<()> {
        self.peers[peer].sync.flush().await
    }

    /// The tip set peer `peer` currently holds for `tree` (sorted). Empty if the
    /// peer has never seen the tree.
    pub async fn tips(&self, peer: usize, tree: &ID) -> Result<Vec<ID>> {
        let mut tips = self.peers[peer]
            .instance
            .backend()
            .snapshot(tree)
            .await?
            .into_tips();
        tips.sort();
        Ok(tips)
    }

    /// True if the named `peers` all agree on `tree`'s tip set — the convergence
    /// invariant. The caller names which peers should have converged; a peer that
    /// never received the tree has empty tips and will not match.
    pub async fn converged(&self, peers: &[usize], tree: &ID) -> Result<bool> {
        let mut reference: Option<Vec<ID>> = None;
        for &i in peers {
            let tips = self.tips(i, tree).await?;
            match &reference {
                None => reference = Some(tips),
                Some(r) if *r != tips => return Ok(false),
                Some(_) => {}
            }
        }
        Ok(true)
    }

    /// Whether *every* peer agrees on `tree`'s tip set — the common convergence
    /// check. Shorthand for [`converged`] over all peers; the explicit
    /// `&[peers]` form stays for partition tests that expect only a subset to
    /// agree.
    ///
    /// [`converged`]: Cluster::converged
    pub async fn converged_all(&self, tree: &ID) -> Result<bool> {
        let all: Vec<usize> = (0..self.peers.len()).collect();
        self.converged(&all, tree).await
    }

    /// Drive bidirectional [`exchange`] across every peer pair, round after
    /// round, until the whole cluster holds an identical tip set for `tree` —
    /// then return `true`. Bounded to `peers` rounds (a complete graph converges
    /// in one, the budget is slack for safety); returns the final convergence
    /// status if the budget is spent without settling.
    ///
    /// Quiescent only: there must be no concurrent writes while this runs (it
    /// has no way to observe them). Every peer must already hold and serve
    /// `tree` so it can answer an exchange — `bootstrap` then [`Peer::serve`] on
    /// each joiner. The fixpoint barrier the N-peer / partition-heal tests
    /// assert against.
    ///
    /// [`exchange`]: Cluster::exchange
    pub async fn converge(&self, tree: &ID) -> Result<bool> {
        let n = self.peers.len();
        for _ in 0..n.max(1) {
            if self.converged_all(tree).await? {
                return Ok(true);
            }
            for i in 0..n {
                for j in (i + 1)..n {
                    self.exchange(i, j, tree).await?;
                }
            }
        }
        self.converged_all(tree).await
    }

    // ===== invariant assertions =====
    //
    // Tip-set equality ([`converged`]) proves two peers *agree*, but it is a weak
    // invariant: it says nothing about *what* they agreed on. Two peers can share
    // a tip set yet differ below it, or converge onto a state that quietly dropped
    // a signed entry, or store a received entry as `Failed`. These walk the full
    // entry set behind the tips and assert the properties tip equality misses.
    // They panic (not return `false`) with a diagnostic — invariant violation is a
    // test failure, and the message should name the offending peer and entry.

    /// The concrete local backend engine for peer `peer`. `Cluster` peers always
    /// run on an in-memory backend, so the off-seam raw reads the invariant checks
    /// need — the full entry dump ([`BackendImpl::get_tree`]) and per-entry
    /// verification status — are always reachable through it.
    fn local_engine(&self, peer: usize) -> Arc<dyn BackendImpl> {
        self.peers[peer]
            .instance
            .backend()
            .local_engine()
            .expect("Cluster peers run on a local in-memory backend")
    }

    /// Every entry peer `peer` holds for `tree`, in id order. The full DAG of the
    /// tree — settings, auth, and every store — not just the tips.
    pub async fn entries(&self, peer: usize, tree: &ID) -> Result<Vec<Entry>> {
        let mut entries = self.local_engine(peer).get_tree(tree).await?;
        entries.sort_by_key(|e| e.id());
        Ok(entries)
    }

    /// The id of every entry peer `peer` holds for `tree`, sorted.
    pub async fn entry_ids(&self, peer: usize, tree: &ID) -> Result<Vec<ID>> {
        Ok(self
            .entries(peer, tree)
            .await?
            .into_iter()
            .map(|e| e.id())
            .collect())
    }

    /// Assert no peer in `peers` is missing an entry another holds for `tree` —
    /// the merge converged onto the *union* of histories, never silently dropping
    /// one peer's signed entry. Stronger than [`converged`], which only compares
    /// tips.
    ///
    /// Limitation: if *every* peer dropped the same entry the union is also short
    /// it, so this can't see that loss — use [`assert_all_present`] with an
    /// externally-known id set for the absolute form.
    ///
    /// [`converged`]: Cluster::converged
    /// [`assert_all_present`]: Cluster::assert_all_present
    pub async fn assert_no_lost_entries(&self, peers: &[usize], tree: &ID) -> Result<()> {
        use std::collections::BTreeSet;
        let mut union: BTreeSet<ID> = BTreeSet::new();
        let mut per_peer: Vec<(usize, BTreeSet<ID>)> = Vec::with_capacity(peers.len());
        for &p in peers {
            let ids: BTreeSet<ID> = self.entry_ids(p, tree).await?.into_iter().collect();
            union.extend(ids.iter().cloned());
            per_peer.push((p, ids));
        }
        for (p, ids) in &per_peer {
            let missing: Vec<&ID> = union.difference(ids).collect();
            assert!(
                missing.is_empty(),
                "peer {p} lost {} entr{} other peers hold for the tree: {missing:?}",
                missing.len(),
                if missing.len() == 1 { "y" } else { "ies" },
            );
        }
        Ok(())
    }

    /// Assert every id in `expected` is present on every peer in `peers`. The
    /// absolute form of [`assert_no_lost_entries`]: the test names entries it knows
    /// were committed (e.g. ids captured from its own writes) and demands they
    /// survive the merge everywhere.
    ///
    /// [`assert_no_lost_entries`]: Cluster::assert_no_lost_entries
    pub async fn assert_all_present(
        &self,
        peers: &[usize],
        tree: &ID,
        expected: &[ID],
    ) -> Result<()> {
        for &p in peers {
            let ids: std::collections::BTreeSet<ID> =
                self.entry_ids(p, tree).await?.into_iter().collect();
            let missing: Vec<&ID> = expected.iter().filter(|id| !ids.contains(id)).collect();
            assert!(
                missing.is_empty(),
                "peer {p} is missing expected entries: {missing:?}",
            );
        }
        Ok(())
    }

    /// Assert every entry peer `peer` holds for `tree` carries a well-formed
    /// signature. A synced CRDT under global auth must never store an unsigned or
    /// malformed-signature entry; this catches one that slipped through.
    pub async fn assert_all_signed(&self, peer: usize, tree: &ID) -> Result<()> {
        for e in self.entries(peer, tree).await? {
            assert!(
                !e.sig.is_unsigned(),
                "peer {peer} holds an unsigned entry: {}",
                e.id(),
            );
            if let Some(reason) = e.sig.malformed_reason() {
                panic!(
                    "peer {peer} holds a malformed-signature entry {}: {reason}",
                    e.id(),
                );
            }
        }
        Ok(())
    }

    /// Assert no entry peer `peer` holds for `tree` is in the `Failed` verification
    /// state — every entry, including those received over sync, verified against
    /// the tree's auth. Stronger than tip equality: a peer can converge on the
    /// right tips while having stored a received entry that does not verify.
    ///
    /// This is only a meaningful convergence invariant once sync runs a per-entry
    /// verification pass that promotes received entries after their signing
    /// context arrives. On a build where sync ingestion records a placeholder
    /// status instead of a real signature check (see the TODO on
    /// [`VerificationStatus`] and `docs/src/design/verification.md`), the stored
    /// status does not reflect verification and this assertion should not be used
    /// — a bootstrapped peer legitimately holds entries marked `Failed` that no
    /// pass has yet promoted. Provided for the harness's forward path: exercise it
    /// once verification-on-ingest is in place.
    ///
    /// [`VerificationStatus`]: crate::backend::VerificationStatus
    pub async fn assert_all_verified(&self, peer: usize, tree: &ID) -> Result<()> {
        let engine = self.local_engine(peer);
        for e in self.entries(peer, tree).await? {
            let status = engine.get_verification_status(&e.id()).await?;
            assert!(
                matches!(status, VerificationStatus::Verified),
                "peer {peer} stored entry {} as {status:?}, expected Verified",
                e.id(),
            );
        }
        Ok(())
    }
}

/// One peer in a [`Cluster`]: a full `Instance` plus the handles a test needs to
/// act as that peer. It does **not** hold the peer's application databases — the
/// test opens and keeps those.
pub struct Peer {
    instance: Instance,
    user: User,
    key_id: PublicKey,
    sync: Arc<Sync>,
    address: Address,
}

impl Peer {
    /// This peer's `Instance`.
    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// This peer's logged-in user session.
    pub fn user(&self) -> &User {
        &self.user
    }

    /// Mutable user session, for `create_database` / `open_database` directly.
    pub fn user_mut(&mut self) -> &mut User {
        &mut self.user
    }

    /// This peer's signing key id (the `SigKey` for its database operations).
    pub fn key_id(&self) -> &PublicKey {
        &self.key_id
    }

    /// The display name of this peer's signing key, for naming it in a bootstrap
    /// request.
    pub fn key_name(&self) -> &str {
        KEY_NAME
    }

    /// This peer's sync handle.
    pub fn sync(&self) -> &Arc<Sync> {
        &self.sync
    }

    /// The address other peers use to reach this peer.
    pub fn address(&self) -> &Address {
        &self.address
    }

    /// Mark `tree` sync-enabled on this peer so its sync handler will serve it to
    /// bootstrapping peers. Delegates to [`User::enable_sync`], which flips the
    /// user's preference and recomputes the host's combined sync state — the same
    /// path a real consumer takes. The database must already be tracked (it is, on
    /// any peer that created it via `create_database` or joined it via
    /// [`Cluster::bootstrap`]). Pure plumbing: set whatever auth the test needs on
    /// the database *before* calling this.
    ///
    /// [`User::enable_sync`]: crate::user::User::enable_sync
    pub async fn serve(&mut self, tree: &ID) -> Result<()> {
        self.user.enable_sync(tree).await
    }
}

/// Register `pubkey` as a peer of `sync`, treating an already-registered peer as
/// success — a prior bootstrap commonly registers it first.
async fn register_peer_idempotent(sync: &Sync, pubkey: &PublicKey) -> Result<()> {
    match sync.register_peer(pubkey, Some("peer")).await {
        Ok(()) => Ok(()),
        Err(crate::Error::Sync(e))
            if matches!(*e, crate::sync::error::SyncError::PeerAlreadyExists(_)) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

// ===== auth tools (policy-neutral: apply the keys the caller passes) =====

/// Apply per-key auth to a database via a settings transaction. The caller
/// chooses the keys and permissions; this just writes them.
pub async fn add_auth_keys(db: &Database, keys: &[(&PublicKey, AuthKey)]) -> Result<()> {
    let txn = db.new_transaction().await?;
    let settings = txn.get_settings()?;
    for (pubkey, key) in keys {
        settings.set_auth_key(pubkey, key.clone()).await?;
    }
    txn.commit().await?;
    Ok(())
}

/// Set the global (wildcard) auth key on a database via a settings transaction.
/// The caller chooses the permission level.
pub async fn set_global_auth_key(db: &Database, key: AuthKey) -> Result<()> {
    let txn = db.new_transaction().await?;
    let settings = txn.get_settings()?;
    settings.set_global_auth_key(key).await?;
    txn.commit().await?;
    Ok(())
}

// ===== SimTransport: in-memory, controllable transport (Tier 1 seam) =====
//
// `HttpLoopback` is real HTTP over loopback: it delivers in wired order, so
// "convergence is order-independent" is unprovable and a partition can only be
// modelled coarsely (don't call `exchange`). `SimTransport` swaps the one seam
// the harness left open — [`TestTransport`] — for an in-process fabric that
// routes a [`SyncRequest`] straight to the target peer's [`SyncHandler`]: no
// sockets, no ports, deterministic, and *controllable*. A test holds a
// [`SimNetwork`] handle and partitions links mid-run.
//
// This is Tier 1 of the harness. It does not replace Tier 0 — it plugs into it:
// `Cluster::builder().transport(Arc::new(SimLoopback::new(net.clone())))`.

/// In-memory message fabric shared by every [`SimTransport`] in a cluster, and
/// the control handle a test uses to inject faults. A drop-in for
/// [`HttpLoopback`] via [`ClusterBuilder::transport`] that additionally lets a
/// test [`partition`] links and [`heal`] them.
///
/// `Clone` is a shared handle (an `Arc` inside): the copy a test keeps and the
/// copies inside each peer's transport all see the same fabric.
///
/// [`partition`]: SimNetwork::partition
/// [`heal`]: SimNetwork::heal
#[derive(Clone, Default)]
pub struct SimNetwork {
    inner: Arc<std::sync::Mutex<SimState>>,
}

#[derive(Default)]
struct SimState {
    /// Peer address -> that peer's serving handler, populated when it serves.
    handlers: std::collections::HashMap<String, Arc<dyn SyncHandler>>,
    /// Directed links currently dropping traffic: `(from_addr, to_addr)`.
    blocked: std::collections::HashSet<(String, String)>,
    /// Monotonic id source for peer addresses.
    next_id: usize,
}

impl SimNetwork {
    /// A fresh, empty fabric.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SimState> {
        self.inner.lock().expect("SimNetwork mutex poisoned")
    }

    /// Hand out the next unique peer address (`sim-peer-N`, in serve order).
    fn alloc_address(&self) -> String {
        let mut s = self.lock();
        let id = s.next_id;
        s.next_id += 1;
        format!("sim-peer-{id}")
    }

    fn register(&self, address: &str, handler: Arc<dyn SyncHandler>) {
        self.lock().handlers.insert(address.to_string(), handler);
    }

    fn unregister(&self, address: &str) {
        self.lock().handlers.remove(address);
    }

    /// Clone out the handler for `address` (drops the lock before any await).
    fn handler_for(&self, address: &str) -> Option<Arc<dyn SyncHandler>> {
        self.lock().handlers.get(address).cloned()
    }

    fn is_blocked(&self, from: &str, to: &str) -> bool {
        self.lock()
            .blocked
            .contains(&(from.to_string(), to.to_string()))
    }

    /// Drop all traffic between `a` and `b` in *both* directions until [`heal`].
    /// A send across a blocked link fails as a connection error, so an
    /// auto-sync peer's queued entries stay pending and redeliver after heal —
    /// a message-level partition, finer than withholding `exchange` calls.
    ///
    /// [`heal`]: SimNetwork::heal
    pub fn partition(&self, a: &Address, b: &Address) {
        let mut s = self.lock();
        s.blocked.insert((a.address.clone(), b.address.clone()));
        s.blocked.insert((b.address.clone(), a.address.clone()));
    }

    /// Restore traffic between `a` and `b` (both directions).
    pub fn heal(&self, a: &Address, b: &Address) {
        let mut s = self.lock();
        s.blocked.remove(&(a.address.clone(), b.address.clone()));
        s.blocked.remove(&(b.address.clone(), a.address.clone()));
    }

    /// Restore every link in the fabric.
    pub fn heal_all(&self) {
        self.lock().blocked.clear();
    }
}

/// [`TestTransport`] backed by a [`SimNetwork`]: an in-memory drop-in for
/// [`HttpLoopback`]. Build a cluster over it with
/// `Cluster::builder().transport(Arc::new(SimLoopback::new(net.clone())))` and
/// keep `net` to drive partitions.
pub struct SimLoopback {
    network: SimNetwork,
}

impl SimLoopback {
    /// Wrap a [`SimNetwork`]. Share one network across the cluster (clone the
    /// handle) so the test and every peer route through the same fabric.
    pub fn new(network: SimNetwork) -> Self {
        Self { network }
    }
}

#[async_trait]
impl TestTransport for SimLoopback {
    async fn serve(&self, sync: &Sync) -> Result<Address> {
        let address = self.network.alloc_address();
        sync.register_transport(
            "sim",
            SimTransportBuilder {
                address: address.clone(),
                network: self.network.clone(),
            },
        )
        .await?;
        // accept_connections awaits StartServer, which calls start_server and
        // registers our handler before returning — no post-serve race.
        sync.accept_connections().await?;
        Ok(Address::new(SimTransport::TRANSPORT_TYPE, address))
    }
}

/// Builder that hands the peer's address + shared fabric to its [`SimTransport`].
struct SimTransportBuilder {
    address: String,
    network: SimNetwork,
}

#[async_trait]
impl TransportBuilder for SimTransportBuilder {
    type Transport = SimTransport;

    async fn build(self, _persisted: Doc) -> Result<(Self::Transport, Option<Doc>)> {
        Ok((
            SimTransport {
                address: self.address,
                network: self.network,
                running: false,
            },
            None,
        ))
    }
}

/// In-memory [`SyncTransport`]. Routes a [`SyncRequest`] straight to the target
/// peer's [`SyncHandler`] through the shared [`SimNetwork`] — no sockets, no
/// serialization — and honors the network's partition state.
pub struct SimTransport {
    /// This peer's own sim address (the key its handler is registered under).
    address: String,
    network: SimNetwork,
    running: bool,
}

impl SimTransport {
    const TRANSPORT_TYPE: &'static str = "sim";
}

#[async_trait]
impl SyncTransport for SimTransport {
    fn transport_type(&self) -> &'static str {
        Self::TRANSPORT_TYPE
    }

    fn can_handle_address(&self, address: &Address) -> bool {
        address.transport_type == Self::TRANSPORT_TYPE
    }

    async fn start_server(&mut self, handler: Arc<dyn SyncHandler>) -> Result<()> {
        self.network.register(&self.address, handler);
        self.running = true;
        Ok(())
    }

    async fn stop_server(&mut self) -> Result<()> {
        self.network.unregister(&self.address);
        self.running = false;
        Ok(())
    }

    async fn send_request(&self, address: &Address, request: &SyncRequest) -> Result<SyncResponse> {
        if !self.can_handle_address(address) {
            return Err(SyncError::UnsupportedTransport {
                transport_type: address.transport_type.clone(),
            }
            .into());
        }
        // A partitioned link looks like a connection failure to the sender; the
        // background sync layer keeps the entries queued for a later flush.
        if self.network.is_blocked(&self.address, &address.address) {
            return Err(SyncError::ConnectionFailed {
                address: address.address.clone(),
                reason: "sim link partitioned".to_string(),
            }
            .into());
        }
        let handler = self.network.handler_for(&address.address).ok_or_else(|| {
            SyncError::ConnectionFailed {
                address: address.address.clone(),
                reason: "no sim peer serving this address".to_string(),
            }
        })?;

        // Mirror the HTTP transport's context: only SyncTree carries a pubkey.
        let peer_pubkey = match request {
            SyncRequest::SyncTree(r) => r.peer_pubkey.clone(),
            _ => None,
        };
        let context = RequestContext {
            remote_address: Some(Address::new(Self::TRANSPORT_TYPE, self.address.clone())),
            peer_pubkey,
        };
        Ok(handler.handle_request(request, &context).await)
    }

    fn is_server_running(&self) -> bool {
        self.running
    }

    fn get_server_address(&self) -> Result<String> {
        if self.running {
            Ok(self.address.clone())
        } else {
            Err(SyncError::ServerNotRunning.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crdt::Doc, store::DocStore};

    async fn write(db: &Database, key: &str, value: &str) -> Result<()> {
        let tx = db.new_transaction().await?;
        tx.get_store::<DocStore>("data")
            .await?
            .set_string(key, value)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn read(db: &Database, key: &str) -> Result<String> {
        let tx = db.new_transaction().await?;
        tx.get_store::<DocStore>("data")
            .await?
            .get_string(key)
            .await
    }

    /// Peer 0 creates a database (auth chosen by the test) and serves it; it
    /// writes `a`; peer 1 bootstraps and opens it. Returns the room id and both
    /// peers' open handles — the shared starting point for the tests below.
    async fn shared_room(net: &mut Cluster) -> Result<(ID, Database, Database)> {
        let key0 = net.peer(0).key_id().clone();
        let device0 = net.peer(0).instance().id();
        let mut settings = Doc::new();
        settings.set("name", "chat");
        let db0 = net
            .peer_mut(0)
            .user_mut()
            .create_database(settings, &key0)
            .await?;
        let room = db0.root_id().clone();

        add_auth_keys(
            &db0,
            &[
                (&key0, AuthKey::active(Some("admin"), Permission::Admin(10))),
                (
                    &device0,
                    AuthKey::active(Some("device"), Permission::Admin(10)),
                ),
            ],
        )
        .await?;
        set_global_auth_key(&db0, AuthKey::active(None, Permission::Admin(10))).await?;
        net.peer_mut(0).serve(&room).await?;
        write(&db0, "a", "from-peer-0").await?;

        net.bootstrap(0, 1, &room, Permission::Write(10)).await?;
        let db1 = net.peer_mut(1).user_mut().open_database(&room).await?;
        assert_eq!(
            read(&db1, "a").await?,
            "from-peer-0",
            "bootstrap carries the write"
        );
        Ok((room, db0, db1))
    }

    /// Manual mode: peer 1 writes, an explicit `exchange` brings peer 0 up to
    /// date, and both converge. Sync is fully ordered by the test.
    #[tokio::test]
    async fn exchange_round_trips_and_converges() -> Result<()> {
        let mut net = Cluster::builder().peers(2).build().await?;
        let (room, db0, db1) = shared_room(&mut net).await?;

        write(&db1, "b", "from-peer-1").await?;
        net.exchange(1, 0, &room).await?;

        assert_eq!(read(&db0, "a").await?, "from-peer-0");
        assert_eq!(read(&db0, "b").await?, "from-peer-1");
        assert!(net.converged(&[0, 1], &room).await?);
        Ok(())
    }

    /// Background mode: after `auto_sync`, commits propagate on their own — no
    /// `exchange` per write. `flush` is the only barrier the test needs.
    #[tokio::test]
    async fn auto_sync_propagates_on_commit() -> Result<()> {
        let mut net = Cluster::builder().peers(2).build().await?;
        let (room, db0, db1) = shared_room(&mut net).await?;

        net.auto_sync(0, 1, &room).await?;

        // Peer 0 commits; no exchange call — auto-sync carries it.
        write(&db0, "c", "auto-from-0").await?;
        net.flush(0).await?;
        assert_eq!(read(&db1, "c").await?, "auto-from-0");

        // And the reverse direction.
        write(&db1, "d", "auto-from-1").await?;
        net.flush(1).await?;
        assert_eq!(read(&db0, "d").await?, "auto-from-1");

        assert!(net.converged(&[0, 1], &room).await?);
        Ok(())
    }

    /// [`SimTransport`] is a drop-in for [`HttpLoopback`]: the same bootstrap +
    /// `exchange` flow converges over the in-memory fabric, no sockets involved.
    #[tokio::test]
    async fn sim_transport_is_a_drop_in() -> Result<()> {
        let mut net = Cluster::builder()
            .peers(2)
            .transport(Arc::new(SimLoopback::new(SimNetwork::new())))
            .build()
            .await?;
        let (room, db0, db1) = shared_room(&mut net).await?;

        write(&db1, "b", "from-peer-1").await?;
        net.exchange(1, 0, &room).await?;

        assert_eq!(read(&db0, "a").await?, "from-peer-0");
        assert_eq!(read(&db0, "b").await?, "from-peer-1");
        assert!(net.converged(&[0, 1], &room).await?);
        Ok(())
    }

    /// A partition drops traffic at the message level: with `auto_sync` wired,
    /// a commit on peer 0 cannot reach peer 1 while the link is cut, then
    /// redelivers from the still-pending queue once the link heals. This is what
    /// `HttpLoopback` can't do — there, withholding `exchange` is the only
    /// partition, and it can't model "wired but not delivering".
    #[tokio::test]
    async fn sim_partition_blocks_then_heal_delivers() -> Result<()> {
        let fabric = SimNetwork::new();
        let mut net = Cluster::builder()
            .peers(2)
            .transport(Arc::new(SimLoopback::new(fabric.clone())))
            .build()
            .await?;
        let (room, db0, db1) = shared_room(&mut net).await?;
        let addr0 = net.peer(0).address().clone();
        let addr1 = net.peer(1).address().clone();

        net.auto_sync(0, 1, &room).await?;

        // Cut the link, then commit on peer 0. The flush attempt cannot reach
        // peer 1 (a connection error to the sender), so the entry stays queued.
        fabric.partition(&addr0, &addr1);
        write(&db0, "c", "during-partition").await?;
        let _ = net.flush(0).await; // send fails across the cut; entry remains queued

        assert!(
            read(&db1, "c").await.is_err(),
            "peer 1 must not see the write while partitioned"
        );
        assert!(
            !net.converged(&[0, 1], &room).await?,
            "peers must diverge under partition"
        );

        // Heal and flush again: the queued entry now reaches peer 1.
        fabric.heal(&addr0, &addr1);
        net.flush(0).await?;

        assert_eq!(read(&db1, "c").await?, "during-partition");
        assert!(net.converged(&[0, 1], &room).await?);
        Ok(())
    }
}
