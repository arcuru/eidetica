//! Remote connection client for the Eidetica service.
//!
//! `RemoteConnection` connects to an Eidetica service server and forwards
//! storage operations as RPC calls. It backs the `RemoteBackend` implementation
//! of the `Backend` seam and is not itself a `BackendImpl`.
//!
//! Authentication uses the client-side-signing flow described in the Service
//! Architecture doc § Security Model: `RemoteConnection::trusted_login` drives
//! the daemon's `TrustedLoginAccount` / `TrustedLoginProve` challenge-response,
//! decrypts the user's root signing key in-process, and signs the challenge
//! locally. The daemon never sees the password or the plaintext signing key.
//! After login, subsequent backend operations travel inside the `Authenticated`
//! envelope and are dispatched against the user's identity; the daemon gates
//! each one per-tree against the target database's auth settings.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};

use tokio::net::{
    UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
};
#[cfg(test)]
use tokio::sync::mpsc;
use tokio::sync::{Mutex, oneshot};

use crate::account::AccountError;
use crate::account::crypto::{decrypt_private_key, derive_encryption_key};
use crate::account::types::{AccountInfo, KeyStorage};
use crate::auth::crypto::PrivateKey;
use crate::auth::crypto::{PublicKey, create_challenge_response};
use crate::auth::types::SigKey;
use crate::backend::{
    InstanceMetadata, RecordMutations, RecordPage, RecordRange, StoreStateRequest,
};
use crate::entry::{Entry, ID};
use crate::instance::{CallbackId, WeakInstance};
use crate::service::error::service_error_to_eidetica_error;
#[cfg(test)]
use crate::service::protocol::ServerFrame;
use crate::service::protocol::{
    AuthenticatedDbRequest, DatabaseOp, Handshake, HandshakeAck, IncomingServerFrame, MergeState,
    Notification, PROTOCOL_REVISION, PROTOCOL_VERSION, ReadScope, ServiceRequest, ServiceResponse,
    TicketBootstrapOutcome, TicketBootstrapRequest, TransactionContext, WireCrdtValue, read_frame,
    write_frame,
};
use crate::snapshot::Snapshot;

/// Grace period before sending cancellation for a dropped registration.
/// A replacement always has a distinct ID and its own ordered initial Snapshot.
const IDLE_GRACE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// How often the sweep task wakes up to check for expired `Idle`
/// entries. Half the grace window so an entry that becomes Idle right
/// after a sweep tick still gets unsubscribed within roughly one grace
/// window's worth of clock time.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Returns the idle grace window.
///
/// Production callers get [`IDLE_GRACE_WINDOW`]. Under the `testing`
/// feature, [`set_idle_grace_window_for_test`] can install an override
/// so the lazy-unsubscribe sweep path is exercisable without waiting
/// the full production grace window.
fn idle_grace_window() -> std::time::Duration {
    #[cfg(feature = "testing")]
    {
        if let Some(d) = TEST_IDLE_GRACE_WINDOW.get() {
            return *d;
        }
    }
    IDLE_GRACE_WINDOW
}

/// Returns the sweep interval.
///
/// Production callers get [`SWEEP_INTERVAL`]. Under the `testing`
/// feature, [`set_sweep_interval_for_test`] can install an override
/// so the sweep fires promptly in test suites.
fn sweep_interval() -> std::time::Duration {
    #[cfg(feature = "testing")]
    {
        if let Some(d) = TEST_SWEEP_INTERVAL.get() {
            return *d;
        }
    }
    SWEEP_INTERVAL
}

#[cfg(feature = "testing")]
static TEST_IDLE_GRACE_WINDOW: std::sync::OnceLock<std::time::Duration> =
    std::sync::OnceLock::new();

#[cfg(feature = "testing")]
static TEST_SWEEP_INTERVAL: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();

/// Override the idle grace window for tests.
///
/// Call once, before any connection that spawns the sweep task. The
/// value is process-global (a `OnceLock`); subsequent calls are
/// no-ops.
#[cfg(feature = "testing")]
pub fn set_idle_grace_window_for_test(d: std::time::Duration) {
    let _ = TEST_IDLE_GRACE_WINDOW.set(d);
}

