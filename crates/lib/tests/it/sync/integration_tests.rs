use eidetica::{
    Entry, Error,
    sync::{Address, transports::http::HttpTransport},
};

use super::helpers::*;

#[tokio::test]
async fn test_sync_with_http_transport() {
    let (_base_db, sync) = setup().await;

    // Enable HTTP transport and start server
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync.accept_connections().await.unwrap();

    // Get the actual bound address
    let server_addr = sync.get_server_address().await.unwrap();
    let http_address = Address::http(&server_addr);

    // Test the new protocol by sending entries
    let entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": "value"}"#)
        .build()
        .expect("Entry should build successfully");

    sync.send_entries(vec![entry], &http_address).await.unwrap();

    // Stop server
    sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_multiple_sync_instances_communication() {
    // Create two separate sync instances
    let (_base_db1, sync_server) = setup().await;
    let (_base_db2, sync_client) = setup().await;

    // Enable HTTP transport on both
    sync_server
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync_client
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Start server on first instance
    sync_server.accept_connections().await.unwrap();

    // Get the actual bound address from the server instance
    let server_addr = sync_server.get_server_address().await.unwrap();

    // Test communication by sending entries from client to server
    let entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"message": "hello from client"}"#)
        .build()
        .expect("Entry should build successfully");

    let http_address = Address::http(&server_addr);
    sync_client
        .send_entries(vec![entry], &http_address)
        .await
        .unwrap();

    // Clean up
    sync_server.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_send_entries_http() {
    // Create two separate sync instances
    let (_base_db1, sync_server) = setup().await;
    let (_base_db2, sync_client) = setup().await;

    // Enable HTTP transport on both
    sync_server
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync_client
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Start server on first instance
    sync_server.accept_connections().await.unwrap();

    // Get the actual bound address from the server instance
    let server_addr = sync_server.get_server_address().await.unwrap();

    // Create some test entries
    let entry1 = Entry::root_builder()
        .set_subtree_data("users", r#"{"user1": "data1"}"#)
        .build()
        .expect("Entry should build successfully");
    let entry2 = Entry::root_builder()
        .set_subtree_data("users", r#"{"user2": "data2"}"#)
        .build()
        .expect("Entry should build successfully");
    let entries = vec![entry1, entry2];

    // Send entries from client to server
    let http_address = Address::http(&server_addr);
    sync_client
        .send_entries(entries, &http_address)
        .await
        .unwrap();

    // Clean up
    sync_server.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_sync_without_transport_enabled() {
    let (_base_db, sync) = setup().await;

    // Attempting to send entries without enabling transport should fail
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let result = sync
        .send_entries(vec![entry], &Address::http("127.0.0.1:8084"))
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        Error::Sync(ref sync_err) => {
            assert!(sync_err.is_configuration_error());
        }
        _ => panic!("Expected Sync error, got {err:?}"),
    }
}

#[tokio::test]
async fn test_sync_server_without_transport_enabled() {
    let (_base_db, sync) = setup().await;

    // Attempting to start server without enabling transport should fail
    let result = sync.accept_connections().await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        Error::Sync(ref sync_err) => {
            assert!(sync_err.is_configuration_error());
        }
        _ => panic!("Expected Sync error, got {err:?}"),
    }
}

#[tokio::test]
async fn test_sync_connect_to_invalid_address() {
    let (_base_db, sync) = setup().await;
    sync.register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Try to send entries to a non-existent server
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let result = sync
        .send_entries(vec![entry], &Address::http("127.0.0.1:19998"))
        .await;
    assert!(result.is_err());
}

/// A request that hangs must not stop unrelated requests from being served.
///
/// The background engine handles commands one at a time, so awaiting a request
/// inline holds the engine for as long as the peer takes to answer — and a peer
/// that accepts a connection and then goes quiet takes the full transport
/// deadline. A deployment carrying a handful of retired peers therefore starves
/// the live one, which presents as "everything times out" rather than as one
/// absent peer.
#[tokio::test]
async fn a_hung_request_does_not_block_an_unrelated_one() {
    use std::{sync::Arc, time::Duration};

    // A healthy peer, answering normally.
    let (_server_db, sync_server) = setup().await;
    sync_server
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync_server.accept_connections().await.unwrap();
    let live = Address::http(sync_server.get_server_address().await.unwrap());

    // A black hole: completes the TCP handshake, then never answers. This is
    // what a peer that has gone away behind a live NAT looks like — connecting
    // succeeds, so nothing fails fast.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let black_hole = Address::http(listener.local_addr().unwrap().to_string());
    let accepting = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((conn, _)) = listener.accept().await {
            held.push(conn);
        }
    });

    let (_client_db, client) = setup().await;
    client
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();
    let client = Arc::new(client);

    let entry = || {
        vec![
            Entry::root_builder()
                .set_subtree_data("data", r#"{"k": "v"}"#)
                .build()
                .expect("Failed to build entry"),
        ]
    };

    // Put the doomed request in flight first and give it time to be picked up.
    let hung = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.send_entries(entry(), &black_hole).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The healthy peer answers in milliseconds; allow generously more than that
    // but far less than the transport deadline the hung request is burning.
    let served =
        tokio::time::timeout(Duration::from_secs(5), client.send_entries(entry(), &live)).await;

    hung.abort();
    accepting.abort();

    let served = served.expect("a healthy peer was starved by an unrelated hung request");
    served.expect("sending to the healthy peer failed");
}
