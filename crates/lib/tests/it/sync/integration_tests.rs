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

    hold_verified_entries(&sync, std::slice::from_ref(&entry)).await;
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
    hold_verified_entries(&sync_client, std::slice::from_ref(&entry)).await;
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

    hold_verified_entries(&sync_client, &entries).await;
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
    hold_verified_entries(&sync, std::slice::from_ref(&entry)).await;
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
    hold_verified_entries(&sync, std::slice::from_ref(&entry)).await;
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

    let entries = vec![
        Entry::root_builder()
            .set_subtree_data("data", r#"{"k": "v"}"#)
            .build()
            .expect("Failed to build entry"),
    ];
    hold_verified_entries(&client, &entries).await;
    // Put the doomed request in flight first and give it time to be picked up.
    let hung = {
        let client = Arc::clone(&client);
        let entries = entries.clone();
        tokio::spawn(async move { client.send_entries(entries, &black_hole).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The healthy peer answers in milliseconds; allow generously more than that
    // but far less than the transport deadline the hung request is burning.
    let served =
        tokio::time::timeout(Duration::from_secs(5), client.send_entries(entries, &live)).await;

    hung.abort();
    accepting.abort();

    let served = served.expect("a healthy peer was starved by an unrelated hung request");
    served.expect("sending to the healthy peer failed");
}

/// A stale address must not hide a working one, whichever order they sit in.
///
/// A peer's address list only grows — anything that changes address on restart
/// appends and leaves the old entry behind — so a peer that has ever moved is
/// the normal case, not an edge case. Dialing only one entry makes such a peer
/// permanently unreachable while it is up and serving, and the failure presents
/// as a timeout, which reads as "the peer is down".
///
/// Both orders are checked because passing in only one is exactly what a
/// first-address implementation does.
#[tokio::test]
async fn a_stale_address_does_not_hide_a_working_one() {
    for stale_first in [true, false] {
        let (_si, _su, _sk, _sdb, tree_id, server_sync) =
            setup_public_sync_enabled_server("server_user", "server_key", "db").await;
        let live = start_sync_server(&server_sync).await;

        // Bind then drop, so the port is free: connecting is refused rather
        // than hanging, which keeps the test quick while still being an
        // address that cannot serve.
        let dead = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap().to_string();
            drop(l);
            Address::http(addr)
        };

        let (client_instance, _cu, _ck, client_sync) =
            setup_sync_enabled_client("client_user", "client_key").await;
        client_sync
            .register_transport("http", HttpTransport::builder())
            .await
            .unwrap();

        let server_pubkey = server_sync.get_device_pubkey().unwrap();
        client_sync
            .register_peer(&server_pubkey, Some("server"))
            .await
            .unwrap();
        let order = if stale_first {
            [dead, live]
        } else {
            [live, dead]
        };
        for addr in order {
            client_sync
                .add_peer_address(&server_pubkey, addr)
                .await
                .unwrap();
        }

        client_sync
            .sync_tree_with_peer(&server_pubkey, &tree_id)
            .await
            .unwrap_or_else(|e| {
                panic!("sync failed with stale_first={stale_first}: {e}");
            });
        client_sync.flush().await.ok();

        assert!(
            client_instance.has_database(&tree_id).await,
            "tree did not arrive with stale_first={stale_first}"
        );
        server_sync.stop_server().await.unwrap();
    }
}

/// A hung handshake must not stall a handshake with a different peer.
///
/// Same defect as `a_hung_request_does_not_block_an_unrelated_one`, on the
/// command most exposed to it: a handshake is aimed at a peer the engine has
/// never reached, which is exactly the peer most likely not to be there.
#[tokio::test]
async fn a_hung_handshake_does_not_block_an_unrelated_one() {
    use std::{sync::Arc, time::Duration};

    let (_server_db, sync_server) = setup().await;
    sync_server
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync_server.accept_connections().await.unwrap();
    let live = Address::http(sync_server.get_server_address().await.unwrap());

    // Completes the TCP handshake, then never answers — what a peer that went
    // away behind a live NAT looks like. Nothing fails fast.
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

    let hung = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.connect_to_peer(&black_hole).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;

    let served = tokio::time::timeout(Duration::from_secs(5), client.connect_to_peer(&live)).await;

    hung.abort();
    accepting.abort();

    let served = served.expect("a reachable peer's handshake was starved by a hung one");
    served.expect("handshake with the healthy peer failed");
}

/// Forward to `target`, but stall `delay` before opening the upstream connection.
///
/// Used to fix the outcome of an address race. Two live HTTP servers on loopback
/// both answer in single-digit milliseconds, so which one wins is a coin toss —
/// and a coin-toss regression passes half the time, which is worse than no test.
async fn delaying_proxy(target: String, delay: std::time::Duration) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            let target = target.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                if let Ok(mut outbound) = tokio::net::TcpStream::connect(&target).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    addr
}

/// An address that answers as somebody else must not be taken as the route.
///
/// Racing handshakes across a peer's addresses picks a route, but a handshake
/// also says *who answered*. A peer's address list only ever grows, so a stale
/// entry can be reoccupied by an unrelated node — which completes a handshake
/// perfectly well. Selecting that node both misdirects the exchange and, because
/// the route is chosen once, stops the working address behind it from ever being
/// tried: precisely the permanent unreachability this path exists to remove,
/// reintroduced one layer up.
///
/// The stranger is made to win the race outright by putting the real server
/// behind a delaying proxy, so a route selector that ignores identity fails here
/// every run rather than half of them.
#[tokio::test]
async fn an_address_answering_as_another_peer_does_not_hide_the_real_one() {
    use std::time::Duration;

    let (_si, _su, _sk, _sdb, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "db").await;
    let _real = start_sync_server(&server_sync).await;
    let real_raw = server_sync.get_server_address_for("http").await.unwrap();

    // A second, unrelated node: live, speaking the protocol, and not our peer.
    let (_oi, _ou, _ok, other_sync) = setup_sync_enabled_client("other_user", "other_key").await;
    let stranger = start_sync_server(&other_sync).await;

    let slow_real = Address::http(delaying_proxy(real_raw, Duration::from_millis(750)).await);

    let (client_instance, _cu, _ck, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    let server_pubkey = server_sync.get_device_pubkey().unwrap();
    client_sync
        .register_peer(&server_pubkey, Some("server"))
        .await
        .unwrap();
    for addr in [stranger, slow_real] {
        client_sync
            .add_peer_address(&server_pubkey, addr)
            .await
            .unwrap();
    }

    client_sync
        .sync_tree_with_peer(&server_pubkey, &tree_id)
        .await
        .expect("a stranger winning the handshake race hid the real peer");
    client_sync.flush().await.ok();

    assert!(
        client_instance.has_database(&tree_id).await,
        "tree did not arrive from the peer we asked for"
    );

    server_sync.stop_server().await.unwrap();
    other_sync.stop_server().await.unwrap();
}