/// Override the sweep interval for tests.
///
/// Call once, before any connection that spawns the sweep task. The
/// value is process-global (a `OnceLock`); subsequent calls are
/// no-ops.
#[cfg(feature = "testing")]
pub fn set_sweep_interval_for_test(d: std::time::Duration) {
    let _ = TEST_SWEEP_INTERVAL.set(d);
}

/// Per-connection session state, populated by `trusted_login` on success.
///
/// Holds only the public key the daemon verified during challenge-response.
/// The plaintext signing key is intentionally **not** stored here — it lives
/// in the `AccountSession::key_manager` session that owns this connection. The
/// daemon-side `ConnectionState::Authenticated` is what carries the
/// `login_id` (for chunk 6's cache scoping); the client doesn't need it.
#[derive(Clone, Debug)]
struct SessionState {
    session_pubkey: PublicKey,
}

/// A distinct wire registration. Cancellation is lazily sent by the sweep;
/// IDs are never reused, so an old unsubscribe cannot cancel a new watch.
struct RemoteSubscription {
    tree: ID,
    identity: SigKey,
    idle_since: Option<std::time::Instant>,
}

/// Internal state for a remote connection, wrapped in Arc for Clone.
struct RemoteConnectionInner {
    /// Owns the write half of the socket. `tokio::sync::Mutex` because
    /// `write_frame` is async (held across awaits). Only ever held for the
    /// duration of one frame's write plus the FIFO push into [`Self::pending`];
    /// the await on the response itself happens *after* the lock is released
    /// so concurrent callers don't serialise on read-side latency.
    writer: Mutex<Option<OwnedWriteHalf>>,
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
    subscriptions: std::sync::Mutex<HashMap<CallbackId, RemoteSubscription>>,
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
    /// Reader task cancellation, used when the connection is retired.
    reader_abort: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
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

    /// Acquire the pending-queue lock, tolerating poisoning.
    fn pending_lock(
        &self,
    ) -> std::sync::MutexGuard<'_, VecDeque<oneshot::Sender<ServiceResponse>>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mark this connection dead and drop all dependent state. Used by
    /// the reader task on its own exit path and by the sweep when an
    /// `UnsubscribeWrites` times out (a daemon that can't ack a trivial
    /// hashmap removal in seconds is broken; tear down and let the
    /// caller reconnect).
    ///
    /// 1. Mark `closed` with `Release` ordering so future `request()`
    ///    calls short-circuit before pushing senders into an orphan
    ///    queue.
    /// 2. Drain `pending` — every awaiting caller sees `RecvError`
    ///    and surfaces `ConnectionAborted`.
    /// 3. Cancel attached registrations and drop retained subscription state.
    /// 4. Abort the reader task, including when a sweep retires the connection.
    fn mark_dead(&self) {
        self.closed.store(true, Ordering::Release);
        self.pending_lock().clear();
        let subscriptions =
            std::mem::take(&mut *self.subscriptions.lock().unwrap_or_else(|p| p.into_inner()));
        let instance = self
            .weak_instance
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .and_then(|w| w.upgrade());
        if let Some(instance) = instance {
            for (id, sub) in subscriptions {
                instance.remove_write_callback(&sub.tree, id);
            }
        }
        if let Some(handle) = self
            .reader_abort
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            handle.abort();
        }
    }
}

/// A connection to a remote Eidetica service server over a Unix domain socket.
///
/// `RemoteConnection` backs the `RemoteBackend` implementation of the `Backend`
/// seam. It provides the storage operations as inherent methods, plus additional
/// coordination methods like `notify_entry_written`.
///
/// Cloning is cheap (Arc-backed).
///
/// **Teardown.** The reader task holds a strong `Arc<RemoteConnectionInner>`
/// and `inner` owns the socket's `WriteHalf`, so neither half of the split
/// stream can drop on its own: the reader parks in `read_frame` until the
/// daemon EOFs, and the daemon only EOFs once our socket closes. The
/// `live` token breaks that cycle — when the last user-facing handle drops
/// it calls `RemoteConnectionInner::mark_dead`, aborting the reader so
/// its `Arc` is released, `inner` drops, and the `WriteHalf` closes. The
/// daemon then sees EOF and scrubs the connection's subscriptions via its
/// `ConnectionGuard`.
#[derive(Clone)]
pub struct RemoteConnection {
    inner: Arc<RemoteConnectionInner>,
    /// `Some` on every user-facing handle; `None` on internal handles
    /// minted from a task that must not keep the connection alive (the
    /// sweep). Cloning a user handle clones the token, so teardown fires
    /// only when the last one goes.
    _live: Option<Arc<ConnLiveness>>,
}

