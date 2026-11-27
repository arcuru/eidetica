//! Protocol definitions for sync communication.
//!
//! This module defines transport-agnostic message types that can be
//! used across different network transports (HTTP, Iroh, Bluetooth, etc.).

use serde::{Deserialize, Serialize};

use super::peer_types::Address;
use crate::{
    auth::{
        AuthError, Permission,
        crypto::{
            PrivateKey, PublicKey, create_challenge_response, generate_challenge,
            verify_challenge_response,
        },
    },
    crdt::Doc,
    entry::{Entry, ID},
    snapshot::Snapshot,
};

/// Handshake request sent when establishing a peer connection.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HandshakeRequest {
    // FIXME: device_id and public_key are functionally identical
    /// Unique device identifier
    pub device_id: PublicKey,
    /// Ed25519 public key of the sender
    pub public_key: PublicKey,
    /// Optional human-readable display name
    pub display_name: Option<String>,
    /// Protocol version number
    pub protocol_version: u32,
    /// Random challenge bytes for signature verification
    pub challenge: Vec<u8>,
    /// Addresses where this peer can be reached for sync
    pub listen_addresses: Vec<Address>,
}

/// Information about a tree available for sync
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TreeInfo {
    /// The root ID of the tree
    pub tree_id: ID,
    /// Optional human-readable name for the tree
    pub name: Option<String>,
    /// Number of entries in the tree
    pub entry_count: usize,
    /// Unix timestamp of last modification
    pub last_modified: u64,
}

/// Handshake response sent in reply to a handshake request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HandshakeResponse {
    // FIXME: device_id and public_key are functionally identical
    /// Unique device identifier
    pub device_id: PublicKey,
    /// Ed25519 public key of the responder
    pub public_key: PublicKey,
    /// Optional human-readable display name
    pub display_name: Option<String>,
    /// Protocol version number
    pub protocol_version: u32,
    /// Signed challenge from the request
    pub challenge_response: Vec<u8>,
    /// New challenge for mutual authentication
    pub new_challenge: Vec<u8>,
    /// Trees available for synchronization
    pub available_trees: Vec<TreeInfo>,
}

/// Unified sync request for both bootstrap and incremental sync
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SyncTreeRequest {
    /// Database ID to sync
    pub tree_id: ID,
    /// Our current tips (empty set signals bootstrap needed)
    pub our_tips: Snapshot,
    /// Device public key of the requesting peer (used for automatic tree/peer relationship tracking)
    pub peer_pubkey: Option<PublicKey>,
    /// Authentication key requesting access (for bootstrap).
    ///
    /// A request that names this key must carry matching proof in `auth` before
    /// its approval lifecycle is read or changed. Anonymous public sync omits it.
    pub requesting_key: Option<PublicKey>,
    /// Key name/identifier for the requesting key
    pub requesting_key_name: Option<String>,
    /// Desired permission level for bootstrap
    pub requested_permission: Option<Permission>,
    /// Free-form context the requester attaches for the approver to inspect
    /// when deciding whether to grant access. Carried verbatim onto the stored
    /// `BootstrapRequest`.
    #[serde(default)]
    pub metadata: Option<Doc>,
    /// Proof that the caller holds the private half of the key it is claiming.
    ///
    /// Required whenever `requesting_key` claims an identity, including manual
    /// approval requests and named-key requests on public databases. Anonymous
    /// public sync omits both fields.
    #[serde(default)]
    pub auth: Option<SyncRequestAuth>,
}

