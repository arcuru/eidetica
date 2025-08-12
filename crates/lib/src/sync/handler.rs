//! Shared business logic for handling sync requests.
//!
//! This module contains transport-agnostic handlers that process
//! sync requests and generate responses. These handlers can be
//! used by any transport implementation.

use super::protocol::SyncResponse;
use crate::entry::Entry;

/// Handle a sync request and generate an appropriate response.
///
/// This is the main entry point for processing sync messages,
/// regardless of which transport they arrived through.
///
/// # Arguments
/// * `request` - The sync request (&[Entry]) to process
///
/// # Returns
/// The appropriate response for the given request.
pub async fn handle_request(request: &[Entry]) -> SyncResponse {
    // TODO: Process the received entries - store them in the local database
    // For now, just acknowledge receipt
    let count = request.len();
    println!("Received {count} entries for synchronization");

    if count == 1 {
        SyncResponse::Ack
    } else {
        SyncResponse::Count(count)
    }
}
