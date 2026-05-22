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

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, RwLock};

use lru::LruCache;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::auth::crypto::PrivateKey;
use crate::auth::crypto::{PublicKey, create_challenge_response};
use crate::auth::types::SigKey;
use crate::backend::InstanceMetadata;
use crate::entry::{Entry, ID};
use crate::service::error::service_error_to_eidetica_error;
use crate::service::protocol::{
    AuthenticatedDbRequest, DatabaseOp, Handshake, HandshakeAck, MergeState, PROTOCOL_VERSION,
    ReadScope, ServiceRequest, ServiceResponse, TransactionContext, WireCrdtValue, read_frame,
    write_frame,
};
use crate::user::UserError;
use crate::user::crypto::{decrypt_private_key, derive_encryption_key};
use crate::user::types::{KeyStorage, UserInfo};

/// Default cap on the client-side CRDT-state LRU. Matches `MAX_FRAME_SIZE`
/// (64 MiB) so a single oversized cached blob can still ride the wire.
const CLIENT_CACHE_CAPACITY_BYTES: usize = 64 * 1024 * 1024;

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

/// Internal state for a remote connection, wrapped in Arc for Clone.
struct RemoteConnectionInner {
    stream: Mutex<(ReadHalf<UnixStream>, WriteHalf<UnixStream>)>,
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
    /// Performs the protocol handshake and returns a connection ready for use.
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

        Ok(Self {
            inner: Arc::new(RemoteConnectionInner {
                stream: Mutex::new((reader, writer)),
                session: RwLock::new(None),
                registered_keys: Mutex::new(HashSet::new()),
                crdt_cache: std::sync::Mutex::new(ClientCrdtCache::new(
                    CLIENT_CACHE_CAPACITY_BYTES,
                )),
            }),
        })
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

    /// Send a request and read the response.
    async fn request(&self, req: ServiceRequest) -> crate::Result<ServiceResponse> {
        let mut stream = self.inner.stream.lock().await;
        let (ref mut reader, ref mut writer) = *stream;
        write_frame(writer, &req).await?;
        let resp: ServiceResponse = read_frame(reader).await?.ok_or_else(|| {
            crate::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "Server closed connection unexpectedly",
            ))
        })?;
        Ok(resp)
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
