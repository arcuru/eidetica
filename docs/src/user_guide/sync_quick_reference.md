# Sync Quick Reference

A concise reference for Eidetica's synchronization API with common usage patterns and code snippets.

## Setup and Initialization

### Basic Sync Setup

```rust,ignore
use eidetica::{BaseDB, backend::InMemory};

// Create database with sync enabled
let backend = Box::new(InMemory::new());
let db = BaseDB::new(backend).with_sync()?;

// Add authentication key
db.add_private_key("device_key")?;

// Enable transport
db.sync_mut()?.enable_http_transport()?;
db.sync_mut()?.start_server("127.0.0.1:8080")?;
```

### Understanding BackgroundSync

```rust,ignore
// The BackgroundSync engine starts automatically with transport
db.sync_mut()?.enable_http_transport()?; // Starts background thread

// Background thread handles:
// - Command processing (immediate)
// - Periodic sync (5 min intervals)
// - Retry queue (30 sec intervals)
// - Connection checks (60 sec intervals)

// All sync operations are automatic - no manual queue management needed
```

## Peer Management

### Connect to Remote Peer

```rust,ignore
use eidetica::sync::{Address, peer_types::PeerStatus};

// HTTP connection
let addr = Address::http("192.168.1.100:8080")?;
let peer_key = db.sync_mut()?.connect_to_peer(&addr).await?;

// Iroh P2P connection
let addr = Address::iroh("iroh://peer_id@relay.example.com")?;
let peer_key = db.sync_mut()?.connect_to_peer(&addr).await?;

// Activate peer
db.sync_mut()?.update_peer_status(&peer_key, PeerStatus::Active)?;
```

### Manual Peer Registration

```rust,ignore
// Register peer manually
let peer_key = "ed25519:abc123...";
db.sync_mut()?.register_peer(peer_key, Some("Alice's Device"))?;

// Add addresses
db.sync_mut()?.add_peer_address(peer_key, Address::http("192.168.1.100:8080")?)?;
db.sync_mut()?.add_peer_address(peer_key, Address::iroh("iroh://peer_id")?)?;

// Note: Registration does NOT immediately connect to the peer
// Connection happens lazily during next sync operation or periodic sync (5 min)
// Use connect_to_peer() for immediate connection if needed
```

### Peer Status Management

```rust,ignore
// List all peers
let peers = db.sync()?.list_peers()?;
for peer in peers {
    println!("{}: {} ({})",
        peer.pubkey,
        peer.display_name.unwrap_or("Unknown".to_string()),
        peer.status
    );
}

// Get specific peer info
if let Some(peer) = db.sync()?.get_peer_info(&peer_key)? {
    println!("Status: {:?}", peer.status);
    println!("Addresses: {:?}", peer.addresses);
}

// Update peer status
db.sync_mut()?.update_peer_status(&peer_key, PeerStatus::Inactive)?;
```

## Tree Synchronization

### Setup Tree Sync Relationships

```rust,ignore
// Create tree
let tree = db.new_tree(Doc::new(), "device_key")?;
let tree_id = tree.root_id().to_string();

// Enable sync with peer
db.sync_mut()?.add_tree_sync(&peer_key, &tree_id)?;

// List synced trees for peer
let trees = db.sync()?.get_peer_trees(&peer_key)?;

// List peers syncing a tree
let peers = db.sync()?.get_tree_peers(&tree_id)?;
```

### Remove Sync Relationships

```rust,ignore
// Remove tree from sync with peer
db.sync_mut()?.remove_tree_sync(&peer_key, &tree_id)?;

// Remove peer completely
db.sync_mut()?.remove_peer(&peer_key)?;
```

## Data Operations (Auto-Sync)

### Basic Data Changes

```rust,ignore
use eidetica::subtree::DocStore;

// Any tree operation automatically triggers sync
let op = tree.new_operation()?;
let store = op.get_subtree::<DocStore>("data")?;

store.set_string("message", "Hello World")?;
store.set_path("user.name", "Alice")?;
store.set_path("user.age", 30)?;

// Commit triggers sync hooks automatically
op.commit()?; // Entries queued for sync to all configured peers
```

### Bulk Operations

```rust,ignore
// Multiple operations in single commit
let op = tree.new_operation()?;
let store = op.get_subtree::<DocStore>("data")?;

for i in 0..100 {
    store.set_string(&format!("item_{}", i), &format!("value_{}", i))?;
}

// Single commit, single sync entry
op.commit()?;
```

## Monitoring and Diagnostics

### Server Control

```rust,ignore
// Start/stop sync server
let sync = db.sync_mut()?;
sync.start_server("127.0.0.1:8080")?;

// Check server status
if sync.is_server_running() {
    let addr = sync.get_server_address()?;
    println!("Server running at: {}", addr);
}

// Stop server
sync.stop_server()?;
```