/// A caller's proof of key possession for one sync request.
///
/// The signature covers the responding server, the tree, the claimed tips, and
/// a timestamp/nonce pair, so a captured request cannot be replayed to the same
/// server, redirected to a different one, or reused for a different tree.
///
/// # What this does not defend against
///
/// This authenticates the *requester to the server*; it does not protect the
/// channel. Over a plaintext transport an attacker on the network path still
/// reads the served entries and can relay a live signed request to keep the
/// response for itself. Confidentiality against a network attacker requires an
/// encrypted transport (Iroh's QUIC), not this signature.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SyncRequestAuth {
    /// The key whose authority the caller is claiming on the tree.
    pub key: PublicKey,
    /// Milliseconds since the Unix epoch, from the caller's clock.
    pub timestamp_ms: u64,
    /// Random per-request value; makes each signature single-use.
    pub nonce: Vec<u8>,
    /// Signature over [`SyncRequestAuth::signing_bytes`].
    pub signature: Vec<u8>,
    /// Membership proofs use a separate domain and bind the requested permission.
    #[serde(default, skip_serializing_if = "is_false")]
    membership: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl SyncRequestAuth {
    /// Sign a request to `server_pubkey` for `tree_id` at `tips`.
    pub fn sign(
        signing_key: &PrivateKey,
        server_pubkey: &PublicKey,
        tree_id: &ID,
        tips: &Snapshot,
        timestamp_ms: u64,
    ) -> Self {
        Self::sign_access(
            signing_key,
            server_pubkey,
            tree_id,
            tips,
            timestamp_ms,
            None,
        )
    }

    /// `membership` binds explicit membership and its requested permission.
    /// `None` preserves the existing ordinary sync proof format.
    pub(crate) fn sign_access(
        signing_key: &PrivateKey,
        server_pubkey: &PublicKey,
        tree_id: &ID,
        tips: &Snapshot,
        timestamp_ms: u64,
        membership: Option<Permission>,
    ) -> Self {
        let nonce = generate_challenge();
        let signature = create_challenge_response(
            Self::signing_bytes(
                server_pubkey,
                tree_id,
                tips,
                timestamp_ms,
                &nonce,
                membership,
            ),
            signing_key,
        );
        Self {
            key: signing_key.public_key(),
            timestamp_ms,
            nonce,
            signature,
            membership: membership.is_some(),
        }
    }

    /// Verify the signature against the request it claims to cover.
    ///
    /// Freshness and single-use are the caller's responsibility — a valid
    /// signature says nothing about when it was made.
    pub fn verify(
        &self,
        server_pubkey: &PublicKey,
        tree_id: &ID,
        tips: &Snapshot,
    ) -> Result<(), AuthError> {
        self.verify_access(server_pubkey, tree_id, tips, None)
    }

    pub(crate) fn verify_access(
        &self,
        server_pubkey: &PublicKey,
        tree_id: &ID,
        tips: &Snapshot,
        requested_permission: Option<Permission>,
    ) -> Result<(), AuthError> {
        let membership = if self.membership {
            Some(
                requested_permission.ok_or_else(|| AuthError::InvalidAuthConfiguration {
                    reason: "membership proof requires a requested permission".into(),
                })?,
            )
        } else {
            None
        };
        verify_challenge_response(
            Self::signing_bytes(
                server_pubkey,
                tree_id,
                tips,
                self.timestamp_ms,
                &self.nonce,
                membership,
            ),
            &self.signature,
            &self.key,
        )
    }

    pub(crate) fn requires_direct_key(&self) -> bool {
        self.membership
    }

    /// The exact bytes covered by the signature.
    ///
    /// Every field is length-prefixed so that no two distinct requests can
    /// produce the same byte string.
    fn signing_bytes(
        server_pubkey: &PublicKey,
        tree_id: &ID,
        tips: &Snapshot,
        timestamp_ms: u64,
        nonce: &[u8],
        membership: Option<Permission>,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut push = |field: &[u8]| {
            bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
            bytes.extend_from_slice(field);
        };

        match membership {
            None => push(SYNC_REQUEST_DOMAIN),
            Some(permission) => {
                push(b"eidetica/sync/key-membership/v1");
                match permission {
                    Permission::Read => push(b"read"),
                    Permission::Write(priority) => {
                        push(b"write");
                        push(&priority.to_be_bytes());
                    }
                    Permission::Admin(priority) => {
                        push(b"admin");
                        push(&priority.to_be_bytes());
                    }
                }
            }
        }
        push(server_pubkey.to_string().as_bytes());
        push(tree_id.to_string().as_bytes());
        push(&(tips.len() as u64).to_be_bytes());
        for tip in tips.tips() {
            push(tip.to_string().as_bytes());
        }
        push(&timestamp_ms.to_be_bytes());
        push(nonce);
        bytes
    }
}

/// Domain separator, so a sync signature can never be mistaken for a signature
/// over an entry or a handshake challenge.
const SYNC_REQUEST_DOMAIN: &[u8] = b"eidetica/sync/tree-request/v1";

#[cfg(test)]
mod request_auth_tests {
    use super::*;

