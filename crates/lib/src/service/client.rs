//! Remote connection client for the Eidetica service.
//!
//! `RemoteConnection` connects to an Eidetica service server and forwards
//! storage operations as RPC calls. It is used as the `Backend::Remote` variant
//! and is not a `BackendImpl` — dispatch happens through the `Backend` enum.
//!
//! Authentication uses the client-side-signing flow described in the Service
//! Architecture doc § Security Model: `RemoteConnection::trusted_login` drives
//! the daemon's `TrustedLoginUser` / `TrustedLoginProve` challenge-response,
//! decrypts the user's root signing key in-process, and signs the challenge
//! locally. The daemon never sees the password or the plaintext signing key.
//! After login, subsequent backend operations travel inside the `Authenticated`
//! envelope and are dispatched against the user's identity; the daemon gates
//! each one per-tree against the target database's auth settings.

use std::path::Path;
use std::sync::{Arc, RwLock};

use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::auth::crypto::PrivateKey;
use crate::auth::crypto::{PublicKey, create_challenge_response};
use crate::auth::types::SigKey;
use crate::backend::InstanceMetadata;
use crate::entry::{Entry, ID};
use crate::instance::WriteSource;
use crate::service::error::service_error_to_eidetica_error;
use crate::service::protocol::{
    AuthenticatedDbRequest, AuthenticatedRequest, BackendOp, DatabaseOp, Handshake, HandshakeAck,
    ReadScope, ServiceRequest, ServiceResponse, TransactionContext, WireCrdtValue,
    PROTOCOL_VERSION, read_frame, write_frame,
};
use crate::user::UserError;
use crate::user::crypto::{decrypt_private_key, derive_encryption_key};
use crate::user::types::{KeyStorage, UserInfo};

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
    /// TODO (before multi-user / untrusted clients): the `read()/write()
    /// .unwrap()` at the use sites panics on poisoning. Blast radius is
    /// limited to this one connection (unlike the daemon-shared cache lock),
    /// but it should still recover the guard or use a non-poisoning lock
    /// rather than abort the connection.
    session: RwLock<Option<SessionState>>,
}

