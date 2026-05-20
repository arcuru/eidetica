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

use std::collections::HashSet;
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
use crate::service::error::service_error_to_eidetica_error;
use crate::service::protocol::{
    AuthenticatedDbRequest, AuthenticatedRequest, BackendOp, DatabaseOp, Handshake, HandshakeAck,
    MergeState, ReadScope, ServiceRequest, ServiceResponse, TransactionContext, WireCrdtValue,
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
    /// Pubkeys this client has already proven possession of on this
    /// connection (via `SessionKeyChallenge`/`SessionKeyRegister`), plus the
    /// login pubkey added in `trusted_login`. Lets `register_session_key`
    /// short-circuit when the key has already been registered, avoiding
    /// per-request wire chatter for the common case where a single per-DB
    /// key is reused across many ops.
    registered_keys: Mutex<HashSet<PublicKey>>,
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
                registered_keys: Mutex::new(HashSet::new()),
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
                *self.inner.session.write().unwrap() = Some(SessionState {
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
    /// Used by every `Database` handle whose `RemoteDatabaseOps` carries a
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
    pub(crate) async fn register_session_key(
        &self,
        signing_key: &PrivateKey,
    ) -> crate::Result<()> {
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

    // === Backend operations (retained) ===

    /// Build a `SigKey` from the session pubkey, when logged in.
    pub fn session_identity(&self) -> Option<SigKey> {
        self.inner
            .session
            .read()
            .unwrap()
            .as_ref()
            .map(|s| SigKey::from_pubkey(&s.session_pubkey))
    }

    pub async fn get(&self, id: &ID) -> crate::Result<Entry> {
        let identity = self.session_identity().unwrap_or_default();
        let resp = self
            .request_ok(ServiceRequest::Authenticated(Box::new(
                AuthenticatedRequest {
                    root_id: ID::default(),
                    identity,
                    request: BackendOp::Get { id: id.clone() },
                },
            )))
            .await?;
        Self::expect_entry(resp)
    }

    pub async fn get_instance_metadata(&self) -> crate::Result<Option<InstanceMetadata>> {
        let resp = self.request_ok(ServiceRequest::GetInstanceMetadata).await?;
        match resp {
            ServiceResponse::InstanceMetadata(meta) => Ok(meta),
            other => Err(unexpected_response("InstanceMetadata", &other)),
        }
    }

    pub async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> crate::Result<()> {
        let identity = self.session_identity().unwrap_or_default();
        let resp = self
            .request_ok(ServiceRequest::Authenticated(Box::new(
                AuthenticatedRequest {
                    root_id: ID::default(),
                    identity,
                    request: BackendOp::SetInstanceMetadata {
                        metadata: metadata.clone(),
                    },
                },
            )))
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
}

fn unexpected_response(expected: &str, actual: &ServiceResponse) -> crate::Error {
    crate::Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Expected {expected} response, got {actual:?}"),
    ))
}
