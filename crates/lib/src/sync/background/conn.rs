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
    auth::crypto::{PublicKey, generate_challenge, verify_challenge_response},
    backend::VerificationStatus,
    entry::Entry,
};

impl BackgroundSync {
    /// Connect to a peer and perform handshake
    pub(super) async fn connect_to_peer(&mut self, address: &Address) -> Result<PublicKey> {
        // Generate challenge for authentication
        let challenge = generate_challenge();

        // Get our device info from instance
        let instance = self.instance()?;
        let public_key = instance.id();

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
            device_id: public_key.clone(),
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
                    Err(Error::Sync(ref e)) if matches!(**e, SyncError::PeerAlreadyExists(_)) => {
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

        // Integrity check: the root entry's content must hash to the declared
        // tree_id. Cross-algorithm bootstraps fail loudly here; supporting them
        // requires multi-CID-per-entry storage (see issue #37).
        let derived = response.root_entry.id();
        if derived != response.tree_id {
            return Err(SyncError::InvalidEntry(format!(
                "root entry content hashes to {} but bootstrap response declares tree_id {}",
                derived, response.tree_id
            ))
            .into());
        }

        // Combine root entry with all other entries into a single batch
        let mut all_entries = Vec::with_capacity(1 + response.all_entries.len());
        all_entries.push(response.root_entry);
        all_entries.extend(response.all_entries);

        // Store all entries and fire callbacks once
        self.store_received_entries(&response.tree_id, all_entries)
            .await?;

        info!(tree_id = %response.tree_id, "Bootstrap completed successfully");
        Ok(())
    }

    /// Handle incremental response by storing missing entries
    pub(super) async fn handle_incremental_response(
        &self,
        response: IncrementalResponse,
    ) -> Result<()> {
        trace!(tree_id = %response.tree_id, "Processing incremental response");

        // Store missing entries and fire callbacks
        self.store_received_entries(&response.tree_id, response.missing_entries)
            .await?;

        debug!(tree_id = %response.tree_id, "Incremental sync completed");
        Ok(())
    }

    /// Validate and store received entries from peer, firing remote write callbacks.
    ///
    /// Validates entry integrity and parent existence, then stores the batch
    /// through `Instance::put_remote_entries` which fires callbacks once for the
    /// entire batch.
    pub(super) async fn store_received_entries(
        &self,
        tree_id: &crate::entry::ID,
        entries: Vec<Entry>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        // Note: Height-based sorting would require tree context
        // For now, we rely on the sender to provide entries in correct order

        // Per-entry hash integrity isn't meaningful here: these entries arrive
        // without declared IDs, and we store them under whatever `entry.id()`
        // derives locally. Substitution of individual entries would fail the
        // parent-existence check below (a forged entry's children wouldn't
        // connect to genuine parents). Root-level integrity is verified against
        // the declared tree_id in the bootstrap handler.
        //
        // Parents may be either already-stored or earlier in this same batch
        // (bootstrap ships chains of new entries together), so we accept both.
        let in_batch: std::collections::HashSet<crate::entry::ID> =
            entries.iter().map(|e| e.id()).collect();
        let instance = self.instance()?;
        for entry in &entries {
            if let Ok(parents) = entry.parents() {
                for parent_id in &parents {
                    if in_batch.contains(parent_id) {
                        continue;
                    }
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
        }

        // Bootstrap/incremental sync paths historically marked these entries
        // Verified after the parent-existence check above. That hasn't changed
        // here, but the verification gap (no signature check) is tracked via
        // the TODO in `Sync::store_received_entries`.
        instance
            .put_remote_entries(tree_id, VerificationStatus::Verified, entries)
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to store entries: {e}")))?;

        Ok(())
    }
}
