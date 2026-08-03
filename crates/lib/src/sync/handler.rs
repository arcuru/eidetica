//! Sync request handler trait and implementation.
//!
//! This module contains transport-agnostic handlers that process
//! sync requests and generate responses. These handlers can be
//! used by any transport implementation through the SyncHandler trait.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

use async_trait::async_trait;
use tracing::{Instrument, debug, error, info, info_span, trace, warn};

use super::{
    bootstrap_request_manager::{BootstrapRequest, BootstrapRequestManager, RequestStatus},
    peer_manager::PeerManager,
    peer_types::Address,
    protocol::{
        BootstrapResponse, HandshakeRequest, HandshakeResponse, IncrementalResponse,
        PROTOCOL_VERSION, RequestContext, SyncRequest, SyncResponse, SyncTreeRequest,
    },
    user_sync_manager::UserSyncManager,
};
use crate::{
    Database, Entry, Error, Instance, Result, WeakInstance,
    auth::{
        Permission,
        crypto::{PublicKey, create_challenge_response, generate_challenge},
    },
    crdt::Doc,
    entry::ID,
    store::SettingsStore,
    sync::error::SyncError,
};

/// Trait for handling sync requests with database access.
///
/// Implementations of this trait can process sync requests and generate
/// appropriate responses, with full access to the database backend for
/// storing and retrieving entries.
#[async_trait]
pub trait SyncHandler: Send + std::marker::Sync {
    /// Handle a sync request and generate an appropriate response.
    ///
    /// This is the main entry point for processing sync messages,
    /// regardless of which transport they arrived through.
    ///
    /// # Arguments
    /// * `request` - The sync request to process
    /// * `context` - Context about the request (remote address, etc.)
    ///
    /// # Returns
    /// The appropriate response for the given request.
    async fn handle_request(&self, request: &SyncRequest, context: &RequestContext)
    -> SyncResponse;
}

/// How far a request's timestamp may sit from our clock, in either direction.
///
/// Bounds how long a captured signature stays useful. Peers with clocks further
/// apart than this cannot sync — the window trades clock tolerance against
/// replay exposure.
const MAX_REQUEST_AGE_MS: u64 = 60_000;

/// Default implementation of SyncHandler with database backend access.
pub struct SyncHandlerImpl {
    instance: WeakInstance,
    sync_tree_id: ID,
    /// Nonces spent within the freshness window, keyed by the claiming key.
    ///
    /// Makes each signature single-use: without this, anyone who observes a
    /// signed request can replay it verbatim until it ages out.
    spent_nonces: Mutex<HashMap<(PublicKey, Vec<u8>), u64>>,
}

impl SyncHandlerImpl {
    /// Create a new SyncHandlerImpl with the given instance.
    ///
    /// # Arguments
    /// * `instance` - Database instance for storing and retrieving entries
    /// * `sync_tree_id` - Root ID of the sync database for storing bootstrap requests
    pub fn new(instance: Instance, sync_tree_id: ID) -> Self {
        Self {
            instance: instance.downgrade(),
            sync_tree_id,
            spent_nonces: Mutex::new(HashMap::new()),
        }
    }

    /// Authenticate a request that is about to be served data.
    ///
    /// Returns the key the caller has proven it holds. Checks, in order:
    /// the signature covers *this* request to *us*, the request is fresh, and
    /// its nonce has not been spent.
    ///
    /// This does not decide authorization — see [`Database::can_access`].
    fn authenticate_request(&self, request: &SyncTreeRequest) -> Result<PublicKey> {
        let auth = request
            .auth
            .as_ref()
            .ok_or_else(|| SyncError::AuthenticationRequired(request.tree_id.to_string()))?;

        let instance = self.instance()?;
        auth.verify(&instance.id(), &request.tree_id, &request.our_tips)
            .map_err(|_| {
                SyncError::AuthenticationFailed("invalid request signature".to_string())
            })?;

        let now = instance.clock().now_millis();
        if now.abs_diff(auth.timestamp_ms) > MAX_REQUEST_AGE_MS {
            return Err(SyncError::AuthenticationFailed(
                "request timestamp outside the freshness window".to_string(),
            )
            .into());
        }

        // The map holds one entry per verified request in the last window, so
        // it is bounded by request rate, not by uptime. Cap it if a peer can
        // ever outrun that.
        let mut spent = self
            .spent_nonces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        spent.retain(|_, spent_at| now.abs_diff(*spent_at) <= MAX_REQUEST_AGE_MS);
        if spent
            .insert((auth.key.clone(), auth.nonce.clone()), auth.timestamp_ms)
            .is_some()
        {
            return Err(
                SyncError::AuthenticationFailed("request nonce already spent".to_string()).into(),
            );
        }

        Ok(auth.key.clone())
    }

