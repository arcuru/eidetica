//! Service server: accepts Unix socket connections and dispatches `BackendImpl` operations.
//!
//! The server wraps an `Instance` (not just a backend) so it can handle write
//! notifications through the Instance's callback system.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;
use tokio::sync::watch;

use crate::Instance;
use crate::auth::crypto::{PublicKey, generate_challenge, verify_challenge_response};
use crate::auth::errors::AuthError;
use crate::auth::types::{Permission, SigKey};
use crate::auth::validation::permissions::resolve_identity_permission;
use crate::backend::CacheScope;
use crate::database::Database;
use crate::entry::ID;
use crate::instance::WriteSource;
use crate::service::error::ServiceError;
use crate::service::protocol::{
    AuthenticatedDbRequest, AuthenticatedRequest, BackendOp, DatabaseOp, HandshakeAck, MergeState,
    PROTOCOL_VERSION, ServiceRequest, ServiceResponse, read_frame, write_frame,
};
use crate::user::system_databases::lookup_user_record;

/// Per-connection authentication state.
///
/// A connection moves `PreAuth → AwaitingProof → Authenticated` on a successful
/// `TrustedLoginUser` / `TrustedLoginProve` exchange. A failed proof drops the
/// connection back to `PreAuth` so the client can retry without reconnecting.
/// Any other request while in `AwaitingProof` also resets the state so a
/// half-finished login can't be exploited mid-flight.
///
/// "Trusted" refers to the assumption that whoever can reach this socket is
/// already authorised by filesystem permissions (mode 0600 under
/// `$XDG_RUNTIME_DIR`); see the protocol module docs and the Service
/// Architecture brain doc § Trusted login threat model.
#[derive(Debug, Clone)]
enum ConnectionState {
    /// No login attempt yet, or last attempt failed/abandoned.
    PreAuth,
    /// `TrustedLoginUser` succeeded; waiting for the client's `TrustedLoginProve`.
    AwaitingProof {
        username: String,
        user_uuid: String,
        challenge: Vec<u8>,
        expected_pubkey: PublicKey,
    },
    /// Login completed. `login_pubkey` is the verified root pubkey for the
    /// user, established at `TrustedLoginProve` time. `session_keyset` is the
    /// set of pubkeys the client has further proven possession of via
    /// `SessionKeyChallenge`/`SessionKeyRegister`; it always contains
    /// `login_pubkey` and may include additional per-DB keys the user owns.
    /// The dispatch path for `Authenticated`/`AuthenticatedDb` requests
    /// validates the identity hint against this set and gates against the
    /// resulting *acting* pubkey, so a single connection can drive ops on
    /// databases authored by any key the user has proven they hold.
    /// `user_uuid` is the per-user scope key for the unified CRDT-state
    /// cache (see [`crate::backend::CacheScope::User`]).
    /// `pending_key_challenges` holds outstanding registration challenges,
    /// keyed by the pubkey the challenge was issued for. Each challenge is
    /// single-use: the matching `SessionKeyRegister` consumes it whether
    /// verification succeeds or fails.
    #[allow(dead_code)] // username surfaces in audit/logging follow-ups
    Authenticated {
        username: String,
        user_uuid: String,
        login_pubkey: PublicKey,
        session_keyset: HashSet<PublicKey>,
        pending_key_challenges: HashMap<PublicKey, Vec<u8>>,
    },
}

/// Eidetica service server that listens on a Unix domain socket.
///
/// The server wraps a full `Instance` so it can dispatch both storage operations
/// (via the backend) and write callbacks (via `Instance::put_entry()`'s notification path).
///
/// CRDT-state caching lives in the underlying `BackendImpl` (scope-keyed
/// via [`crate::backend::CacheScope`]); wire handlers route through
/// `instance.backend()` directly rather than keeping a separate
/// service-layer cache.
pub struct ServiceServer {
    instance: Instance,
    socket_path: PathBuf,
}