### Sync State Tracking

```rust,ignore
// Get sync state manager
let op = db.sync()?.sync_tree().new_operation()?;
let state_manager = SyncStateManager::new(&op);

// Get sync cursor for peer-tree relationship
let cursor = state_manager.get_sync_cursor(&peer_key, &tree_id)?;
if let Some(cursor) = cursor {
    println!("Last synced: {:?}", cursor.last_synced_entry);
    println!("Total synced: {}", cursor.total_synced_count);
}

// Get peer metadata
let metadata = state_manager.get_sync_metadata(&peer_key)?;
if let Some(meta) = metadata {
    println!("Successful syncs: {}", meta.successful_sync_count);
    println!("Failed syncs: {}", meta.failed_sync_count);
}
```

### Sync State Tracking

```rust,ignore
use eidetica::sync::state::SyncStateManager;

// Get sync tree operation
let op = sync.sync_tree().new_operation()?;
let state_manager = SyncStateManager::new(&op);

// Check sync cursor
let cursor = state_manager.get_sync_cursor(&peer_key, &tree_id)?;
println!("Last synced: {:?}", cursor.last_synced_entry);
println!("Total synced: {}", cursor.total_synced_count);

// Check sync metadata
let metadata = state_manager.get_sync_metadata(&peer_key)?;
println!("Success rate: {:.2}%", metadata.sync_success_rate() * 100.0);
println!("Avg duration: {:.1}ms", metadata.average_sync_duration_ms);

// Get recent sync history
let history = state_manager.get_sync_history(&peer_key, Some(10))?;
for entry in history {
    println!("Sync {}: {} entries in {:.1}ms",
        entry.sync_id, entry.entries_count, entry.duration_ms);
}
```

## Error Handling

### Common Error Patterns

```rust,ignore
use eidetica::sync::SyncError;

// Connection errors
match sync.connect_to_peer(&addr).await {
    Ok(peer_key) => println!("Connected: {}", peer_key),
    Err(e) if e.is_sync_error() => {
        match e.sync_error().unwrap() {
            SyncError::HandshakeFailed(msg) => {
                eprintln!("Handshake failed: {}", msg);
                // Retry with different address or check credentials
            },
            SyncError::NoTransportEnabled => {
                eprintln!("Enable transport first");
                sync.enable_http_transport()?;
            },
            SyncError::PeerNotFound(key) => {
                eprintln!("Peer {} not registered", key);
                // Register peer first
            },
            _ => eprintln!("Other sync error: {}", e),
        }
    },
    Err(e) => eprintln!("Non-sync error: {}", e),
}
```

### Monitoring Sync Health

```rust,ignore
// Check server status
if !sync.is_server_running() {
    eprintln!("Warning: Sync server not running");
}

// Monitor peer connectivity
let peers = sync.list_peers()?;
for peer in peers {
    if peer.status != PeerStatus::Active {
        eprintln!("Warning: Peer {} is {}", peer.pubkey, peer.status);
    }
}

// Sync happens automatically, but you can monitor state
// via the SyncStateManager for diagnostics
```

## Configuration Examples

### Development Setup

```rust,ignore
// Fast, responsive sync for development
// Enable HTTP transport for easy debugging
db.sync_mut()?.enable_http_transport()?;
db.sync_mut()?.start_server("127.0.0.1:8080")?;

// Connect to local test peer
let addr = Address::http("127.0.0.1:8081")?;
let peer = db.sync_mut()?.connect_to_peer(&addr).await?;
```

### Production Setup

```rust,ignore
// Use Iroh for production deployments (defaults to n0's relay servers)
db.sync_mut()?.enable_iroh_transport()?;

// Or configure for specific environments:
use iroh::RelayMode;
use eidetica::sync::transports::iroh::IrohTransport;

// Custom relay server (e.g., enterprise deployment)
let relay_url: iroh::RelayUrl = "https://relay.example.com".parse()?;
let relay_node = iroh::RelayNode {
    url: relay_url,
    quic: Some(Default::default()),
};
let transport = IrohTransport::builder()
    .relay_mode(RelayMode::Custom(iroh::RelayMap::from_iter([relay_node])))
    .build()?;
db.sync_mut()?.enable_iroh_transport_with_config(transport)?;

// Connect to peers
let addr = Address::iroh(peer_node_id)?;
let peer = db.sync_mut()?.connect_to_peer(&addr).await?;

// Sync happens automatically:
// - Immediate on commit
// - Retry with exponential backoff
// - Periodic sync every 5 minutes
```

### Multi-Database Setup

