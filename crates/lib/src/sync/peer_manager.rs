//! Internal peer management for the sync module.
//!
//! This module handles all peer registration, status tracking, and tree-peer
//! sync relationships. It operates on the sync tree but doesn't own it.

use super::error::SyncError;
use super::peer_types::{Address, ConnectionState, PeerInfo, PeerStatus};
use crate::atomicop::AtomicOp;
use crate::subtree::DocStore;
use crate::{Error, Result};

/// Private constants for peer management subtree names
pub(super) const PEERS_SUBTREE: &str = "peers"; // Maps peer pubkey -> PeerInfo
pub(super) const TREES_SUBTREE: &str = "trees"; // Maps tree ID -> list of peer pubkeys

/// Internal peer manager for the sync module.
///
/// This struct manages all peer-related operations for the sync module,
/// operating on an AtomicOp to stage changes.
pub(super) struct PeerManager<'a> {
    op: &'a AtomicOp,
}

impl<'a> PeerManager<'a> {
    /// Create a new PeerManager that operates on the given AtomicOp.
    pub(super) fn new(op: &'a AtomicOp) -> Self {
        Self { op }
    }

    /// Generate a consistent path for peer storage.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    /// * `field` - Optional field name (e.g., "status", "display_name")
    ///
    /// # Returns
    /// A dot-notation path for the peer data
    fn peer_path(pubkey: impl AsRef<str>, field: Option<&str>) -> String {
        match field {
            Some(field) => format!("{}.{field}", pubkey.as_ref()),
            None => pubkey.as_ref().to_string(),
        }
    }

    /// Generate a consistent path for tree sync record storage.
    ///
    /// # Arguments
    /// * `tree_id` - The tree root ID
    /// * `field` - Optional field name (e.g., "peer_pubkeys")
    ///
    /// # Returns
    /// A dot-notation path for the tree sync data
    fn tree_path(tree_id: impl AsRef<str>, field: Option<&str>) -> String {
        match field {
            Some(field) => format!("{}.{field}", tree_id.as_ref()),
            None => tree_id.as_ref().to_string(),
        }
    }

