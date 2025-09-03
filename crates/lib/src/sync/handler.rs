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
            SyncRequest::Handshake(handshake_req) => self.handle_handshake(handshake_req).await,
            SyncRequest::SendEntries(entries) => {
                // Process and store the received entries
                let count = entries.len();
                tracing::info!("Received {count} entries for synchronization");

                // Store entries in the backend as unverified (from sync)
                let mut stored_count = 0usize;
                for entry in entries {
                    match self.backend.put_unverified(entry.clone()) {
                        Ok(_) => {
                            stored_count += 1;
                        }
                        Err(e) => {
                            tracing::error!("Failed to store entry {}: {}", entry.id(), e);
                            // Continue processing other entries rather than failing completely
                        }
                    }
                }

                if count <= 1 {
                    SyncResponse::Ack
                } else {
                    SyncResponse::Count(stored_count)
                }
            }
            SyncRequest::GetTips(req) => self.handle_get_tips(req).await,
            SyncRequest::GetEntries(req) => self.handle_get_entries(req).await,
        }
    }
}

impl SyncHandlerImpl {
    /// Handle a handshake request from a peer.
    async fn handle_handshake(&self, request: &HandshakeRequest) -> SyncResponse {
        // Check protocol version compatibility
        if request.protocol_version != PROTOCOL_VERSION {
            return SyncResponse::Error(format!(
                "Protocol version mismatch: expected {}, got {}",
                PROTOCOL_VERSION, request.protocol_version
            ));
        }

        // Get device signing key from backend
        let signing_key = match self.backend.get_private_key(&self.device_key_name) {
            Ok(Some(key)) => key,
            Ok(None) => return SyncResponse::Error("Device key not found".to_string()),
            Err(e) => return SyncResponse::Error(format!("Failed to get signing key: {e}")),
        };

        // Generate device ID and public key from signing key
        let verifying_key = signing_key.verifying_key();
        let public_key = format_public_key(&verifying_key);
        let device_id = public_key.clone(); // Device ID is the public key

        // Sign the challenge with our device key to prove identity
        let challenge_response = create_challenge_response(&request.challenge, &signing_key);

        // Generate a new challenge for mutual authentication
        let new_challenge = generate_challenge();

        SyncResponse::Handshake(HandshakeResponse {
            device_id,
            public_key,
            display_name: Some("Eidetica Peer".to_string()),
            protocol_version: PROTOCOL_VERSION,
            challenge_response,
            new_challenge,
        })
    }

    /// Handle a request for tree tips.
    async fn handle_get_tips(&self, request: &GetTipsRequest) -> SyncResponse {
        match self.backend.get_tips(&request.tree_id) {
            Ok(tips) => SyncResponse::Tips(GetTipsResponse {
                tree_id: request.tree_id.clone(),
                tips,
            }),
            Err(e) => SyncResponse::Error(format!(
                "Failed to get tips for tree {}: {e}",
                request.tree_id
            )),
        }
    }

    /// Handle a request for specific entries.
    async fn handle_get_entries(&self, request: &GetEntriesRequest) -> SyncResponse {
        let mut entries = Vec::with_capacity(request.entry_ids.len());

        for entry_id in &request.entry_ids {
            match self.backend.get(entry_id) {
                Ok(entry) => entries.push(entry),
                Err(e) if e.is_not_found() => {
                    return SyncResponse::Error(format!("Entry not found: {entry_id}"));
                }
                Err(e) => {
                    return SyncResponse::Error(format!("Failed to get entry {entry_id}: {e}"));
                }
            }
        }

        SyncResponse::Entries(GetEntriesResponse { entries })
    }
}
