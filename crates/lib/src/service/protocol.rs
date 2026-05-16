//! Wire protocol types for the Eidetica service.
//!
//! The protocol uses length-prefixed JSON frames over a Unix domain socket.
//! Each frame is a 4-byte big-endian length followed by the JSON payload.
//!
//! ## Request shape
//!
//! `ServiceRequest` is a flat enum holding pre-authentication lifecycle messages
//! (`TrustedLoginUser`, `TrustedLoginProve`), the pre-auth `GetInstanceMetadata`
//! query, and an `Authenticated` wrapper that carries every storage operation
//! (including any user-management writes against `_users`). The wrapper
//! bundles the `(root_id, identity)` scope so the server can validate each
//! backend op against the connection's session pubkey and the target database's
//! auth settings. Pre-auth verification of the session pubkey happens once at
//! login via a challenge-response handshake.
//!
//! The login lifecycle is **trusted** in the sense that the daemon ships the
//! user's encrypted credentials (salt + AES-GCM ciphertext) to anyone who can
//! connect to the socket and asks for them. This is safe in the local-socket
//! model — filesystem permissions on the socket already bound the caller set
//! to processes that could read the underlying DB files directly. A network
//! transport would need a different shape (PAKE: OPAQUE/SRP) so the server
//! doesn't release the blob until the client proves password knowledge in a
//! way that doesn't leak it. The `TrustedLogin*` naming is a load-bearing
//! reminder of that assumption — see § Trusted login threat model in the
//! Service Architecture doc.
//!
//! Chunk 2 settled the wire shape; chunk 3 wired up real daemon-side
//! challenge-response. The per-request permission gate on `Authenticated`
//! requests lands in a later chunk; for now clients populate
//! `root_id`/`identity` with defaults until the client-side login flow ships.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::auth::types::{Permission, SigKey};
use crate::backend::{InstanceMetadata, VerificationStatus};
use crate::entry::{Entry, ID};
use crate::instance::WriteSource;
use crate::service::error::ServiceError;
use crate::user::UserInfo;

/// Protocol version. Version 0 indicates an unstable protocol that may change
/// without notice between releases.
pub const PROTOCOL_VERSION: u32 = 0;

/// Maximum frame size: 64 MiB.
pub const MAX_FRAME_SIZE: u32 = 64 * 1024 * 1024;

/// Handshake message sent by the client on connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handshake {
    pub protocol_version: u32,
}

/// Handshake acknowledgment sent by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeAck {
    pub protocol_version: u32,
}

/// Backend storage operations that the server dispatches on behalf of an
/// authenticated client.
///
/// This is a deliberately curated **subset** of the `BackendImpl` trait: only
/// the operations a remote client actually needs and that can be authorised
/// against a database's `auth_settings`. Backend-internal primitives
/// (verification-status scans, root-to-target path collection, sorted-parent
/// walks, cache clears) are intentionally *not* on the wire — they remain
/// local-only trait methods. Plus `NotifyEntryWritten` for write-coordination
/// via `Instance::dispatch_write_callbacks`. `BackendOp` is always carried
/// inside `ServiceRequest::Authenticated` so the server has the
/// `(root_id, identity)` scope it needs to authorise the op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendOp {
    // === Entry operations ===
    Get {
        id: ID,
    },
    Put {
        verification_status: VerificationStatus,
        entry: Entry,
    },

    // === Tips ===
    GetTips {
        tree: ID,
    },
    GetStoreTips {
        tree: ID,
        store: String,
    },
    GetStoreTipsUpToEntries {
        tree: ID,
        store: String,
        main_entries: Vec<ID>,
    },

    // === Tree/Store traversal ===
    FindMergeBase {
        tree: ID,
        store: String,
        entry_ids: Vec<ID>,
    },
    GetTree {
        tree: ID,
    },
    GetStore {
        tree: ID,
        store: String,
    },
    GetTreeFromTips {
        tree: ID,
        tips: Vec<ID>,
    },
    GetStoreFromTips {
        tree: ID,
        store: String,
        tips: Vec<ID>,
    },

    // === CRDT cache ===
    GetCachedCrdtState {
        entry_id: ID,
        store: String,
    },
    CacheCrdtState {
        entry_id: ID,
        store: String,
        state: Vec<u8>,
    },

    // === Path operations ===
    GetPathFromTo {
        tree_id: ID,
        store: String,
        from_id: ID,
        to_ids: Vec<ID>,
    },

    // === Instance metadata (write side) ===
    SetInstanceMetadata {
        metadata: InstanceMetadata,
    },

    // === Write coordination ===
    NotifyEntryWritten {
        tree_id: ID,
        entry_id: ID,
        source: WriteSource,
    },
}