/// Drop token wired to `RemoteConnection::_live`. Holds a strong `inner`
/// so `mark_dead` is always callable; that `Arc` is released immediately
/// after, as part of this drop.
struct ConnLiveness(Arc<RemoteConnectionInner>);

/// An interrupted frame cannot share its connection with another request.
/// Dropping an owned write half shuts down the socket's write side immediately.
struct FrameWriteGuard<'a> {
    inner: &'a RemoteConnectionInner,
    writer: &'a mut Option<OwnedWriteHalf>,
    complete: bool,
}

impl Drop for FrameWriteGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.writer.take();
            self.inner.mark_dead();
        }
    }
}

impl Drop for ConnLiveness {
    fn drop(&mut self) {
        self.0.mark_dead();
    }
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
    /// task that demuxes [`ServerFrame`](crate::service::protocol::ServerFrame)s:
    /// `Response` frames pop the next pending oneshot in FIFO order,
    /// `Notification` frames dispatch into
    /// the attached `Instance`'s callback registry (after
    /// [`Self::attach_instance`] has been called).
    pub async fn connect(path: impl AsRef<Path>) -> crate::Result<Self> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        let (mut reader, mut writer) = stream.into_split();

        // Send handshake
        let handshake = Handshake {
            protocol_version: PROTOCOL_VERSION,
            protocol_revision: crate::service::protocol::PROTOCOL_REVISION,
        };
        write_frame(&mut writer, &handshake).await?;