```rust,ignore
// Run multiple sync-enabled databases
let db1 = BaseDB::new(Box::new(InMemory::new())).with_sync()?;
db1.sync_mut()?.enable_http_transport()?;
db1.sync_mut()?.start_server("127.0.0.1:8080")?;

let db2 = BaseDB::new(Box::new(InMemory::new())).with_sync()?;
db2.sync_mut()?.enable_http_transport()?;
db2.sync_mut()?.start_server("127.0.0.1:8081")?;

// Connect them together
let addr = Address::http("127.0.0.1:8080")?;
let peer = db2.sync_mut()?.connect_to_peer(&addr).await?;
```

## Testing Patterns

### Testing with Iroh (No Relays)

```rust,ignore
#[tokio::test]
async fn test_iroh_sync_local() -> Result<()> {
    use iroh::RelayMode;
    use eidetica::sync::transports::iroh::IrohTransport;

    // Configure Iroh for local testing (no relay servers)
    let transport1 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()?;
    let transport2 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()?;

    // Setup databases with local Iroh transport
    let db1 = BaseDB::new(Box::new(InMemory::new())).with_sync()?;
    db1.sync_mut()?.enable_iroh_transport_with_config(transport1)?;
    db1.sync_mut()?.start_server("ignored")?; // Iroh manages its own addresses

    let db2 = BaseDB::new(Box::new(InMemory::new())).with_sync()?;
    db2.sync_mut()?.enable_iroh_transport_with_config(transport2)?;
    db2.sync_mut()?.start_server("ignored")?;

    // Get the serialized NodeAddr (includes direct addresses)
    let addr1 = db1.sync()?.get_server_address()?;
    let addr2 = db2.sync()?.get_server_address()?;

    // Connect peers using full NodeAddr info
    let peer1 = db2.sync_mut()?.connect_to_peer(&Address::iroh(&addr1)).await?;
    let peer2 = db1.sync_mut()?.connect_to_peer(&Address::iroh(&addr2)).await?;

    // Now they can sync directly via P2P
    Ok(())
}
```

### Mock Peer Setup (HTTP)

```rust,ignore
#[tokio::test]
async fn test_sync_between_peers() -> Result<()> {
    // Setup first peer
    let db1 = BaseDB::new(Box::new(InMemory::new())).with_sync()?;
    db1.add_private_key("peer1")?;
    db1.sync_mut()?.enable_http_transport()?;
    db1.sync_mut()?.start_server("127.0.0.1:0")?; // Random port

    let addr1 = db1.sync()?.get_server_address()?;

    // Setup second peer
    let db2 = BaseDB::new(Box::new(InMemory::new())).with_sync()?;
    db2.add_private_key("peer2")?;
    db2.sync_mut()?.enable_http_transport()?;

    // Connect peers
    let addr = Address::http(&addr1)?;
    let peer1_key = db2.sync_mut()?.connect_to_peer(&addr).await?;
    db2.sync_mut()?.update_peer_status(&peer1_key, PeerStatus::Active)?;

    // Setup sync relationship
    let tree1 = db1.new_tree(Doc::new(), "peer1")?;
    let tree2 = db2.new_tree(Doc::new(), "peer2")?;

    db2.sync_mut()?.add_tree_sync(&peer1_key, &tree1.root_id().to_string())?;

    // Test sync
    let op1 = tree1.new_operation()?;
    let store1 = op1.get_subtree::<DocStore>("data")?;
    store1.set_string("test", "value")?;
    op1.commit()?;

    // Wait for sync
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify sync occurred
    // ... verification logic

    Ok(())
}
```

## Best Practices Summary

### ✅ Do

- Enable sync before creating trees you want to synchronize
- Use `PeerStatus::Active` only for peers you want to sync with
- Use Iroh transport for production deployments
- Monitor sync state and peer connectivity
- Handle network failures gracefully
- Let BackgroundSync handle retry logic automatically

### ❌ Don't

- Disable sync hooks on trees you want to synchronize
- Manually manage sync queues (BackgroundSync handles this)
- Ignore sync errors in production code
- Use HTTP transport for high-volume production (prefer Iroh)
- Assume sync is instantaneous (it's eventually consistent)

### 🔧 Troubleshooting Checklist

1. **Sync not working?**

   - Check transport is enabled and server started
   - Verify peer status is `Active`
   - Confirm tree sync relationships configured
   - Check network connectivity

2. **Performance issues?**

   - Consider using Iroh transport
   - Check for network bottlenecks
   - Verify retry queue isn't growing unbounded
   - Monitor peer connectivity status

3. **Memory usage high?**

   - Check for dead/unresponsive peers
   - Verify retry queue is processing correctly
   - Consider restarting sync to clear state

4. **Sync delays?**
   - Remember sync is immediate on commit
   - Check if entries are in retry queue
   - Verify network is stable
   - Check peer responsiveness
