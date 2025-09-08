# Synchronization Guide

Eidetica's synchronization system enables real-time data synchronization between distributed peers in a decentralized network. This guide covers how to set up, configure, and use the sync features.

## Overview

The sync system uses a **BackgroundSync architecture** with command-pattern communication:

- **Single background thread** handles all sync operations
- **Command-channel communication** between frontend and backend
- **Automatic change detection** via hook system
- **Multiple transport protocols** (HTTP, Iroh P2P)
- **Database-level sync relationships** for granular control
- **Authentication and security** using Ed25519 signatures
- **Persistent state tracking** via DocStore

## Quick Start

### 1. Enable Sync on Your Database

```rust,ignore
use eidetica::{Instance, backend::InMemory};

// Create a database with sync enabled
let backend = Box::new(InMemory::new());
let db = Instance::new(backend).with_sync()?;

// Add a private key for authentication
db.add_private_key("device_key")?;
```

### 2. Enable a Transport Protocol

<!-- TODO: Example uses sync API that doesn't match current implementation - sync_mut() method doesn't exist on Instance/Database -->

```rust,ignore
// Enable HTTP transport
db.sync_mut()?.enable_http_transport()?;

// Start a server to accept connections
db.sync_mut()?.start_server("127.0.0.1:8080")?;
```

### 3. Connect to a Remote Peer

<!-- TODO: Example uses sync API methods that don't exist in current implementation -->

```rust,ignore
use eidetica::sync::{Address, peer_types::PeerStatus};

// Create an address for the remote peer
let remote_addr = Address::http("192.168.1.100:8080")?;

// Connect and perform handshake
let peer_pubkey = db.sync_mut()?.connect_to_peer(&remote_addr).await?;

// Activate the peer for syncing
db.sync_mut()?.update_peer_status(&peer_pubkey, PeerStatus::Active)?;
```

### 4. Set Up Database Synchronization

<!-- TODO: Example uses sync_mut() method that doesn't exist in current implementation -->

```rust,ignore
// Create a database to sync
let database = db.new_database(Doc::new(), "device_key")?;
let tree_id = database.root_id().to_string();

// Configure this database to sync with the peer
db.sync_mut()?.add_tree_sync(&peer_pubkey, &tree_id)?;
```

### 5. Automatic Synchronization

Once configured, any changes to the database will automatically be queued for synchronization:

<!-- TODO: Example uses sync_mut() method that doesn't exist in current implementation -->

```rust,ignore
// Make changes to the database - these will be auto-synced
let op = database.new_transaction()?;
let store = op.get_subtree::<DocStore>("data")?;
store.set_string("message", "Hello, distributed world!")?;
op.commit()?; // This triggers sync queue entry
```

## Transport Protocols

### HTTP Transport

The HTTP transport uses REST APIs for synchronization:

<!-- TODO: Example uses sync API methods that don't match current implementation -->

```rust,ignore
// Enable HTTP transport
sync.enable_http_transport()?;

// Start server
sync.start_server("127.0.0.1:8080")?;

// Connect to remote peer
let addr = Address::http("peer.example.com:8080")?;
let peer_key = sync.connect_to_peer(&addr).await?;
```

### Iroh P2P Transport (Recommended)

Iroh provides direct peer-to-peer connectivity with NAT traversal:

<!-- TODO: Example uses sync API methods that don't match current implementation -->

```rust,ignore
// Enable Iroh transport with production defaults (uses n0's relay servers)
sync.enable_iroh_transport()?;

// Or configure for specific environments:
use iroh::RelayMode;
use eidetica::sync::transports::iroh::IrohTransport;

// For local testing without internet (fast, no relays)
let transport = IrohTransport::builder()
    .relay_mode(RelayMode::Disabled)
    .build()?;
sync.enable_iroh_transport_with_config(transport)?;

// For staging/testing environments
let transport = IrohTransport::builder()
    .relay_mode(RelayMode::Staging)
    .build()?;
sync.enable_iroh_transport_with_config(transport)?;

// For enterprise deployments with custom relay servers
use iroh::{RelayMap, RelayNode, RelayUrl};

let relay_url: RelayUrl = "https://relay.example.com".parse()?;
let relay_node = RelayNode {
    url: relay_url,
    quic: Some(Default::default()), // Enable QUIC for better performance
};
let transport = IrohTransport::builder()
    .relay_mode(RelayMode::Custom(RelayMap::from_iter([relay_node])))
    .build()?;
sync.enable_iroh_transport_with_config(transport)?;

// Start the Iroh server (binds to its own ports)
sync.start_server("ignored")?; // Iroh manages its own addressing

// Get the server address for sharing with peers
let my_address = sync.get_server_address()?;
// This returns a JSON string containing:
// - node_id: Your cryptographic node identity
// - direct_addresses: Socket addresses where you can be reached

// Connect to a peer using their address
let addr = Address::iroh(&peer_address_json)?;
let peer_key = sync.connect_to_peer(&addr).await?;

// Or if you only have the node ID (will use relays to discover)
let addr = Address::iroh(peer_node_id)?;
let peer_key = sync.connect_to_peer(&addr).await?;
```

