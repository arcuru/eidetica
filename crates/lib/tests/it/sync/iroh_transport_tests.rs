use eidetica::sync::{
    Address,
    transports::{SyncTransport, iroh::IrohTransport},
};

#[tokio::test]
async fn test_iroh_transport_server_lifecycle() {
    let mut transport = IrohTransport::new().unwrap();

    // Server should not be running initially
    assert!(!transport.is_server_running());

    // Start server (Iroh ignores the address parameter and uses its own addressing)
    transport.start_server("ignored").await.unwrap();
    assert!(transport.is_server_running());

    // Stop server
    transport.stop_server().await.unwrap();
    assert!(!transport.is_server_running());
}

#[tokio::test]
async fn test_iroh_transport_double_start_error() {
    let mut transport = IrohTransport::new().unwrap();

    // Start server
    transport.start_server("ignored").await.unwrap();

    // Attempting to start again should fail
    let result = transport.start_server("ignored").await;
    assert!(result.is_err());

    // Clean up
    transport.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_iroh_transport_stop_without_start() {
    let mut transport = IrohTransport::new().unwrap();

    // Attempting to stop when not running should fail
    let result = transport.stop_server().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_iroh_transport_get_server_address() {
    let mut transport = IrohTransport::new().unwrap();

    // Should return error when no server is running
    let result = transport.get_server_address();
    assert!(result.is_err());

    // Start server
    transport.start_server("ignored").await.unwrap();

    // Should return the node ID
    let addr = transport.get_server_address().unwrap();
    assert!(!addr.is_empty());
    // Iroh node IDs are typically 64 hex characters (32 bytes)
    assert!(addr.len() >= 32);

    // Stop server
    transport.stop_server().await.unwrap();

    // Should return error again after stopping
    let result = transport.get_server_address();
    assert!(result.is_err());
}

#[tokio::test]
async fn test_iroh_transport_send_request_no_endpoint() {
    use eidetica::entry::Entry;

    let transport = IrohTransport::new().unwrap();

    // Try to send request without initializing endpoint
    let entry = Entry::builder("test").build();
    let result = transport
        .send_request(&Address::iroh("invalid_node_id"), &[entry])
        .await;

    assert!(result.is_err());
}

// Note: Testing actual client-server communication with Iroh requires
// more complex setup involving discovery mechanisms and relay servers.
// These tests focus on the basic transport lifecycle and error conditions.

#[tokio::test]
async fn test_iroh_transport_integration_lifecycle() {
    let mut server_transport = IrohTransport::new().unwrap();

    // Test complete server lifecycle
    assert!(!server_transport.is_server_running());

    // Start server
    server_transport.start_server("ignored").await.unwrap();
    assert!(server_transport.is_server_running());

    // Get server address
    let server_addr = server_transport.get_server_address().unwrap();
    assert!(!server_addr.is_empty());

    // Try to start again (should fail)
    let result = server_transport.start_server("ignored").await;
    assert!(result.is_err());

    // Stop server
    server_transport.stop_server().await.unwrap();
    assert!(!server_transport.is_server_running());

    // Try to stop again (should fail)
    let result = server_transport.stop_server().await;
    assert!(result.is_err());
}

// Test that demonstrates the P2P nature of Iroh transport
#[tokio::test]
async fn test_iroh_transport_p2p_addressing() {
    let mut transport1 = IrohTransport::new().unwrap();
    let mut transport2 = IrohTransport::new().unwrap();

    // Start both transports
    transport1.start_server("ignored").await.unwrap();
    transport2.start_server("ignored").await.unwrap();

    // Get their addresses (node IDs)
    let addr1 = transport1.get_server_address().unwrap();
    let addr2 = transport2.get_server_address().unwrap();

    // Addresses should be different
    assert_ne!(addr1, addr2);

    // Both should be valid node ID format (hex string)
    assert!(addr1.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(addr2.chars().all(|c| c.is_ascii_hexdigit()));

    // Clean up
    transport1.stop_server().await.unwrap();
    transport2.stop_server().await.unwrap();
}