    /// Whether this request may be served entries from `tree_id`.
    ///
    /// A database with no auth configured, or with a global `*` grant, is
    /// world-readable by design and needs no credentials. Otherwise the caller
    /// must prove it holds a key that [`Database::can_access`] accepts —
    /// directly, through the global grant, or through a delegated tree.
    async fn authorize_read(&self, request: &SyncTreeRequest) -> Result<()> {
        if !self.check_if_database_has_auth(&request.tree_id).await? {
            return Ok(());
        }

        let key = self.authenticate_request(request)?;
        if !Database::can_access(&self.instance()?, &request.tree_id, &key, &Permission::Read)
            .await?
        {
            return Err(SyncError::PermissionDenied(format!(
                "key {key} is not authorized to read {}",
                request.tree_id
            ))
            .into());
        }

        Ok(())
    }

    /// Upgrade the weak instance reference to a strong reference.
    pub(super) fn instance(&self) -> Result<Instance> {
        self.instance
            .upgrade()
            .ok_or_else(|| SyncError::InstanceDropped.into())
    }

    /// Get access to the sync tree for bootstrap request management.
    ///
    /// # Returns
    /// A Database instance for the sync tree with device key authentication.
    async fn get_sync_tree(&self) -> Result<Database> {
        // Load sync tree with the device key
        let instance = self.instance()?;
        let signing_key = instance.signing_key()?.clone();
        Ok(Database::open(&instance, &self.sync_tree_id)
            .await?
            .with_key(signing_key))
    }

    /// Store a bootstrap request in the sync database for manual approval.
    ///
    /// # Arguments
    /// * `tree_id` - ID of the tree being requested
    /// * `requesting_key` - Public key of the requesting device
    /// * `requesting_key_name` - Name of the requesting key
    /// * `requested_permission` - Permission level being requested
    ///
    /// # Returns
    /// The generated UUID for the stored request
    async fn store_bootstrap_request(
        &self,
        tree_id: &ID,
        requesting_key: &PublicKey,
        requesting_key_name: &str,
        requested_permission: &Permission,
        metadata: Option<Doc>,
    ) -> Result<String> {
        let sync_tree = self.get_sync_tree().await?;
        let txn = sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&txn);

        let request = BootstrapRequest {
            tree_id: tree_id.clone(),
            requesting_pubkey: requesting_key.clone(),
            requesting_key_name: requesting_key_name.to_string(),
            requested_permission: *requested_permission,
            timestamp: self.instance()?.clock().now_rfc3339(),
            status: RequestStatus::Pending,
            // TODO: We need to get the actual peer address from the transport layer
            // For now, use a placeholder that will need to be fixed when implementing notifications
            peer_address: Address {
                transport_type: "unknown".to_string(),
                address: "unknown".to_string(),
            },
            // TODO(bootstrap-metadata-bound): `metadata` is unbounded, remote-supplied
            // data persisted to the system sync tree before any approval decision. A
            // hostile peer can spam large/many pending requests (storage/DoS on the
            // multi-tenant boundary). Bound the metadata size here, and cap pending
            // requests per peer, before this is exposed to untrusted peers. Note the
            // `Doc` is already deserialized at the protocol layer ahead of the
            // sync-enabled gate, so the size cap ideally belongs there too.
            metadata,
        };

        let request_id = manager.store_request(request).await?;
        txn.commit().await?;

        Ok(request_id)
    }
}

#[async_trait]
impl SyncHandler for SyncHandlerImpl {
    async fn handle_request(
        &self,
        request: &SyncRequest,
        context: &RequestContext,
    ) -> SyncResponse {
        match request {
            SyncRequest::Handshake(handshake_req) => {
                debug!("Received handshake request");
                self.handle_handshake(handshake_req, context).await
            }
            SyncRequest::SyncTree(sync_req) => {
                debug!(tree_id = %sync_req.tree_id, tips_count = sync_req.our_tips.len(), "Received sync tree request");
                self.handle_sync_tree(sync_req, context).await
            }
            SyncRequest::SendEntries(entries) => {
                // Process and store the received entries
                let count = entries.len();
                info!(count = count, "Received entries for synchronization");

                let instance = match self.instance() {
                    Ok(i) => i,
                    Err(e) => return SyncResponse::Error(format!("Instance dropped: {e}")),
                };

                // Group entries by tree_id so we can fire callbacks per-database.
                // BTreeMap so iteration order is deterministic (sorted by id);
                // sender order within a tree is preserved by per-tree push order.
                //
                // Root entries declare an empty `tree.root` and act as their
                // own tree_id. Non-root entries always carry a tree_id;
                // well-formed peers should never send a non-root entry with
                // `root() == None`. If they do, the entry ends up filed under
                // its own id and parent-existence checks downstream reject it.
                let mut by_tree: BTreeMap<ID, Vec<Entry>> = BTreeMap::new();
                for entry in entries {
                    let tree_id = entry.root().unwrap_or_else(|| entry.id());
                    by_tree.entry(tree_id).or_default().push(entry.clone());
                }

                let mut stored_count = 0usize;
                for (tree_id, tree_entries) in by_tree {
                    let batch_size = tree_entries.len();
                    // Entries arrive over the wire without per-entry signature
                    // verification; `put_remote_entries` stores them
                    // Unverified so a future re-verification pass can promote
                    // them.
                    match instance.put_remote_entries(&tree_id, tree_entries).await {
                        Ok(n) => {
                            stored_count += n;
                            debug!(tree_id = %tree_id, requested = batch_size, stored = n, "Stored entries");
                        }
                        Err(e) => {
                            error!(tree_id = %tree_id, error = %e, "Failed to store entries batch");
                        }
                    }
                }

                debug!(
                    received = count,
                    stored = stored_count,
                    "Completed entry synchronization"
                );
                if count <= 1 {
                    SyncResponse::Ack
                } else {
                    SyncResponse::Count(stored_count)
                }
            }
        }
    }
}