**Relay Modes:**

- `RelayMode::Default` - Production n0 relay servers (default, recommended for most users)
- `RelayMode::Disabled` - Direct P2P only, no relays (for local testing, requires direct connectivity)
- `RelayMode::Staging` - n0's staging relay servers (for testing against staging infrastructure)
- `RelayMode::Custom(RelayMap)` - Your own relay servers (for enterprise/private deployments)

**How Iroh Connectivity Works:**

1. Peers discover each other through relay servers or direct addresses
2. Attempt direct connection via NAT hole-punching (~90% success rate)
3. Fall back to relay if direct connection fails
4. Automatically upgrade to direct connection when possible

## Sync Configuration

### BackgroundSync Architecture

The sync system automatically starts a background thread when transport is enabled:

```rust,ignore
// The BackgroundSync engine starts automatically when you enable transport
sync.enable_http_transport()?;  // This starts the background thread

// The background thread runs an event loop with:
// - Command processing (immediate)
// - Periodic sync timer (5 minutes)
// - Retry queue timer (30 seconds)
// - Connection check timer (60 seconds)
```

### Automatic Sync Behavior

Once configured, the system handles everything automatically:

```rust,ignore
// When you commit changes, they're sent immediately
let op = database.new_transaction()?;
op.commit()?;  // Sync hook sends command to background thread

// Failed sends are retried with exponential backoff
// 2^attempts seconds delay (max 64 seconds)
// Configurable max attempts before dropping

// No manual queue management or worker control needed
// The BackgroundSync engine handles all operations
```

## Peer Management

### Registering Peers

```rust,ignore
// Register a peer manually
sync.register_peer("ed25519:abc123...", Some("Alice's Device"))?;

// Add multiple addresses for the same peer
sync.add_peer_address(&peer_key, Address::http("192.168.1.100:8080")?)?;
sync.add_peer_address(&peer_key, Address::iroh("iroh://peer_id@relay")?)?;
```

### Peer Status Management

```rust,ignore
use eidetica::sync::peer_types::PeerStatus;

// Activate peer for syncing
sync.update_peer_status(&peer_key, PeerStatus::Active)?;

// Pause syncing with a peer
sync.update_peer_status(&peer_key, PeerStatus::Inactive)?;

// Get peer information
if let Some(peer_info) = sync.get_peer_info(&peer_key)? {
    println!("Peer: {} ({})", peer_info.display_name.unwrap_or("Unknown".to_string()), peer_info.status);
}
```

### Database Sync Relationships

```rust,ignore
// Add database to sync relationship
sync.add_tree_sync(&peer_key, &tree_id)?;

// List all databases synced with a peer
let synced_trees = sync.get_peer_trees(&peer_key)?;

// List all peers syncing a specific database
let syncing_peers = sync.get_tree_peers(&tree_id)?;

// Remove database from sync relationship
sync.remove_tree_sync(&peer_key, &tree_id)?;
```

## Security

### Authentication

All sync operations use Ed25519 digital signatures:

```rust,ignore
// The sync system automatically uses your device key for authentication
// Add additional keys if needed
db.add_private_key("backup_key")?;

// Set a specific key as default for a database
database.set_default_auth_key("backup_key");
```

### Peer Verification

During handshake, peers exchange and verify public keys:

```rust,ignore
// The connect_to_peer method automatically:
// 1. Exchanges public keys
// 2. Verifies signatures
// 3. Registers the verified peer
let verified_peer_key = sync.connect_to_peer(&addr).await?;
```