impl ServiceServer {
    /// Create a new service server.
    ///
    /// # Arguments
    /// * `instance` - The Instance to serve. The server holds a strong reference.
    /// * `socket_path` - Path for the Unix domain socket.
    pub fn new(instance: Instance, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            instance,
            socket_path: socket_path.into(),
        }
    }

    /// Get the socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Run the server until the shutdown signal is received.
    ///
    /// Removes any stale socket file, creates the parent directory, binds the
    /// listener, and loops accepting connections. Each connection is handled in
    /// a spawned task. On shutdown, the socket file is cleaned up.
    ///
    /// # Arguments
    /// * `shutdown` - A watch receiver; the server stops when the sender is dropped.
    pub async fn run(&self, mut shutdown: watch::Receiver<()>) -> crate::Result<()> {
        // Remove stale socket if it exists
        if self.socket_path.exists() {
            tokio::fs::remove_file(&self.socket_path).await?;
        }

        // Create parent directory with owner-only permissions (0700)
        if let Some(parent) = self.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
            tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;

        // Restrict socket to owner-only access (0600)
        tokio::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600))
            .await?;

        tracing::info!("Service server listening on {}", self.socket_path.display());

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _addr)) => {
                            let instance = self.instance.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, instance).await {
                                    tracing::debug!("Connection handler error: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("Failed to accept connection: {e}");
                        }
                    }
                }
                _ = shutdown.changed() => {
                    tracing::info!("Service server shutting down");
                    break;
                }
            }
        }

        // Clean up socket file
        let _ = tokio::fs::remove_file(&self.socket_path).await;
        Ok(())
    }
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    instance: Instance,
) -> crate::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);

    // 1. Read and validate handshake
    let handshake: crate::service::protocol::Handshake = match read_frame(&mut reader).await? {
        Some(h) => h,
        None => return Ok(()), // Client disconnected before handshake
    };

    if handshake.protocol_version != PROTOCOL_VERSION {
        // Send error ack and close
        let ack = HandshakeAck {
            protocol_version: PROTOCOL_VERSION,
        };
        write_frame(&mut writer, &ack).await?;
        return Err(crate::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Protocol version mismatch: client={}, server={}",
                handshake.protocol_version, PROTOCOL_VERSION
            ),
        )));
    }

    // Send handshake ack
    let ack = HandshakeAck {
        protocol_version: PROTOCOL_VERSION,
    };
    write_frame(&mut writer, &ack).await?;

    // 2. Request/response loop with per-connection auth state
    let mut state = ConnectionState::PreAuth;
    let loop_result: crate::Result<()> = async {
        loop {
            let request: ServiceRequest = match read_frame(&mut reader).await? {
                Some(req) => req,
                None => break, // Clean EOF
            };

            let response = dispatch(&instance, &mut state, request).await;
            write_frame(&mut writer, &response).await?;
        }
        Ok(())
    }
    .await;

    // Session teardown: nothing to reclaim. The unified CRDT-state cache
    // lives in `BackendImpl` and is bounded by its own eviction policy
    // (byte-bounded LRU on the in-memory backend; disk-bounded on SQL).
    // Per-user cache slots survive disconnect intentionally so a
    // reconnecting client recovers materialized state from tier 2 without
    // recomputing from entries.

    loop_result
}

/// Dispatch a service request to the appropriate Instance/Backend method.
async fn dispatch(
    instance: &Instance,
    state: &mut ConnectionState,
    request: ServiceRequest,
) -> ServiceResponse {
    match dispatch_inner(instance, state, request).await {
        Ok(resp) => resp,
        Err(e) => ServiceResponse::Error(ServiceError::from(&e)),
    }
}

