//! Comprehensive unit tests for sync protocol handlers.
//!
//! This module tests the core synchronization protocol functionality including
//! tip exchange, entry retrieval, and validation.

use eidetica::{
    entry::{Entry, ID},
    sync::{Sync, protocol::*},
};

use super::helpers;

/// Create a test sync instance with sample data.
async fn create_test_sync_with_data() -> (Sync, ID, Vec<Entry>) {
    let (_base_db, sync) = helpers::setup();

    // Create some test entries
    let entry1 = Entry::builder("test_tree_root")
        .set_subtree_data("data", r#"{"test": "entry1"}"#)
        .build().expect("Entry should build successfully");

    let entry2 = Entry::builder("test_tree_root")
        .add_parent(entry1.id().as_str())
        .set_subtree_data("data", r#"{"test": "entry2"}"#)
        .build().expect("Entry should build successfully");

    let entry3 = Entry::builder("test_tree_root")
        .add_parent(entry2.id().as_str())
        .set_subtree_data("data", r#"{"test": "entry3"}"#)
        .build().expect("Entry should build successfully");

    let entries = vec![entry1.clone(), entry2.clone(), entry3.clone()];

    // Store entries in backend
    for entry in &entries {
        sync.backend().put_verified(entry.clone()).unwrap();
    }

    let tree_root_id: ID = "test_tree_root".into();
    (sync, tree_root_id, entries)
}

#[tokio::test]
async fn test_handshake_protocol() {
    let (_base_db, sync) = helpers::setup();

    let handshake_request = HandshakeRequest {
        device_id: "test_device".to_string(),
        public_key: "ed25519:test_key".to_string(),
        display_name: Some("Test Device".to_string()),
        protocol_version: PROTOCOL_VERSION,
        challenge: vec![1, 2, 3, 4, 5],
    };

    let request = SyncRequest::Handshake(handshake_request.clone());
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Handshake(handshake_response) => {
            // Check that we got a valid handshake response
            assert_eq!(handshake_response.protocol_version, PROTOCOL_VERSION);
            assert!(!handshake_response.device_id.is_empty());
            assert!(!handshake_response.public_key.is_empty());

            // Verify that the challenge_response is a valid signature of our challenge
            use base64ct::{Base64, Encoding};
            use eidetica::auth::crypto::{parse_public_key, verify_signature};

            let verifying_key = parse_public_key(&handshake_response.public_key)
                .expect("Should have valid public key format");
            let signature_b64 = Base64::encode_string(&handshake_response.challenge_response);
            let signature_valid =
                verify_signature(&handshake_request.challenge, &signature_b64, &verifying_key)
                    .expect("Signature verification should not error");

            assert!(
                signature_valid,
                "Challenge response should be a valid signature of our challenge"
            );
            assert!(!handshake_response.new_challenge.is_empty());
        }
        _ => panic!("Expected handshake response, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_handshake_protocol_version_mismatch() {
    let (_base_db, sync) = helpers::setup();

    let handshake_request = HandshakeRequest {
        device_id: "test_device".to_string(),
        public_key: "ed25519:test_key".to_string(),
        display_name: Some("Test Device".to_string()),
        protocol_version: 999, // Invalid version
        challenge: vec![1, 2, 3, 4, 5],
    };

    let request = SyncRequest::Handshake(handshake_request);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Error(msg) => {
            assert!(msg.contains("Protocol version mismatch"));
        }
        _ => panic!("Expected error response, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_get_tips_protocol() {
    let (sync, tree_root_id, _entries) = create_test_sync_with_data().await;

    let get_tips_request = GetTipsRequest {
        tree_id: tree_root_id.clone(),
    };

    let request = SyncRequest::GetTips(get_tips_request.clone());
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Tips(tips_response) => {
            assert_eq!(tips_response.tree_id, tree_root_id);
            // Should have at least one tip
            assert!(!tips_response.tips.is_empty());
        }
        _ => panic!("Expected tips response, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_get_tips_nonexistent_tree() {
    let (_base_db, sync) = helpers::setup();

    let nonexistent_tree_id: ID = "nonexistent_tree".into();
    let get_tips_request = GetTipsRequest {
        tree_id: nonexistent_tree_id,
    };

    let request = SyncRequest::GetTips(get_tips_request);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Tips(tips_response) => {
            // Should return empty tips for nonexistent tree
            assert!(tips_response.tips.is_empty());
        }
        _ => panic!("Expected tips response, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_get_entries_protocol() {
    let (sync, _tree_root_id, entries) = create_test_sync_with_data().await;

    let entry_ids: Vec<ID> = entries.iter().map(|e| e.id()).collect();
    let get_entries_request = GetEntriesRequest {
        entry_ids: entry_ids.clone(),
    };

    let request = SyncRequest::GetEntries(get_entries_request);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Entries(entries_response) => {
            assert_eq!(entries_response.entries.len(), entries.len());

            // Verify we got the correct entries
            for (expected, actual) in entries.iter().zip(entries_response.entries.iter()) {
                assert_eq!(expected.id(), actual.id());
            }
        }
        _ => panic!("Expected entries response, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_get_entries_single_entry() {
    let (sync, _tree_root_id, entries) = create_test_sync_with_data().await;

    let single_entry_id = entries[0].id();
    let get_entries_request = GetEntriesRequest {
        entry_ids: vec![single_entry_id.clone()],
    };

    let request = SyncRequest::GetEntries(get_entries_request);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Entries(entries_response) => {
            assert_eq!(entries_response.entries.len(), 1);
            assert_eq!(entries_response.entries[0].id(), single_entry_id);
        }
        _ => panic!("Expected entries response, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_get_entries_nonexistent() {
    let (_base_db, sync) = helpers::setup();

    let nonexistent_id: ID = "nonexistent_entry".into();
    let get_entries_request = GetEntriesRequest {
        entry_ids: vec![nonexistent_id.clone()],
    };

    let request = SyncRequest::GetEntries(get_entries_request);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Error(msg) => {
            assert!(msg.contains("Entry not found"));
            assert!(msg.contains(nonexistent_id.as_str()));
        }
        _ => panic!("Expected error response, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_get_entries_partial_missing() {
    let (sync, _tree_root_id, entries) = create_test_sync_with_data().await;

    let existing_id = entries[0].id();
    let nonexistent_id: ID = "nonexistent_entry".into();

    let get_entries_request = GetEntriesRequest {
        entry_ids: vec![existing_id, nonexistent_id.clone()],
    };

    let request = SyncRequest::GetEntries(get_entries_request);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Error(msg) => {
            // Should fail on the first missing entry
            assert!(msg.contains("Entry not found"));
            assert!(msg.contains(nonexistent_id.as_str()));
        }
        _ => panic!("Expected error response, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_send_entries_protocol() {
    let (_base_db, sync) = helpers::setup();

    let single_entry = Entry::builder("test_root")
        .set_subtree_data("data", r#"{"test": "single"}"#)
        .build().expect("Entry should build successfully");

    let request = SyncRequest::SendEntries(vec![single_entry]);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Ack => {
            // Expected for single entry
        }
        _ => panic!("Expected Ack response for single entry, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_send_multiple_entries_protocol() {
    let (_base_db, sync) = helpers::setup();

    let entry1 = Entry::builder("test_root")
        .set_subtree_data("data", r#"{"test": "entry1"}"#)
        .build().expect("Entry should build successfully");

    let entry2 = Entry::builder("test_root")
        .set_subtree_data("data", r#"{"test": "entry2"}"#)
        .build().expect("Entry should build successfully");

    let request = SyncRequest::SendEntries(vec![entry1, entry2]);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Count(count) => {
            assert_eq!(count, 2);
        }
        _ => panic!("Expected Count response for multiple entries, got: {response:?}"),
    }
}

#[tokio::test]
async fn test_send_entries_empty_list() {
    let (_base_db, sync) = helpers::setup();

    let request = SyncRequest::SendEntries(vec![]);
    let response = helpers::handle_request(&sync, &request).await;

    match response {
        SyncResponse::Ack => {
            // Should handle empty list gracefully
        }
        _ => panic!("Expected Ack response for empty entry list, got: {response:?}"),
    }
}
