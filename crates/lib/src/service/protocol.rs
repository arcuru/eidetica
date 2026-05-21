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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::auth::crypto::PublicKey;
use crate::auth::types::{Permission, SigKey};
use crate::backend::InstanceMetadata;
use crate::entry::{Entry, ID};
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
/// This minimal surface retains only `Get` (entry fetch by id) and
/// `SetInstanceMetadata` (admin-gated instance config writes). All other
/// read/write paths have moved to [`DatabaseOp`] via the
/// [`AuthenticatedDbRequest`] envelope. `BackendOp` is always carried
/// inside `ServiceRequest::Authenticated` so the server has the
/// `(root_id, identity)` scope it needs to authorise the op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendOp {
    /// Fetch a single entry by id. Gated post-fetch by the entry's owning tree.
    Get { id: ID },
    /// Rewrite the daemon's instance metadata (system-DB pointers).
    /// Gated by `Admin` on `_databases`. Boxed to keep the enum's stack
    /// footprint small — `InstanceMetadata` dominates the variant size.
    SetInstanceMetadata { metadata: Box<InstanceMetadata> },
}

impl BackendOp {
    /// Returns `None` for every variant: `Get` carries no inline tree id,
    /// and `SetInstanceMetadata` targets a daemon-global system tree
    /// (`_databases`) rather than a caller-named one — gated explicitly in
    /// the server dispatch.
    pub fn tree_id(&self) -> Option<&ID> {
        match self {
            BackendOp::Get { .. } | BackendOp::SetInstanceMetadata { .. } => None,
        }
    }

    /// `SetInstanceMetadata` requires `Admin` (gated against `_databases`).
    /// `Get` is `Read` (gated post-fetch on the entry's owning tree).
    pub fn required_permission(&self) -> Permission {
        match self {
            BackendOp::SetInstanceMetadata { .. } => Permission::Admin(0),
            BackendOp::Get { .. } => Permission::Read,
        }
    }
}

// ===========================================================================
// Database-level wire API (additive, parallel to `BackendOp`).
//
// `BackendOp` mirrors the storage trait, so the whole verification/CRDT/auth
// stack runs client-side over raw storage primitives — the wire sits *below*
// the layer where the security model lives. The ops below instead let the
// server run the `Database` layer on its local instance: verify-on-read and
// the Verified frontier become server-side by construction, and every op is
// intrinsically (tree, store, identity)-scoped. Carried in
// `ServiceRequest::AuthenticatedDb`; the legacy `BackendOp`/`Authenticated`
// path is untouched and removed only in the final cutover.
// ===========================================================================

/// Which projection of the DAG an op observes. Mirrors the `Database`
/// read posture: a write's parent tips are the tips of the *same* projection
/// the caller reads (see the Verification Model design doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReadScope {
    /// Default-safe: only the maximal all-`Verified` ancestor-closed prefix.
    #[default]
    Verified,
    /// Also include `Unverified` entries (`Failed` always dropped). The
    /// caller explicitly opted in via `Database::allow_unverified()`.
    AllowUnverified,
}

/// A CRDT store's materialized state on the wire. Concrete `Store<T>` typing
/// stays client-side sugar over this; the cache path already ships
/// `serde_json` bytes today, so this introduces no new representation.
pub type WireCrdtValue = serde_json::Value;

/// Everything a client needs to build **and sign** an entry locally without
/// further round-trips. The client owns its keys, so signing stays
/// client-side; only the inputs `Transaction::commit` reads from storage
/// before signing travel here. Heights accompany each parent so the client
/// computes entry height without a follow-up `GetEntry` per parent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionContext {
    /// Main-tree parent tips with their heights, in the caller's `scope`.
    pub main_parents: Vec<(ID, u64)>,
    /// Per-store parent tips (with heights) reachable from `main_parents`.
    pub subtree_parents: BTreeMap<String, Vec<(ID, u64)>>,
    /// `_settings` tips this transaction pins in signed metadata.
    pub settings_tips: Vec<ID>,
    /// Merged `_settings` state the entry is authored against (used to build
    /// the auth settings the signature is validated under).
    pub settings_value: WireCrdtValue,
}

/// Response for ComputeMergeState: lowest common ancestor + path to tips.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeState {
    pub merge_base: ID,
    pub path: Vec<ID>,
}

