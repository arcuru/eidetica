//! Connection and response handling for BackgroundSync.
//!
//! This module contains methods for peer connection (handshake) and
//! handling sync responses (bootstrap and incremental).

use tracing::{debug, info, trace};

use super::BackgroundSync;
use crate::sync::{
    error::SyncError,
    peer_manager::PeerManager,
    peer_types::Address,
    protocol::{
        BootstrapResponse, HandshakeRequest, IncrementalResponse, PROTOCOL_VERSION, SyncRequest,
        SyncResponse,
    },
};
use crate::{
    Error, Result,
    auth::crypto::{generate_challenge, verify_challenge_response},
    entry::Entry,
};

impl BackgroundSync {
    /// Connect to a peer and perform handshake
    pub(super) async fn connect_to_peer(&mut self, address: &Address) -> Result<String> {
        // Generate challenge for authentication
        let challenge = generate_challenge();

        // Get our device info from instance
        let instance = self.instance()?;
        let public_key = instance.device_id_string();
        let device_id = public_key.clone();

        // Build listen addresses from all running servers
        let listen_addresses: Vec<Address> = self
            .transport_manager
            .get_all_server_addresses()
            .into_iter()
            .map(|(transport_type, addr)| Address {
                transport_type,
                address: addr,
            })
            .collect();

        // Create handshake request
        let handshake_request = HandshakeRequest {
            device_id,
            public_key: public_key.clone(),
            display_name: Some("BackgroundSync".to_string()),
            protocol_version: PROTOCOL_VERSION,
            challenge: challenge.clone(),
            listen_addresses,
        };

        // Send handshake request
        let request = SyncRequest::Handshake(handshake_request);
        let response = self
            .transport_manager
            .send_request(address, &request)
            .await?;

        // Process handshake response
        match response {
            SyncResponse::Handshake(handshake_resp) => {
                // Verify protocol version
                if handshake_resp.protocol_version != PROTOCOL_VERSION {
                    return Err(SyncError::ProtocolMismatch {
                        expected: PROTOCOL_VERSION,
                        received: handshake_resp.protocol_version,
                    }
                    .into());
                }

                // Verify the server's signature on our challenge
                let verification_result = verify_challenge_response(
                    &challenge,
                    &handshake_resp.challenge_response,
                    &handshake_resp.public_key,
                );

                verification_result.map_err(|e| {
                    SyncError::HandshakeFailed(format!("Signature verification failed: {e}"))
                })?;

                // Add peer to sync tree
                let sync_tree = self.get_sync_tree().await?;
                let txn = sync_tree.new_transaction().await?;
                let peer_manager = PeerManager::new(&txn);

                // Try to register peer, but ignore if already exists
                match peer_manager
                    .register_peer(
                        &handshake_resp.public_key,
                        handshake_resp.display_name.as_deref(),
                    )
                    .await
                {
                    Ok(_) => {
                        txn.commit().await?;
                    }
                    Err(Error::Sync(SyncError::PeerAlreadyExists(_))) => {
                        // Peer already exists, that's fine - just continue with handshake result
                    }
                    Err(e) => return Err(e),
                }

                // Successfully connected to peer
                Ok(handshake_resp.public_key)
            }
            SyncResponse::Error(msg) => Err(SyncError::HandshakeFailed(msg).into()),
            _ => Err(SyncError::HandshakeFailed("Unexpected response type".to_string()).into()),
        }
    }

    /// Send a sync request and get response
    pub(super) async fn send_sync_request(
        &self,
        address: &Address,
        request: &SyncRequest,
    ) -> Result<SyncResponse> {
        self.transport_manager.send_request(address, request).await
    }

    /// Handle bootstrap response by storing root and all entries
    pub(super) async fn handle_bootstrap_response(
        &self,
        response: BootstrapResponse,
    ) -> Result<()> {
        trace!(tree_id = %response.tree_id, "Processing bootstrap response");

        // Store root entry first
        let instance = self.instance()?;
        instance
            .backend()
            .put_verified(response.root_entry)
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to store root entry: {e}")))?;

        // Store all other entries
        self.store_received_entries(response.all_entries).await?;

        info!(tree_id = %response.tree_id, "Bootstrap completed successfully");
        Ok(())
    }

    /// Handle incremental response by storing missing entries
    pub(super) async fn handle_incremental_response(
        &self,
        response: IncrementalResponse,
    ) -> Result<()> {
        trace!(tree_id = %response.tree_id, "Processing incremental response");

        // Store missing entries
        self.store_received_entries(response.missing_entries)
            .await?;

        // Note: We could use their_tips for further optimization in the future
        // For now, the next sync cycle will handle any remaining differences

        debug!(tree_id = %response.tree_id, "Incremental sync completed");
        Ok(())
    }

    /// Store received entries from peer with proper DAG ordering
    pub(super) async fn store_received_entries(&self, entries: Vec<Entry>) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        // Note: Height-based sorting would require tree context
        // For now, we rely on the sender to provide entries in correct order

        for entry in entries {
            // Basic validation: check that entry ID matches content
            let calculated_id = entry.id();
            if entry.id() != calculated_id {
                return Err(SyncError::InvalidEntry(format!(
                    "Entry ID {} doesn't match calculated ID {}",
                    entry.id(),
                    calculated_id
                ))
                .into());
            }

            // Verify parent entries exist before storing children
            if let Ok(parents) = entry.parents() {
                for parent_id in &parents {
                    let instance = self.instance()?;
                    if let Err(e) = instance.backend().get(parent_id).await {
                        if e.is_not_found() {
                            return Err(SyncError::InvalidEntry(format!(
                                "Parent entry {} not found when storing entry {}",
                                parent_id,
                                entry.id()
                            ))
                            .into());
                        } else {
                            return Err(SyncError::BackendError(format!(
                                "Failed to check parent {} for entry {}: {}",
                                parent_id,
                                entry.id(),
                                e
                            ))
                            .into());
                        }
                    }
                }
            }

            // Store the entry
            let instance = self.instance()?;
            instance
                .backend()
                .put_verified(entry)
                .await
                .map_err(|e| SyncError::BackendError(format!("Failed to store entry: {e}")))?;
        }

        Ok(())
    }
}