/// A connection to a remote Eidetica service server over a Unix domain socket.
///
/// `RemoteConnection` is used as the `Backend::Remote` variant. It provides
/// the same operations as `BackendImpl` as inherent methods, plus additional
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
            }),
        })
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

    /// Wrap a `BackendOp` in the `Authenticated` envelope and send it.
    ///
    /// Populates `identity` from the connection's session state when logged
    /// in (`SigKey::from_pubkey(&session_pubkey)`); falls back to
    /// `SigKey::default()` pre-login so the resulting request reliably hits
    /// the server's "not authenticated" gate rather than a more confusing
    /// downstream error.
    ///
    /// `root_id` is stamped from `op.tree_id()` when the op carries one. The
    /// server doesn't currently rely on this field — it uses `op.tree_id()`
    /// directly for the chunk-5b gate — but populating it keeps the envelope
    /// honest and gives future per-request audit logs a single source of
    /// truth for "what tree was this op scoped to?".
    async fn backend_request(&self, op: BackendOp) -> crate::Result<ServiceResponse> {
        let identity = match self.inner.session.read().unwrap().as_ref() {
            Some(s) => SigKey::from_pubkey(&s.session_pubkey),
            None => SigKey::default(),
        };
        let root_id = op.tree_id().cloned().unwrap_or_default();
        self.request_ok(ServiceRequest::Authenticated(Box::new(
            AuthenticatedRequest {
                root_id,
                identity,
                request: op,
            },
        )))
        .await
    }

    /// Wrap a `DatabaseOp` in the `AuthenticatedDb` envelope and send it.
    ///
    /// Database-level analogue of [`backend_request`]: same `(root_id,
    /// identity)` scope and the same `request_ok` error conversion, carrying
    /// a `DatabaseOp` in an `AuthenticatedDbRequest`. Unlike `backend_request`,
    /// `root_id` is caller-supplied — DatabaseOps carry no inline tree id,
    /// but every variant is tree-scoped by construction.
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
                *self.inner.session.write().unwrap() = Some(SessionState {
                    session_pubkey: credentials.root_key_id.clone(),
                });
                Ok((user_uuid, user_info, signing_key))
            }
            other => Err(unexpected_response("TrustedLoginOk", &other)),
        }
    }

    // === Response extraction helpers ===

    fn expect_entry(resp: ServiceResponse) -> crate::Result<Entry> {
        match resp {
            ServiceResponse::Entry(e) => Ok(e),
            other => Err(unexpected_response("Entry", &other)),
        }
    }

    fn expect_entries(resp: ServiceResponse) -> crate::Result<Vec<Entry>> {
        match resp {
            ServiceResponse::Entries(e) => Ok(e),
            other => Err(unexpected_response("Entries", &other)),
        }
    }

    fn expect_id(resp: ServiceResponse) -> crate::Result<ID> {
        match resp {
            ServiceResponse::Id(id) => Ok(id),
            other => Err(unexpected_response("Id", &other)),
        }
    }

    fn expect_ids(resp: ServiceResponse) -> crate::Result<Vec<ID>> {
        match resp {
            ServiceResponse::Ids(ids) => Ok(ids),
            other => Err(unexpected_response("Ids", &other)),
        }
    }

    fn expect_ok(resp: ServiceResponse) -> crate::Result<()> {
        match resp {
            ServiceResponse::Ok => Ok(()),
            other => Err(unexpected_response("Ok", &other)),
        }
    }

    // === Storage operations (matching BackendImpl surface) ===

    pub async fn get(&self, id: &ID) -> crate::Result<Entry> {
        let resp = self
            .backend_request(BackendOp::Get { id: id.clone() })
            .await?;
        Self::expect_entry(resp)
    }

    /// Send an entry to the server for storage.
    ///
    /// The wire carries no verification status: a peer cannot assert that an
    /// entry is verified. The server stores all wire-submitted entries as
    /// [`VerificationStatus::Unverified`] and may promote them only via its
    /// own local verification pass.
    pub async fn put(&self, entry: Entry) -> crate::Result<()> {
        let resp = self.backend_request(BackendOp::Put { entry }).await?;
        Self::expect_ok(resp)
    }

    pub async fn get_tips(&self, tree: &ID) -> crate::Result<Vec<ID>> {
        let resp = self
            .backend_request(BackendOp::GetTips { tree: tree.clone() })
            .await?;
        Self::expect_ids(resp)
    }

    pub async fn get_store_tips(&self, tree: &ID, store: &str) -> crate::Result<Vec<ID>> {
        let resp = self
            .backend_request(BackendOp::GetStoreTips {
                tree: tree.clone(),
                store: store.to_string(),
            })
            .await?;
        Self::expect_ids(resp)
    }

    pub async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        main_entries: &[ID],
    ) -> crate::Result<Vec<ID>> {
        let resp = self
            .backend_request(BackendOp::GetStoreTipsUpToEntries {
                tree: tree.clone(),
                store: store.to_string(),
                main_entries: main_entries.to_vec(),
            })
            .await?;
        Self::expect_ids(resp)
    }

    pub async fn find_merge_base(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> crate::Result<ID> {
        let resp = self
            .backend_request(BackendOp::FindMergeBase {
                tree: tree.clone(),
                store: store.to_string(),
                entry_ids: entry_ids.to_vec(),
            })
            .await?;
        Self::expect_id(resp)
    }

    pub async fn get_tree(&self, tree: &ID) -> crate::Result<Vec<Entry>> {
        let resp = self
            .backend_request(BackendOp::GetTree { tree: tree.clone() })
            .await?;
        Self::expect_entries(resp)
    }

    pub async fn get_store(&self, tree: &ID, store: &str) -> crate::Result<Vec<Entry>> {
        let resp = self
            .backend_request(BackendOp::GetStore {
                tree: tree.clone(),
                store: store.to_string(),
            })
            .await?;
        Self::expect_entries(resp)
    }

    pub async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> crate::Result<Vec<Entry>> {
        let resp = self
            .backend_request(BackendOp::GetTreeFromTips {
                tree: tree.clone(),
                tips: tips.to_vec(),
            })
            .await?;
        Self::expect_entries(resp)
    }

    pub async fn get_store_from_tips(
        &self,
        tree: &ID,
        store: &str,
        tips: &[ID],
    ) -> crate::Result<Vec<Entry>> {
        let resp = self
            .backend_request(BackendOp::GetStoreFromTips {
                tree: tree.clone(),
                store: store.to_string(),
                tips: tips.to_vec(),
            })
            .await?;
        Self::expect_entries(resp)
    }

    pub async fn get_cached_crdt_state(
        &self,
        entry_id: &ID,
        store: &str,
    ) -> crate::Result<Option<Vec<u8>>> {
        let resp = self
            .backend_request(BackendOp::GetCachedCrdtState {
                entry_id: entry_id.clone(),
                store: store.to_string(),
            })
            .await?;
        match resp {
            ServiceResponse::CachedCrdtState(state) => Ok(state),
            other => Err(unexpected_response("CachedCrdtState", &other)),
        }
    }

    pub async fn cache_crdt_state(
        &self,
        entry_id: &ID,
        store: &str,
        state: Vec<u8>,
    ) -> crate::Result<()> {
        let resp = self
            .backend_request(BackendOp::CacheCrdtState {
                entry_id: entry_id.clone(),
                store: store.to_string(),
                state,
            })
            .await?;
        Self::expect_ok(resp)
    }

    pub async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> crate::Result<Vec<ID>> {
        let resp = self
            .backend_request(BackendOp::GetPathFromTo {
                tree_id: tree_id.clone(),
                store: store.to_string(),
                from_id: from_id.clone(),
                to_ids: to_ids.to_vec(),
            })
            .await?;
        Self::expect_ids(resp)
    }

    pub async fn get_instance_metadata(&self) -> crate::Result<Option<InstanceMetadata>> {
        let resp = self.request_ok(ServiceRequest::GetInstanceMetadata).await?;
        match resp {
            ServiceResponse::InstanceMetadata(meta) => Ok(meta),
            other => Err(unexpected_response("InstanceMetadata", &other)),
        }
    }

    pub async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> crate::Result<()> {
        let resp = self
            .backend_request(BackendOp::SetInstanceMetadata {
                metadata: metadata.clone(),
            })
            .await?;
        Self::expect_ok(resp)
    }

    // === Write coordination ===

    /// Notify the server that an entry was written, triggering server-side callbacks.
    pub async fn notify_entry_written(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        source: WriteSource,
    ) -> crate::Result<()> {
        let resp = self
            .backend_request(BackendOp::NotifyEntryWritten {
                tree_id: tree_id.clone(),
                entry_id: entry_id.clone(),
                source,
            })
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
            .db_request(root_id, identity, DatabaseOp::BeginTransaction { stores, scope })
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
                DatabaseOp::GetStoreEntries {
                    store,
                    tips,
                    scope,
                },
            )
            .await?;
        match resp {
            ServiceResponse::Entries(entries) => Ok(entries),
            other => Err(unexpected_response("Entries", &other)),
        }
    }

    /// Fetch the database's Verified-frontier tips.
    pub async fn get_verified_tips(
        &self,
        root_id: ID,
        identity: SigKey,
    ) -> crate::Result<Vec<ID>> {
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
            .db_request(root_id, identity, DatabaseOp::SubmitSignedEntry { entry })
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
}

fn unexpected_response(expected: &str, actual: &ServiceResponse) -> crate::Error {
    crate::Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Expected {expected} response, got {actual:?}"),
    ))
}