/// Database-level operations the server runs on its local `Database`.
///
/// The target database (`root_id`) and identity claim travel in
/// [`AuthenticatedDbRequest`], exactly as `BackendOp` rides
/// [`AuthenticatedRequest`]; gates are reused unchanged (Read for begin/get*,
/// Write for submit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DatabaseOp {
    /// Acquire everything needed to build+sign a transaction locally for the
    /// given stores, with parents drawn from `scope`'s projection. Gate Read.
    BeginTransaction {
        stores: Vec<String>,
        scope: ReadScope,
    },
    /// Submit a finished, client-signed entry. The server stores it
    /// `Unverified` and runs its **own** verification pass — it never trusts
    /// a submitted entry's claimed validity. Submit is *verification-gated,
    /// not session-gated*: it requires only an authenticated connection, and
    /// the per-tree permission gate is **not** applied (the server's
    /// verification pass against the tree's pinned auth is the boundary). The
    /// `required_permission()` value below is advisory only for this variant.
    SubmitSignedEntry { entry: Box<Entry> },
    /// The database's Verified-frontier tips (server runs `Database::get_tips`
    /// on its local instance). Gate Read.
    GetVerifiedTips,
    /// Server-materialized merged state of an **unencrypted** store, against
    /// the server's own Verified frontier. Gate Read.
    GetStoreState { store: String },
    /// Ordered (by subtree height), verified, opaque store entries reachable
    /// from `tips` in `scope` — the universal primitive, incl. encrypted
    /// stores (client decrypts+merges locally). Gate Read.
    GetStoreEntries {
        store: String,
        tips: Vec<ID>,
        scope: ReadScope,
    },
    /// Subtree tips reachable from given main-tree entry IDs.
    /// Used by Transaction internals to discover store entries.
    GetStoreTipsUpToEntries { store: String, up_to: Vec<ID> },

    /// Lowest common ancestor + path to tip entries in a store DAG.
    /// Fused to one RPC: the only caller always calls find_merge_base
    /// then get_path_from_to in sequence.
    ComputeMergeState { store: String, entry_ids: Vec<ID> },

    /// Fetch a single entry by id (gated post-fetch by its owning tree). Gate
    /// Read.
    GetEntry { id: ID },

    /// Look up a cached materialized CRDT state. Server returns the previously
    /// `CacheCrdtState`-submitted blob for `(session user, root_id, key, store)`,
    /// or `None` on miss. Gate Read.
    ///
    /// Used by [`crate::database::ops::RemoteDatabaseOps::get_cached_crdt_state`]
    /// as the second tier of a two-level cache: the client first checks its own
    /// per-connection LRU, then falls back to this RPC. The daemon's cache is
    /// the cross-session source of truth.
    GetCachedCrdtState { store: String, key: ID },

    /// Stash a client-computed materialized CRDT state for `(session user,
    /// root_id, key, store)`. Gate Read.
    ///
    /// **Per-user trust model**: the daemon stores whatever bytes the
    /// authenticated user sends, scoped to their `user_uuid`. The blob is
    /// **opaque** to the daemon — ciphertext for encrypted stores, plaintext
    /// for plain ones — and the daemon performs no verification of the
    /// merge result. The trust boundary is the same one the client would have
    /// with a local-only cache: only the submitting user can poison their
    /// future reads on this slot.
    ///
    /// **Tip-based natural expiry**: keys are derived from tip sets (see
    /// `create_merge_cache_id`), so an entry whose tip set has advanced is
    /// simply unreachable — future reads miss against a fresh key. Stale
    /// entries fall out of the LRU under memory pressure rather than via
    /// explicit invalidation.
    CacheCrdtState {
        store: String,
        key: ID,
        blob: Vec<u8>,
    },
}

impl DatabaseOp {
    /// Minimum permission the caller needs against the target database.
    ///
    /// Only `SubmitSignedEntry` mutates; everything else is a read. Every
    /// read variant is tree-scoped via the request's `root_id`, so the
    /// per-tree gate always runs for reads — there is no tree-less
    /// fall-through. `SubmitSignedEntry` is the exception: the server skips
    /// the per-tree gate for submit and relies on its own verification pass,
    /// so the `Write(0)` returned here is advisory only for that variant
    /// (kept for completeness / non-submit callers that inspect it).
    pub fn required_permission(&self) -> Permission {
        match self {
            DatabaseOp::SubmitSignedEntry { .. } => Permission::Write(0),
            DatabaseOp::GetStoreTipsUpToEntries { .. } => Permission::Read,
            DatabaseOp::ComputeMergeState { .. } => Permission::Read,
            _ => Permission::Read,
        }
    }
}