/// Inner dispatch that returns Result for ergonomic error handling.
async fn dispatch_inner(
    instance: &Instance,
    state: &mut ConnectionState,
    request: ServiceRequest,
) -> crate::Result<ServiceResponse> {
    match request {
        // === Pre-auth: login handshake ===
        ServiceRequest::TrustedLoginUser { username } => {
            handle_trusted_login_user(instance, state, username).await
        }
        ServiceRequest::TrustedLoginProve { signature } => {
            handle_trusted_login_prove(state, &signature)
        }

        // === Pre-auth: server identity ===
        ServiceRequest::GetInstanceMetadata => {
            let metadata = instance.backend().get_instance_metadata().await?;
            Ok(ServiceResponse::InstanceMetadata(metadata))
        }

        // === Post-auth: extend the session keyset ===
        ServiceRequest::SessionKeyChallenge { pubkey } => {
            handle_session_key_challenge(state, pubkey)
        }
        ServiceRequest::SessionKeyRegister { pubkey, signature } => {
            handle_session_key_register(state, pubkey, &signature)
        }

        // === Authenticated backend operations ===
        //
        // Gate 1 (chunk 5a): the connection must have completed `TrustedLogin*`.
        // Gate 2 (chunk 5b): if the op carries a tree id, the *acting* pubkey
        // must resolve to at least the op's required permission against that
        // tree's `auth_settings`. The acting pubkey is the identity hint
        // (when present and provably in the keyset) or `login_pubkey`
        // otherwise — see `resolve_acting_pubkey`.
        ServiceRequest::Authenticated(inner) => {
            let (login_pubkey, keyset_snapshot) = match state {
                ConnectionState::Authenticated {
                    login_pubkey,
                    session_keyset,
                    ..
                } => (login_pubkey.clone(), session_keyset.clone()),
                _ => {
                    return Err(crate::Error::Auth(Box::new(
                        AuthError::InvalidAuthConfiguration {
                            reason: "backend operation requires an authenticated connection; \
                                 complete TrustedLogin* first"
                                .to_string(),
                        },
                    )));
                }
            };

            let AuthenticatedRequest {
                root_id: _,
                identity,
                request,
            } = *inner;

            // The identity hint must be in the session keyset (proven
            // possession this connection). Absent hint → act as login pubkey.
            let acting_pubkey = resolve_acting_pubkey(&identity, &login_pubkey, &keyset_snapshot)?;

            if let Some(tree_id) = request.tree_id() {
                gate_tree_permission(
                    instance,
                    &acting_pubkey,
                    &identity,
                    tree_id,
                    request.required_permission(),
                    // create-flow passthrough: a not-yet-propagated tree
                    // must be waved through so `Database::create` works.
                    false,
                )
                .await?;
            }

            // Gate 3: cross-tree admin-only ops. `SetInstanceMetadata`
            // rewrites the daemon's pointers to its own system DBs; an
            // instance admin is, by construction, a user with Admin on
            // `_databases` (the first-user bootstrap is what grants this).
            // The op carries no inline tree id, so we resolve the caller's
            // permission against `_databases.auth_settings` explicitly.
            // D8: fail closed — `_databases` always exists on an
            // initialized daemon and this is never a creation flow, so the
            // create-flow passthrough must not apply here (it would let any
            // authenticated user rewrite system-DB pointers if `_databases`
            // were ever unreadable).
            if matches!(request, BackendOp::SetInstanceMetadata { .. }) {
                gate_tree_permission(
                    instance,
                    &acting_pubkey,
                    &identity,
                    instance.databases_db_id(),
                    Permission::Admin(0),
                    true,
                )
                .await?;
            }

            dispatch_backend_op(instance, &acting_pubkey, &identity, request).await
        }

        // === Authenticated Database-level operations ===
        //
        // Same gates as `Authenticated`, but every `DatabaseOp` carries its
        // target `root_id` explicitly, so the per-tree permission gate is
        // *unconditional* — there is no tree-less op to fall through it. That
        // structural tree-scoping is what the Database-level shape buys; the
        // legacy `BackendOp`/`Authenticated` path above is untouched.
        ServiceRequest::AuthenticatedDb(inner) => {
            let (login_pubkey, keyset_snapshot, session_user_uuid) = match state {
                ConnectionState::Authenticated {
                    login_pubkey,
                    session_keyset,
                    user_uuid,
                    ..
                } => (
                    login_pubkey.clone(),
                    session_keyset.clone(),
                    user_uuid.clone(),
                ),
                _ => {
                    return Err(crate::Error::Auth(Box::new(
                        AuthError::InvalidAuthConfiguration {
                            reason: "database operation requires an authenticated connection; \
                                 complete TrustedLogin* first"
                                .to_string(),
                        },
                    )));
                }
            };

            let AuthenticatedDbRequest {
                root_id,
                identity,
                op,
            } = *inner;

            // Submit is verification-gated, not session-gated.
            //
            // Reads are session-gated (confidentiality boundary); submits
            // are verification-gated (integrity boundary). `SubmitSignedEntry`
            // requires only an *authenticated* connection (gate 1, the
            // `ConnectionState::Authenticated` match above, still applies).
            // Which tree the entry belongs to, and whether its signer may
            // write that tree, is decided by the server's own verification
            // pass in the handler (store `Unverified`, then
            // `Database::open(...).verify()`) against the tree's *real*
            // pinned auth lineage — not by who holds the socket. An attacker
            // without a key the tree's auth grants cannot produce a
            // `Verified` entry, and unverified junk is excluded from every
            // default read by the frontier cut, so the per-tree session gate
            // adds no correctness or isolation property here; it only blocks
            // a legitimate transporter (e.g. an admin session carrying a
            // user-signed genesis). See the verification-gated-submit design
            // doc for the full threat analysis.
            let is_submit = matches!(op, DatabaseOp::SubmitSignedEntry { .. });

            // Submit accepts any identity hint (admin transports user-signed
            // entries); every other op resolves an acting pubkey from the
            // keyset and gates per-tree against it.
            let acting_pubkey = if is_submit {
                // Use the hint if it parses as a pubkey (for submit metadata),
                // otherwise fall back to login_pubkey — submit doesn't gate
                // on this value.
                identity
                    .hint()
                    .pubkey
                    .clone()
                    .unwrap_or_else(|| login_pubkey.clone())
            } else {
                resolve_acting_pubkey(&identity, &login_pubkey, &keyset_snapshot)?
            };

            // Per-tree permission gate. Unconditional for every op *except*
            // submit; create-flow passthrough (false) for the rest, so a
            // not-yet-propagated tree is waved through and database creation
            // works, identical to the `Authenticated` path. Submit skips the
            // gate entirely (verification in the handler is its boundary).
            if !is_submit {
                gate_tree_permission(
                    instance,
                    &acting_pubkey,
                    &identity,
                    &root_id,
                    op.required_permission(),
                    false,
                )
                .await?;
            }

            dispatch_database_op(
                instance,
                &acting_pubkey,
                &identity,
                &session_user_uuid,
                root_id,
                op,
            )
            .await
        }
    }
}

/// Resolve the *acting* pubkey for a session-gated op.
///
/// The identity hint, when present, must be in the connection's session
/// keyset (proof of possession registered via `SessionKeyChallenge` /
/// `SessionKeyRegister`, or established at login time). Returning the hint
/// as the acting pubkey lets the per-tree gate check the actual key the
/// caller wants to act as, not the connection-wide login key.
///
/// An absent hint defaults to the login pubkey — matches the pre-keyset
/// behavior where every op acted as the login identity.
fn resolve_acting_pubkey(
    identity: &SigKey,
    login_pubkey: &PublicKey,
    session_keyset: &HashSet<PublicKey>,
) -> crate::Result<PublicKey> {
    match &identity.hint().pubkey {
        Some(claimed) if session_keyset.contains(claimed) => Ok(claimed.clone()),
        Some(claimed) => Err(crate::Error::Auth(Box::new(
            AuthError::SigningKeyMismatch {
                reason: format!(
                    "request identity claims pubkey '{claimed}' but it is not in the session keyset; \
                     register it first via SessionKeyChallenge/SessionKeyRegister"
                ),
            },
        ))),
        None => Ok(login_pubkey.clone()),
    }
}

