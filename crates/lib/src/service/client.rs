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
//! envelope and are dispatched against the user's identity (see chunk 5 of
//! Branch C for the per-request permission gate).

use std::path::Path;
use std::sync::Arc;

use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::auth::crypto::create_challenge_response;
use crate::auth::types::SigKey;
use crate::backend::{InstanceMetadata, VerificationStatus};
use crate::entry::{Entry, ID};
use crate::instance::WriteSource;
use crate::service::error::service_error_to_eidetica_error;
use crate::service::protocol::{
    AuthenticatedRequest, BackendOp, Handshake, HandshakeAck, PROTOCOL_VERSION, ServiceRequest,
    ServiceResponse, read_frame, write_frame,
};
use crate::user::UserError;
use crate::user::crypto::{decrypt_private_key, derive_encryption_key};
use crate::user::types::KeyStorage;

/// Internal state for a remote connection, wrapped in Arc for Clone.
struct RemoteConnectionInner {
    stream: Mutex<(ReadHalf<UnixStream>, WriteHalf<UnixStream>)>,
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
    /// `root_id` and `identity` are still placeholders pending the chunk-5
    /// gate work, which will both require the connection to be in
    /// `Authenticated` state on the server side and validate the claimed
    /// identity against the session pubkey. Until then the server accepts
    /// any identity on this envelope and dispatches the inner op.
    async fn backend_request(&self, op: BackendOp) -> crate::Result<ServiceResponse> {
        self.request_ok(ServiceRequest::Authenticated(Box::new(
            AuthenticatedRequest {
                root_id: ID::default(),
                identity: SigKey::default(),
                request: op,
            },
        )))
        .await
    }

    /// Authenticate this connection as `username` by completing the
    /// `TrustedLogin*` handshake against the daemon.
    ///
    /// Flow: send `TrustedLoginUser` → receive challenge + encrypted credentials
    /// → derive the password-encryption key locally (Argon2id) and decrypt the
    /// root signing key in-process (or take it raw for passwordless users) →
    /// sign the challenge → send `TrustedLoginProve` → expect `TrustedLoginOk`.
    ///
    /// The daemon never sees the password or the plaintext signing key; the
    /// trust model for shipping the encrypted blob over the socket is captured
    /// in the Service Architecture doc § Trusted login threat model.
    ///
    /// On success the connection's server-side state is `Authenticated`, and
    /// subsequent `Authenticated`-wrapped backend ops are dispatched (chunk 5
    /// will additionally validate the per-request identity claim).
    pub async fn trusted_login(&self, username: &str, password: Option<&str>) -> crate::Result<()> {
        // Step 1: name the user, receive challenge + encrypted credentials.
        let resp = self
            .request_ok(ServiceRequest::TrustedLoginUser {
                username: username.to_string(),
            })
            .await?;
        let (challenge, credentials) = match resp {
            ServiceResponse::TrustedLoginChallenge {
                challenge,
                credentials,
            } => (challenge, credentials),
            other => return Err(unexpected_response("TrustedLoginChallenge", &other)),
        };

        // Step 2: decrypt the root signing key locally. Cross-check that the
        // caller's password/no-password matches the credential's salt/no-salt;
        // a mismatch is the same UX-level error as a wrong password.
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
            ServiceResponse::TrustedLoginOk => Ok(()),
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

    pub async fn get_verification_status(&self, id: &ID) -> crate::Result<VerificationStatus> {
        let resp = self
            .backend_request(BackendOp::GetVerificationStatus { id: id.clone() })
            .await?;
        match resp {
            ServiceResponse::VerificationStatus(vs) => Ok(vs),
            other => Err(unexpected_response("VerificationStatus", &other)),
        }
    }

    pub async fn put(
        &self,
        verification_status: VerificationStatus,
        entry: Entry,
    ) -> crate::Result<()> {
        let resp = self
            .backend_request(BackendOp::Put {
                verification_status,
                entry,
            })
            .await?;
        Self::expect_ok(resp)
    }

    pub async fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> crate::Result<()> {
        let resp = self
            .backend_request(BackendOp::UpdateVerificationStatus {
                id: id.clone(),
                verification_status,
            })
            .await?;
        Self::expect_ok(resp)
    }

    pub async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> crate::Result<Vec<ID>> {
        let resp = self
            .backend_request(BackendOp::GetEntriesByVerificationStatus { status })
            .await?;
        Self::expect_ids(resp)
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

    pub async fn all_roots(&self) -> crate::Result<Vec<ID>> {
        let resp = self.backend_request(BackendOp::AllRoots).await?;
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

    pub async fn collect_root_to_target(
        &self,
        tree: &ID,
        store: &str,
        target_entry: &ID,
    ) -> crate::Result<Vec<ID>> {
        let resp = self
            .backend_request(BackendOp::CollectRootToTarget {
                tree: tree.clone(),
                store: store.to_string(),
                target_entry: target_entry.clone(),
            })
            .await?;
        Self::expect_ids(resp)
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

    pub async fn clear_crdt_cache(&self) -> crate::Result<()> {
        let resp = self.backend_request(BackendOp::ClearCrdtCache).await?;
        Self::expect_ok(resp)
    }

    pub async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> crate::Result<Vec<ID>> {
        let resp = self
            .backend_request(BackendOp::GetSortedStoreParents {
                tree_id: tree_id.clone(),
                entry_id: entry_id.clone(),
                store: store.to_string(),
            })
            .await?;
        Self::expect_ids(resp)
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

    // === User management RPCs ===

    pub async fn create_user(
        &self,
        username: &str,
        password: Option<&str>,
    ) -> crate::Result<String> {
        let resp = self
            .request_ok(ServiceRequest::CreateUser {
                username: username.to_string(),
                password: password.map(|s| s.to_string()),
            })
            .await?;
        match resp {
            ServiceResponse::UserCreated(uuid) => Ok(uuid),
            other => Err(unexpected_response("UserCreated", &other)),
        }
    }

    pub async fn list_users(&self) -> crate::Result<Vec<String>> {
        let resp = self.request_ok(ServiceRequest::ListUsers).await?;
        match resp {
            ServiceResponse::Users(users) => Ok(users),
            other => Err(unexpected_response("Users", &other)),
        }
    }
}

fn unexpected_response(expected: &str, actual: &ServiceResponse) -> crate::Error {
    crate::Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Expected {expected} response, got {actual:?}"),
    ))
}
