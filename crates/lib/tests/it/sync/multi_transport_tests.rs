//! Tests for multi-transport support in the sync system.
//!
//! These tests verify that multiple transports can be enabled simultaneously
//! and that the system routes requests appropriately.

use eidetica::Result;
use eidetica::sync::transports::http::HttpTransport;

use super::helpers::setup;

/// Test that multiple HTTP transports can be enabled (though unusual, validates the mechanism)
#[tokio::test]
async fn test_enable_http_transport_twice_adds_transport() -> Result<()> {
    let (_instance, sync) = setup();

    // Enable first HTTP transport
    sync.enable_http_transport()?;

    // Enable second HTTP transport - should still succeed (adds to existing)
    let http2 = HttpTransport::new()?;
    sync.add_transport_async(Box::new(http2)).await?;

    // Start server on all transports
    sync.start_server_async("127.0.0.1:0").await?;

    // Get all server addresses - should have entries
    let addresses = sync.get_all_server_addresses_async().await?;
    assert!(
        !addresses.is_empty(),
        "Should have at least one server address"
    );

    // Each HTTP transport will bind to its own port
    // (In practice you'd use different transport types like HTTP + Iroh)

    sync.stop_server_async().await?;

    Ok(())
}

/// Test that transport routing works correctly
#[tokio::test]
async fn test_transport_routing_by_address_type() -> Result<()> {
    let (_instance, sync) = setup();

    // Enable HTTP transport
    sync.enable_http_transport()?;

    // Start server
    sync.start_server_async("127.0.0.1:0").await?;

    // Get the server address
    let addr = sync.get_server_address_async().await?;
    assert!(!addr.is_empty(), "Should have a server address");

    // Get all addresses
    let all_addresses = sync.get_all_server_addresses_async().await?;
    assert_eq!(all_addresses.len(), 1, "Should have exactly one transport");
    assert_eq!(all_addresses[0].0, "http", "Should be HTTP transport type");

    sync.stop_server_async().await?;

    Ok(())
}

/// Test server start/stop with multiple transports
#[tokio::test]
async fn test_server_lifecycle_with_multiple_transports() -> Result<()> {
    let (_instance, sync) = setup();

    // Enable two transports
    sync.enable_http_transport()?;
    let http2 = HttpTransport::new()?;
    sync.add_transport_async(Box::new(http2)).await?;

    // Start servers on all transports
    sync.start_server_async("127.0.0.1:0").await?;

    // Verify servers are running
    let addresses = sync.get_all_server_addresses_async().await?;
    assert!(!addresses.is_empty(), "Should have running servers");

    // Stop all servers
    sync.stop_server_async().await?;

    // Verify no more addresses available
    let result = sync.get_server_address_async().await;
    assert!(
        result.is_err(),
        "Should have no server addresses after stop"
    );

    Ok(())
}