/// Dispatch a Database-level op against the server's local `Database`.
///
/// Additive sibling of `dispatch_backend_op`. The caller has already verified
/// the session identity and run the unconditional per-tree permission gate on
/// `root_id`. Because the server runs the `Database` layer here, verify-on-read
/// and the Verified frontier are server-side **by construction**.
async fn dispatch_database_op(
    instance: &Instance,
    acting_pubkey: &PublicKey,
    identity: &SigKey,
    user_uuid: &str,
    root_id: ID,
    op: DatabaseOp,
) -> crate::Result<ServiceResponse> {
    match op {
        DatabaseOp::GetEntry { id } => {
            let entry = instance.backend().get(&id).await?;
            // Same post-fetch owning-tree Read gate as `BackendOp::Get`:
            // a raw entry id carries no inline tree, so the pre-dispatch
            // gate could not cover it.
            gate_entry_read(instance, acting_pubkey, identity, &entry).await?;
            Ok(ServiceResponse::Entry(entry))
        }

        DatabaseOp::GetVerifiedTips => {
            // The server runs the Database layer, so `get_tips()` returns the
            // Verified frontier by construction — no client-side verify, no
            // remote-detection heuristic.
            let db = Database::open(instance, &root_id).await?;
            let tips = db.get_tips().await?;
            Ok(ServiceResponse::Ids(tips))
        }

        DatabaseOp::SubmitSignedEntry { entry } => {
            // The client signed this entry; the server does NOT trust its
            // claimed validity. Store it `Unverified`, then run our OWN
            // verification pass against the entry's pinned settings. A
            // poisoned entry never reaches `Verified` and is excluded from
            // every default read by the frontier cut — D1 is closed by
            // construction here, not by gate-hardening a raw `Put`.
            instance
                .put_entry(
                    &root_id,
                    crate::backend::VerificationStatus::Unverified,
                    *entry,
                    WriteSource::Remote,
                )
                .await?;
            Database::open(instance, &root_id).await?.verify().await?;
            Ok(ServiceResponse::Ok)
        }

        DatabaseOp::BeginTransaction { stores, scope } => {
            // Single-sourced: both this handler and the Phase-3 remote seam
            // call `Database::transaction_context`, so `Transaction::commit`'s
            // build-sign path has one source of truth.
            let db = Database::open(instance, &root_id).await?;
            let ctx = db.transaction_context(&stores, scope).await?;
            Ok(ServiceResponse::TransactionContext(ctx))
        }

        DatabaseOp::GetStoreState { store } => {
            // Server-materialized merged state (unencrypted stores only).
            // Encrypted stores must use GetStoreEntries instead — the
            // ephemeral transaction here has no encryptor, and Doc
            // deserialization would fail on ciphertext.
            let db = Database::open(instance, &root_id).await?;
            let value = db.get_store_state(&store).await?;
            Ok(ServiceResponse::CrdtValue(value))
        }

        DatabaseOp::GetStoreEntries { store, tips, scope } => {
            // Universal primitive (encrypted + unencrypted): returns raw
            // Entry records with opaque data in canonical CRDT replay order.
            // For encrypted stores the client decrypts+merges locally.
            let db = Database::open(instance, &root_id).await?;
            let entries = db.get_store_entries(&store, &tips, scope).await?;
            Ok(ServiceResponse::Entries(entries))
        }

        DatabaseOp::GetStoreTipsUpToEntries { store, up_to } => {
            let db = Database::open(instance, &root_id).await?;
            let ids = db
                .ops()
                .get_store_tips_up_to_entries(&root_id, &store, &up_to)
                .await?;
            Ok(ServiceResponse::Ids(ids))
        }

        DatabaseOp::ComputeMergeState { store, entry_ids } => {
            let db = Database::open(instance, &root_id).await?;
            let merge_base = db
                .ops()
                .find_merge_base(&root_id, &store, &entry_ids)
                .await?;
            let path = db
                .ops()
                .get_path_from_to(&root_id, &store, &merge_base, &entry_ids)
                .await?;
            Ok(ServiceResponse::MergeState(MergeState { merge_base, path }))
        }

        DatabaseOp::GetCachedCrdtState { store, key } => {
            // Per-tree Read gate already ran above. Try the caller's own
            // User-scoped slot first (where client-uploaded ciphertext for
            // encrypted stores lives), then fall back to Shared (where the
            // daemon's own materialization of unencrypted stores lives).
            // The fallback is what gives cross-user dedup on plaintext
            // stores: alice triggers a server materialization, blob lands
            // in Shared, bob's later read finds it without recomputing.
            let backend = instance.backend();
            let mut blob = backend
                .get_cached_crdt_state(
                    &CacheScope::User(user_uuid.to_string()),
                    &key,
                    &store,
                )
                .await?;
            if blob.is_none() {
                blob = backend
                    .get_cached_crdt_state(&CacheScope::Shared, &key, &store)
                    .await?;
            }
            Ok(ServiceResponse::CachedCrdtState(blob))
        }

        DatabaseOp::CacheCrdtState { store, key, blob } => {
            // Per-tree Read gate already ran above. Per-user trust: the
            // blob is opaque (cipher- or plaintext) and stored verbatim;
            // only the submitting user can read it back. We never promote
            // a client upload to Shared — the daemon can't verify the
            // merge result, so cross-user visibility would be a poison
            // vector. Shared writes only come from the daemon's own
            // LocalDatabaseOps path.
            instance
                .backend()
                .cache_crdt_state(
                    CacheScope::User(user_uuid.to_string()),
                    &key,
                    &store,
                    blob,
                )
                .await?;
            Ok(ServiceResponse::Ok)
        }
    }
}

