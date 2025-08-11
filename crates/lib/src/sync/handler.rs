//! Shared business logic for handling sync requests.
//!
//! This module contains transport-agnostic handlers that process
//! sync requests and generate responses. These handlers can be
//! used by any transport implementation.

use super::protocol::{SyncRequest, SyncResponse};

/// Handle a sync request and generate an appropriate response.
///
/// This is the main entry point for processing sync messages,
/// regardless of which transport they arrived through.
///
/// # Arguments
/// * `request` - The sync request to handle
///
/// # Returns
/// The appropriate response for the given request.
pub async fn handle_request(request: SyncRequest) -> SyncResponse {
    match request {
        SyncRequest::Hello => SyncResponse::Hello("Hello from Eidetica Sync!".to_string()),
        SyncRequest::Status => SyncResponse::Status("Sync Status: Active".to_string()),
    }
}