    /// Register a new remote peer in the sync network.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key (formatted as ed25519:base64)
    /// * `display_name` - Optional human-readable name for the peer
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) fn register_peer(
        &self,
        pubkey: impl Into<String>,
        display_name: Option<&str>,
    ) -> Result<()> {
        let pubkey = pubkey.into();
        let peer_info = PeerInfo::new(&pubkey, display_name);
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;

        // Check if peer already exists using path-based check
        if peers.contains_path(Self::peer_path(&pubkey as &str, None)) {
            return Err(Error::Sync(SyncError::PeerAlreadyExists(pubkey.clone())));
        }

        // Store peer info using path-based structure
        peers.set_path(
            Self::peer_path(&pubkey as &str, Some("pubkey")),
            peer_info.pubkey.clone(),
        )?;
        if let Some(name) = &peer_info.display_name {
            peers.set_path(
                Self::peer_path(&pubkey as &str, Some("display_name")),
                name.clone(),
            )?;
        }
        peers.set_path(
            Self::peer_path(&pubkey as &str, Some("first_seen")),
            peer_info.first_seen.clone(),
        )?;
        peers.set_path(
            Self::peer_path(&pubkey as &str, Some("last_seen")),
            peer_info.last_seen.clone(),
        )?;
        peers.set_path(
            Self::peer_path(&pubkey as &str, Some("status")),
            match peer_info.status {
                PeerStatus::Active => "active".to_string(),
                PeerStatus::Inactive => "inactive".to_string(),
                PeerStatus::Blocked => "blocked".to_string(),
            },
        )?;

        // Store addresses if any
        if !peer_info.addresses.is_empty() {
            let addresses_json = serde_json::to_string(&peer_info.addresses).unwrap_or_default();
            peers.set_path(
                Self::peer_path(&pubkey as &str, Some("addresses")),
                addresses_json,
            )?;
        }

        Ok(())
    }

    /// Update complete peer information.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    /// * `peer_info` - The updated peer information
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) fn update_peer_info(
        &self,
        pubkey: impl AsRef<str>,
        peer_info: PeerInfo,
    ) -> Result<()> {
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;

        // Check if peer exists
        if !peers.contains_path(Self::peer_path(pubkey.as_ref(), None)) {
            return Err(Error::Sync(SyncError::PeerNotFound(
                pubkey.as_ref().to_string(),
            )));
        }

        // Update all peer fields
        peers.set_path(
            Self::peer_path(pubkey.as_ref(), Some("pubkey")),
            peer_info.pubkey.clone(),
        )?;

        if let Some(name) = &peer_info.display_name {
            peers.set_path(
                Self::peer_path(pubkey.as_ref(), Some("display_name")),
                name.clone(),
            )?;
        }

        peers.set_path(
            Self::peer_path(pubkey.as_ref(), Some("first_seen")),
            peer_info.first_seen.clone(),
        )?;

        peers.set_path(
            Self::peer_path(pubkey.as_ref(), Some("last_seen")),
            peer_info.last_seen.clone(),
        )?;

        // Update status
        let status_str = match peer_info.status {
            PeerStatus::Active => "active",
            PeerStatus::Inactive => "inactive",
            PeerStatus::Blocked => "blocked",
        };
        peers.set_path(
            Self::peer_path(pubkey.as_ref(), Some("status")),
            status_str.to_string(),
        )?;

        // Update connection state
        let connection_state_str = match &peer_info.connection_state {
            ConnectionState::Disconnected => "disconnected",
            ConnectionState::Connecting => "connecting",
            ConnectionState::Connected => "connected",
            ConnectionState::Failed(msg) => &format!("failed:{msg}"),
        };
        peers.set_path(
            Self::peer_path(pubkey.as_ref(), Some("connection_state")),
            connection_state_str.to_string(),
        )?;

        // Update optional fields
        if let Some(last_sync) = &peer_info.last_successful_sync {
            peers.set_path(
                Self::peer_path(pubkey.as_ref(), Some("last_successful_sync")),
                last_sync.clone(),
            )?;
        }

        peers.set_path(
            Self::peer_path(pubkey.as_ref(), Some("connection_attempts")),
            peer_info.connection_attempts as i64,
        )?;

        if let Some(error) = &peer_info.last_error {
            peers.set_path(
                Self::peer_path(pubkey.as_ref(), Some("last_error")),
                error.clone(),
            )?;
        }

        // Store addresses if any
        if !peer_info.addresses.is_empty() {
            let addresses_json = serde_json::to_string(&peer_info.addresses).unwrap_or_default();
            peers.set_path(
                Self::peer_path(pubkey.as_ref(), Some("addresses")),
                addresses_json,
            )?;
        }

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
    pub(super) fn update_peer_status(
        &self,
        pubkey: impl AsRef<str>,
        status: PeerStatus,
    ) -> Result<()> {
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;

        // Check if peer exists
        if !peers.contains_path(Self::peer_path(pubkey.as_ref(), None)) {
            return Err(Error::Sync(SyncError::PeerNotFound(
                pubkey.as_ref().to_string(),
            )));
        }

        // Update status using path-based modification
        let status_str = match status {
            PeerStatus::Active => "active",
            PeerStatus::Inactive => "inactive",
            PeerStatus::Blocked => "blocked",
        };
        peers.set_path(
            Self::peer_path(pubkey.as_ref(), Some("status")),
            status_str.to_string(),
        )?;

        // Update last_seen timestamp
        let now = chrono::Utc::now().to_rfc3339();
        peers.set_path(Self::peer_path(pubkey.as_ref(), Some("last_seen")), now)?;

        Ok(())
    }

    /// Get information about a registered peer.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    ///
    /// # Returns
    /// The peer information if found, None otherwise.
    pub(super) fn get_peer_info(&self, pubkey: impl AsRef<str>) -> Result<Option<PeerInfo>> {
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;

        // Check if peer exists using path-based check
        if !peers.contains_path(Self::peer_path(pubkey.as_ref(), None)) {
            return Ok(None);
        }

        // Get peer fields using path-based access
        let peer_pubkey = peers
            .get_path_as::<String>(&Self::peer_path(pubkey.as_ref(), Some("pubkey")))
            .map_err(|_| {
                Error::Sync(SyncError::SerializationError(
                    "Missing pubkey field".to_string(),
                ))
            })?;

        let display_name = peers
            .get_path_as::<String>(&Self::peer_path(pubkey.as_ref(), Some("display_name")))
            .ok();

        let first_seen = peers
            .get_path_as::<String>(&Self::peer_path(pubkey.as_ref(), Some("first_seen")))
            .map_err(|_| {
                Error::Sync(SyncError::SerializationError(
                    "Missing first_seen field".to_string(),
                ))
            })?;

        let last_seen = peers
            .get_path_as::<String>(&Self::peer_path(pubkey.as_ref(), Some("last_seen")))
            .map_err(|_| {
                Error::Sync(SyncError::SerializationError(
                    "Missing last_seen field".to_string(),
                ))
            })?;

        let status_str = peers
            .get_path_as::<String>(&Self::peer_path(pubkey.as_ref(), Some("status")))
            .unwrap_or_else(|_| "active".to_string());
        let status = match status_str.as_str() {
            "active" => PeerStatus::Active,
            "inactive" => PeerStatus::Inactive,
            "blocked" => PeerStatus::Blocked,
            _ => PeerStatus::Active, // Default
        };

        // Get connection state if present
        let connection_state_str = peers
            .get_path_as::<String>(&Self::peer_path(pubkey.as_ref(), Some("connection_state")))
            .unwrap_or_else(|_| "disconnected".to_string());
        let connection_state = match connection_state_str.as_str() {
            "disconnected" => ConnectionState::Disconnected,
            "connecting" => ConnectionState::Connecting,
            "connected" => ConnectionState::Connected,
            s if s.starts_with("failed:") => {
                ConnectionState::Failed(s.strip_prefix("failed:").unwrap_or("").to_string())
            }
            _ => ConnectionState::Disconnected,
        };

        let last_successful_sync = peers
            .get_path_as::<String>(&Self::peer_path(
                pubkey.as_ref(),
                Some("last_successful_sync"),
            ))
            .ok();

        let connection_attempts = peers
            .get_path_as::<i64>(&Self::peer_path(
                pubkey.as_ref(),
                Some("connection_attempts"),
            ))
            .map(|v| v as u32)
            .unwrap_or(0);

        let last_error = peers
            .get_path_as::<String>(&Self::peer_path(pubkey.as_ref(), Some("last_error")))
            .ok();

        let mut peer_info = PeerInfo {
            pubkey: peer_pubkey,
            display_name,
            first_seen,
            last_seen,
            status,
            addresses: Vec::new(),
            connection_state,
            last_successful_sync,
            connection_attempts,
            last_error,
        };

        // Parse addresses if present
        if let Ok(addresses_json) =
            peers.get_path_as::<String>(&Self::peer_path(pubkey.as_ref(), Some("addresses")))
            && let Ok(addresses) = serde_json::from_str(&addresses_json)
        {
            peer_info.addresses = addresses;
        }

        // Only return non-blocked peers
        if peer_info.status != PeerStatus::Blocked {
            Ok(Some(peer_info))
        } else {
            Ok(None)
        }
    }

    /// List all registered peers.
    ///
    /// # Returns
    /// A vector of all registered peer information.
    pub(super) fn list_peers(&self) -> Result<Vec<PeerInfo>> {
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;
        let all_peers = peers.get_all()?;
        let mut peer_list = Vec::new();

        // Extract pubkeys (top-level keys are peer pubkeys)
        for pubkey in all_peers.keys() {
            // Get peer info using path-based access
            if let Some(peer_info) = self.get_peer_info(pubkey)? {
                peer_list.push(peer_info);
            }
        }

        Ok(peer_list)
    }

    /// Remove a peer from the sync network.
    ///
    /// This removes the peer entry and all associated sync relationships.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) fn remove_peer(&self, pubkey: impl AsRef<str>) -> Result<()> {
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;

        // Mark peer as blocked instead of removing (using path-based access)
        if peers.contains_path(Self::peer_path(pubkey.as_ref(), None)) {
            peers.set_path(
                Self::peer_path(pubkey.as_ref(), Some("status")),
                "blocked".to_string(),
            )?;
        }

        // Remove peer from all tree sync lists using path-based access
        let trees = self.op.get_subtree::<DocStore>(TREES_SUBTREE)?;
        let all_keys = trees.get_all()?.keys().cloned().collect::<Vec<_>>();
        for tree_id in all_keys {
            let peer_list_path = Self::tree_path(&tree_id, Some("peer_pubkeys"));
            if let Ok(peer_list_json) = trees.get_path_as::<String>(&peer_list_path)
                && let Ok(mut peer_pubkeys) = serde_json::from_str::<Vec<String>>(&peer_list_json)
            {
                let initial_len = peer_pubkeys.len();
                peer_pubkeys.retain(|p| p.as_str() != pubkey.as_ref());

                if peer_pubkeys.len() != initial_len {
                    // Peer was removed
                    if peer_pubkeys.is_empty() {
                        trees.delete(&tree_id)?;
                    } else {
                        let updated_json = serde_json::to_string(&peer_pubkeys).unwrap_or_default();
                        trees.set_path(&peer_list_path, updated_json)?;
                    }
                }
            }
        }

        Ok(())
    }

    // === Tree Sync Relationship Methods ===

    /// Add a tree to the sync relationship with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `tree_root_id` - The root ID of the tree to sync
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) fn add_tree_sync(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<()> {
        // First check if peer exists using path-based check
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;
        if !peers.contains_path(Self::peer_path(peer_pubkey.as_ref(), None)) {
            return Err(Error::Sync(SyncError::PeerNotFound(
                peer_pubkey.as_ref().to_string(),
            )));
        }

        let trees = self.op.get_subtree::<DocStore>(TREES_SUBTREE)?;

        // Get existing peer list for this tree, or create empty list
        let peer_list_path = Self::tree_path(tree_root_id.as_ref(), Some("peer_pubkeys"));
        let mut peer_pubkeys: Vec<String> = trees
            .get_path_as::<String>(&peer_list_path)
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(Vec::new);

        // Add peer if not already present
        if !peer_pubkeys.contains(&peer_pubkey.as_ref().to_string()) {
            peer_pubkeys.push(peer_pubkey.as_ref().to_string());

            // Store the updated list using path-based access (as JSON)
            let peer_list_json = serde_json::to_string(&peer_pubkeys).unwrap_or_default();
            trees.set_path(&peer_list_path, peer_list_json)?;

            // Also store tree_id for consistency
            trees.set_path(
                Self::tree_path(tree_root_id.as_ref(), Some("tree_id")),
                tree_root_id.as_ref().to_string(),
            )?;
        }

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
    pub(super) fn remove_tree_sync(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<()> {
        let trees = self.op.get_subtree::<DocStore>(TREES_SUBTREE)?;

        // Get existing peer list for this tree
        let peer_list_path = Self::tree_path(tree_root_id.as_ref(), Some("peer_pubkeys"));
        if let Ok(peer_list_json) = trees.get_path_as::<String>(&peer_list_path)
            && let Ok(mut peer_pubkeys) = serde_json::from_str::<Vec<String>>(&peer_list_json)
        {
            // Remove the peer from the list
            let initial_len = peer_pubkeys.len();
            peer_pubkeys.retain(|p| p.as_str() != peer_pubkey.as_ref());

            if peer_pubkeys.len() != initial_len {
                // Peer was removed
                if peer_pubkeys.is_empty() {
                    // Remove the entire tree record if no peers left
                    trees.delete(tree_root_id.as_ref())?;
                } else {
                    // Update the peer list
                    let updated_json = serde_json::to_string(&peer_pubkeys).unwrap_or_default();
                    trees.set_path(&peer_list_path, updated_json)?;
                }
            }
        }

        Ok(())
    }

    /// Get the list of trees synced with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    ///
    /// # Returns
    /// A vector of tree root IDs synced with this peer.
    pub(super) fn get_peer_trees(&self, peer_pubkey: impl AsRef<str>) -> Result<Vec<String>> {
        let trees = self.op.get_subtree::<DocStore>(TREES_SUBTREE)?;
        let all_trees = trees.get_all()?;
        let mut synced_trees = Vec::new();

        for tree_id in all_trees.keys() {
            let peer_list_path = Self::tree_path(tree_id, Some("peer_pubkeys"));
            if let Ok(peer_list_json) = trees.get_path_as::<String>(&peer_list_path)
                && let Ok(peer_pubkeys) = serde_json::from_str::<Vec<String>>(&peer_list_json)
                && peer_pubkeys.contains(&peer_pubkey.as_ref().to_string())
            {
                synced_trees.push(tree_id.clone());
            }
        }

        Ok(synced_trees)
    }

    /// Get all peers that sync a specific tree.
    ///
    /// # Arguments
    /// * `tree_root_id` - The root ID of the tree
    ///
    /// # Returns
    /// A vector of peer public keys that sync this tree.
    pub(super) fn get_tree_peers(&self, tree_root_id: impl AsRef<str>) -> Result<Vec<String>> {
        let trees = self.op.get_subtree::<DocStore>(TREES_SUBTREE)?;
        let peer_list_path = Self::tree_path(tree_root_id.as_ref(), Some("peer_pubkeys"));
        Ok(trees
            .get_path_as::<String>(&peer_list_path)
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(Vec::new))
    }

    /// Check if a tree is synced with a specific peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `tree_root_id` - The root ID of the tree
    ///
    /// # Returns
    /// True if the tree is synced with the peer, false otherwise.
    pub(super) fn is_tree_synced_with_peer(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<bool> {
        let trees = self.op.get_subtree::<DocStore>(TREES_SUBTREE)?;
        let peer_list_path = Self::tree_path(tree_root_id.as_ref(), Some("peer_pubkeys"));
        match trees.get_path_as::<String>(&peer_list_path) {
            Ok(peer_list_json) => {
                if let Ok(peer_pubkeys) = serde_json::from_str::<Vec<String>>(&peer_list_json) {
                    Ok(peer_pubkeys.contains(&peer_pubkey.as_ref().to_string()))
                } else {
                    Ok(false)
                }
            }
            Err(_) => Ok(false),
        }
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
    pub(super) fn add_address(&self, peer_pubkey: impl AsRef<str>, address: Address) -> Result<()> {
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;

        // Check if peer exists
        if !peers.contains_path(Self::peer_path(peer_pubkey.as_ref(), None)) {
            return Err(Error::Sync(SyncError::PeerNotFound(
                peer_pubkey.as_ref().to_string(),
            )));
        }

        // Get current addresses
        let mut all_addresses: Vec<Address> = peers
            .get_path_as::<String>(&Self::peer_path(peer_pubkey.as_ref(), Some("addresses")))
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(Vec::new);

        // Add the new address if not already present
        if !all_addresses.contains(&address) {
            all_addresses.push(address);

            // Store updated addresses
            let addresses_json = serde_json::to_string(&all_addresses).unwrap_or_default();
            peers.set_path(
                Self::peer_path(peer_pubkey.as_ref(), Some("addresses")),
                addresses_json,
            )?;

            // Update last_seen timestamp
            let now = chrono::Utc::now().to_rfc3339();
            peers.set_path(
                Self::peer_path(peer_pubkey.as_ref(), Some("last_seen")),
                now,
            )?;
        }

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
    pub(super) fn remove_address(
        &self,
        peer_pubkey: impl AsRef<str>,
        address: &Address,
    ) -> Result<bool> {
        let peers = self.op.get_subtree::<DocStore>(PEERS_SUBTREE)?;

        // Check if peer exists
        if !peers.contains_path(Self::peer_path(peer_pubkey.as_ref(), None)) {
            return Err(Error::Sync(SyncError::PeerNotFound(
                peer_pubkey.as_ref().to_string(),
            )));
        }

        // Get current addresses
        let mut all_addresses: Vec<Address> = peers
            .get_path_as::<String>(&Self::peer_path(peer_pubkey.as_ref(), Some("addresses")))
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(Vec::new);

        // Remove the address
        let initial_len = all_addresses.len();
        all_addresses.retain(|a| a != address);

        if all_addresses.len() != initial_len {
            // Address was removed, update storage
            let addresses_json = serde_json::to_string(&all_addresses).unwrap_or_default();
            peers.set_path(
                Self::peer_path(peer_pubkey.as_ref(), Some("addresses")),
                addresses_json,
            )?;

            // Update last_seen timestamp
            let now = chrono::Utc::now().to_rfc3339();
            peers.set_path(
                Self::peer_path(peer_pubkey.as_ref(), Some("last_seen")),
                now,
            )?;

            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Get addresses for a peer, optionally filtered by transport type.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `transport_type` - Optional transport type filter
    ///
    /// # Returns
    /// A vector of addresses matching the criteria.
    pub(super) fn get_addresses(
        &self,
        peer_pubkey: impl AsRef<str>,
        transport_type: Option<&str>,
    ) -> Result<Vec<Address>> {
        if let Some(peer_info) = self.get_peer_info(peer_pubkey)? {
            match transport_type {
                Some(transport) => Ok(peer_info
                    .get_addresses(transport)
                    .into_iter()
                    .cloned()
                    .collect()),
                None => Ok(peer_info.addresses),
            }
        } else {
            Ok(Vec::new())
        }
    }
}