impl BackendOp {
    /// Returns the tree this op targets, when it carries one inline.
    ///
    /// Used by the server to load `auth_settings` for permission resolution and
    /// by the client to stamp `AuthenticatedRequest::root_id` so the daemon's
    /// gate has the same scope the op operates on.
    ///
    /// Returns `None` for:
    /// - Ops keyed by entry id alone (`Get`, `Put`, `GetCachedCrdtState`,
    ///   `CacheCrdtState`) — resolving the entry's tree requires an extra
    ///   backend read, so the gate can't run here in `dispatch_inner`.
    ///   `Get` is instead gated *post-fetch* on the entry's resolved owning
    ///   tree (D2, `gate_entry_read` in `server.rs`). `Put` per-tree gating
    ///   plus server-side verification is the deferred D1 work (A3/P0). The
    ///   cache ops are per-user namespaced server-side.
    /// - Cross-tree ops with no inherent scope (`SetInstanceMetadata`, which
    ///   carries its own explicit `Admin`-on-`_databases` gate).
    pub fn tree_id(&self) -> Option<&ID> {
        match self {
            BackendOp::GetTips { tree }
            | BackendOp::GetStoreTips { tree, .. }
            | BackendOp::GetStoreTipsUpToEntries { tree, .. }
            | BackendOp::FindMergeBase { tree, .. }
            | BackendOp::GetTree { tree }
            | BackendOp::GetStore { tree, .. }
            | BackendOp::GetTreeFromTips { tree, .. }
            | BackendOp::GetStoreFromTips { tree, .. } => Some(tree),

            BackendOp::GetPathFromTo { tree_id, .. }
            | BackendOp::NotifyEntryWritten { tree_id, .. } => Some(tree_id),

            BackendOp::Get { .. }
            | BackendOp::Put { .. }
            | BackendOp::GetCachedCrdtState { .. }
            | BackendOp::CacheCrdtState { .. }
            | BackendOp::SetInstanceMetadata { .. } => None,
        }
    }

    /// Minimum permission level required to dispatch this op against a tree.
    ///
    /// Read traversal ops require `Read`; anything that mutates tree state
    /// (entries, verification status, CRDT cache, write callbacks) requires
    /// `Write`. The priority value carried in `Write(_)` is *not significant*
    /// here — callers should match on the variant (or compare via
    /// `Permission::can_write` / `can_admin`) rather than ordering, since the
    /// op doesn't know what priority the user actually has.
    ///
    /// Only consulted when `tree_id().is_some()`; cross-tree ops fall through
    /// the per-tree gate.
    pub fn required_permission(&self) -> Permission {
        match self {
            BackendOp::Put { .. }
            | BackendOp::CacheCrdtState { .. }
            | BackendOp::NotifyEntryWritten { .. } => Permission::Write(0),
            _ => Permission::Read,
        }
    }
}

/// Payload of an `Authenticated` service request.
///
/// Bundles the database scope (`root_id`) and identity claim (`identity`) with
/// the backend op the client wants to run. Carried boxed inside
/// `ServiceRequest::Authenticated` to keep the top-level enum's stack footprint
/// flat — both `SigKey` and `BackendOp::Put` are large.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticatedRequest {
    /// Root entry of the database this op targets. Used by the server to look
    /// up auth settings for permission resolution.
    pub root_id: ID,
    /// Identity claim for the op. The server verifies this against the
    /// connection's session pubkey before dispatching the inner request.
    pub identity: SigKey,
    /// Backend operation to execute.
    pub request: BackendOp,
}