## Monitoring and Diagnostics

### Sync Operations

The BackgroundSync engine handles all operations automatically:

```rust,ignore
// Entries are synced immediately when committed
// No manual queue management needed

// The background thread handles:
// - Immediate sending of new entries
// - Retry queue with exponential backoff
// - Periodic sync every 5 minutes
// - Connection health checks every minute

// Server status
let is_running = sync.is_server_running();
let server_addr = sync.get_server_address()?;
```

### Sync State Tracking

```rust,ignore
use eidetica::sync::state::SyncStateManager;

// Get sync state for a database-peer relationship
let op = sync.sync_tree().new_operation()?;
let state_manager = SyncStateManager::new(&op);

let cursor = state_manager.get_sync_cursor(&peer_key, &tree_id)?;
println!("Last synced: {:?}", cursor.last_synced_entry);

let metadata = state_manager.get_sync_metadata(&peer_key)?;
println!("Success rate: {:.2}%", metadata.sync_success_rate() * 100.0);
```

## Error Handling

The sync system provides detailed error reporting:

```rust,ignore
use eidetica::sync::SyncError;

match sync.connect_to_peer(&addr).await {
    Ok(peer_key) => println!("Connected to peer: {}", peer_key),
    Err(e) if e.is_sync_error() => {
        match e.sync_error().unwrap() {
            SyncError::HandshakeFailed(msg) => eprintln!("Handshake failed: {}", msg),
            SyncError::NoTransportEnabled => eprintln!("No transport protocol enabled"),
            SyncError::PeerNotFound(key) => eprintln!("Peer not found: {}", key),
            _ => eprintln!("Sync error: {}", e),
        }
    },
    Err(e) => eprintln!("Other error: {}", e),
}
```

## Best Practices

### 1. **Use Iroh Transport for Production**

Iroh provides better NAT traversal and P2P capabilities than HTTP.

### 2. **Understand Automatic Sync Behavior**

The BackgroundSync engine handles operations automatically:

- Entries sync immediately when committed
- Failed sends retry with exponential backoff (2^attempts seconds)
- Periodic sync runs every 5 minutes for all peers

### 3. **Monitor Sync Health**

Regularly check sync statistics and peer status to ensure healthy operation.

### 4. **Handle Network Failures Gracefully**

The sync system automatically retries failed operations, but your application should handle temporary disconnections.

### 5. **Secure Your Private Keys**

Store device keys securely and use different keys for different purposes when appropriate.

## Advanced Topics

### Custom Sync Hooks

You can implement custom sync hooks to extend the sync system:

```rust,ignore
use eidetica::sync::hooks::{SyncHook, SyncHookContext};

struct CustomSyncHook;

impl SyncHook for CustomSyncHook {
    fn on_entry_committed(&self, context: &SyncHookContext) -> Result<()> {
        println!("Entry {} committed to database {}", context.entry.id(), context.tree_id);
        // Custom logic here
        Ok(())
    }
}
```

### Multiple Database Instances

You can run multiple sync-enabled databases in the same process:

```rust,ignore
// Database 1
let db1 = Instance::new(Box::new(InMemory::new())).with_sync()?;
db1.sync_mut()?.enable_http_transport()?;
db1.sync_mut()?.start_server("127.0.0.1:8080")?;

// Database 2
let db2 = Instance::new(Box::new(InMemory::new())).with_sync()?;
db2.sync_mut()?.enable_http_transport()?;
db2.sync_mut()?.start_server("127.0.0.1:8081")?;

// Connect them
let addr = Address::http("127.0.0.1:8080")?;
let peer_key = db2.sync_mut()?.connect_to_peer(&addr).await?;
```

## Troubleshooting

### Common Issues

**"No transport enabled" error:**

- Ensure you've called `enable_http_transport()` or `enable_iroh_transport()`

**Sync not happening:**

- Check peer status is `Active`
- Verify database sync relationships are configured
- Check network connectivity between peers

**Performance issues:**

- Consider using Iroh transport for better performance
- Check retry queue for persistent failures
- Verify network connectivity is stable

**Authentication failures:**

- Ensure private keys are properly configured
- Verify peer public keys are correct
- Check that peers are using compatible protocol versions
