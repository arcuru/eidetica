//! Shared business logic for handling sync requests.
//!
//! This module contains transport-agnostic handlers that process
//! sync requests and generate responses. These handlers can be
//! used by any transport implementation.

use super::protocol::{
    HandshakeRequest, HandshakeResponse, PROTOCOL_VERSION, SyncRequest, SyncResponse,
};

/// Handle a sync request and generate an appropriate response.
///
/// This is the main entry point for processing sync messages,
/// regardless of which transport they arrived through.
///
/// NOTE: This is currently not used by the HTTP transport which has its own
/// simplified handler. This will be integrated in a future phase when we
/// have proper Sync instance passing to the transport layer.
///
/// # Arguments
/// * `request` - The sync request to process
///
/// # Returns
/// The appropriate response for the given request.
pub async fn handle_request(request: &SyncRequest) -> SyncResponse {
    match request {
        SyncRequest::Handshake(handshake_req) => handle_handshake(handshake_req).await,
        SyncRequest::SendEntries(entries) => {
            // TODO: Process the received entries - store them in the local database
            // For now, just acknowledge receipt
            let count = entries.len();
            println!("Received {count} entries for synchronization");

            if count == 1 {
                SyncResponse::Ack
            } else {
                SyncResponse::Count(count)
            }
        }
        SyncRequest::GetTips(_req) => {
            // TODO: Implement tree tip retrieval
            SyncResponse::Error("GetTips not yet implemented".to_string())
        }
        SyncRequest::GetEntries(_req) => {
            // TODO: Implement entry retrieval
            SyncResponse::Error("GetEntries not yet implemented".to_string())
        }
    }
}

/// Handle a handshake request from a peer.
async fn handle_handshake(request: &HandshakeRequest) -> SyncResponse {
    // Check protocol version compatibility
    if request.protocol_version != PROTOCOL_VERSION {
        return SyncResponse::Error(format!(
            "Protocol version mismatch: expected {}, got {}",
            PROTOCOL_VERSION, request.protocol_version
        ));
    }

    // TODO: Get device ID and public key from sync settings
    // For now, use placeholder values
    let device_id = "server_device".to_string();
    let public_key = "ed25519:server_key".to_string();

    // TODO: Implement signature verification of the challenge
    // For now, just echo back the challenge as the response
    let challenge_response = request.challenge.clone();

    // Generate a new challenge for mutual authentication
    let new_challenge = generate_challenge();

    SyncResponse::Handshake(HandshakeResponse {
        device_id,
        public_key,
        display_name: Some("Sync Server".to_string()),
        protocol_version: PROTOCOL_VERSION,
        challenge_response,
        new_challenge,
    })
}

/// Generate random challenge bytes for authentication.
fn generate_challenge() -> Vec<u8> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut challenge = vec![0u8; 32];
    rng.fill(&mut challenge[..]);
    challenge
}