        // Read ack
        let ack: HandshakeAck = read_frame(&mut reader).await?.ok_or_else(|| {
            crate::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "Server closed connection during handshake",
            ))
        })?;

        if ack.protocol_version != PROTOCOL_VERSION || ack.protocol_revision != PROTOCOL_REVISION {
            return Err(crate::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Protocol version mismatch: client={}.{}, server={}.{}",
                    PROTOCOL_VERSION,
                    PROTOCOL_REVISION,
                    ack.protocol_version,
                    ack.protocol_revision
                ),
            )));
        }

        let inner = Arc::new(RemoteConnectionInner {
            writer: Mutex::new(Some(writer)),
            pending: std::sync::Mutex::new(VecDeque::new()),
            weak_instance: std::sync::Mutex::new(None),
            session: RwLock::new(None),
            registered_keys: Mutex::new(HashSet::new()),
            subscriptions: std::sync::Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            reader_abort: std::sync::Mutex::new(None),
        });

        // Spawn the reader task. It holds an Arc clone of `inner` so the
        // connection (and its pending queue) stay live as long as any
        // request is in flight, and exits cleanly on EOF / read error /
        // failure-to-deserialize, or on the abort the `live` token fires
        // when the last user handle drops. On exit it drops the remaining oneshot
        // senders (surfaces as `RecvError` on awaiting `request()`s) and
        // also drops every per-tree worker channel, which causes those
        // workers to exit. No separate dispatch task — per-tree workers
        // are spawned lazily by the reader on first notification per
        // tree.
        let inner_for_reader = inner.clone();
        let reader_handle = tokio::spawn(run_reader_task(reader, inner_for_reader));
        *inner.reader_abort.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(reader_handle.abort_handle());

        // Spawn the lazy-unsubscribe sweep. It holds a `Weak<inner>` so
        // it doesn't extend `inner`'s lifetime; exits when `weak.upgrade()`
        // returns `None` (the connection is being torn down).
        let weak_for_sweep = Arc::downgrade(&inner);
        tokio::spawn(run_sweep_task(weak_for_sweep));

        let _live = Some(Arc::new(ConnLiveness(inner.clone())));
        Ok(Self { inner, _live })
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
    /// Check `closed` before locking, under the lock, and after enqueueing.
    /// An interrupted or failed write shuts down the write half and clears all
    /// pending responses. Dropping only the response wait leaves its FIFO slot
    /// for the reader to consume, so subsequent responses keep their association.
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
            // Cancellation/error while writing retires the whole connection:
            // a partial frame cannot be completed by a subsequent request.
            let mut frame = FrameWriteGuard {
                inner: &self.inner,
                writer: &mut writer,
                complete: false,
            };
            self.inner.pending_lock().push_back(tx);
            // Reader teardown publishes closed before draining pending. The
            // slot must exist before this last check or it can miss the drain.
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(connection_aborted());
            }
            write_frame(frame.writer.as_mut().ok_or_else(connection_aborted)?, &req).await?;
            frame.complete = true;
        }
        rx.await.map_err(|_| connection_aborted())
    }

    /// Send a request and convert error responses to `crate::Error`.
    pub(crate) async fn request_ok(&self, req: ServiceRequest) -> crate::Result<ServiceResponse> {
        let resp = self.request(req).await?;
        match resp {
            ServiceResponse::Error(e) => Err(service_error_to_eidetica_error(e)),
            ServiceResponse::Unsupported => Err(crate::Error::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "service operation is not supported by the peer",
            ))),
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
    /// Flow: send `TrustedLoginAccount` → receive challenge + the user's full
    /// `AccountInfo` (encrypted credentials, user-database id, status) → derive
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
    /// build the `AccountSession` session without a second wire read of `_users` —
    /// reads through the wire always travel as the authenticated user, which
    /// with the per-tree gate means a fresh user without permissions on
    /// `_users` would not be able to re-fetch it.
    pub(crate) async fn trusted_login(
        &self,
        username: &str,
        password: Option<&str>,
    ) -> crate::Result<(String, AccountInfo, PrivateKey)> {
        // Step 1: name the user, receive challenge + user record.
        let resp = self
            .request_ok(ServiceRequest::TrustedLoginAccount {
                username: username.to_string(),
            })
            .await?;
        let (challenge, login_id, account_info) = match resp {
            ServiceResponse::TrustedLoginChallenge {
                challenge,
                login_id,
                account_info,
            } => (challenge, login_id, account_info),
            other => return Err(unexpected_response("TrustedLoginChallenge", &other)),
        };

        // Step 2: decrypt the root signing key locally. Cross-check that the
        // caller's password/no-password matches the credential's salt/no-salt;
        // a mismatch is the same UX-level error as a wrong password.
        let credentials = &account_info.credentials;
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
                    AccountError::PasswordRequired {
                        operation: "decrypt root key for remote login".to_string(),
                    }
                })?;
                let kek = derive_encryption_key(pwd, salt)?;
                decrypt_private_key(ciphertext, nonce, &kek)?
            }
            _ => return Err(AccountError::InvalidPassword.into()),
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
                Ok((login_id, account_info, signing_key))
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

    pub(crate) async fn database_ticket(
        &self,
        tree_id: &ID,
        identity: SigKey,
    ) -> crate::Result<crate::sync::DatabaseTicket> {
        let response = self
            .db_request(tree_id.clone(), identity, DatabaseOp::CreateTicket)
            .await?;
        match response {
            ServiceResponse::DatabaseTicket(ticket) => Ok(ticket),
            other => Err(unexpected_response("DatabaseTicket", &other)),
        }
    }

    /// Join a database from `ticket` through the daemon's sync engine.
    ///
    /// The daemon picks the route and sends the request; `signing_key` stays
    /// in this process and signs only the peer-bound request proof. The key is
    /// first registered in the connection's session keyset, which the daemon
    /// requires before acting for it. `now_millis` is the proof timestamp.
    ///
    /// Pending and rejected requests return the same
    /// [`SyncError::BootstrapPending`](crate::sync::SyncError::BootstrapPending)
    /// and [`SyncError::BootstrapRejected`](crate::sync::SyncError::BootstrapRejected)
    /// an embedded [`Sync`](crate::sync::Sync) would.
    pub(crate) async fn bootstrap_ticket(
        &self,
        ticket: &crate::sync::DatabaseTicket,
        signing_key: &PrivateKey,
        requesting_key_name: &str,
        requested_permission: crate::auth::Permission,
        metadata: Option<crate::crdt::Doc>,
        now_millis: u64,
    ) -> crate::Result<()> {
        use crate::sync::{SyncError, protocol::SyncRequestAuth};

        self.register_session_key(signing_key).await?;
        let response = self
            .request_ok(ServiceRequest::TicketBootstrapPrepare {
                ticket: ticket.clone(),
            })
            .await?;
        let (address, peer, tips) = match response {
            ServiceResponse::TicketBootstrapRoute {
                address,
                peer,
                tips,
            } => (address, peer, tips),
            other => return Err(unexpected_response("TicketBootstrapRoute", &other)),
        };
        let auth =
            SyncRequestAuth::sign(signing_key, &peer, ticket.database_id(), &tips, now_millis);
        let response = self
            .request_ok(ServiceRequest::TicketBootstrap(Box::new(
                TicketBootstrapRequest {
                    database_id: ticket.database_id().clone(),
                    address,
                    peer,
                    tips,
                    requesting_key_name: requesting_key_name.to_string(),
                    requested_permission,
                    metadata,
                    auth,
                },
            )))
            .await?;
        match response {
            ServiceResponse::TicketBootstrapOutcome(outcome) => match outcome {
                TicketBootstrapOutcome::Complete => Ok(()),
                TicketBootstrapOutcome::Pending {
                    request_id,
                    message,
                } => Err(SyncError::BootstrapPending {
                    request_id,
                    message,
                }
                .into()),
                TicketBootstrapOutcome::Rejected {
                    request_id,
                    message,
                } => Err(SyncError::BootstrapRejected {
                    request_id,
                    message,
                }
                .into()),
            },
            other => Err(unexpected_response("TicketBootstrapOutcome", &other)),
        }
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

    /// Wait until `mark_dead` sets the closed flag used by `request()`.
    ///
    /// Tests must observe this flag rather than assume EOF has been processed
    /// after a fixed delay. This does not wait for teardown to finish.
    /// Callers must bound the wait with a timeout.
    #[cfg(feature = "testing")]
    pub async fn wait_closed_for_test(&self) {
        while !self.inner.closed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
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

    /// Each local registration has its own wire identity and initial Snapshot.
    pub(crate) async fn subscribe_writes(
        &self,
        id: CallbackId,
        tree: ID,
        identity: SigKey,
        scope: ReadScope,
    ) -> crate::Result<()> {
        self.inner
            .subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                id,
                RemoteSubscription {
                    tree: tree.clone(),
                    identity: identity.clone(),
                    idle_since: None,
                },
            );
        let result = self
            .db_request(
                tree,
                identity,
                DatabaseOp::SubscribeWrites {
                    subscription_id: id.0,
                    scope,
                },
            )
            .await
            .and_then(Self::expect_ok);
        if result.is_err() {
            self.inner
                .subscriptions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
        }
        result
    }

    /// Drop never waits on socket I/O. A distinct ID prevents re-registration
    /// from racing a deferred unsubscribe for an older watch on the same tree.
    pub(crate) fn transition_to_idle(&self, id: CallbackId) {
        if let Some(sub) = self
            .inner
            .subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(&id)
        {
            sub.idle_since = Some(std::time::Instant::now());
        }
    }

    pub(crate) async fn unsubscribe_writes(
        &self,
        id: CallbackId,
        tree: ID,
        identity: SigKey,
    ) -> crate::Result<()> {
        self.db_request(
            tree,
            identity,
            DatabaseOp::UnsubscribeWrites {
                subscription_id: id.0,
            },
        )
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

    /// Fetch the database's Verified-frontier tips (convenience wrapper).
    pub async fn get_verified_tips(
        &self,
        root_id: ID,
        identity: SigKey,
    ) -> crate::Result<Snapshot> {
        self.get_snapshot(root_id, identity, ReadScope::Verified)
            .await
    }

    /// Fetch the selected frontier with this handle's acting identity.
    pub async fn get_snapshot(
        &self,
        root_id: ID,
        identity: SigKey,
        scope: ReadScope,
    ) -> crate::Result<Snapshot> {
        let resp = self
            .db_request(root_id, identity, DatabaseOp::GetSnapshot { scope })
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

    /// Re-run historical authentication on the daemon, under the caller's
    /// per-database identity. No client-supplied status is accepted.
    pub async fn verify_entry_signature(
        &self,
        root_id: ID,
        identity: SigKey,
        id: ID,
    ) -> crate::Result<bool> {
        let resp = self
            .db_request(root_id, identity, DatabaseOp::VerifyEntrySignature { id })
            .await?;
        match resp {
            ServiceResponse::EntrySignatureValid(valid) => Ok(valid),
            other => Err(unexpected_response("EntrySignatureValid", &other)),
        }
    }

    /// Subtree tips reachable from given main-tree entries.
    pub async fn store_snapshot_at(
        &self,
        root_id: ID,
        identity: SigKey,
        store: String,
        up_to: Vec<ID>,
    ) -> crate::Result<Snapshot> {
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

    /// Resolve cached state, returning its opaque server-issued view onto the published record set.
    pub async fn resolve_store_state(
        &self,
        identity: SigKey,
        request: StoreStateRequest,
    ) -> crate::Result<Option<String>> {
        match self
            .db_request(
                request.database.clone(),
                identity,
                DatabaseOp::ResolveStoreState { request },
            )
            .await?
        {
            ServiceResponse::RecordView(view) => Ok(view),
            other => Err(unexpected_response("RecordView", &other)),
        }
    }

    /// Begin a private build, returning its opaque capability.
    pub async fn begin_store_state_staging(
        &self,
        identity: SigKey,
        request: StoreStateRequest,
    ) -> crate::Result<String> {
        match self
            .db_request(
                request.database.clone(),
                identity,
                DatabaseOp::BeginStoreStateStaging { request },
            )
            .await?
        {
            ServiceResponse::Token(token) => Ok(token),
            other => Err(unexpected_response("Token", &other)),
        }
    }

    /// Upload one idempotent chunk of records into the private build.
    pub async fn stage_store_state_records(
        &self,
        root: ID,
        identity: SigKey,
        token: String,
        chunk_id: u64,
        records: RecordMutations,
    ) -> crate::Result<()> {
        self.db_request(
            root,
            identity,
            DatabaseOp::StageStoreStateRecords {
                token,
                chunk_id,
                records: records.into_iter().collect(),
            },
        )
        .await
        .and_then(Self::expect_ok)
    }

    /// Publish the private build and return a view onto the published record set.
    pub async fn publish_store_state(
        &self,
        root: ID,
        identity: SigKey,
        token: String,
    ) -> crate::Result<String> {
        match self
            .db_request(root, identity, DatabaseOp::PublishStoreState { token })
            .await?
        {
            ServiceResponse::RecordView(Some(view)) => Ok(view),
            other => Err(unexpected_response("RecordView", &other)),
        }
    }

    /// Discard an unfinished private build and its bytes.
    pub async fn abort_store_state(
        &self,
        root: ID,
        identity: SigKey,
        token: String,
    ) -> crate::Result<()> {
        self.db_request(root, identity, DatabaseOp::AbortStoreState { token })
            .await
            .and_then(Self::expect_ok)
    }

    /// Read one record through a resolved view.
    pub async fn store_state_record_get(
        &self,
        root: ID,
        identity: SigKey,
        view: String,
        key: Vec<u8>,
    ) -> crate::Result<Option<Vec<u8>>> {
        match self
            .db_request(
                root,
                identity,
                DatabaseOp::StoreStateRecordGet { view, key },
            )
            .await?
        {
            ServiceResponse::Record(record) => Ok(record),
            other => Err(unexpected_response("Record", &other)),
        }
    }

    /// Read one bounded ordered page through a resolved view.
    #[allow(clippy::too_many_arguments)]
    pub async fn store_state_record_scan(
        &self,
        root: ID,
        identity: SigKey,
        view: String,
        range: RecordRange,
        after: Option<Vec<u8>>,
        max_records: u32,
        max_encoded_bytes: u32,
    ) -> crate::Result<RecordPage> {
        match self
            .db_request(
                root,
                identity,
                DatabaseOp::StoreStateRecordScan {
                    view,
                    range,
                    after,
                    max_records,
                    max_encoded_bytes,
                },
            )
            .await?
        {
            ServiceResponse::RecordPage(page) => Ok(page),
            other => Err(unexpected_response("RecordPage", &other)),
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
/// Loops on [`read_frame`] and demuxes by [`IncomingServerFrame`] variant:
///
/// - `Response(r)`: pop the front of the pending FIFO and resolve its
///   oneshot. If the queue is empty something has gone badly wrong
///   (server sent more responses than the client issued requests) — log
///   and continue.
///   Unknown responses still consume exactly one slot as Unsupported; unknown
///   optional notifications consume no slots and invoke no callbacks.
/// - `Notification(Notification::DatabaseWrite { … })`: upgrade the
///   attached `WeakInstance` and route the event into its callback
///   mailbox via `Instance::deliver_snapshot`. If no
///   instance is attached yet or the instance has been dropped, the
///   notification is silently dropped — both are expected end-states,
///   not errors.
///
/// Exit conditions: clean EOF (server closed), any read error, or any
/// deserialisation error. On exit the task drops its `Arc<inner>`, which
/// in turn drops every remaining oneshot sender in `pending`, surfacing
/// as a `RecvError` on each awaiting `request()` (translated to a
/// connection-closed `io::Error` there).
async fn run_reader_task(mut reader: OwnedReadHalf, inner: Arc<RemoteConnectionInner>) {
    loop {
        let frame_result: crate::Result<Option<IncomingServerFrame>> =
            read_frame(&mut reader).await;
        let frame = match frame_result {
            Ok(Some(f)) => f,
            Ok(None) => break, // Clean EOF
            Err(e) => {
                tracing::debug!("RemoteConnection reader error: {e}");
                break;
            }
        };

        match frame {
            IncomingServerFrame::Response(resp) => {
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
            IncomingServerFrame::Notification(Some(notif)) => {
                route_notification(&inner, notif);
            }
            IncomingServerFrame::Notification(None) => {}
        }
    }

    // Mark the connection dead with `Release` ordering paired against
    // `request()`'s `Acquire` load: any post-exit caller that observes
    // `true` is guaranteed to also see the drained `pending` queue and
    // bail with `ConnectionAborted` instead of pushing a sender no one
    // will ever pop. The helper also cancels attached watch registrations.
    inner.mark_dead();
}

/// The socket reader only offers a Snapshot to its exact mailbox; it never
/// invokes or awaits user work. No per-tree transport dispatch queue is needed.
fn route_notification(inner: &Arc<RemoteConnectionInner>, notification: Notification) {
    let instance = inner
        .weak_instance
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .and_then(|w| w.upgrade());
    if let Some(instance) = instance {
        match notification {
            Notification::DatabaseWrite {
                root_id,
                subscription_id,
                snapshot,
            } => instance.deliver_snapshot(&root_id, CallbackId(subscription_id), snapshot),
        }
    }
}

const UNSUBSCRIBE_RTT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn run_sweep_task(weak_inner: Weak<RemoteConnectionInner>) {
    let mut ticker = tokio::time::interval(sweep_interval());
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let Some(inner) = weak_inner.upgrade() else {
            return;
        };
        if inner.closed.load(Ordering::Acquire) {
            return;
        }
        let expired = {
            let mut subscriptions = inner
                .subscriptions
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let now = std::time::Instant::now();
            let ids: Vec<_> = subscriptions
                .iter()
                .filter_map(|(id, sub)| {
                    sub.idle_since
                        .filter(|since| now.duration_since(*since) >= idle_grace_window())
                        .map(|_| *id)
                })
                .collect();
            ids.into_iter()
                .map(|id| (id, subscriptions.remove(&id).unwrap()))
                .collect::<Vec<_>>()
        };
        let conn = RemoteConnection { inner, _live: None };
        for (id, sub) in expired {
            match tokio::time::timeout(
                UNSUBSCRIBE_RTT_TIMEOUT,
                conn.unsubscribe_writes(id, sub.tree, sub.identity),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::debug!(%error, "Unsubscribe failed"),
                Err(_) => {
                    conn.inner.mark_dead();
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod compatibility_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Exercise the real connect/handshake/background-reader path, not a mock FIFO.
    pub(super) async fn connect_peer() -> (RemoteConnection, UnixStream) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peer.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let peer = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handshake: Handshake = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(handshake.protocol_version, 0);
            write_frame(
                &mut stream,
                &HandshakeAck {
                    protocol_version: 0,
                    protocol_revision: crate::service::protocol::PROTOCOL_REVISION,
                },
            )
            .await
            .unwrap();
            stream
        };
        let (conn, peer) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(RemoteConnection::connect(&path), peer)
        })
        .await
        .unwrap();
        (conn.unwrap(), peer)
    }

    pub(super) async fn read_request(peer: &mut UnixStream, label: &str) {
        let request: ServiceRequest =
            tokio::time::timeout(Duration::from_secs(2), read_frame(peer))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert!(
            matches!(request, ServiceRequest::TrustedLoginAccount { username } if username == label)
        );
    }

    /// Build a `RemoteConnection` over a socketpair — enough to exercise the
    /// subscription state machine without a daemon. No reader task is
    /// spawned and no wire traffic is sent; the peer end is returned so the
    /// caller keeps the socket open for the duration of the test.
    pub(super) fn test_conn() -> (RemoteConnection, tokio::net::UnixStream) {
        let (client_side, peer) = tokio::net::UnixStream::pair().unwrap();
        let (_reader, writer) = client_side.into_split();
        let inner = Arc::new(RemoteConnectionInner {
            writer: Mutex::new(Some(writer)),
            pending: std::sync::Mutex::new(VecDeque::new()),
            weak_instance: std::sync::Mutex::new(None),
            session: RwLock::new(None),
            registered_keys: Mutex::new(HashSet::new()),
            subscriptions: std::sync::Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            reader_abort: std::sync::Mutex::new(None),
        });
        (RemoteConnection { inner, _live: None }, peer)
    }
}

#[cfg(test)]
mod frame_cancellation_tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn cancelled_partial_service_frame_retires_connection() {
        let (conn, mut peer) = super::tests::test_conn();
        // Larger than a socket's send buffer, but below the protocol cap.
        let request = ServiceRequest::TrustedLoginAccount {
            username: "x".repeat(2 * 1024 * 1024),
        };
        let mut sending = Box::pin(conn.request(request));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut sending)
                .await
                .is_err()
        );
        let mut prefix = [0; 4];
        peer.read_exact(&mut prefix).await.unwrap();
        let expected = u32::from_be_bytes(prefix) as usize;
        assert!(
            expected > 1024 * 1024,
            "positive control: frame write never began"
        );
        drop(sending);
        assert!(
            conn.inner.closed.load(Ordering::Acquire),
            "cancelled partial frame left a live FIFO connection"
        );
        assert!(
            conn.inner.pending_lock().is_empty(),
            "cancelled frame stranded a waiter"
        );
        assert!(
            conn.request(ServiceRequest::GetInstanceMetadata)
                .await
                .is_err()
        );
        let mut remainder = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut remainder))
            .await
            .expect("retired frame must close the write side")
            .unwrap();
        assert!(
            remainder.len() < expected,
            "test did not cancel a partial frame"
        );
    }

    #[tokio::test]
    async fn cancelled_response_wait_keeps_fifo_association() {
        let (conn, mut peer) = super::tests::connect_peer().await;
        let mut first = Box::pin(conn.request(ServiceRequest::TrustedLoginAccount {
            username: "first".into(),
        }));
        tokio::select! {
            _ = super::tests::read_request(&mut peer, "first") => {},
            result = &mut first => panic!("first response unexpectedly completed: {result:?}"),
        }
        drop(first);
        assert!(
            !conn.inner.closed.load(Ordering::Acquire),
            "dropping a response waiter retired a complete frame"
        );
        let second = conn.request(ServiceRequest::TrustedLoginAccount {
            username: "second".into(),
        });
        let respond = async {
            super::tests::read_request(&mut peer, "second").await;
            for challenge in [vec![1], vec![2]] {
                write_frame(
                    &mut peer,
                    &ServerFrame::Response(Box::new(ServiceResponse::SessionKeyChallenge {
                        challenge,
                    })),
                )
                .await
                .unwrap();
            }
        };
        let (response, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(second, respond)
        })
        .await
        .unwrap();
        let response = response.unwrap();
        assert!(
            matches!(response, ServiceResponse::SessionKeyChallenge { challenge } if challenge == vec![2]),
            "cancelled response was misassociated"
        );
    }

    #[tokio::test]
    async fn failed_frame_write_retires_fifo() {
        let (conn, peer) = super::tests::test_conn();
        drop(peer);
        assert!(
            conn.request(ServiceRequest::GetInstanceMetadata)
                .await
                .is_err()
        );
        assert!(conn.inner.closed.load(Ordering::Acquire));
        assert!(conn.inner.pending_lock().is_empty());
        assert!(
            conn.request(ServiceRequest::GetInstanceMetadata)
                .await
                .is_err()
        );
    }
}
