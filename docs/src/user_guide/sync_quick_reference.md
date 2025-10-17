# Sync Quick Reference

A concise reference for Eidetica's synchronization API with common usage patterns and code snippets.

## Setup and Initialization

### Basic Sync Setup

<!-- Code block ignored: Attempts to bind to network port during testing -->

```rust,ignore
use eidetica::{Instance, backend::InMemory};

// Create database with sync enabled
let backend = Box::new(InMemory::new());
let db = Instance::open(backend)?.with_sync()?;

// Add authentication key
db.add_private_key("device_key")?;

// Enable transport
let sync = db.sync_mut().unwrap();
sync.enable_http_transport()?;
sync.start_server_async("127.0.0.1:8080").await?;
```

### Understanding BackgroundSync

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory};
#
# fn main() -> eidetica::Result<()> {
# // Setup database instance with sync capability
# let backend = Box::new(InMemory::new());
# let mut db = Instance::open(backend)?.with_sync()?;
#
// The BackgroundSync engine starts automatically with transport
let sync = db.sync_mut().unwrap();
sync.enable_http_transport()?; // Starts background thread

// Background thread configuration and behavior:
// - Command processing (immediate response to commits)
// - Periodic sync operations (5 minute intervals)
// - Retry queue processing (30 second intervals)
// - Connection health checks (60 second intervals)

// All sync operations are automatic - no manual queue management needed
println!("BackgroundSync configured with automatic operation timers");
# Ok(())
# }
```

## Peer Management

### Authenticated Bootstrap (Recommended for New Databases)

<!-- Code block ignored: Requires network connectivity and authentication flow -->

```rust,ignore
// For new devices joining existing databases with authentication
sync.sync_with_peer_for_bootstrap(
    "peer.example.com:8080",
    &tree_id,
    "device_key",                    // Local authentication key
    eidetica::auth::Permission::Write // Requested permission level
).await?;

// This automatically:
// 1. Connects to peer and performs handshake
// 2. Requests database access with specified permission level
// 3. Receives auto-approved access (or manual approval in production)
// 4. Downloads complete database state
// 5. Grants authenticated write access
```

### Simplified Sync (Legacy/Existing Databases)

<!-- Code block ignored: Requires network connectivity to peer server -->

```rust,ignore
// Single call handles connection, handshake, and sync detection
sync.sync_with_peer("peer.example.com:8080", Some(&tree_id)).await?;

// This automatically:
// 1. Connects to peer and performs handshake
// 2. Bootstraps database if you don't have it locally
// 3. Syncs incrementally if you already have the database
// 4. Handles peer registration internally
```

### Database Discovery

<!-- Code block ignored: Makes actual HTTP requests during testing -->

```rust,ignore
// Discover available databases on a peer
let available_trees = sync.discover_peer_trees("peer.example.com:8080").await?;
for tree in available_trees {
    println!("Available: {} ({} entries)", tree.tree_id, tree.entry_count);
}

// Bootstrap from discovered database
if let Some(tree) = available_trees.first() {
    sync.sync_with_peer("peer.example.com:8080", Some(&tree.tree_id)).await?;
}
```

### Manual Peer Registration (Advanced)

<!-- Code block ignored: Uses low-level APIs requiring complex peer setup -->

```rust,ignore
// Register peer manually (for advanced use cases)
let peer_key = "ed25519:abc123...";
sync.register_peer(peer_key, Some("Alice's Device"))?;

// Add addresses
sync.add_peer_address(peer_key, Address::http("192.168.1.100:8080")?)?;
sync.add_peer_address(peer_key, Address::iroh("iroh://peer_id")?)?;

// Use low-level sync with registered peer
sync.sync_tree_with_peer(&peer_key, &tree_id).await?;

