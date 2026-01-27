//! Sync request handler trait and implementation.
//!
//! This module contains transport-agnostic handlers that process
//! sync requests and generate responses. These handlers can be
//! used by any transport implementation through the SyncHandler trait.

use async_trait::async_trait;
use tracing::{Instrument, debug, error, info, info_span, trace, warn};

use super::{
    ADMIN_KEY_NAME,
    bootstrap_request_manager::{BootstrapRequest, BootstrapRequestManager, RequestStatus},
    peer_manager::PeerManager,
    peer_types::Address,
    protocol::{
        BootstrapResponse, HandshakeRequest, HandshakeResponse, IncrementalResponse,
        PROTOCOL_VERSION, RequestContext, SyncRequest, SyncResponse, SyncTreeRequest, TreeInfo,
    },
    user_sync_manager::UserSyncManager,
};
use crate::{
    Database, Error, Instance, Result, WeakInstance,
    auth::{
        KeyStatus, Permission,
        crypto::{create_challenge_response, format_public_key, generate_challenge},
    },
    entry::{Entry, ID},
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

/// Default implementation of SyncHandler with database backend access.
pub struct SyncHandlerImpl {
    instance: WeakInstance,
    sync_tree_id: ID,
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
        }
    }

    /// Upgrade the weak instance reference to a strong reference.
    fn instance(&self) -> Result<Instance> {
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
        let signing_key = instance.device_key().clone();

        Database::open(
            self.instance()?,
            &self.sync_tree_id,
            signing_key,
            ADMIN_KEY_NAME.to_string(),
        )
        .await
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
        requesting_key: &str,
        requesting_key_name: &str,
        requested_permission: &Permission,
    ) -> Result<String> {
        let sync_tree = self.get_sync_tree().await?;
        let op = sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&op);

        let request = BootstrapRequest {
            tree_id: tree_id.clone(),
            requesting_pubkey: requesting_key.to_string(),
            requesting_key_name: requesting_key_name.to_string(),
            requested_permission: requested_permission.clone(),
            timestamp: self.instance()?.clock().now_rfc3339(),
            status: RequestStatus::Pending,
            // TODO: We need to get the actual peer address from the transport layer
            // For now, use a placeholder that will need to be fixed when implementing notifications
            peer_address: Address {
                transport_type: "unknown".to_string(),
                address: "unknown".to_string(),
            },
        };

        let request_id = manager.store_request(request).await?;
        op.commit().await?;

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

                // Get instance once before loop
                let instance = match self.instance() {
                    Ok(i) => i,
                    Err(e) => return SyncResponse::Error(format!("Instance dropped: {e}")),
                };

                // Store entries in the backend as unverified (from sync)
                let mut stored_count = 0usize;
                for entry in entries {
                    match instance.backend().put_unverified(entry.clone()).await {
                        Ok(_) => {
                            stored_count += 1;
                            trace!(entry_id = %entry.id(), "Stored entry successfully");
                        }
                        Err(e) => {
                            error!(entry_id = %entry.id(), error = %e, "Failed to store entry");
                            // Continue processing other entries rather than failing completely
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
        requesting_pubkey: &str,
    ) -> Result<Option<Permission>> {
        let database = Database::open_unauthenticated(tree_id.clone(), &self.instance()?)?;
        let transaction = database.new_transaction().await?;
        let settings_store = SettingsStore::new(&transaction)?;
        let auth_settings = settings_store.get_auth_settings().await?;

        let results = auth_settings.find_all_sigkeys_for_pubkey(requesting_pubkey);

        if results.is_empty() {
            return Ok(None);
        }

        // Results are sorted highest first, so take the first one
        Ok(Some(results[0].1.clone()))
    }

    /// Check if the requesting key already has sufficient permissions through existing auth.
    ///
    /// This uses the AuthSettings.can_access() method to check if the requesting key
    /// already has sufficient permissions (including through global '*' permissions).
    ///
    /// # Arguments
    /// * `tree_id` - The database/tree ID to check auth settings for
    /// * `requesting_pubkey` - The public key making the request
    /// * `requested_permission` - The permission level being requested
    ///
    /// # Returns
    /// - `Ok(true)` if key has sufficient permission
    /// - `Ok(false)` if key lacks sufficient permission or auth check fails
    async fn check_existing_auth_permission(
        &self,
        tree_id: &ID,
        requesting_pubkey: &str,
        requested_permission: &Permission,
    ) -> Result<bool> {
        // Use open_unauthenticated since we only need to read auth settings
        let database = Database::open_unauthenticated(tree_id.clone(), &self.instance()?)?;
        let settings_store = database.get_settings().await?;

        let auth_settings = settings_store.get_auth_settings().await?;

        // Use the AuthSettings.can_access() method to check permissions
        if auth_settings.can_access(requesting_pubkey, requested_permission) {
            debug!(
                tree_id = %tree_id,
                requesting_pubkey = %requesting_pubkey,
                requested_permission = ?requested_permission,
                "Key has sufficient permission for bootstrap access"
            );
            return Ok(true);
        }

        Ok(false)
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
        let database = Database::open_unauthenticated(tree_id.clone(), &self.instance()?)?;
        let transaction = database.new_transaction().await?;
        let settings_store = SettingsStore::new(&transaction)?;

        let auth_settings = settings_store.get_auth_settings().await?;

        // Check if auth settings is completely empty (no auth configured)
        if auth_settings.as_doc().is_empty() {
            debug!(
                tree_id = %tree_id,
                "Database has no auth configured - allowing unauthenticated access"
            );
            return Ok(false); // No auth required
        }

        // Auth is configured - check if there's an Active global "*" permission
        if let Ok(global_key) = auth_settings.get_key_by_pubkey("*")
            && *global_key.status() == KeyStatus::Active
        {
            debug!(
                tree_id = %tree_id,
                global_permission = ?global_key.permissions(),
                "Database has global '*' permission - allowing unauthenticated access"
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

        let signing_key = instance.device_key().clone();

        let sync_database = match Database::open(
            instance.clone(),
            &self.sync_tree_id,
            signing_key,
            ADMIN_KEY_NAME.to_string(),
        )
        .await
        {
            Ok(db) => db,
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
        peer_pubkey: &str,
        display_name: Option<&str>,
        advertised_addresses: &[Address],
        remote_address: &Option<Address>,
    ) -> Result<()> {
        let sync_tree = self.get_sync_tree().await?;
        let op = sync_tree.new_transaction().await?;
        let peer_manager = PeerManager::new(&op);

        // Try to register the peer (ignore if already exists)
        match peer_manager.register_peer(peer_pubkey, display_name).await {
            Ok(()) => {
                info!(peer_pubkey = %peer_pubkey, "Registered new incoming peer");
            }
            Err(Error::Sync(SyncError::PeerAlreadyExists(_))) => {
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

        op.commit().await?;
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
    async fn track_tree_sync_relationship(&self, tree_id: &ID, peer_pubkey: &str) -> Result<()> {
        let sync_tree = self.get_sync_tree().await?;
        let op = sync_tree.new_transaction().await?;
        let peer_manager = PeerManager::new(&op);

        // Add the tree sync relationship
        peer_manager.add_tree_sync(peer_pubkey, tree_id).await?;
        op.commit().await?;

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
            let signing_key = instance.device_key().clone();

            // Generate device ID and public key from signing key
            let verifying_key = signing_key.verifying_key();
            let public_key = format_public_key(&verifying_key);
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
                return self.handle_bootstrap_request(&request.tree_id,
                                                  request.requesting_key.as_deref(),
                                                  request.requesting_key_name.as_deref(),
                                                  request.requested_permission.clone()).await;
            }

            // Handle incremental sync (peer has existing data, needs updates)
            debug!(tree_id = %request.tree_id, peer_tips = request.our_tips.len(), "Handling incremental sync");
            self.handle_incremental_sync(&request.tree_id, &request.our_tips).await
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
    async fn handle_bootstrap_request(
        &self,
        tree_id: &ID,
        requesting_key: Option<&str>,
        requesting_key_name: Option<&str>,
        requested_permission: Option<Permission>,
    ) -> SyncResponse {
        // SECURITY: Check if database has sync enabled (FIRST CHECK - before anything else)
        // This prevents information leakage about database existence
        if !self.is_database_sync_enabled(tree_id).await {
            warn!(
                tree_id = %tree_id,
                "Sync request for non-sync-enabled database - rejecting as not found"
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
                warn!(tree_id = %tree_id, "Tree not found for bootstrap");
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
                    .check_existing_auth_permission(tree_id, key, &permission)
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
                            .store_bootstrap_request(tree_id, key, key_name, &permission)
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
            .filter(|entry| entry.id() != tree_id)
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

    /// Handle incremental sync request
    async fn handle_incremental_sync(&self, tree_id: &ID, peer_tips: &[ID]) -> SyncResponse {
        // SECURITY: Check if database has sync enabled (FIRST CHECK - before anything else)
        // This prevents information leakage about database existence
        if !self.is_database_sync_enabled(tree_id).await {
            warn!(
                tree_id = %tree_id,
                "Incremental sync request for non-sync-enabled database - rejecting as not found"
            );
            return SyncResponse::Error(format!("Tree not found: {tree_id}"));
        }

        // Get our current tips
        let instance = match self.instance() {
            Ok(i) => i,
            Err(e) => return SyncResponse::Error(format!("Instance dropped: {e}")),
        };
        let our_tips = match instance.backend().get_tips(tree_id).await {
            Ok(tips) => tips,
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

    /// Get list of available trees for discovery
    async fn get_available_trees(&self) -> Vec<TreeInfo> {
        // Get all root entries in the backend
        let instance = match self.instance() {
            Ok(i) => i,
            Err(e) => {
                error!(error = %e, "Failed to get instance");
                return Vec::new();
            }
        };
        match instance.backend().all_roots().await {
            Ok(roots) => {
                let mut tree_infos = Vec::new();
                for root_id in roots {
                    // Get basic tree info
                    if let Ok(entry_count) = self.count_tree_entries(&root_id).await {
                        tree_infos.push(TreeInfo {
                            tree_id: root_id,
                            name: None, // Could extract from tree metadata in the future
                            entry_count,
                            last_modified: 0, // Could track modification times in the future
                        });
                    }
                }
                tree_infos
            }
            Err(e) => {
                error!(error = %e, "Failed to get available trees");
                Vec::new()
            }
        }
    }

    /// Collect all entries in a tree (excluding the root)
    #[allow(dead_code)]
    async fn collect_all_tree_entries(&self, tree_id: &ID) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let tips = self.instance()?.backend().get_tips(tree_id).await?;
        to_visit.extend(tips);

        // Traverse the DAG depth-first
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) || entry_id == *tree_id {
                continue; // Skip root and already visited
            }
            visited.insert(entry_id.clone());

            match self.instance()?.backend().get(&entry_id).await {
                Ok(entry) => {
                    // Add parents to visit list
                    if let Ok(parent_ids) = entry.parents() {
                        for parent_id in parent_ids {
                            if !visited.contains(&parent_id) && parent_id != *tree_id {
                                to_visit.push_back(parent_id);
                            }
                        }
                    }
                    entries.push(entry);
                }
                Err(e) if e.is_not_found() => {
                    warn!(entry_id = %entry_id, "Entry not found during traversal");
                }
                Err(e) => {
                    error!(entry_id = %entry_id, error = %e, "Error during traversal");
                    return Err(e);
                }
            }
        }

        Ok(entries)
    }

    /// Collect ALL entries in a tree for bootstrap (including root)
    async fn collect_all_entries_for_bootstrap(&self, tree_id: &ID) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let tips = self.instance()?.backend().get_tips(tree_id).await?;
        to_visit.extend(tips);

        // Traverse the DAG depth-first, INCLUDING the root
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) {
                continue; // Skip already visited (but don't skip root)
            }
            visited.insert(entry_id.clone());

            match self.instance()?.backend().get(&entry_id).await {
                Ok(entry) => {
                    // Add parents to visit list
                    if let Ok(parent_ids) = entry.parents() {
                        for parent_id in parent_ids {
                            if !visited.contains(&parent_id) {
                                to_visit.push_back(parent_id);
                            }
                        }
                    }
                    entries.push(entry);
                }
                Err(e) if e.is_not_found() => {
                    warn!(entry_id = %entry_id, "Entry not found during traversal");
                }
                Err(e) => {
                    error!(entry_id = %entry_id, error = %e, "Error during traversal");
                    return Err(e);
                }
            }
        }

        // IMPORTANT: Reverse the entries so parents come before children
        // The traversal collects children first (starting from tips), but we need
        // to store parents first for proper tip tracking
        entries.reverse();

        Ok(entries)
    }

    /// Find entries that peer is missing
    async fn find_missing_entries_for_peer(
        &self,
        our_tips: &[ID],
        peer_tips: &[ID],
    ) -> Result<Vec<Entry>> {
        // Find tips they don't have
        let missing_tip_ids: Vec<_> = our_tips
            .iter()
            .filter(|tip_id| !peer_tips.contains(tip_id))
            .cloned()
            .collect();

        if missing_tip_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Collect ancestors
        super::utils::collect_ancestors_to_send(
            self.instance()?.backend().as_backend_impl(),
            &missing_tip_ids,
            peer_tips,
        )
        .await
    }

    /// Count entries in a tree
    async fn count_tree_entries(&self, tree_id: &ID) -> Result<usize> {
        let mut count = 1; // Include root
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let tips = self.instance()?.backend().get_tips(tree_id).await?;
        to_visit.extend(tips);

        // Count all entries
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) || entry_id == *tree_id {
                continue;
            }
            visited.insert(entry_id.clone());
            count += 1;

            if let Ok(entry) = self.instance()?.backend().get(&entry_id).await
                && let Ok(parent_ids) = entry.parents()
            {
                for parent_id in parent_ids {
                    if !visited.contains(&parent_id) && parent_id != *tree_id {
                        to_visit.push_back(parent_id);
                    }
                }
            }
        }

        Ok(count)
    }
}