/// Handle `ServiceRequest::TrustedLoginUser`: look up the user's full record,
/// mint a challenge, and move the connection into `AwaitingProof`.
async fn handle_trusted_login_user(
    instance: &Instance,
    state: &mut ConnectionState,
    username: String,
) -> crate::Result<ServiceResponse> {
    // Look up the full `UserInfo`. The encrypted root key + salt ship to the
    // client so it can decrypt locally and sign the challenge in the same
    // round-trip; the daemon never sees the password or the plaintext key.
    // The non-credential fields (user_database_id, status) ride along so the
    // client can build the `User` session after proof without a second wire
    // read of `_users`. If the lookup fails (no such user, disabled,
    // duplicates), drop back to `PreAuth` and bubble the error.
    let users_db = instance.users_db().await?;
    let (user_uuid, user_info) = match lookup_user_record(&users_db, &username).await {
        Ok(v) => v,
        Err(e) => {
            *state = ConnectionState::PreAuth;
            return Err(e);
        }
    };

    let expected_pubkey = user_info.credentials.root_key_id.clone();
    let challenge = generate_challenge();
    *state = ConnectionState::AwaitingProof {
        username,
        user_uuid: user_uuid.clone(),
        challenge: challenge.clone(),
        expected_pubkey,
    };
    Ok(ServiceResponse::TrustedLoginChallenge {
        challenge,
        user_uuid,
        user_info,
    })
}

/// Handle `ServiceRequest::TrustedLoginProve`: verify the signature against the
/// stored challenge and either transition to `Authenticated` or drop back
/// to `PreAuth`.
fn handle_trusted_login_prove(
    state: &mut ConnectionState,
    signature: &[u8],
) -> crate::Result<ServiceResponse> {
    let (username, user_uuid, challenge, expected_pubkey) =
        match std::mem::replace(state, ConnectionState::PreAuth) {
            ConnectionState::AwaitingProof {
                username,
                user_uuid,
                challenge,
                expected_pubkey,
            } => (username, user_uuid, challenge, expected_pubkey),
            other => {
                // Restore the (unexpected) state we just took out so subsequent
                // requests see consistent state.
                *state = other;
                return Err(crate::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "TrustedLoginProve received outside of AwaitingProof state",
                )));
            }
        };

    match verify_challenge_response(&challenge, signature, &expected_pubkey) {
        Ok(()) => {
            let mut session_keyset = HashSet::new();
            session_keyset.insert(expected_pubkey.clone());
            *state = ConnectionState::Authenticated {
                username,
                user_uuid,
                login_pubkey: expected_pubkey,
                session_keyset,
                pending_key_challenges: HashMap::new(),
            };
            Ok(ServiceResponse::TrustedLoginOk)
        }
        Err(e) => {
            // Already reset to PreAuth via the mem::replace above.
            Err(crate::Error::Auth(Box::new(e)))
        }
    }
}

/// Handle `ServiceRequest::SessionKeyChallenge`: mint a single-use challenge
/// bound to `pubkey` and stash it in the connection's pending-challenges map.
///
/// Requires an authenticated connection. A repeat call for the same pubkey
/// overwrites the prior challenge — last-issued wins, so a stale challenge
/// can't be replayed.
fn handle_session_key_challenge(
    state: &mut ConnectionState,
    pubkey: PublicKey,
) -> crate::Result<ServiceResponse> {
    match state {
        ConnectionState::Authenticated {
            pending_key_challenges,
            ..
        } => {
            let challenge = generate_challenge();
            pending_key_challenges.insert(pubkey, challenge.clone());
            Ok(ServiceResponse::SessionKeyChallenge { challenge })
        }
        _ => Err(crate::Error::Auth(Box::new(
            AuthError::InvalidAuthConfiguration {
                reason: "SessionKeyChallenge requires an authenticated connection; \
                     complete TrustedLogin* first"
                    .to_string(),
            },
        ))),
    }
}

