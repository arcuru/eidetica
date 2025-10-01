//! Sync request handler trait and implementation.
//!
//! This module contains transport-agnostic handlers that process
//! sync requests and generate responses. These handlers can be
//! used by any transport implementation through the SyncHandler trait.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{Instrument, debug, error, info, info_span, trace, warn};

use super::{
    bootstrap_request_manager::{
        BootstrapRequest, BootstrapRequestManager, RequestStatus, current_timestamp,
    },
    peer_types::Address,
    protocol::{
        BootstrapResponse, HandshakeRequest, HandshakeResponse, IncrementalResponse,
        PROTOCOL_VERSION, SyncRequest, SyncResponse, SyncTreeRequest, TreeInfo,
    },
};
use crate::{
    Database,
    auth::{
        crypto::{create_challenge_response, format_public_key, generate_challenge},
        types::AuthKey,
    },
    backend::BackendDB,
    entry::ID,
    store::SettingsStore,
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
    ///
    /// # Returns
    /// The appropriate response for the given request.
    async fn handle_request(&self, request: &SyncRequest) -> SyncResponse;
}

/// Default implementation of SyncHandler with database backend access.
pub struct SyncHandlerImpl {
    backend: Arc<dyn BackendDB>,
    device_key_name: String,
    sync_tree_id: ID,
}

impl SyncHandlerImpl {
    /// Create a new SyncHandlerImpl with the given backend.
    ///
    /// # Arguments
    /// * `backend` - Database backend for storing and retrieving entries
    /// * `device_key_name` - Name of the device signing key
    /// * `sync_tree_id` - Root ID of the sync database for storing bootstrap requests
    pub fn new(
        backend: Arc<dyn BackendDB>,
        device_key_name: impl Into<String>,
        sync_tree_id: ID,
    ) -> Self {
        Self {
            backend,
            device_key_name: device_key_name.into(),
            sync_tree_id,
        }
    }

    /// Get access to the sync tree for bootstrap request management.
    ///
    /// # Returns
    /// A Database instance for the sync tree with device key authentication.
    fn get_sync_tree(&self) -> crate::Result<Database> {
        let mut sync_tree = Database::new_from_id(self.sync_tree_id.clone(), self.backend.clone())?;
        sync_tree.set_default_auth_key(&self.device_key_name);
        Ok(sync_tree)
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
        requested_permission: &crate::auth::Permission,
    ) -> crate::Result<String> {
        let sync_tree = self.get_sync_tree()?;
        let op = sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&op);

        let request = BootstrapRequest {
            tree_id: tree_id.clone(),
            requesting_pubkey: requesting_key.to_string(),
            requesting_key_name: requesting_key_name.to_string(),
            requested_permission: requested_permission.clone(),
            timestamp: current_timestamp(),
            status: RequestStatus::Pending,
            // TODO: We need to get the actual peer address from the transport layer
            // For now, use a placeholder that will need to be fixed when implementing notifications
            peer_address: Address {
                transport_type: "unknown".to_string(),
                address: "unknown".to_string(),
            },
        };

        let request_id = manager.store_request(request)?;
        op.commit()?;

        Ok(request_id)
    }
}