/// Payload of an `Authenticated` service request.
///
/// Bundles the database scope (`root_id`) and identity claim (`identity`) with
/// the backend op the client wants to run. Carried boxed inside
/// `ServiceRequest::Authenticated` to keep the top-level enum's stack footprint
/// flat — `SigKey` and `BackendOp::SetInstanceMetadata`'s payload are large.
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

/// Payload of an `AuthenticatedDb` service request.
///
/// Database-level analogue of [`AuthenticatedRequest`]: same `(root_id,
/// identity)` scope and the same server-side gate flow, carrying a
/// [`DatabaseOp`] instead of a [`BackendOp`]. Boxed for the same reason —
/// `SigKey` and `DatabaseOp::SubmitSignedEntry` are large.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticatedDbRequest {
    /// Root entry of the database this op targets (auth-settings lookup +
    /// the implicit tree scope every `DatabaseOp` carries by construction).
    pub root_id: ID,
    /// Identity claim; verified against the connection's session pubkey
    /// before dispatch, exactly as for `BackendOp`.
    pub identity: SigKey,
    /// Database operation to execute.
    pub op: DatabaseOp,
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

    // === Post-auth: extend the connection's session keyset ===
    /// Step 1 of registering an additional pubkey on an already-authenticated
    /// connection. The client names a `pubkey`; the server issues a random
    /// challenge bound to that pubkey. The pubkey is added to the keyset only
    /// after the client returns a valid signature in `SessionKeyRegister`.
    ///
    /// Session-key registration extends the connection's identity from the
    /// single `login_pubkey` (from `TrustedLogin*`) to a *set* of pubkeys the
    /// client has proven possession of. Per-tree reads gate against this set,
    /// so a user can drive operations on databases authored by any of their
    /// per-DB keys without re-authenticating the whole connection.
    SessionKeyChallenge { pubkey: PublicKey },
    /// Step 2 of registering an additional pubkey. Carries a signature over
    /// the challenge issued by the matching `SessionKeyChallenge`. Server
    /// verifies the signature with the named `pubkey`; on success the pubkey
    /// joins the connection's session keyset and the challenge is consumed.
    SessionKeyRegister {
        pubkey: PublicKey,
        signature: Vec<u8>,
    },

    // === Authenticated wrapper for every backend operation ===
    /// All backend storage ops travel inside this wrapper. The inner
    /// `AuthenticatedRequest` carries `(root_id, identity, request)` and is
    /// boxed to keep the enum's discriminated size compact.
    Authenticated(Box<AuthenticatedRequest>),

    // === Authenticated wrapper for Database-level operations ===
    /// Database-level ops travel inside this wrapper (additive, parallel to
    /// `Authenticated`). The inner `AuthenticatedDbRequest` carries
    /// `(root_id, identity, op)`; same gate flow, boxed for the same reason.
    AuthenticatedDb(Box<AuthenticatedDbRequest>),
}

/// Response from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceResponse {
    /// Single entry
    Entry(Entry),
    /// Multiple entries
    Entries(Vec<Entry>),
    /// Multiple IDs
    Ids(Vec<ID>),
    /// Success with no data
    Ok,
    /// Transaction-build context (response to `DatabaseOp::BeginTransaction`).
    TransactionContext(TransactionContext),
    /// Materialized CRDT store state (response to `DatabaseOp::GetStoreState`).
    CrdtValue(WireCrdtValue),
    /// Merge state: lowest common ancestor + path to tips (response to
    /// `DatabaseOp::ComputeMergeState`).
    MergeState(MergeState),
    /// Optional instance metadata
    InstanceMetadata(Option<InstanceMetadata>),
    /// Optional cached CRDT state blob (response to
    /// `DatabaseOp::GetCachedCrdtState`). `None` on cache miss; the daemon
    /// does not synthesize a value, so the client falls back to recomputing
    /// from store entries.
    CachedCrdtState(Option<Vec<u8>>),
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
    /// Challenge bytes returned in response to `SessionKeyChallenge`. The
    /// client signs these with the named pubkey's private key and returns the
    /// signature in `SessionKeyRegister`.
    SessionKeyChallenge { challenge: Vec<u8> },
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
    fn test_request_get_instance_metadata_serde() {
        let req = ServiceRequest::GetInstanceMetadata;
        let json = serde_json::to_string(&req).unwrap();
        let req2: ServiceRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(req2, ServiceRequest::GetInstanceMetadata));
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