    #[test]
    fn membership_proof_binds_mode_and_permission() {
        let key = PrivateKey::generate();
        let peer = PrivateKey::generate().public_key();
        let root = ID::from_bytes("identity");
        let tips = Snapshot::EMPTY;
        let permission = Permission::Write(5);
        let proof = SyncRequestAuth::sign_access(&key, &peer, &root, &tips, 100, Some(permission));
        let wire = serde_json::to_value(&proof).unwrap();
        let decoded: SyncRequestAuth = serde_json::from_value(wire).unwrap();
        assert!(decoded.requires_direct_key());
        assert!(
            decoded
                .verify_access(&peer, &root, &tips, Some(permission))
                .is_ok()
        );
        assert!(
            decoded
                .verify_access(&peer, &root, &tips, Some(Permission::Write(6)))
                .is_err()
        );
        assert!(decoded.verify(&peer, &root, &tips).is_err());

        // Legacy readers see the ordinary proof fields only. A membership
        // proof must not pass ordinary verification on that projection.
        let mut ordinary_projection = decoded;
        ordinary_projection.membership = false;
        assert!(ordinary_projection.verify(&peer, &root, &tips).is_err());
        let ordinary = SyncRequestAuth::sign(&key, &peer, &root, &tips, 100);
        assert!(
            serde_json::to_value(&ordinary)
                .unwrap()
                .get("membership")
                .is_none()
        );
        assert!(ordinary.verify(&peer, &root, &tips).is_ok());
    }
}

/// Bootstrap response containing complete tree state
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BootstrapResponse {
    /// Database ID being bootstrapped
    pub tree_id: ID,
    /// The root entry of the tree
    pub root_entry: Entry,
    /// All entries in the tree (excluding root)
    pub all_entries: Vec<Entry>,
    /// Whether the requesting key was approved and added
    pub key_approved: bool,
    /// The permission level granted (if approved)
    pub granted_permission: Option<Permission>,
}

/// Incremental sync response for existing trees
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct IncrementalResponse {
    /// Database ID being synced
    pub tree_id: ID,
    /// Peer's current tips
    pub their_tips: Vec<ID>,
    /// Entries missing from our tree
    pub missing_entries: Vec<Entry>,
}

/// Request messages that can be sent to a sync peer.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncRequest {
    /// Initial handshake request
    Handshake(HandshakeRequest),
    /// Unified tree sync request (handles both bootstrap and incremental)
    SyncTree(SyncTreeRequest),
    /// Send entries for synchronization (backward compatibility)
    SendEntries(Vec<Entry>),
}

/// Response messages returned from a sync peer.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncResponse {
    /// Handshake response
    Handshake(HandshakeResponse),
    /// Full database bootstrap for new peers
    Bootstrap(BootstrapResponse),
    /// Incremental sync for existing peers
    Incremental(IncrementalResponse),
    /// Bootstrap request pending manual approval
    BootstrapPending {
        /// Unique identifier for the pending request
        request_id: String,
        /// Human-readable message about the pending status
        message: String,
    },
    /// Bootstrap request was rejected by an administrator.
    BootstrapRejected {
        /// Identifier of the rejected request
        request_id: String,
        /// Human-readable rejection detail
        message: String,
    },
    /// Acknowledgment that entries were received successfully
    Ack,
    /// Number of entries received (for multiple entries)
    Count(usize),
    /// Error response
    Error(String),
}

/// Current protocol version - 0 indicates unstable.
///
/// `#[non_exhaustive]` does not protect wire compatibility: a peer on an older
/// version fails to deserialize an unknown variant. Adding a variant to any
/// serialized type in this protocol (handshake, sync requests/responses) is a
/// version bump, not a backward-compatible addition. See
/// [`crate::instance::WriteSource`] for the same rule on the service wire.
pub const PROTOCOL_VERSION: u32 = 0;

/// Context information about the incoming request.
///
/// This struct captures metadata about the connection that initiated
/// the request, allowing the handler to know where the request came from.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// The remote address from which this request originated.
    /// Extracted from the transport layer's connection metadata.
    pub remote_address: Option<Address>,
    /// The public key the peer claims for relationship tracking.
    ///
    /// **Unverified.** Transports copy it out of the request body; nothing
    /// proves the sender holds the matching private key. Authorization uses
    /// [`SyncTreeRequest::auth`], never this field.
    pub peer_pubkey: Option<PublicKey>,
}