impl SyncHandlerImpl {
    /// Get the highest permission level a key has in the database's auth settings.
    ///
    /// This looks up all permissions the key has (direct + global wildcard) and returns
    /// the highest one. Used for auto-detecting permissions during bootstrap.
    ///
    /// # Arguments
    /// * `tree_id` - The database/tree ID to check auth settings for
    /// * `requesting_pubkey` - The public key to look up
    ///
    /// # Returns
    /// - `Ok(Some(Permission))` if key has any permissions
    /// - `Ok(None)` if key not found in auth settings
    /// - `Err` if database access fails
    async fn get_key_highest_permission(
        &self,
        tree_id: &ID,
        requesting_pubkey: &PublicKey,
    ) -> Result<Option<Permission>> {
        let database = Database::open(&self.instance()?, tree_id).await?;
        let transaction = database.new_transaction().await?;
        let settings_store = SettingsStore::new(&transaction)?;
        let auth_settings = settings_store.auth_snapshot().await?;

        let results = auth_settings.find_all_sigkeys_for_pubkey(requesting_pubkey);

        if results.is_empty() {
            return Ok(None);
        }

        // Results are sorted highest first, so take the first one
        Ok(Some(results[0].1))
    }

    /// Check that the caller signed this request with `claimed_key`.
    ///
    /// `requesting_key` is a name the client picks; it decides which key an
    /// approval would grant, and on its own it must never unlock data. Anything
    /// that serves entries on the strength of a claimed key needs this first.
    fn prove_possession_if_required(
        &self,
        request: &SyncTreeRequest,
        claimed_key: &PublicKey,
        auth_configured: bool,
    ) -> Result<()> {
        if !auth_configured {
            // World-readable database: nothing is being protected, so there is
            // nothing to prove.
            return Ok(());
        }
        self.prove_possession(request, claimed_key)
    }

    /// Check that the caller signed this request with `claimed_key`.
    fn prove_possession(&self, request: &SyncTreeRequest, claimed_key: &PublicKey) -> Result<()> {
        let proven = self.authenticate_request(request)?;
        if proven != *claimed_key {
            return Err(SyncError::AuthenticationFailed(format!(
                "request is signed by {proven}, which is not the claimed key {claimed_key}"
            ))
            .into());
        }
        Ok(())
    }

    /// Check if the caller holds a key that already has sufficient permissions.
    ///
    /// Possession first, then authority. Authority resolves through
    /// [`Database::can_access`], the pubkey-only access decision, which covers
    /// direct grants, the global `*` grant, and authority that reaches this
    /// tree only through a *delegated* tree. Without the delegated case a
    /// delegated-only key is bounced to manual approval and hangs.
    ///
    /// Delegation discovery is **one hop deep**: a key reachable only through a
    /// chain of delegations still falls through to manual approval. See
    /// [`Database::can_access`] for why bootstrap searches where entry
    /// validation walks a named path.
    ///
    /// # Returns
    /// - `Ok(true)` if the caller proved a key with sufficient permission
    /// - `Ok(false)` if possession failed, or the key lacks permission
    async fn check_proven_auth_permission(
        &self,
        request: &SyncTreeRequest,
        requesting_pubkey: &PublicKey,
        requested_permission: &Permission,
        auth_configured: bool,
    ) -> Result<bool> {
        let tree_id = &request.tree_id;
        if let Err(e) =
            self.prove_possession_if_required(request, requesting_pubkey, auth_configured)
        {
            warn!(
                tree_id = %tree_id,
                requesting_pubkey = %requesting_pubkey,
                error = %e,
                "Bootstrap key claim not proven - falling back to the approval queue"
            );
            return Ok(false);
        }

        let granted = Database::can_access(
            &self.instance()?,
            tree_id,
            requesting_pubkey,
            requested_permission,
        )
        .await?;
        if granted {
            debug!(
                tree_id = %tree_id,
                requesting_pubkey = %requesting_pubkey,
                requested_permission = ?requested_permission,
                "Key has sufficient permission for bootstrap access"
            );
        }
        Ok(granted)
    }