/// Handle `ServiceRequest::SessionKeyRegister`: verify the signature against
/// the matching pending challenge and, on success, add `pubkey` to the
/// connection's session keyset.
///
/// The challenge is consumed (removed) whether verification succeeds or fails,
/// so a bad signature can't be retried against the same challenge.
fn handle_session_key_register(
    state: &mut ConnectionState,
    pubkey: PublicKey,
    signature: &[u8],
) -> crate::Result<ServiceResponse> {
    match state {
        ConnectionState::Authenticated {
            session_keyset,
            pending_key_challenges,
            ..
        } => {
            let challenge = pending_key_challenges.remove(&pubkey).ok_or_else(|| {
                crate::Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                    reason: format!(
                        "no outstanding SessionKeyChallenge for pubkey '{pubkey}'; \
                         issue the challenge before registering"
                    ),
                }))
            })?;
            verify_challenge_response(&challenge, signature, &pubkey)
                .map_err(|e| crate::Error::Auth(Box::new(e)))?;
            session_keyset.insert(pubkey);
            Ok(ServiceResponse::Ok)
        }
        _ => Err(crate::Error::Auth(Box::new(
            AuthError::InvalidAuthConfiguration {
                reason: "SessionKeyRegister requires an authenticated connection; \
                     complete TrustedLogin* first"
                    .to_string(),
            },
        ))),
    }
}

/// Resolve `pubkey`'s permission against `tree_id`'s `auth_settings` and reject
/// the request if the resolved level doesn't cover `required`.
///
/// If the database doesn't exist on this daemon yet and `require_existing`
/// is false, the gate passes through so the dispatched op surfaces its own
/// response (NotFound, empty result, or — for write coordination — a
/// no-op). This is what keeps the legitimate "create a new database" flow
/// working: `Database::create` reads tips on the tree before its root entry
/// has propagated, so an outright denial here would break creation. The
/// cost is that callers can still distinguish "no such database" from
/// "exists but no access"; closing that existence-leak channel is filed as
/// a follow-up.
///
/// `require_existing = true` flips that to **fail closed**: an absent
/// target is denied rather than waved through. Used for the
/// `SetInstanceMetadata` admin gate (D8) — `_databases` always exists on an
/// initialized daemon and that op is never a creation flow, so the
/// create-flow passthrough there would only ever be a fail-open hole that
/// lets any authenticated user rewrite the daemon's system-DB pointers if
/// `_databases` were ever unreadable.
///
/// When the gate does fire, the denial error is the same shape regardless
/// of which sub-check failed (key not in auth_settings, mismatched hint,
/// insufficient permission level): no internal detail about *why* leaks
/// back over the wire.
///
/// System databases (`_users`, `_databases`, `_sync`, `_instance`) are gated
/// like any other tree: callers must hold the required permission in the
/// system DB's `auth_settings`. The instance-admin bootstrap (first user on
/// the device) writes the first user as `Admin(0)` on `_users` and
/// `_databases`, which is how legitimate administrative access is granted —
/// the previous hardcoded read exemption is gone. The daemon's device-keyed
/// local path still handles internal system-database maintenance writes that
/// originate inside the server.
async fn gate_tree_permission(
    instance: &Instance,
    pubkey: &PublicKey,
    identity: &SigKey,
    tree_id: &ID,
    required: Permission,
    require_existing: bool,
) -> crate::Result<()> {
    let denied = || {
        crate::Error::Auth(Box::new(AuthError::PermissionDenied {
            reason: format!("tree {tree_id}: pubkey {pubkey} not permitted for {required:?}"),
        }))
    };

    if !instance.has_database(tree_id).await {
        return if require_existing {
            Err(denied())
        } else {
            Ok(())
        };
    }

    let database = Database::open(instance, tree_id).await?;
    let settings_store = database.get_settings().await?;
    let auth_settings = settings_store.auth_snapshot().await?;

    let resolved =
        match resolve_identity_permission(pubkey, identity, &auth_settings, Some(instance)).await {
            Ok(p) => p,
            // Resolution failures (key not found, mismatch, etc.) collapse to the
            // same shape as an insufficient-permission denial, so the client
            // can't tell whether its identity was unknown or merely too low.
            Err(_) => return Err(denied()),
        };

    let allowed = match required {
        Permission::Read => true,
        Permission::Write(_) => resolved.can_write(),
        Permission::Admin(_) => resolved.can_admin(),
    };

    if !allowed {
        return Err(denied());
    }

    Ok(())
}

/// Per-tree read gate for ops keyed by a raw entry id, which therefore
/// carry no inline tree id and never hit the pre-dispatch `tree_id()` gate
/// (`Get`). The tree to authorise against is only knowable *after* the
/// fetch: it is the entry's claimed `tree.root`, or — for a tree-root
/// entry, whose `root()` is `None` — the entry's own id.
///
/// Model B (hard multi-tenant boundary): system DBs are unencrypted and
/// protected solely by this gate, so a raw cross-tree `Get` MUST resolve
/// and check the real owning tree before returning content. Delegates to
/// `gate_tree_permission`, so the `has_database`-absent passthrough and the
/// opaque denial shape are identical to the inline-tree-id path.
async fn gate_entry_read(
    instance: &Instance,
    pubkey: &PublicKey,
    identity: &SigKey,
    entry: &crate::entry::Entry,
) -> crate::Result<()> {
    let owning_tree = entry.root().unwrap_or_else(|| entry.id());
    // create-flow passthrough (false): a Get against a tree not yet
    // registered on this daemon must be waved through, same as the
    // inline-tree-id path.
    gate_tree_permission(
        instance,
        pubkey,
        identity,
        &owning_tree,
        Permission::Read,
        false,
    )
    .await
}