/// Top-level request from client to server.
///
/// The shape is intentionally flat: pre-auth lifecycle and queries sit beside
/// the `Authenticated` wrapper rather than under a nested enum. This makes the
/// pre-auth surface visible at a glance and keeps the server's dispatch
/// branches symmetric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceRequest {
    // === Pre-auth: trusted login handshake ===
    /// Step 1 of the trusted login flow. Client names a user; server responds
    /// with a `TrustedLoginChallenge` carrying random bytes the client must
    /// sign. The "Trusted" qualifier is a load-bearing reminder that this flow
    /// assumes the caller is already trusted by the socket's filesystem
    /// permissions — over a network transport this would need PAKE instead.
    TrustedLoginUser { username: String },
    /// Step 2 of the trusted login flow. Client returns a signature over the
    /// challenge from `TrustedLoginUser`, computed with the user's root key.
    /// Server verifies against the stored pubkey and, on success, marks the
    /// connection authenticated.
    TrustedLoginProve { signature: Vec<u8> },

    // === Pre-auth: queries safe before login ===
    /// Fetch the server's instance metadata (including device id). Used by
    /// `Instance::connect` during the handshake to establish server identity.
    GetInstanceMetadata,

    // === Authenticated wrapper for every backend operation ===
    /// All backend storage ops travel inside this wrapper. The inner
    /// `AuthenticatedRequest` carries `(root_id, identity, request)` and is
    /// boxed to keep the enum's discriminated size compact.
    Authenticated(Box<AuthenticatedRequest>),
}

/// Response from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceResponse {
    /// Single entry
    Entry(Entry),
    /// Multiple entries
    Entries(Vec<Entry>),
    /// Single ID
    Id(ID),
    /// Multiple IDs
    Ids(Vec<ID>),
    /// Success with no data
    Ok,
    /// Optional cached CRDT state
    CachedCrdtState(Option<Vec<u8>>),
    /// Optional instance metadata
    InstanceMetadata(Option<InstanceMetadata>),
    /// Error response
    Error(ServiceError),
    /// Challenge bytes returned in response to `TrustedLoginUser`, plus the
    /// user's full record so the client can derive the password→key, decrypt
    /// the root signing key locally, sign the challenge in a single
    /// round-trip, and then build the `User` session from data the daemon
    /// already returned — no second wire read of `_users` is required.
    ///
    /// `user_info.credentials` carries the (encrypted) root private key, its
    /// `KeyStorage` envelope (algorithm/ciphertext/nonce for password-protected
    /// users, raw `PrivateKey` for passwordless users), and the Argon2id salt
    /// when password-protected. The non-credential fields (user_database_id,
    /// status, timestamps) are what `User::new` consumes after the proof
    /// step succeeds. See § Trusted login threat model in the Service
    /// Architecture doc for why this is safe to ship to anyone who can
    /// reach the socket.
    TrustedLoginChallenge {
        challenge: Vec<u8>,
        user_uuid: String,
        user_info: UserInfo,
    },
    /// Trusted login succeeded; the connection is now authenticated.
    TrustedLoginOk,
}