    /// Check if a database requires authentication for unauthenticated requests.
    ///
    /// This method checks if the database requires authentication for bootstrap requests
    /// that don't provide credentials. A database allows unauthenticated access if:
    /// 1. It has no auth settings configured at all (empty auth), OR
    /// 2. It has a global `*` permission configured that allows unauthenticated access
    ///
    /// # Arguments
    /// * `tree_id` - The database/tree ID to check auth configuration for
    ///
    /// # Returns
    /// - `Ok(true)` if database requires authentication (has auth but no global permission)
    /// - `Ok(false)` if database allows unauthenticated access (no auth or has global permission)
    /// - `Err` if the check fails
    async fn check_if_database_has_auth(&self, tree_id: &ID) -> Result<bool> {
        let database = Database::open(&self.instance()?, tree_id).await?;
        let transaction = database.new_transaction().await?;
        let settings_store = SettingsStore::new(&transaction)?;

        let auth_settings = settings_store.auth_snapshot().await?;

        // Check if auth settings is completely empty (no auth configured)
        if auth_settings.as_doc().is_empty() {
            debug!(
                tree_id = %tree_id,
                "Database has no auth configured - allowing unauthenticated access"
            );
            return Ok(false); // No auth required
        }

        // Auth is configured - check if there's an Active global permission
        if let Ok(global_key) = auth_settings.get_global_key()
            && global_key.is_active()
        {
            debug!(
                tree_id = %tree_id,
                global_permission = ?global_key.permissions(),
                "Database has global permission - allowing unauthenticated access"
            );
            return Ok(false); // Global permission allows unauthenticated access
        }

        // Auth is configured but no global permission - require authentication
        debug!(
            tree_id = %tree_id,
            "Database has auth configured without global permission - requiring authentication"
        );
        Ok(true) // Auth required
    }

    /// Check if a database has sync enabled by at least one user.
    ///
    /// This is a security-critical check that determines if a database should accept
    /// any sync requests at all. A database is only eligible for sync if at least one
    /// user has it in their preferences with `sync_enabled: true`.
    ///
    /// # Security
    /// This method implements fail-closed behavior:
    /// - Returns `false` on any error (no information leakage)
    /// - Returns `false` if no users have the database in preferences
    /// - Returns `false` if combined_settings.sync_enabled is false
    /// - Only returns `true` if explicitly enabled
    ///
    /// # Arguments
    /// * `tree_id` - The ID of the database to check
    ///
    /// # Returns
    /// `true` if the database has sync enabled, `false` otherwise (including errors)
    async fn is_database_sync_enabled(&self, tree_id: &ID) -> bool {
        let instance = match self.instance() {
            Ok(i) => i,
            Err(_) => return false, // Fail closed
        };

        let signing_key = match instance.signing_key() {
            Ok(k) => k.clone(),
            Err(_) => return false, // Fail closed
        };

        let sync_database = match Database::open(&instance, &self.sync_tree_id).await {
            Ok(db) => db.with_key(signing_key),
            Err(_) => return false, // Fail closed
        };

        let transaction = match sync_database.new_transaction().await {
            Ok(tx) => tx,
            Err(_) => return false, // Fail closed
        };

        // Use UserSyncManager to get combined settings
        let user_mgr = UserSyncManager::new(&transaction);
        match user_mgr.get_combined_settings(tree_id).await {
            Ok(Some(settings)) => settings.sync_enabled,
            _ => false, // Fail closed: no settings or error
        }
    }

    /// Register an incoming peer and add their addresses to the peer list.
    ///
    /// This method registers a peer that initiated a connection to us during handshake.
    /// It adds both the peer-advertised addresses and the transport-provided remote address.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `display_name` - Optional display name for the peer
    /// * `advertised_addresses` - Addresses the peer advertised in their handshake
    /// * `remote_address` - The actual address from which the connection originated
    ///
    /// # Returns
    /// Result indicating success or failure of registration
    async fn register_incoming_peer(
        &self,
        peer_pubkey: &PublicKey,
        display_name: Option<&str>,
        advertised_addresses: &[Address],
        remote_address: &Option<Address>,
    ) -> Result<()> {
        let sync_tree = self.get_sync_tree().await?;
        let txn = sync_tree.new_transaction().await?;
        let peer_manager = PeerManager::new(&txn);

        // Try to register the peer (ignore if already exists)
        match peer_manager.register_peer(peer_pubkey, display_name).await {
            Ok(()) => {
                info!(peer_pubkey = %peer_pubkey, "Registered new incoming peer");
            }
            Err(Error::Sync(ref e)) if matches!(**e, SyncError::PeerAlreadyExists(_)) => {
                debug!(peer_pubkey = %peer_pubkey, "Peer already registered, updating addresses");
            }
            Err(e) => return Err(e),
        }

        // Add all advertised addresses
        for addr in advertised_addresses {
            if let Err(e) = peer_manager.add_address(peer_pubkey, addr.clone()).await {
                warn!(peer_pubkey = %peer_pubkey, address = ?addr, error = %e, "Failed to add advertised address");
            }
        }

        // Add the remote address from transport if available
        if let Some(addr) = remote_address
            && let Err(e) = peer_manager.add_address(peer_pubkey, addr.clone()).await
        {
            warn!(peer_pubkey = %peer_pubkey, address = ?addr, error = %e, "Failed to add remote address");
        }

        txn.commit().await?;
        Ok(())
    }

