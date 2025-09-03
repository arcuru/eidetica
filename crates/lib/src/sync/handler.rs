//! Sync request handler trait and implementation.
//!
//! This module contains transport-agnostic handlers that process
//! sync requests and generate responses. These handlers can be
//! used by any transport implementation through the SyncHandler trait.

use super::protocol::{
    GetEntriesRequest, GetEntriesResponse, GetTipsRequest, GetTipsResponse, HandshakeRequest,
    HandshakeResponse, PROTOCOL_VERSION, SyncRequest, SyncResponse,
};
use crate::auth::crypto::{create_challenge_response, format_public_key, generate_challenge};
use crate::backend::Database;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{Instrument, debug, error, info, info_span, trace, warn};

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
    backend: Arc<dyn Database>,
    device_key_name: String,
}

impl SyncHandlerImpl {
    /// Create a new SyncHandlerImpl with the given backend.
    ///
    /// # Arguments
    /// * `backend` - Database backend for storing and retrieving entries
    /// * `device_key_name` - Name of the device signing key
    pub fn new(backend: Arc<dyn Database>, device_key_name: impl Into<String>) -> Self {
        Self {
            backend,
            device_key_name: device_key_name.into(),
        }
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
            SyncRequest::GetTips(req) => {
                debug!(tree_id = %req.tree_id, "Received get tips request");
                self.handle_get_tips(req).await
            }
            SyncRequest::GetEntries(req) => {
                debug!(count = req.entry_ids.len(), "Received get entries request");
                self.handle_get_entries(req).await
            }
        }
    }
}

impl SyncHandlerImpl {
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

            info!(
                our_device_id = %device_id,
                peer_device_id = %request.device_id,
                "Handshake completed successfully"
            );

            SyncResponse::Handshake(HandshakeResponse {
                device_id,
                public_key,
                display_name: Some("Eidetica Peer".to_string()),
                protocol_version: PROTOCOL_VERSION,
                challenge_response,
                new_challenge,
            })
        }
        .instrument(info_span!("handle_handshake", peer = %request.device_id))
        .await
    }

    /// Handle a request for tree tips.
    async fn handle_get_tips(&self, request: &GetTipsRequest) -> SyncResponse {
        async move {
            trace!(tree_id = %request.tree_id, "Retrieving tree tips");

            match self.backend.get_tips(&request.tree_id) {
                Ok(tips) => {
                    debug!(
                        tree_id = %request.tree_id,
                        tip_count = tips.len(),
                        "Retrieved tree tips successfully"
                    );
                    SyncResponse::Tips(GetTipsResponse {
                        tree_id: request.tree_id.clone(),
                        tips,
                    })
                }
                Err(e) => {
                    error!(
                        tree_id = %request.tree_id,
                        error = %e,
                        "Failed to get tips for tree"
                    );
                    SyncResponse::Error(format!(
                        "Failed to get tips for tree {}: {e}",
                        request.tree_id
                    ))
                }
            }
        }
        .instrument(info_span!("handle_get_tips", tree = %request.tree_id))
        .await
    }

    /// Handle a request for specific entries.
    async fn handle_get_entries(&self, request: &GetEntriesRequest) -> SyncResponse {
        async move {
            debug!(
                entry_count = request.entry_ids.len(),
                "Retrieving requested entries"
            );

            let mut entries = Vec::with_capacity(request.entry_ids.len());

            for entry_id in &request.entry_ids {
                trace!(entry_id = %entry_id, "Retrieving entry");

                match self.backend.get(entry_id) {
                    Ok(entry) => {
                        trace!(entry_id = %entry_id, "Entry retrieved successfully");
                        entries.push(entry);
                    }
                    Err(e) if e.is_not_found() => {
                        warn!(entry_id = %entry_id, "Entry not found");
                        return SyncResponse::Error(format!("Entry not found: {entry_id}"));
                    }
                    Err(e) => {
                        error!(entry_id = %entry_id, error = %e, "Failed to get entry");
                        return SyncResponse::Error(format!("Failed to get entry {entry_id}: {e}"));
                    }
                }
            }

            info!(
                requested = request.entry_ids.len(),
                retrieved = entries.len(),
                "Completed entries retrieval"
            );

            SyncResponse::Entries(GetEntriesResponse { entries })
        }
        .instrument(info_span!(
            "handle_get_entries",
            count = request.entry_ids.len()
        ))
        .await
    }
}
