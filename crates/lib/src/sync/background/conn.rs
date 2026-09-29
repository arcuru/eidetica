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
    entry::Entry,
};

/// Everything a handshake needs, in a form that can leave the engine.
///
/// A handshake is aimed at a peer the engine has not talked to before, which is
/// exactly the peer most likely not to be there — so it is the command most
/// likely to hold the loop for a full connect deadline. Every field here is
/// cheap to clone, so the whole operation runs on a task of its own.
///
/// The peer registration at the end is a transaction against the sync database,
/// not engine state, so it travels with the network call rather than having to
/// come back for it.
pub(super) struct HandshakeCtx {
    pub(super) transport: std::sync::Arc<dyn crate::sync::transports::SyncTransport>,
    pub(super) instance: crate::instance::WeakInstance,
    pub(super) sync_tree_id: crate::entry::ID,
    pub(super) listen_addresses: Vec<Address>,
}

/// Connect to a peer and perform the handshake.
pub(super) async fn run_handshake(ctx: HandshakeCtx, address: Address) -> Result<PublicKey> {
    let HandshakeCtx {
        transport,
        instance,
        sync_tree_id,
        listen_addresses,
    } = ctx;
    // Generate challenge for authentication
    let challenge = generate_challenge();

    // Get our device info from instance
    let instance = instance
        .upgrade()
        .ok_or_else(|| crate::Error::from(SyncError::InstanceDropped))?;
    let public_key = instance.id();

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
    let response = transport.send_request(&address, &request).await?;

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
            let signing_key = instance.signing_key()?.clone();
            let sync_tree = crate::Database::open(&instance, &sync_tree_id)
                .await?
                .with_key(signing_key);
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

impl BackgroundSync {
    /// Handle bootstrap response by storing root and all entries
    pub(super) async fn handle_bootstrap_response(
        &self,
        response: BootstrapResponse,
    ) -> Result<crate::database::VerifyReport> {
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
        let report = self
            .store_received_entries(&response.tree_id, all_entries)
            .await?;

        info!(tree_id = %response.tree_id, "Bootstrap completed successfully");
        Ok(report)
    }

    /// Handle incremental response by storing missing entries
    pub(super) async fn handle_incremental_response(
        &self,
        response: IncrementalResponse,
    ) -> Result<crate::database::VerifyReport> {
        trace!(tree_id = %response.tree_id, "Processing incremental response");

        // Store missing entries and fire callbacks
        let report = self
            .store_received_entries(&response.tree_id, response.missing_entries)
            .await?;

        debug!(tree_id = %response.tree_id, "Incremental sync completed");
        Ok(report)
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
    ) -> Result<crate::database::VerifyReport> {
        if entries.is_empty() {
            return Ok(crate::database::VerifyReport::default());
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
        // Verified after the parent-existence check above. They are now stored
        // Unverified like all off-node arrivals: a parent-existence check is
        // not signature verification, and only this node's local validation
        // pass may assign Verified.
        instance
            .put_remote_entries(tree_id, entries)
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to store entries: {e}")).into())
    }
}
