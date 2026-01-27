//! Peer management, sync relationships, and address handling for the sync system.

use tokio::sync::oneshot;
use tracing::info;

use super::{
    Address, ConnectionState, PeerId, PeerInfo, PeerStatus, Sync, SyncError, SyncHandle,
    SyncPeerInfo, SyncStatus, background::SyncCommand, peer_manager::PeerManager,
};
use crate::{Result, entry::ID};

impl Sync {
    // === Peer Management Methods ===

    /// Register a new remote peer in the sync network.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key (formatted as ed25519:base64)
    /// * `display_name` - Optional human-readable name for the peer
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub async fn register_peer(
        &self,
        pubkey: impl Into<String>,
        display_name: Option<&str>,
    ) -> Result<()> {
        let pubkey_str = pubkey.into();

        // Store in sync tree via PeerManager
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .register_peer(&pubkey_str, display_name)
            .await?;
        op.commit().await?;

        // Background sync will read peer info directly from sync tree when needed
        Ok(())
    }

    /// Update the status of a registered peer.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    /// * `status` - The new status for the peer
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub async fn update_peer_status(
        &self,
        pubkey: impl AsRef<str>,
        status: PeerStatus,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .update_peer_status(pubkey.as_ref(), status)
            .await?;
        op.commit().await?;
        Ok(())
    }

