use eidetica::sync::{
    Address,
    protocol::SyncRequest,
    transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport},
};

/// Test that both HTTP and Iroh transports follow the same interface
#[tokio::test]
async fn test_transport_interface_consistency() {
    let mut http_transport = HttpTransport::new().unwrap();
    let mut iroh_transport = IrohTransport::new().unwrap();

    // Both should not be running initially
    assert!(!http_transport.is_server_running());
    assert!(!iroh_transport.is_server_running());

    // Both should fail to get address when not running
    assert!(http_transport.get_server_address().is_err());
    assert!(iroh_transport.get_server_address().is_err());

    // Both should fail to stop when not running
    assert!(http_transport.stop_server().await.is_err());
    assert!(iroh_transport.stop_server().await.is_err());

    // Start both servers
    let handler = super::helpers::setup_test_handler();
    http_transport
        .start_server("127.0.0.1:0", handler)
        .await
        .unwrap();
    let handler = super::helpers::setup_test_handler();
    iroh_transport
        .start_server("ignored", handler)
        .await
        .unwrap();

    // Both should now be running
    assert!(http_transport.is_server_running());
    assert!(iroh_transport.is_server_running());

    // Both should return addresses
    let http_addr = http_transport.get_server_address().unwrap();
    let iroh_addr = iroh_transport.get_server_address().unwrap();

    assert!(!http_addr.is_empty());
    assert!(!iroh_addr.is_empty());

    // HTTP address should be IP:port format
    assert!(http_addr.contains(":"));
    assert!(http_addr.starts_with("127.0.0.1:"));

    // Iroh address should be JSON format with node_id and direct_addresses
    assert!(iroh_addr.contains("node_id"));
    assert!(iroh_addr.contains("direct_addresses"));

    // Both should fail to start again
    let handler2 = super::helpers::setup_test_handler();
    let handler3 = super::helpers::setup_test_handler();
    assert!(
        http_transport
            .start_server("127.0.0.1:0", handler2)
            .await
            .is_err()
    );
    assert!(
        iroh_transport
            .start_server("ignored", handler3)
            .await
            .is_err()
    );

    // Clean up both
    http_transport.stop_server().await.unwrap();
    iroh_transport.stop_server().await.unwrap();

    // Both should not be running after stop
    assert!(!http_transport.is_server_running());
    assert!(!iroh_transport.is_server_running());
}

/// Test error handling consistency across transports
#[tokio::test]
async fn test_transport_error_handling_consistency() {
    use eidetica::Entry;

    let http_transport = HttpTransport::new().unwrap();
    let iroh_transport = IrohTransport::new().unwrap();

    // Both should fail to send requests when no server is running
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let request = SyncRequest::SendEntries(vec![entry.clone()]);
    let http_result = http_transport
        .send_request(&Address::http("127.0.0.1:59999"), &request)
        .await;
    let iroh_result = iroh_transport
        .send_request(
            &Address::iroh("invalid_node_id"),
            &SyncRequest::SendEntries(vec![entry]),
        )
        .await;

    assert!(http_result.is_err());
    assert!(iroh_result.is_err());
}

/// Test that transport creation doesn't interfere with each other
#[tokio::test]
async fn test_transport_isolation() {
    // Create multiple instances of each transport type
    let mut http1 = HttpTransport::new().unwrap();
    let mut http2 = HttpTransport::new().unwrap();
    let mut iroh1 = IrohTransport::new().unwrap();
    let mut iroh2 = IrohTransport::new().unwrap();

    // All should be able to start servers independently
    let handler1 = super::helpers::setup_test_handler();
    let handler2 = super::helpers::setup_test_handler();
    let handler3 = super::helpers::setup_test_handler();
    let handler4 = super::helpers::setup_test_handler();
    http1.start_server("127.0.0.1:0", handler1).await.unwrap();
    http2.start_server("127.0.0.1:0", handler2).await.unwrap();
    iroh1.start_server("ignored", handler3).await.unwrap();
    iroh2.start_server("ignored", handler4).await.unwrap();

    // All should have different addresses
    let addr1 = http1.get_server_address().unwrap();
    let addr2 = http2.get_server_address().unwrap();
    let addr3 = iroh1.get_server_address().unwrap();
    let addr4 = iroh2.get_server_address().unwrap();

    // HTTP addresses should be different (different ports)
    assert_ne!(addr1, addr2);

    // Iroh addresses should be different (different node IDs)
    assert_ne!(addr3, addr4);

    // HTTP and Iroh addresses should be in different formats
    assert!(addr1.contains(":")); // HTTP format has port
    assert!(addr2.contains(":")); // HTTP format has port
    // Iroh addresses now contain JSON with node_id and direct_addresses
    assert!(addr3.contains("node_id")); // Iroh JSON format
    assert!(addr4.contains("node_id")); // Iroh JSON format

    // Clean up all
    http1.stop_server().await.unwrap();
    http2.stop_server().await.unwrap();
    iroh1.stop_server().await.unwrap();
    iroh2.stop_server().await.unwrap();
}

/// Test the SyncTransport trait can be used polymorphically
#[tokio::test]
async fn test_transport_polymorphism() {
    let mut transports: Vec<Box<dyn SyncTransport + Send>> = vec![
        Box::new(HttpTransport::new().unwrap()),
        Box::new(IrohTransport::new().unwrap()),
    ];

    // Test that all transports implement the same interface
    for (i, transport) in transports.iter_mut().enumerate() {
        assert!(!transport.is_server_running());

        let addr = if i == 0 { "127.0.0.1:0" } else { "ignored" };
        let handler = super::helpers::setup_test_handler();
        transport.start_server(addr, handler).await.unwrap();

        assert!(transport.is_server_running());

        let server_addr = transport.get_server_address().unwrap();
        assert!(!server_addr.is_empty());

        transport.stop_server().await.unwrap();
        assert!(!transport.is_server_running());
    }
}

/// Test concurrent operation of different transport types
#[tokio::test]
async fn test_concurrent_transport_operation() {
    // Test that HTTP and Iroh can operate simultaneously
    let mut http_transport = HttpTransport::new().unwrap();
    let mut iroh_transport = IrohTransport::new().unwrap();

    // Start both concurrently
    let handler1 = super::helpers::setup_test_handler();
    let handler2 = super::helpers::setup_test_handler();
    let http_future = http_transport.start_server("127.0.0.1:0", handler1);
    let iroh_future = iroh_transport.start_server("ignored", handler2);

    let (http_result, iroh_result) = tokio::join!(http_future, iroh_future);

    http_result.unwrap();
    iroh_result.unwrap();

    // Both should be running
    assert!(http_transport.is_server_running());
    assert!(iroh_transport.is_server_running());

    // Stop both concurrently
    let http_stop = http_transport.stop_server();
    let iroh_stop = iroh_transport.stop_server();

    let (http_stop_result, iroh_stop_result) = tokio::join!(http_stop, iroh_stop);

    http_stop_result.unwrap();
    iroh_stop_result.unwrap();

    // Both should be stopped
    assert!(!http_transport.is_server_running());
    assert!(!iroh_transport.is_server_running());
}