/// Write a length-prefixed JSON frame to an async writer.
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> crate::Result<()> {
    let payload = serde_json::to_vec(value)?;
    let len = payload.len() as u32;
    if len > MAX_FRAME_SIZE {
        return Err(crate::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame too large: {len} bytes (max {MAX_FRAME_SIZE})"),
        )));
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Read a length-prefixed JSON frame from an async reader.
///
/// Returns `None` on clean EOF (connection closed).
pub async fn read_frame<R: AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> crate::Result<Option<T>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_SIZE {
        return Err(crate::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame too large: {len} bytes (max {MAX_FRAME_SIZE})"),
        )));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).await?;
    let value = serde_json::from_slice(&payload)?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to make a simple entry for testing
    fn test_id() -> ID {
        ID::from_bytes("test-entry-id")
    }

    fn wrap(op: BackendOp) -> ServiceRequest {
        ServiceRequest::Authenticated(Box::new(AuthenticatedRequest {
            root_id: ID::default(),
            identity: SigKey::default(),
            request: op,
        }))
    }

    /// Extract the inner `BackendOp` from a deserialised request, panicking if
    /// the variant isn't `Authenticated`.
    fn unwrap_op(req: ServiceRequest) -> BackendOp {
        match req {
            ServiceRequest::Authenticated(inner) => inner.request,
            other => panic!("expected Authenticated, got {other:?}"),
        }
    }

    #[test]
    fn test_handshake_serde() {
        let h = Handshake {
            protocol_version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&h).unwrap();
        let h2: Handshake = serde_json::from_str(&json).unwrap();
        assert_eq!(h2.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn test_handshake_ack_serde() {
        let h = HandshakeAck {
            protocol_version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&h).unwrap();
        let h2: HandshakeAck = serde_json::from_str(&json).unwrap();
        assert_eq!(h2.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn test_request_get_serde() {
        let req = wrap(BackendOp::Get { id: test_id() });
        let json = serde_json::to_string(&req).unwrap();
        let req2: ServiceRequest = serde_json::from_str(&json).unwrap();
        match unwrap_op(req2) {
            BackendOp::Get { id } => assert_eq!(id, test_id()),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_request_get_tips_serde() {
        let req = wrap(BackendOp::GetTips { tree: test_id() });
        let json = serde_json::to_string(&req).unwrap();
        let req2: ServiceRequest = serde_json::from_str(&json).unwrap();
        match unwrap_op(req2) {
            BackendOp::GetTips { tree } => assert_eq!(tree, test_id()),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_request_get_instance_metadata_serde() {
        let req = ServiceRequest::GetInstanceMetadata;
        let json = serde_json::to_string(&req).unwrap();
        let req2: ServiceRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(req2, ServiceRequest::GetInstanceMetadata));
    }

    #[test]
    fn test_request_notify_entry_written_serde() {
        let req = wrap(BackendOp::NotifyEntryWritten {
            tree_id: test_id(),
            entry_id: ID::from_bytes("entry-1"),
            source: WriteSource::Local,
        });
        let json = serde_json::to_string(&req).unwrap();
        let req2: ServiceRequest = serde_json::from_str(&json).unwrap();
        match unwrap_op(req2) {
            BackendOp::NotifyEntryWritten {
                tree_id,
                entry_id,
                source,
            } => {
                assert_eq!(tree_id, test_id());
                assert_eq!(entry_id, ID::from_bytes("entry-1"));
                assert_eq!(source, WriteSource::Local);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_request_trusted_login_user_serde() {
        let req = ServiceRequest::TrustedLoginUser {
            username: "alice".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let req2: ServiceRequest = serde_json::from_str(&json).unwrap();
        match req2 {
            ServiceRequest::TrustedLoginUser { username } => assert_eq!(username, "alice"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_request_trusted_login_prove_serde() {
        let req = ServiceRequest::TrustedLoginProve {
            signature: b"sig-bytes".to_vec(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let req2: ServiceRequest = serde_json::from_str(&json).unwrap();
        match req2 {
            ServiceRequest::TrustedLoginProve { signature } => assert_eq!(signature, b"sig-bytes"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_response_ok_serde() {
        let resp = ServiceResponse::Ok;
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: ServiceResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(resp2, ServiceResponse::Ok));
    }

    #[test]
    fn test_response_ids_serde() {
        let resp = ServiceResponse::Ids(vec![test_id(), ID::from_bytes("other")]);
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: ServiceResponse = serde_json::from_str(&json).unwrap();
        match resp2 {
            ServiceResponse::Ids(ids) => {
                assert_eq!(ids.len(), 2);
                assert_eq!(ids[0], test_id());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_response_error_serde() {
        let se = ServiceError {
            module: "backend".to_string(),
            kind: "EntryNotFound".to_string(),
            message: "Entry not found: abc".to_string(),
        };
        let resp = ServiceResponse::Error(se);
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: ServiceResponse = serde_json::from_str(&json).unwrap();
        match resp2 {
            ServiceResponse::Error(e) => {
                assert_eq!(e.module, "backend");
                assert_eq!(e.kind, "EntryNotFound");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_response_cached_crdt_state_serde() {
        let resp = ServiceResponse::CachedCrdtState(Some(b"state-data".to_vec()));
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: ServiceResponse = serde_json::from_str(&json).unwrap();
        match resp2 {
            ServiceResponse::CachedCrdtState(Some(s)) => assert_eq!(s, b"state-data"),
            _ => panic!("wrong variant"),
        }

        let resp_none = ServiceResponse::CachedCrdtState(None);
        let json_none = serde_json::to_string(&resp_none).unwrap();
        let resp_none2: ServiceResponse = serde_json::from_str(&json_none).unwrap();
        assert!(matches!(resp_none2, ServiceResponse::CachedCrdtState(None)));
    }

    #[test]
    fn test_response_instance_metadata_none_serde() {
        let resp = ServiceResponse::InstanceMetadata(None);
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: ServiceResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(resp2, ServiceResponse::InstanceMetadata(None)));
    }

    #[test]
    fn test_response_trusted_login_challenge_serde() {
        use crate::auth::crypto::generate_keypair;
        use crate::user::{KeyStorage, UserCredentials, UserInfo, UserStatus};

        let (_signing, pubkey) = generate_keypair();
        let user_info = UserInfo {
            username: "alice".to_string(),
            user_database_id: ID::from_bytes("alice-db"),
            credentials: UserCredentials {
                root_key_id: pubkey.clone(),
                root_key: KeyStorage::Encrypted {
                    algorithm: "aes-256-gcm".to_string(),
                    ciphertext: b"ct".to_vec(),
                    nonce: b"123456789012".to_vec(),
                },
                password_salt: Some("salt-string".to_string()),
            },
            created_at: 1_700_000_000,
            status: UserStatus::Active,
        };

        let resp = ServiceResponse::TrustedLoginChallenge {
            challenge: b"random-bytes".to_vec(),
            user_uuid: "uuid-alice".to_string(),
            user_info: user_info.clone(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: ServiceResponse = serde_json::from_str(&json).unwrap();
        match resp2 {
            ServiceResponse::TrustedLoginChallenge {
                challenge,
                user_uuid,
                user_info: ui2,
            } => {
                assert_eq!(challenge, b"random-bytes");
                assert_eq!(user_uuid, "uuid-alice");
                assert_eq!(ui2.username, user_info.username);
                assert_eq!(ui2.user_database_id, user_info.user_database_id);
                assert_eq!(ui2.credentials.root_key_id, pubkey);
                assert_eq!(
                    ui2.credentials.password_salt.as_deref(),
                    Some("salt-string")
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_response_trusted_login_ok_serde() {
        let resp = ServiceResponse::TrustedLoginOk;
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: ServiceResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(resp2, ServiceResponse::TrustedLoginOk));
    }

    #[tokio::test]
    async fn test_frame_roundtrip() {
        // duplex gives two ends: writing to `client` is readable from `server` and vice versa
        let (client, server) = tokio::io::duplex(4096);
        let (mut server_read, _server_write) = tokio::io::split(server);
        let (_client_read, mut client_write) = tokio::io::split(client);

        let req = wrap(BackendOp::Get { id: test_id() });

        let write_handle = tokio::spawn(async move {
            write_frame(&mut client_write, &req).await.unwrap();
        });

        let read_result: Option<ServiceRequest> = read_frame(&mut server_read).await.unwrap();
        write_handle.await.unwrap();

        let result = read_result.unwrap();
        match unwrap_op(result) {
            BackendOp::Get { id } => assert_eq!(id, test_id()),
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn test_frame_eof_returns_none() {
        // Use a real Unix socket pair for proper EOF semantics
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("eof-test.sock");
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let client = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (server_stream, _) = listener.accept().await.unwrap();

        // Drop the server stream to close the connection
        drop(server_stream);

        let (mut reader, _writer) = tokio::io::split(client);
        let result: crate::Result<Option<ServiceRequest>> = read_frame(&mut reader).await;
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_frame_max_size_rejection_on_write() {
        let (client, _server) = tokio::io::duplex(1024);
        let (_read, mut write) = tokio::io::split(client);

        // Create a payload that's too large
        let huge_string = "x".repeat(MAX_FRAME_SIZE as usize + 1);
        let result = write_frame(&mut write, &huge_string).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_frame_max_size_rejection_on_read() {
        let (client, server) = tokio::io::duplex(1024 * 1024);
        let (mut client_read, _client_write) = tokio::io::split(client);
        let (_server_read, mut server_write) = tokio::io::split(server);

        // Write a fake frame header with size > MAX_FRAME_SIZE from the server end
        let fake_len = MAX_FRAME_SIZE + 1;
        tokio::spawn(async move {
            server_write
                .write_all(&fake_len.to_be_bytes())
                .await
                .unwrap();
        });

        let result: crate::Result<Option<ServiceRequest>> = read_frame(&mut client_read).await;
        assert!(result.is_err());
    }
}