    /// Track tree/peer sync relationship when a peer requests a tree.
    ///
    /// This method adds the tree to the peer's sync list, enabling bidirectional
    /// sync for the requested tree. This is critical for `sync_on_commit` to work
    /// in both directions.
    ///
    /// # Arguments
    /// * `tree_id` - The ID of the tree being requested
    /// * `peer_pubkey` - The public key of the peer requesting the tree (device key, not auth key)
    ///
    /// # Returns
    /// Result indicating success or failure
    async fn track_tree_sync_relationship(
        &self,
        tree_id: &ID,
        peer_pubkey: &PublicKey,
    ) -> Result<()> {
        let sync_tree = self.get_sync_tree().await?;
        let txn = sync_tree.new_transaction().await?;
        let peer_manager = PeerManager::new(&txn);

        // Add the tree sync relationship
        peer_manager.add_tree_sync(peer_pubkey, tree_id).await?;
        txn.commit().await?;

        debug!(tree_id = %tree_id, peer_pubkey = %peer_pubkey, "Tracked tree/peer sync relationship");
        Ok(())
    }

    /// Handle a handshake request from a peer.
    async fn handle_handshake(
        &self,
        request: &HandshakeRequest,
        context: &RequestContext,
    ) -> SyncResponse {
        async move {
            debug!(
                peer_device_id = %request.device_id,
                peer_public_key = %request.public_key,
                display_name = ?request.display_name,
                protocol_version = request.protocol_version,
                "Processing handshake request"
            );

            // Check protocol version compatibility
            if request.protocol_version != PROTOCOL_VERSION {
                warn!(
                    expected = PROTOCOL_VERSION,
                    received = request.protocol_version,
                    "Protocol version mismatch"
                );
                return SyncResponse::Error(format!(
                    "Protocol version mismatch: expected {}, got {}",
                    PROTOCOL_VERSION, request.protocol_version
                ));
            }

            // Get device signing key from backend
            let instance = match self.instance() {
                Ok(i) => i,
                Err(e) => {
                    error!(error = %e, "Failed to get instance");
                    return SyncResponse::Error(format!("Failed to get instance: {e}"));
                }
            };
            let signing_key = match instance.signing_key() {
                Ok(k) => k.clone(),
                Err(e) => {
                    error!(error = %e, "Failed to get device key");
                    return SyncResponse::Error(format!("Failed to get device key: {e}"));
                }
            };

            // Generate device ID and public key from signing key
            let public_key = signing_key.public_key();
            let device_id = public_key.clone(); // Device ID is the public key

            // Sign the challenge with our device key to prove identity
            let challenge_response = create_challenge_response(&request.challenge, &signing_key);

            // Generate a new challenge for mutual authentication
            let new_challenge = generate_challenge();

            // Get available trees for discovery
            let available_trees = self.get_available_trees().await;

            // Register the peer and add their addresses to our peer list
            match self.register_incoming_peer(&request.public_key, request.display_name.as_deref(), &request.listen_addresses, &context.remote_address).await {
                Ok(()) => {
                    debug!(peer_pubkey = %request.public_key, "Successfully registered incoming peer");
                }
                Err(e) => {
                    // Log the error but don't fail the handshake - peer registration is best-effort
                    warn!(peer_pubkey = %request.public_key, error = %e, "Failed to register incoming peer");
                }
            }

            info!(
                our_device_id = %device_id,
                peer_device_id = %request.device_id,
                tree_count = available_trees.len(),
                "Handshake completed successfully"
            );

            SyncResponse::Handshake(HandshakeResponse {
                device_id,
                public_key,
                display_name: Some("Eidetica Peer".to_string()),
                protocol_version: PROTOCOL_VERSION,
                challenge_response,
                new_challenge,
                available_trees,
            })
        }
        .instrument(info_span!("handle_handshake", peer = %request.device_id))
        .await
    }