// Note: Manual registration is usually unnecessary
// The sync_with_peer() method handles registration automatically
```

### Peer Status Management

<!-- Code block ignored: Requires established peer connections -->

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

## Database Synchronization

### Create and Share Database

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::DocStore};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let db = Instance::open(backend)?.with_sync()?;
# db.add_private_key("device_key")?;
// Create a database to share
let mut settings = Doc::new();
settings.set_string("name", "My Chat Room");
settings.set_string("description", "A room for team discussions");

let database = db.new_database(settings, "device_key")?;
let tree_id = database.root_id();

// Add some initial data
let op = database.new_transaction()?;
let store = op.get_store::<DocStore>("messages")?;
store.set_string("welcome", "Welcome to the room!")?;
op.commit()?;

// Share the tree_id with others
println!("Room ID: {}", tree_id);
# Ok(())
# }
```

### Bootstrap from Shared Database

<!-- Code block ignored: Requires network connectivity to peer server -->

```rust,ignore
// Join someone else's database using the tree_id
let room_id = "abc123..."; // Received from another user
sync.sync_with_peer("peer.example.com:8080", Some(&room_id)).await?;

// You now have the full database locally
let database = db.load_database(&room_id)?;
let op = database.new_transaction()?;
let store = op.get_store::<DocStore>("messages")?;
println!("Welcome message: {}", store.get_string("welcome")?);
```

### Ongoing Synchronization

<!-- Code block ignored: Requires network connectivity to peer server -->

```rust,ignore
// All changes automatically sync after bootstrap
let op = database.new_transaction()?;
let store = op.get_store::<DocStore>("messages")?;
store.set_string("my_message", "Hello everyone!")?;
op.commit()?; // Automatically syncs to all connected peers

// Manually sync to get latest changes
sync.sync_with_peer("peer.example.com:8080", Some(&tree_id)).await?;
```

### Advanced: Manual Sync Relationships

<!-- Code block ignored: Uses low-level APIs requiring peer setup -->

```rust,ignore
// For fine-grained control (usually not needed)
sync.add_tree_sync(&peer_key, &tree_id)?;

// List synced databases for peer
let databases = sync.get_peer_trees(&peer_key)?;

// List peers syncing a database
let peers = sync.get_tree_peers(&tree_id)?;

// Remove sync relationship
sync.remove_tree_sync(&peer_key, &tree_id)?;
```

## Data Operations (Auto-Sync)

### Basic Data Changes

<!-- Code block ignored: Demonstrates auto-sync concepts rather than compilable code -->

```rust,ignore
use eidetica::store::DocStore;

// Any database operation automatically triggers sync
let op = database.new_transaction()?;
let store = op.get_store::<DocStore>("data")?;

store.set_string("message", "Hello World")?;
store.set_path("user.name", "Alice")?;
store.set_path("user.age", 30)?;

// Commit triggers sync hooks automatically
op.commit()?; // Entries queued for sync to all configured peers
```

### Bulk Operations

<!-- Code block ignored: Demonstrates auto-sync concepts rather than compilable code -->

```rust,ignore
// Multiple operations in single commit
let op = database.new_transaction()?;
let store = op.get_store::<DocStore>("data")?;

for i in 0..100 {
    store.set_string(&format!("item_{}", i), &format!("value_{}", i))?;
}

// Single commit, single sync entry
op.commit()?;
```

## Monitoring and Diagnostics

### Server Control

<!-- Code block ignored: Attempts to bind to network port during testing -->

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

<!-- Code block ignored: Uses internal APIs requiring sync state setup -->

```rust,ignore
// Get sync state manager
let op = db.sync()?.sync_tree().new_operation()?;
let state_manager = SyncStateManager::new(&op);

// Get sync cursor for peer-database relationship
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

<!-- Code block ignored: Uses internal APIs requiring sync state setup -->

```rust,ignore
use eidetica::sync::state::SyncStateManager;

// Get sync database operation
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

<!-- Code block ignored: Requires network connectivity for error examples -->

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

<!-- Code block ignored: Requires established peer connections -->

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

<!-- Code block ignored: Attempts to bind to network port during testing -->

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

<!-- Code block ignored: Complex Iroh setup requiring external relay servers -->

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