#[async_trait]
impl SyncHandler for SyncHandlerImpl {
    async fn handle_request(&self, request: &SyncRequest) -> SyncResponse {
        match request {
            SyncRequest::Handshake(handshake_req) => {
                debug!("Received handshake request");
                self.handle_handshake(handshake_req).await
            }
            SyncRequest::SyncTree(sync_req) => {
                debug!(tree_id = %sync_req.tree_id, tips_count = sync_req.our_tips.len(), "Received sync tree request");
                self.handle_sync_tree(sync_req).await
            }
            SyncRequest::SendEntries(entries) => {
                // Process and store the received entries
                let count = entries.len();
                info!(count = count, "Received entries for synchronization");

                // Store entries in the backend as unverified (from sync)
                let mut stored_count = 0usize;
                for entry in entries {
                    match self.backend.put_unverified(entry.clone()) {
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
    /// Returns whether bootstrap key auto-approval is allowed by policy for this tree.
    ///
    /// This method checks the bootstrap approval policy stored in the target database's
    /// settings. The policy determines whether new devices can automatically gain access
    /// or require manual approval from an administrator.
    ///
    /// # Policy Location
    /// `_settings.auth.policy.bootstrap_auto_approve: bool` (default: false)
    ///
    /// # Security Implications
    /// - `true`: Any device that can reach this sync endpoint can automatically gain
    ///   the permissions they request (up to the maximum allowed by other policies).
    ///   Suitable for development or trusted private networks.
    /// - `false`: All bootstrap requests are queued for manual review by an administrator.
    ///   Recommended for production and public-facing deployments.
    ///
    /// # Arguments
    /// * `tree_id` - The ID of the database/tree to check policy for
    ///
    /// # Returns
    /// - `Ok(true)` if auto-approval is enabled
    /// - `Ok(false)` if manual approval is required (default)
    /// - `Err` if the policy cannot be read
    pub async fn is_bootstrap_auto_approve_allowed(
        &self,
        tree_id: &crate::entry::ID,
    ) -> crate::Result<bool> {
        // Create database instance to access settings through proper Transaction
        let database = Database::new_from_id(tree_id.clone(), self.backend.clone())?;
        let mut transaction = database.new_transaction()?;
        transaction.set_auth_key(&self.device_key_name);
        let settings_store = SettingsStore::new(&transaction)?;

        let auth_settings = settings_store.get_auth_settings()?;

        if let Some(policy_doc) = auth_settings.as_doc().get_as::<crate::crdt::Doc>("policy") {
            // Read as JSON-encoded bool to match set_json storage
            if let Ok(flag) = policy_doc.get_json::<bool>("bootstrap_auto_approve") {
                return Ok(flag);
            }
        }
        Ok(false)
    }

    /// Handle a handshake request from a peer.
    async fn handle_handshake(&self, request: &HandshakeRequest) -> SyncResponse {
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
            let signing_key = match self.backend.get_private_key(&self.device_key_name) {
                Ok(Some(key)) => {
                    debug!(device_key_name = %self.device_key_name, "Retrieved device signing key");
                    key
                }
                Ok(None) => {
                    error!(device_key_name = %self.device_key_name, "Device key not found");
                    return SyncResponse::Error("Device key not found".to_string());
                }
                Err(e) => {
                    error!(device_key_name = %self.device_key_name, error = %e, "Failed to get signing key");
                    return SyncResponse::Error(format!("Failed to get signing key: {e}"));
                }
            };

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
    async fn handle_sync_tree(&self, request: &SyncTreeRequest) -> SyncResponse {
        async move {
            trace!(tree_id = %request.tree_id, "Processing sync tree request");

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
    /// 1. Validates the tree exists
    /// 2. Processes any authentication request (key approval)
    /// 3. Sends all entries from the tree to the peer
    ///
    /// # Authentication Flow
    /// If authentication credentials are provided:
    /// 1. Check bootstrap auto-approval policy
    /// 2. If auto-approve enabled: Add key immediately
    /// 3. If manual approval required: Store request and return BootstrapPending
    /// 4. Track approval status in response
    ///
    /// # Arguments
    /// * `tree_id` - The database/tree to bootstrap
    /// * `requesting_key` - Optional public key requesting access
    /// * `requesting_key_name` - Optional name for the key
    /// * `requested_permission` - Optional permission level requested
    ///
    /// # Returns
    /// - `BootstrapResponse`: Contains entries and approval status
    /// - `BootstrapPending`: Manual approval required (request queued)
    /// - `Error`: Tree not found or processing failure
    async fn handle_bootstrap_request(
        &self,
        tree_id: &crate::entry::ID,
        requesting_key: Option<&str>,
        requesting_key_name: Option<&str>,
        requested_permission: Option<crate::auth::Permission>,
    ) -> SyncResponse {
        // Get the root entry (to verify tree exists)
        let _root_entry = match self.backend.get(tree_id) {
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

        // Handle key approval for bootstrap requests FIRST
        let (key_approved, granted_permission) = if let (
            Some(key),
            Some(key_name),
            Some(permission),
        ) =
            (requesting_key, requesting_key_name, requested_permission)
        {
            info!(
                tree_id = %tree_id,
                requesting_key = %key,
                key_name = %key_name,
                requested_permission = ?permission,
                "Processing key approval request for bootstrap"
            );

            // Check policy to determine if auto-approval is allowed
            match self.is_bootstrap_auto_approve_allowed(tree_id).await {
                Ok(true) => {
                    // Proceed with auto-approval under policy
                    match self
                        .add_key_to_database(tree_id, key_name, key, permission.clone())
                        .await
                    {
                        Ok(_) => {
                            info!(
                                tree_id = %tree_id,
                                key = %key,
                                permission = ?permission,
                                "Successfully approved and added key to database under policy"
                            );
                            (true, Some(permission))
                        }
                        Err(e) => {
                            warn!(
                                tree_id = %tree_id,
                                key = %key,
                                error = %e,
                                "Failed to add key to database"
                            );
                            (false, None)
                        }
                    }
                }
                Ok(false) => {
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
                                message: "Bootstrap request pending manual approval".to_string(),
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
                    error!(tree_id = %tree_id, error = %e, "Failed to evaluate bootstrap approval policy");
                    return SyncResponse::Error(format!("Policy evaluation failed: {e}"));
                }
            }
        } else {
            // No key approval requested
            (false, None)
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
        let root_entry = match self.backend.get(tree_id) {
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
    async fn handle_incremental_sync(
        &self,
        tree_id: &crate::entry::ID,
        peer_tips: &[crate::entry::ID],
    ) -> SyncResponse {
        // Get our current tips
        let our_tips = match self.backend.get_tips(tree_id) {
            Ok(tips) => tips,
            Err(e) => {
                error!(tree_id = %tree_id, error = %e, "Failed to get our tips");
                return SyncResponse::Error(format!("Failed to get tips: {e}"));
            }
        };

        // Find entries peer is missing
        let missing_entries = match self.find_missing_entries_for_peer(&our_tips, peer_tips) {
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
        match self.backend.all_roots() {
            Ok(roots) => {
                let mut tree_infos = Vec::new();
                for root_id in roots {
                    // Get basic tree info
                    if let Ok(entry_count) = self.count_tree_entries(&root_id) {
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
    async fn collect_all_tree_entries(
        &self,
        tree_id: &crate::entry::ID,
    ) -> crate::Result<Vec<crate::entry::Entry>> {
        let mut entries = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let tips = self.backend.get_tips(tree_id)?;
        to_visit.extend(tips);

        // Traverse the DAG depth-first
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) || entry_id == *tree_id {
                continue; // Skip root and already visited
            }
            visited.insert(entry_id.clone());

            match self.backend.get(&entry_id) {
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
    async fn collect_all_entries_for_bootstrap(
        &self,
        tree_id: &crate::entry::ID,
    ) -> crate::Result<Vec<crate::entry::Entry>> {
        let mut entries = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let tips = self.backend.get_tips(tree_id)?;
        to_visit.extend(tips);

        // Traverse the DAG depth-first, INCLUDING the root
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) {
                continue; // Skip already visited (but don't skip root)
            }
            visited.insert(entry_id.clone());

            match self.backend.get(&entry_id) {
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
    fn find_missing_entries_for_peer(
        &self,
        our_tips: &[crate::entry::ID],
        peer_tips: &[crate::entry::ID],
    ) -> crate::Result<Vec<crate::entry::Entry>> {
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
        super::utils::collect_ancestors_to_send(self.backend.as_ref(), &missing_tip_ids, peer_tips)
    }

    /// Count entries in a tree
    fn count_tree_entries(&self, tree_id: &crate::entry::ID) -> crate::Result<usize> {
        let mut count = 1; // Include root
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let tips = self.backend.get_tips(tree_id)?;
        to_visit.extend(tips);

        // Count all entries
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) || entry_id == *tree_id {
                continue;
            }
            visited.insert(entry_id.clone());
            count += 1;

            if let Ok(entry) = self.backend.get(&entry_id)
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

    /// Add a key to the database's authentication settings.
    ///
    /// This method is used during bootstrap auto-approval to add a requesting device's
    /// public key to the target database. It operates with elevated privileges as part
    /// of the sync infrastructure.
    ///
    /// # Authentication Challenge
    /// This method needs to authenticate with the target database to add keys. However,
    /// the sync handler doesn't inherently know which key to use for each database.
    /// Current implementation attempts to operate without authentication, which fails
    /// for databases that require authenticated operations.
    ///
    /// # TODO: Authentication Strategy
    /// Need to implement one of:
    /// 1. Database-to-key mapping in sync configuration
    /// 2. Discovery of admin keys from the database
    /// 3. Special bootstrap authentication mode
    ///
    /// # Arguments
    /// * `tree_id` - The database to add the key to
    /// * `key_name` - Name identifier for the new key
    /// * `public_key` - Ed25519 public key in "ed25519:..." format
    /// * `permission` - Permission level to grant (Admin, Write, Read)
    ///
    /// # Returns
    /// - `Ok(())` if key was successfully added
    /// - `Err` if authentication fails or key cannot be added
    async fn add_key_to_database(
        &self,
        tree_id: &crate::entry::ID,
        key_name: &str,
        public_key: &str,
        permission: crate::auth::Permission,
    ) -> crate::Result<()> {
        debug!(
            tree_id = %tree_id,
            key_name = %key_name,
            public_key = %public_key,
            permission = ?permission,
            "Adding key to database authentication settings"
        );

        // Create database instance to access settings through proper Transaction
        let database = Database::new_from_id(tree_id.clone(), self.backend.clone())?;
        let mut transaction = database.new_transaction()?;
        transaction.set_auth_key(&self.device_key_name);
        let settings_store = SettingsStore::new(&transaction)?;

        // Create the new auth key with validation
        let auth_key = AuthKey::active(public_key.to_string(), permission).unwrap();

        // Set the key using SettingsStore (handles upsert logic)
        match settings_store.set_auth_key(key_name, auth_key.clone()) {
            Ok(_) => {
                debug!(
                    key_name = %key_name,
                    public_key = %public_key,
                    "Successfully set key in auth settings"
                );
            }
            Err(crate::Error::Auth(auth_err)) if auth_err.is_key_name_conflict() => {
                warn!(
                    key_name = %key_name,
                    error = %auth_err,
                    "Key name conflict: different devices using same key name"
                );
                return Err(crate::Error::Auth(auth_err));
            }
            Err(e) => return Err(e),
        }

        // Commit the transaction to persist the changes
        transaction.commit()?;

        info!(
            tree_id = %tree_id,
            key_name = %key_name,
            "Successfully added key to database"
        );

        Ok(())
    }
}