    /// Handle a unified sync tree request (bootstrap or incremental).
    ///
    /// This method routes between two sync modes:
    /// 1. **Bootstrap**: When peer has no tips (empty database), sends complete tree
    /// 2. **Incremental**: When peer has existing tips, sends only new entries
    ///
    /// # Bootstrap Authentication
    /// During bootstrap, if the peer provides authentication credentials:
    /// - `requesting_key`: Public key to add
    /// - `requesting_key_name`: Name for the key
    /// - `requested_permission`: Access level requested
    ///
    /// The handler will evaluate the bootstrap policy and either:
    /// - Auto-approve and add the key immediately
    /// - Store request for manual approval
    /// - Proceed without authentication (anonymous bootstrap)
    async fn handle_sync_tree(
        &self,
        request: &SyncTreeRequest,
        context: &RequestContext,
    ) -> SyncResponse {
        async move {
            trace!(tree_id = %request.tree_id, "Processing sync tree request");

            // Track tree/peer sync relationship for bidirectional sync
            // IMPORTANT: Only use context.peer_pubkey (device key from handshake)
            // Do NOT use request.requesting_key (that's an auth key for database access)
            if let Some(peer_pubkey) = &context.peer_pubkey {
                if let Err(e) = self.track_tree_sync_relationship(&request.tree_id, peer_pubkey).await {
                    // Log the error but don't fail the sync - relationship tracking is best-effort
                    warn!(tree_id = %request.tree_id, peer_pubkey = %peer_pubkey, error = %e, "Failed to track tree/peer relationship");
                }
            } else {
                debug!(tree_id = %request.tree_id, "No peer pubkey in context, skipping relationship tracking");
            }

            // Check if peer needs bootstrap (empty tips indicates no local data)
            if request.our_tips.is_empty() {
                debug!(tree_id = %request.tree_id, "Peer needs bootstrap - sending full tree");
                return self.handle_bootstrap_request(request).await;
            }

            // Handle incremental sync (peer has existing data, needs updates)
            debug!(tree_id = %request.tree_id, peer_tips = request.our_tips.len(), "Handling incremental sync");
            self.handle_incremental_sync(request).await
        }
        .instrument(info_span!("handle_sync_tree", tree = %request.tree_id))
        .await
    }