    /// Get information about a registered peer.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    ///
    /// # Returns
    /// The peer information if found, None otherwise.
    pub async fn get_peer_info(&self, pubkey: impl AsRef<str>) -> Result<Option<PeerInfo>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).get_peer_info(pubkey.as_ref()).await
        // No commit - just reading
    }

    /// List all registered peers.
    ///
    /// # Returns
    /// A vector of all registered peer information.
    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).list_peers().await
        // No commit - just reading
    }

    /// Remove a peer from the sync network.
    ///
    /// This removes the peer entry and all associated sync relationships and transport info.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub async fn remove_peer(&self, pubkey: impl AsRef<str>) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).remove_peer(pubkey.as_ref()).await?;
        op.commit().await?;
        Ok(())
    }

    // === Declarative Sync API ===

    /// Register a peer for syncing (declarative API).
    ///
    /// This is the recommended way to set up syncing. It immediately registers
    /// the peer and tree/peer relationship, then the background sync engine
    /// handles the actual data synchronization.
    ///
    /// # Arguments
    /// * `info` - Information about the peer and sync configuration
    ///
    /// # Returns
    /// A handle for tracking sync status and adding more address hints.
    ///
    /// # Example
    /// ```no_run
    /// # use eidetica::*;
    /// # use eidetica::sync::{SyncPeerInfo, Address, AuthParams};
    /// # async fn example(sync: sync::Sync, peer_pubkey: String, tree_id: entry::ID) -> Result<()> {
    /// // Register peer for syncing
    /// let handle = sync.register_sync_peer(SyncPeerInfo {
    ///     peer_pubkey,
    ///     tree_id,
    ///     addresses: vec![Address {
    ///         transport_type: "http".to_string(),
    ///         address: "http://localhost:8080".to_string(),
    ///     }],
    ///     auth: None,
    ///     display_name: Some("My Peer".to_string()),
    /// }).await?;
    ///
    /// // Optionally wait for initial sync
    /// handle.wait_for_initial_sync().await?;
    ///
    /// // Check status anytime
    /// let status = handle.status().await?;
    /// println!("Has local data: {}", status.has_local_data);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn register_sync_peer(&self, info: SyncPeerInfo) -> Result<SyncHandle> {
        let op = self.sync_tree.new_transaction().await?;
        let peer_mgr = PeerManager::new(&op);

        // Register peer if it doesn't exist
        if peer_mgr.get_peer_info(&info.peer_pubkey).await?.is_none() {
            peer_mgr
                .register_peer(&info.peer_pubkey, info.display_name.as_deref())
                .await?;
        }

        // Add all address hints
        for addr in &info.addresses {
            peer_mgr
                .add_address(&info.peer_pubkey, addr.clone())
                .await?;
        }

        // Register the tree/peer relationship
        peer_mgr
            .add_tree_sync(&info.peer_pubkey, &info.tree_id)
            .await?;

        // TODO: Store auth params if provided for bootstrap
        // For now, auth is passed during the actual sync handshake via on_local_write callback

        op.commit().await?;

        info!(
            peer = %info.peer_pubkey,
            tree = %info.tree_id,
            address_count = info.addresses.len(),
            "Registered peer for syncing"
        );

        Ok(SyncHandle {
            tree_id: info.tree_id,
            peer_pubkey: info.peer_pubkey,
            sync: self.clone(),
        })
    }

    /// Get the current sync status for a tree/peer pair.
    ///
    /// # Arguments
    /// * `tree_id` - The tree to check
    /// * `peer_pubkey` - The peer public key
    ///
    /// # Returns
    /// Current sync status including whether we have local data.
    pub async fn get_sync_status(&self, tree_id: &ID, _peer_pubkey: &str) -> Result<SyncStatus> {
        // Check if we have local data for this tree
        let backend = self.backend()?;
        let our_tips = backend.get_tips(tree_id).await.unwrap_or_default();

        // TODO: Track last_sync time and last_error in sync tree
        // For now, just report if we have data
        Ok(SyncStatus {
            has_local_data: !our_tips.is_empty(),
            last_sync: None,
            last_error: None,
        })
    }

    // === Database Sync Relationship Methods ===

    /// Add a tree to the sync relationship with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `tree_root_id` - The root ID of the tree to sync
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub async fn add_tree_sync(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .add_tree_sync(peer_pubkey.as_ref(), tree_root_id.as_ref())
            .await?;
        op.commit().await?;
        Ok(())
    }

    /// Remove a tree from the sync relationship with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `tree_root_id` - The root ID of the tree to stop syncing
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub async fn remove_tree_sync(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .remove_tree_sync(peer_pubkey.as_ref(), tree_root_id.as_ref())
            .await?;
        op.commit().await?;
        Ok(())
    }

    /// Get the list of trees synced with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    ///
    /// # Returns
    /// A vector of tree root IDs synced with this peer.
    pub async fn get_peer_trees(&self, peer_pubkey: impl AsRef<str>) -> Result<Vec<String>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .get_peer_trees(peer_pubkey.as_ref())
            .await
        // No commit - just reading
    }

    /// Get all peers that sync a specific tree.
    ///
    /// # Arguments
    /// * `tree_root_id` - The root ID of the tree
    ///
    /// # Returns
    /// A vector of peer IDs that sync this tree.
    pub async fn get_tree_peers(&self, tree_root_id: impl AsRef<str>) -> Result<Vec<PeerId>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .get_tree_peers(tree_root_id.as_ref())
            .await
        // No commit - just reading
    }

    /// Connect to a remote peer and perform handshake.
    ///
    /// This method initiates a connection to a peer, performs the handshake protocol,
    /// and automatically registers the peer if successful.
    ///
    /// # Arguments
    /// * `address` - The address of the peer to connect to
    ///
    /// # Returns
    /// A Result containing the peer's public key if successful.
    pub async fn connect_to_peer(&self, address: &Address) -> Result<String> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::ConnectToPeer {
                address: address.clone(),
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Update the connection state of a peer.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    /// * `state` - The new connection state
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub async fn update_peer_connection_state(
        &self,
        pubkey: impl AsRef<str>,
        state: ConnectionState,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        let peer_manager = PeerManager::new(&op);

        // Get current peer info
        let mut peer_info = match peer_manager.get_peer_info(pubkey.as_ref()).await? {
            Some(info) => info,
            None => return Err(SyncError::PeerNotFound(pubkey.as_ref().to_string()).into()),
        };

        // Update connection state
        peer_info.connection_state = state;
        peer_info.touch_at(op.now_rfc3339()?);

        // Save updated peer info
        peer_manager
            .update_peer_info(pubkey.as_ref(), peer_info)
            .await?;
        op.commit().await?;
        Ok(())
    }

    /// Check if a tree is synced with a specific peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `tree_root_id` - The root ID of the tree
    ///
    /// # Returns
    /// True if the tree is synced with the peer, false otherwise.
    pub async fn is_tree_synced_with_peer(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<bool> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .is_tree_synced_with_peer(peer_pubkey.as_ref(), tree_root_id.as_ref())
            .await
        // No commit - just reading
    }

    // === Address Management Methods ===

    /// Add an address to a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `address` - The address to add
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub async fn add_peer_address(
        &self,
        peer_pubkey: impl AsRef<str>,
        address: Address,
    ) -> Result<()> {
        // Update sync tree via PeerManager
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .add_address(peer_pubkey.as_ref(), address)
            .await?;
        op.commit().await?;

        // Background sync will read updated peer info directly from sync tree when needed
        Ok(())
    }

    /// Remove a specific address from a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `address` - The address to remove
    ///
    /// # Returns
    /// A Result indicating success or an error (true if removed, false if not found).
    pub async fn remove_peer_address(
        &self,
        peer_pubkey: impl AsRef<str>,
        address: &Address,
    ) -> Result<bool> {
        let op = self.sync_tree.new_transaction().await?;
        let result = PeerManager::new(&op)
            .remove_address(peer_pubkey.as_ref(), address)
            .await?;
        op.commit().await?;
        Ok(result)
    }

    /// Get addresses for a peer, optionally filtered by transport type.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `transport_type` - Optional transport type filter
    ///
    /// # Returns
    /// A vector of addresses matching the criteria.
    pub async fn get_peer_addresses(
        &self,
        peer_pubkey: impl AsRef<str>,
        transport_type: Option<&str>,
    ) -> Result<Vec<Address>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op)
            .get_addresses(peer_pubkey.as_ref(), transport_type)
            .await
        // No commit - just reading
    }
}