/// Dispatch a `BackendOp` against the daemon's `Instance` backend.
///
/// Called from `dispatch_inner` after the chunk-5a connection-state gate and
/// the chunk-5b per-tree permission gate have both passed. Retains only `Get`
/// (post-fetch tree gate) and `SetInstanceMetadata` (admin gate on
/// `_databases`). All other read/write paths have moved to
/// `dispatch_database_op`.
async fn dispatch_backend_op(
    instance: &Instance,
    acting_pubkey: &PublicKey,
    identity: &SigKey,
    op: BackendOp,
) -> crate::Result<ServiceResponse> {
    let backend = instance.backend();

    match op {
        BackendOp::Get { id } => {
            let entry = backend.get(&id).await?;
            gate_entry_read(instance, acting_pubkey, identity, &entry).await?;
            Ok(ServiceResponse::Entry(entry))
        }
        BackendOp::SetInstanceMetadata { metadata } => {
            backend.set_instance_metadata(&metadata).await?;
            Ok(ServiceResponse::Ok)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::database::InMemory;
    use crate::service::protocol::{Handshake, write_frame};

    /// Helper: start a server on a temp socket, return path + shutdown sender.
    async fn start_test_server() -> (PathBuf, watch::Sender<()>, Instance) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.keep().join("test.sock");
        let (instance, _admin) = Instance::create(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();
        let (tx, rx) = watch::channel(());
        let server = ServiceServer::new(instance.clone(), socket_path.clone());
        tokio::spawn(async move {
            let _ = server.run(rx).await;
        });
        // Wait for the socket to appear (server binds asynchronously). Poll
        // with a short sleep instead of a fixed delay so a slow sandbox
        // (where this test was occasionally flaky under `nix build`) doesn't
        // race the bind step.
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        (socket_path, tx, instance)
    }

    #[tokio::test]
    async fn test_server_starts_and_shuts_down() {
        let (socket_path, tx, _instance) = start_test_server().await;
        assert!(socket_path.exists());
        drop(tx);
        // Poll for cleanup with the same robustness as the bind wait.
        for _ in 0..50 {
            if !socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Socket should be cleaned up
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn test_wrong_protocol_version() {
        let (socket_path, _tx, _instance) = start_test_server().await;

        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);

        // Send wrong version
        let handshake = Handshake {
            protocol_version: 999,
        };
        write_frame(&mut writer, &handshake).await.unwrap();

        // Read ack (server sends its version back)
        let ack: Option<HandshakeAck> = read_frame(&mut reader).await.unwrap();
        let ack = ack.unwrap();
        assert_eq!(ack.protocol_version, PROTOCOL_VERSION);

        // Connection should be closed by server after version mismatch
        // Next read should get EOF
        let result: crate::Result<Option<ServiceResponse>> = read_frame(&mut reader).await;
        assert!(result.unwrap().is_none());
    }

    /// Load-bearing invariant: `ConnectionState` must never hold plaintext
    /// signing material in any variant. The daemon participates in storage
    /// and challenge-response only; the rejected Branch A design held
    /// decrypted user keys server-side and that boundary was reinstated by
    /// design (see Service Architecture doc § Decision record).
    ///
    /// Structure of the test: construct each variant, destructure with named
    /// fields (so adding a field forces this test to be edited), and check
    /// the static type of each field is not `PrivateKey`. A future refactor
    /// that adds e.g. `decrypted_root_key: PrivateKey` to `Authenticated`
    /// would either fail the type check at runtime or fail the destructure
    /// match exhaustiveness at compile time.
    #[test]
    fn connection_state_never_holds_private_key() {
        use crate::auth::crypto::{PrivateKey, generate_keypair};
        use std::any::TypeId;

        fn assert_not_private_key<T: 'static>(_value: &T, label: &str) {
            assert_ne!(
                TypeId::of::<T>(),
                TypeId::of::<PrivateKey>(),
                "ConnectionState field `{label}` is PrivateKey — daemon must not hold plaintext keys"
            );
        }

        let (_signing, pubkey) = generate_keypair();
        let states = [
            ConnectionState::PreAuth,
            ConnectionState::AwaitingProof {
                username: "u".to_string(),
                user_uuid: "uu".to_string(),
                challenge: vec![1, 2, 3],
                expected_pubkey: pubkey.clone(),
            },
            ConnectionState::Authenticated {
                username: "u".to_string(),
                user_uuid: "uu".to_string(),
                login_pubkey: pubkey.clone(),
                session_keyset: {
                    let mut s = HashSet::new();
                    s.insert(pubkey);
                    s
                },
                pending_key_challenges: HashMap::new(),
            },
        ];

        for state in &states {
            match state {
                ConnectionState::PreAuth => {}
                ConnectionState::AwaitingProof {
                    username,
                    user_uuid,
                    challenge,
                    expected_pubkey,
                } => {
                    assert_not_private_key(username, "AwaitingProof::username");
                    assert_not_private_key(user_uuid, "AwaitingProof::user_uuid");
                    assert_not_private_key(challenge, "AwaitingProof::challenge");
                    assert_not_private_key(expected_pubkey, "AwaitingProof::expected_pubkey");
                }
                ConnectionState::Authenticated {
                    username,
                    user_uuid,
                    login_pubkey,
                    session_keyset,
                    pending_key_challenges,
                } => {
                    assert_not_private_key(username, "Authenticated::username");
                    assert_not_private_key(user_uuid, "Authenticated::user_uuid");
                    assert_not_private_key(login_pubkey, "Authenticated::login_pubkey");
                    for k in session_keyset {
                        assert_not_private_key(k, "Authenticated::session_keyset entry");
                    }
                    for (k, ch) in pending_key_challenges {
                        assert_not_private_key(k, "Authenticated::pending_key_challenges key");
                        assert_not_private_key(
                            ch,
                            "Authenticated::pending_key_challenges challenge",
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_authenticated_request_rejected_without_login() {
        // Companion to the integration test `test_unauthenticated_backend_op_rejected`
        // — exercises the same gate path against the raw protocol so a
        // regression here surfaces immediately, not just at the
        // `RemoteConnection` layer.
        let (socket_path, _tx, _instance) = start_test_server().await;

        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);

        write_frame(
            &mut writer,
            &Handshake {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();
        let _ack: Option<HandshakeAck> = read_frame(&mut reader).await.unwrap();

        // Send an AuthenticatedDb request without completing TrustedLogin.
        write_frame(
            &mut writer,
            &ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
                root_id: crate::entry::ID::default(),
                identity: crate::auth::types::SigKey::default(),
                op: DatabaseOp::GetEntry {
                    id: crate::entry::ID::from_bytes("nonexistent"),
                },
            })),
        )
        .await
        .unwrap();

        let resp: Option<ServiceResponse> = read_frame(&mut reader).await.unwrap();
        match resp.unwrap() {
            ServiceResponse::Error(e) => {
                assert_eq!(
                    e.module, "auth",
                    "expected an auth-module error from the gate; got {e:?}"
                );
            }
            other => panic!("Expected gate Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_get_instance_metadata() {
        let (socket_path, _tx, _instance) = start_test_server().await;

        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);

        // Handshake
        write_frame(
            &mut writer,
            &Handshake {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();
        let _ack: Option<HandshakeAck> = read_frame(&mut reader).await.unwrap();

        // Request metadata
        write_frame(&mut writer, &ServiceRequest::GetInstanceMetadata)
            .await
            .unwrap();

        let resp: Option<ServiceResponse> = read_frame(&mut reader).await.unwrap();
        match resp.unwrap() {
            ServiceResponse::InstanceMetadata(Some(_meta)) => {
                // Server was initialized so metadata should exist
            }
            other => panic!("Expected InstanceMetadata(Some), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stale_socket_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        // Create a stale socket file
        tokio::fs::write(&socket_path, "stale").await.unwrap();
        assert!(socket_path.exists());

        let (instance, _admin) = Instance::create(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();
        let (_tx, rx) = watch::channel(());
        let server = ServiceServer::new(instance, socket_path.clone());

        // Server should remove stale socket and bind successfully
        let handle = tokio::spawn(async move { server.run(rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(socket_path.exists());
        // Verify it's actually a socket now by connecting
        let _stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

        handle.abort();
    }

    /// D8 regression: `require_existing` flips the create-flow passthrough
    /// to fail-closed. An absent target is waved through with `false` (the
    /// `Database::create` path) but denied with `true` (the
    /// `SetInstanceMetadata` admin gate), so an unreadable `_databases`
    /// can't become a fail-open hole.
    #[tokio::test]
    async fn test_gate_require_existing_fails_closed_on_absent_db() {
        use crate::auth::crypto::generate_keypair;

        let (instance, _admin) = Instance::create(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();
        let (_sk, pubkey) = generate_keypair();
        let absent = ID::from_bytes("no-such-tree");

        gate_tree_permission(
            &instance,
            &pubkey,
            &SigKey::default(),
            &absent,
            Permission::Admin(0),
            false,
        )
        .await
        .expect("create-flow passthrough must wave an absent tree through");

        let err = gate_tree_permission(
            &instance,
            &pubkey,
            &SigKey::default(),
            &absent,
            Permission::Admin(0),
            true,
        )
        .await
        .expect_err("require_existing must deny an absent tree");
        assert!(
            matches!(&err, crate::Error::Auth(b) if matches!(**b, AuthError::PermissionDenied { .. })),
            "expected PermissionDenied, got: {err:?}",
        );
    }
}