    /// Handle bootstrap request by sending complete tree state and optionally approving auth key.
    ///
    /// Bootstrap is the initial synchronization when a peer has no local data for a tree.
    /// This method:
    /// 1. Validates the tree exists and sync is enabled
    /// 2. Processes authentication and permission resolution
    /// 3. Sends all entries from the tree to the peer
    ///
    /// # Authentication Flow
    ///
    /// The bootstrap process handles three authentication scenarios:
    ///
    /// ## 1. Explicit Permission Request
    /// When all three auth parameters are provided (`requesting_key`, `requesting_key_name`, `requested_permission`):
    /// - Check if key already has sufficient permissions
    /// - If yes: Approve immediately without adding key
    /// - If no: Store request for manual approval and return `BootstrapPending`
    ///
    /// ## 2. Auto-Detection
    /// When key is provided but `requested_permission` is `None`:
    /// - Look up key's existing permissions in database auth settings
    /// - Uses `find_all_sigkeys_for_pubkey()` to find all permissions (direct + global wildcard)
    /// - If key found: Use highest available permission and approve immediately
    /// - If key not found: Reject with authentication error
    ///
    /// ## 3. Unauthenticated Access
    /// When no `requesting_key` is provided:
    /// - Only allowed if database has no auth configured or has global wildcard permission
    /// - Otherwise rejected with authentication required error
    ///
    /// # Note on Key Verification
    ///
    /// This function does not verify that the peer actually controls the `requesting_key`.
    /// The `requesting_key` parameter is an unverified string from the client.
    ///
    /// **This is not a security vulnerability** because:
    /// - Approval only adds the public key to database auth settings
    /// - Actual database access requires signing entries with the corresponding private key
    /// - If an attacker claims someone else's public key, approval grants access to the
    ///   legitimate key holder (who has the private key), not the attacker
    ///
    /// The lack of verification may cause:
    /// - Audit trail confusion (request appears to come from a different identity)
    /// - Admins approving access for keys that didn't actually request it
    ///
    /// # Arguments
    /// * `tree_id` - The database/tree to bootstrap
    /// * `requesting_key` - Optional public key requesting access (unverified, but safe - see above)
    /// * `requesting_key_name` - Optional name/identifier for the key (unverified)
    /// * `requested_permission` - Optional permission level requested (if None, auto-detects from auth settings)
    ///
    /// # Returns
    /// - `BootstrapResponse`: Contains entries and approval status (key_approved, granted_permission)
    /// - `BootstrapPending`: Manual approval required (request queued)
    /// - `Error`: Tree not found, auth required, key not authorized, or processing failure
    async fn handle_bootstrap_request(&self, request: &SyncTreeRequest) -> SyncResponse {
        let tree_id = &request.tree_id;
        let requesting_key = request.requesting_key.as_ref();
        let requesting_key_name = request.requesting_key_name.as_deref();
        let requested_permission = request.requested_permission;
        let metadata = request.metadata.clone();

        // SECURITY: Check if database has sync enabled (FIRST CHECK - before anything else)
        // This prevents information leakage about database existence: the gate
        // returns false both for databases that are absent and for databases that
        // are present-but-not-tracked-for-sync, and we deliberately respond with
        // the same opaque "Tree not found" to peers in either case.
        if !self.is_database_sync_enabled(tree_id).await {
            warn!(
                tree_id = %tree_id,
                requesting_key = ?requesting_key,
                requesting_key_name = ?requesting_key_name,
                "Bootstrap request rejected: database is absent or has no user with sync enabled (responding as not-found)"
            );
            return SyncResponse::Error(format!("Tree not found: {tree_id}"));
        }

        // Get the root entry (to verify tree exists)
        let instance = match self.instance() {
            Ok(i) => i,
            Err(e) => return SyncResponse::Error(format!("Instance dropped: {e}")),
        };
        let _root_entry = match instance.backend().get(tree_id).await {
            Ok(entry) => entry,
            Err(e) if e.is_not_found() => {
                warn!(
                    tree_id = %tree_id,
                    requesting_key = ?requesting_key,
                    requesting_key_name = ?requesting_key_name,
                    "Bootstrap request rejected: a user has this tree marked sync-enabled but the backend has no root entry for it"
                );
                return SyncResponse::Error(format!("Tree not found: {tree_id}"));
            }
            Err(e) => {
                error!(tree_id = %tree_id, error = %e, "Failed to get root entry");
                return SyncResponse::Error(format!("Failed to get tree root: {e}"));
            }
        };

        // Check if database has authentication configured
        let auth_configured = match self.check_if_database_has_auth(tree_id).await {
            Ok(has_auth) => has_auth,
            Err(e) => {
                error!(tree_id = %tree_id, error = %e, "Failed to check if database has auth");
                return SyncResponse::Error(format!("Failed to check database auth: {e}"));
            }
        };

        // If auth is configured but no credentials provided, reject the request
        if auth_configured && requesting_key.is_none() {
            warn!(
                tree_id = %tree_id,
                "Unauthenticated bootstrap request rejected - database requires authentication"
            );
            return SyncResponse::Error(
                "Authentication required: This database requires authenticated access. \
                 Please provide credentials (requesting_key, requesting_key_name, requested_permission) \
                 to bootstrap sync.".to_string()
            );
        }

        // Handle key approval for bootstrap requests FIRST
        let (key_approved, granted_permission) = match (
            requesting_key,
            requesting_key_name,
            requested_permission,
        ) {
            // Case 1: All three parameters provided - explicit permission request
            (Some(key), Some(key_name), Some(permission)) => {
                info!(
                    tree_id = %tree_id,
                    requesting_key = %key,
                    key_name = %key_name,
                    requested_permission = ?permission,
                    "Processing key approval request for bootstrap"
                );

                // Check if the requesting key already has sufficient permissions through existing auth
                match self
                    .check_proven_auth_permission(request, key, &permission, auth_configured)
                    .await
                {
                    Ok(true) => {
                        // Key already has sufficient permission - approve without adding
                        info!(
                            tree_id = %tree_id,
                            key = %key,
                            permission = ?permission,
                            "Bootstrap approved via existing auth permission - no key added"
                        );
                        (true, Some(permission))
                    }
                    Ok(false) => {
                        // No existing permission, store request for manual approval
                        info!(tree_id = %tree_id, "Bootstrap key approval requested - storing for manual approval");

                        // Store the bootstrap request in sync database for manual approval
                        match self
                            .store_bootstrap_request(tree_id, key, key_name, &permission, metadata)
                            .await
                        {
                            Ok(request_id) => {
                                info!(
                                    tree_id = %tree_id,
                                    request_id = %request_id,
                                    "Bootstrap request stored for manual approval"
                                );
                                return SyncResponse::BootstrapPending {
                                    request_id,
                                    message: "Bootstrap request pending manual approval"
                                        .to_string(),
                                };
                            }
                            Err(e) => {
                                error!(
                                    tree_id = %tree_id,
                                    error = %e,
                                    "Failed to store bootstrap request"
                                );
                                return SyncResponse::Error(format!(
                                    "Failed to store bootstrap request: {e}"
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        error!(tree_id = %tree_id, error = %e, "Failed to check global permission for bootstrap");
                        return SyncResponse::Error(format!("Global permission check failed: {e}"));
                    }
                }
            }

            // Case 2: Key provided but permission not specified - auto-detect from auth settings
            (Some(key), Some(_key_name), None) => {
                info!(
                    tree_id = %tree_id,
                    requesting_key = %key,
                    "Auto-detecting permission from auth settings for bootstrap request"
                );

                if let Err(e) = self.prove_possession_if_required(request, key, auth_configured) {
                    warn!(
                        tree_id = %tree_id,
                        requesting_key = %key,
                        error = %e,
                        "Bootstrap request rejected: caller did not prove it holds the key it claims"
                    );
                    return SyncResponse::Error(e.to_string());
                }

                match self.get_key_highest_permission(tree_id, key).await {
                    Ok(Some(permission)) => {
                        info!(
                            tree_id = %tree_id,
                            requesting_key = %key,
                            detected_permission = ?permission,
                            "Approved bootstrap using auto-detected permission from auth settings"
                        );
                        (true, Some(permission))
                    }
                    Ok(None) => {
                        warn!(
                            tree_id = %tree_id,
                            requesting_key = %key,
                            "Key not found in auth settings - rejecting bootstrap request"
                        );
                        return SyncResponse::Error(
                            "Authentication required: provided key is not authorized for this database".to_string()
                        );
                    }
                    Err(e) => {
                        error!(
                            tree_id = %tree_id,
                            requesting_key = %key,
                            error = %e,
                            "Failed to lookup key permissions"
                        );
                        return SyncResponse::Error(format!("Failed to access auth settings: {e}"));
                    }
                }
            }

            // Case 3: No key provided, or key provided without key_name - unauthenticated access
            _ => {
                debug!(
                    tree_id = %tree_id,
                    "No authentication credentials provided - proceeding with unauthenticated bootstrap"
                );
                (false, None)
            }
        };

        // A database with auth configured serves entries only to a caller that
        // proved it holds a key with read access. Cases that fall through
        // without approval (no credentials, or a key with no key name) must not
        // be served just because they reached this point.
        if auth_configured && !key_approved {
            warn!(
                tree_id = %tree_id,
                requesting_key = ?requesting_key,
                "Bootstrap request rejected: no proven authority for a database that requires authentication"
            );
            return SyncResponse::Error(
                SyncError::AuthenticationRequired(tree_id.to_string()).to_string(),
            );
        }

        // NOW collect all entries after key approval (so we get the updated database state)
        let all_entries = match self.collect_all_entries_for_bootstrap(tree_id).await {
            Ok(entries) => entries,
            Err(e) => {
                error!(tree_id = %tree_id, error = %e, "Failed to collect all entries for bootstrap after key approval");
                return SyncResponse::Error(format!(
                    "Failed to collect all entries for bootstrap: {e}"
                ));
            }
        };

        // For bootstrap, we need to send the actual root entry (tree_id) as root_entry
        // The root_entry should always be the tree's root, not a tip
        let instance = match self.instance() {
            Ok(i) => i,
            Err(e) => return SyncResponse::Error(format!("Instance dropped: {e}")),
        };
        let root_entry = match instance.backend().get(tree_id).await {
            Ok(entry) => entry,
            Err(e) => {
                error!(tree_id = %tree_id, error = %e, "Failed to get root entry");
                return SyncResponse::Error(format!("Failed to get root entry: {e}"));
            }
        };

        // Filter out the root from all_entries since we send it separately as root_entry
        let other_entries: Vec<_> = all_entries
            .into_iter()
            .filter(|entry| entry.id() != *tree_id)
            .collect();

        info!(
            tree_id = %tree_id,
            entry_count = other_entries.len() + 1,
            key_approved = key_approved,
            "Sending bootstrap response"
        );

        SyncResponse::Bootstrap(BootstrapResponse {
            tree_id: tree_id.clone(),
            root_entry,
            all_entries: other_entries,
            key_approved,
            granted_permission,
        })
    }

    /// Handle incremental sync request.
    ///
    /// The caller selects this path by sending any non-empty tip list, so it
    /// enforces the same read policy bootstrap does. Without that, a single
    /// fabricated tip — which matches nothing in our DAG and therefore never
    /// stops the ancestor walk — returns the entire tree.
    async fn handle_incremental_sync(&self, request: &SyncTreeRequest) -> SyncResponse {
        let tree_id = &request.tree_id;
        let peer_tips = request.our_tips.tips();

        // SECURITY: Check if database has sync enabled (FIRST CHECK - before anything else)
        // This prevents information leakage about database existence: the gate
        // returns false both for databases that are absent and for databases that
        // are present-but-not-tracked-for-sync, and we deliberately respond with
        // the same opaque "Tree not found" to peers in either case.
        if !self.is_database_sync_enabled(tree_id).await {
            warn!(
                tree_id = %tree_id,
                peer_tip_count = peer_tips.len(),
                "Incremental sync request rejected: database is absent or has no user with sync enabled (responding as not-found)"
            );
            return SyncResponse::Error(format!("Tree not found: {tree_id}"));
        }

        if let Err(e) = self.authorize_read(request).await {
            warn!(
                tree_id = %tree_id,
                peer_tip_count = peer_tips.len(),
                error = %e,
                "Incremental sync request rejected: caller is not authorized to read this database"
            );
            return SyncResponse::Error(e.to_string());
        }

        // Get our current tips
        let instance = match self.instance() {
            Ok(i) => i,
            Err(e) => return SyncResponse::Error(format!("Instance dropped: {e}")),
        };
        let our_tips: Vec<ID> = match instance.backend().snapshot(tree_id).await {
            Ok(snap) => snap.into_tips(),
            Err(e) => {
                error!(tree_id = %tree_id, error = %e, "Failed to get our tips");
                return SyncResponse::Error(format!("Failed to get tips: {e}"));
            }
        };

        // Find entries peer is missing
        let missing_entries = match self
            .find_missing_entries_for_peer(&our_tips, peer_tips)
            .await
        {
            Ok(entries) => entries,
            Err(e) => {
                error!(tree_id = %tree_id, error = %e, "Failed to find missing entries");
                return SyncResponse::Error(format!("Failed to find missing entries: {e}"));
            }
        };

        debug!(
            tree_id = %tree_id,
            our_tips = our_tips.len(),
            peer_tips = peer_tips.len(),
            missing_count = missing_entries.len(),
            "Sending incremental sync response"
        );

        SyncResponse::Incremental(IncrementalResponse {
            tree_id: tree_id.clone(),
            their_tips: our_tips,
            missing_entries,
        })
    }
}