<!-- Code block ignored: Complex multi-instance setup requiring multiple network ports -->

```rust,ignore
// Run multiple sync-enabled databases
let db1 = Instance::open(Box::new(InMemory::new())?.with_sync()?;
db1.sync_mut()?.enable_http_transport()?;
db1.sync_mut()?.start_server("127.0.0.1:8080")?;

let db2 = Instance::open(Box::new(InMemory::new())?.with_sync()?;
db2.sync_mut()?.enable_http_transport()?;
db2.sync_mut()?.start_server("127.0.0.1:8081")?;

// Connect them together
let addr = Address::http("127.0.0.1:8080")?;
let peer = db2.sync_mut()?.connect_to_peer(&addr).await?;
```

## Testing Patterns

### Testing with Iroh (No Relays)

<!-- Code block ignored: Complex test setup requiring Iroh transport configuration -->

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
    let db1 = Instance::open(Box::new(InMemory::new())?.with_sync()?;
    db1.sync_mut()?.enable_iroh_transport_with_config(transport1)?;
    db1.sync_mut()?.start_server("ignored")?; // Iroh manages its own addresses

    let db2 = Instance::open(Box::new(InMemory::new())?.with_sync()?;
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

<!-- Code block ignored: Complex test setup requiring multiple instances and network ports -->

```rust,ignore
#[tokio::test]
async fn test_sync_between_peers() -> Result<()> {
    // Setup first peer
    let db1 = Instance::open(Box::new(InMemory::new())?.with_sync()?;
    db1.add_private_key("peer1")?;
    db1.sync_mut()?.enable_http_transport()?;
    db1.sync_mut()?.start_server("127.0.0.1:0")?; // Random port

    let addr1 = db1.sync()?.get_server_address()?;

    // Setup second peer
    let db2 = Instance::open(Box::new(InMemory::new())?.with_sync()?;
    db2.add_private_key("peer2")?;
    db2.sync_mut()?.enable_http_transport()?;

    // Connect peers
    let addr = Address::http(&addr1)?;
    let peer1_key = db2.sync_mut()?.connect_to_peer(&addr).await?;
    db2.sync_mut()?.update_peer_status(&peer1_key, PeerStatus::Active)?;

    // Setup sync relationship
    let tree1 = db1.new_database(Doc::new(), "peer1")?;
    let tree2 = db2.new_database(Doc::new(), "peer2")?;

    db2.sync_mut()?.add_tree_sync(&peer1_key, &tree1.root_id().to_string())?;

    // Test sync
    let op1 = tree1.new_transaction()?;
    let store1 = op1.get_store::<DocStore>("data")?;
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

- **Use `sync_with_peer()`** for most synchronization needs
- **Enable sync before creating** databases you want to synchronize
- **Use Iroh transport** for production deployments (better NAT traversal)
- **Use `discover_peer_trees()`** to find available databases before syncing
- **Share tree IDs** to allow others to bootstrap from your databases
- **Handle network failures** gracefully (sync system auto-retries)
- **Let BackgroundSync** handle retry logic automatically

### ❌ Don't

- **Manually manage peers** unless you need fine control (use `sync_with_peer()` instead)
- **Disable sync hooks** on databases you want to synchronize
- **Manually manage sync queues** (BackgroundSync handles this)
- **Ignore sync errors** in production code
- **Use HTTP transport** for high-volume production (prefer Iroh)
- **Assume sync is instantaneous** (it's eventually consistent)

### 🚀 New Bootstrap-First Features

- **Zero-state joining**: Join rooms/databases without any local setup
- **Automatic protocol detection**: Bootstrap vs incremental sync handled automatically
- **Simplified API**: Single `sync_with_peer()` call handles everything
- **Database discovery**: Find available databases on peers
- **Bidirectional sync**: Both devices can share and receive databases

### 🔧 Troubleshooting Checklist

1. **Sync not working?**
   - Check transport is enabled and server started
   - Verify peer status is `Active`
   - Confirm database sync relationships configured
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
