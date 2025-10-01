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

```rust,ignore
// Get mutable access to sync module
let sync = db.sync_mut().unwrap();

// Enable HTTP transport
sync.enable_http_transport()?;

// Start a server to accept connections
sync.start_server_async("127.0.0.1:8080").await?;
```

### 3. Authenticated Bootstrap (Recommended)

For new devices joining existing databases, use authenticated bootstrap to request access:

```rust,ignore
// On another device - connect and bootstrap with authentication
let client_sync = client_db.sync_mut().unwrap();
client_sync.enable_http_transport()?;

// Bootstrap with authentication - automatically requests write permission
client_sync.sync_with_peer_for_bootstrap(
    "127.0.0.1:8080",
    &tree_id,
    "device_key",                          // Your local key name
    eidetica::auth::Permission::Write      // Requested permission level
).await?;
```

### 4. Simplified Sync API (Legacy/Existing Databases)

For existing databases or when authentication isn't needed:

```rust,ignore
// This call automatically detects bootstrap vs incremental sync
client_sync.sync_with_peer("127.0.0.1:8080", Some(&tree_id)).await?;
```

### 5. Database Creation and Sharing

```rust,ignore
// Create a database to share
use eidetica::crdt::Doc;
let mut settings = Doc::new();
settings.set_string("name", "My Shared Room");
let database = db.new_database(settings, "device_key")?;
let tree_id = database.root_id();

// Add some data
let op = database.new_transaction()?;
let store = op.get_subtree::<DocStore>("messages")?;
store.set_string("welcome", "Hello, distributed world!")?;
op.commit()?;

// Share the tree_id with other devices
println!("Share this room ID: {}", tree_id);
```

### 6. Discovering Available Rooms

You can discover what databases are available on a peer:

```rust,ignore
// Discover available trees on a peer
let available_trees = sync.discover_peer_trees("127.0.0.1:8080").await?;
for tree in available_trees {
    println!("Available room: {} ({} entries)",
             tree.tree_id, tree.entry_count);
}

// Bootstrap from a specific discovered tree
if let Some(tree) = available_trees.first() {
    sync.sync_with_peer("127.0.0.1:8080", Some(&tree.tree_id)).await?;
}
```

## Transport Protocols

### HTTP Transport

The HTTP transport uses REST APIs for synchronization:

```rust,ignore
// Enable HTTP transport
sync.enable_http_transport()?;

// Start server (async)
sync.start_server_async("127.0.0.1:8080").await?;

// Get server address for sharing
let server_addr = sync.get_server_address_async().await?;
println!("Server running at: {}", server_addr);

// Connect and sync with remote peer (handles handshake automatically)
sync.sync_with_peer("peer.example.com:8080", Some(&tree_id)).await?;
```

### Iroh P2P Transport (Recommended)

Iroh provides direct peer-to-peer connectivity with NAT traversal:

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
sync.start_server_async("ignored").await?; // Iroh manages its own addressing

// Get the server address for sharing with peers
let my_address = sync.get_server_address_async().await?;
// This returns a JSON string containing:
// - node_id: Your cryptographic node identity
// - direct_addresses: Socket addresses where you can be reached

// Connect and sync with peer using bootstrap-first protocol
sync.sync_with_peer(&peer_address_json, Some(&tree_id)).await?;

// Or discover available trees first
let available_trees = sync.discover_peer_trees(&peer_address_json).await?;
if let Some(tree) = available_trees.first() {
    sync.sync_with_peer(&peer_address_json, Some(&tree.tree_id)).await?;
}
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

### Simplified Sync API (Recommended)

The new `sync_with_peer()` method handles peer management automatically:

```rust,ignore
// Automatic peer connection, handshake, and sync in one call
sync.sync_with_peer("peer.example.com:8080", Some(&tree_id)).await?;

// This automatically:
// 1. Performs handshake with the peer
// 2. Registers the peer if not already known
// 3. Bootstraps the database if it doesn't exist locally
// 4. Performs incremental sync if database exists locally
// 5. Stores peer information for future sync operations

// For subsequent sync operations with the same peer
sync.sync_with_peer("peer.example.com:8080", Some(&tree_id)).await?;
// Reuses existing peer registration and performs incremental sync
```

### Manual Peer Management (Advanced)

For advanced use cases, you can manage peers manually:

```rust,ignore
// Register a peer manually
sync.register_peer("ed25519:abc123...", Some("Alice's Device"))?;

// Add multiple addresses for the same peer
sync.add_peer_address(&peer_key, Address::http("192.168.1.100:8080")?)?;
sync.add_peer_address(&peer_key, Address::iroh("iroh://peer_id@relay")?)?;

// Use low-level sync method with registered peers
sync.sync_tree_with_peer(&peer_key, &tree_id).await?;
```

### Peer Information and Status

```rust,ignore
// Get peer information (after connecting)
if let Some(peer_info) = sync.get_peer_info(&peer_key)? {
    println!("Peer: {} ({})",
             peer_info.display_name.unwrap_or("Unknown".to_string()),
             peer_info.status);
}

// List all registered peers
let peers = sync.list_peers()?;
for peer in peers {
    println!("Peer: {} - {}", peer.pubkey, peer.display_name.unwrap_or("Unknown".to_string()));
}
```

### Database Discovery

```rust,ignore
// Discover what databases are available on a peer
let available_trees = sync.discover_peer_trees("peer.example.com:8080").await?;
for tree in available_trees {
    println!("Available database: {} ({} entries)", tree.tree_id, tree.entry_count);

    // Bootstrap any interesting databases
    sync.sync_with_peer("peer.example.com:8080", Some(&tree.tree_id)).await?;
}
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

#### Bootstrap Authentication Flow

When joining a new database, the authenticated bootstrap protocol handles permission requests:

1. **Client Request**: Device requests access with its public key and desired permission level
2. **Policy Check**: Server evaluates bootstrap auto-approval policy (secure by default)
3. **Conditional Approval**: Key approved only if policy explicitly allows
4. **Key Addition**: Server adds the requesting key to the database's authentication settings
5. **Database Transfer**: Complete database state transferred to the client
6. **Access Granted**: Client can immediately make authenticated operations

```rust,ignore
// Configure database with bootstrap policy (server side)
let mut settings = Doc::new();
settings.set_string("name", "Team Database");

let mut auth_doc = Doc::new();
let mut policy_doc = Doc::new();
// Enable auto-approval for team collaboration
policy_doc.set_json("bootstrap_auto_approve", true)?;
auth_doc.set_doc("policy", policy_doc);
settings.set_doc("auth", auth_doc);

let database = instance.new_database(settings, "admin_key")?;

// Bootstrap authentication example (client side)
sync.sync_with_peer_for_bootstrap(
    "peer_address",
    &tree_id,
    "my_device_key",
    Permission::Write  // Request write access
).await?;  // Will succeed if policy allows

// After successful bootstrap, the device can write to the database
let op = database.new_authenticated_operation("my_device_key")?;
// ... make changes ...
op.commit()?;
```

**Security Considerations**:

- Bootstrap requests are **rejected by default** for security
- Auto-approval must be explicitly enabled via policy configuration
- Policy setting: `_settings.auth.policy.bootstrap_auto_approve: bool`
- All key additions are recorded in the immutable database history
- Permission levels are enforced (Read/Write/Admin)

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

## Bootstrap-First Sync Protocol

Eidetica now supports a **bootstrap-first sync protocol** that enables devices to join existing databases (rooms/channels) without requiring pre-existing local state.

### Key Features

**Unified API:** Single `sync_with_peer()` method handles both bootstrap and incremental sync

```rust,ignore
// Works whether you have the database locally or not
sync.sync_with_peer("peer.example.com:8080", Some(&tree_id)).await?;
```

**Automatic Detection:** The protocol automatically detects whether bootstrap or incremental sync is needed:

- **Bootstrap sync:** If you don't have the database locally, the peer sends the complete database
- **Incremental sync:** If you already have the database, only new changes are transferred

**True Bidirectional Sync:** Both peers exchange data in a single sync operation - no separate client/server roles needed

### Protocol Flow

1. **Handshake:** Peers exchange device identities and establish trust
2. **Tree Discovery:** Client requests information about available trees
3. **Tip Comparison:** Compare local vs remote database tips to detect missing data
4. **Bidirectional Transfer:** Both peers send missing entries to each other in a single sync operation
   - Client receives missing entries from server
   - Client automatically detects what server is missing and sends those entries back
   - Uses existing `IncrementalResponse.their_tips` field to enable true bidirectional sync
5. **Verification:** Validate received entries and update local database

### Use Cases

**Joining Chat Rooms:**

```rust,ignore
// Join a chat room by ID
let room_id = "abc123...";
sync.sync_with_peer("chat.example.com", Some(&room_id)).await?;
// Now you have the full chat history and can participate
```

**Document Collaboration:**

```rust,ignore
// Join a collaborative document
let doc_id = "def456...";
sync.sync_with_peer("docs.example.com", Some(&doc_id)).await?;
// You now have the full document and can make edits
```

**Data Synchronization:**

```rust,ignore
// Sync application data to a new device
sync.sync_with_peer("my-server.com", Some(&app_data_id)).await?;
// All your application data is now available locally
```

## Best Practices

### 1. **Use the New Simplified API**

Prefer `sync_with_peer()` over manual peer management:

```rust,ignore
// ✅ Recommended: Automatic connection and sync
sync.sync_with_peer("peer.example.com", Some(&tree_id)).await?;

// ❌ Avoid: Manual peer setup (unless you need fine control)
sync.register_peer(&pubkey, Some("Alice"))?;
sync.add_peer_address(&pubkey, addr)?;
sync.sync_tree_with_peer(&pubkey, &tree_id).await?;
```

### 2. **Use Iroh Transport for Production**

Iroh provides better NAT traversal and P2P capabilities than HTTP.

### 3. **Leverage Database Discovery**

Use `discover_peer_trees()` to find available databases before syncing:

```rust,ignore
let available = sync.discover_peer_trees("peer.example.com").await?;
for tree in available {
    if tree.name == "My Project" {
        sync.sync_with_peer("peer.example.com", Some(&tree.tree_id)).await?;
        break;
    }
}
```

### 4. **Handle Network Failures Gracefully**

The sync system automatically retries failed operations, but your application should handle temporary disconnections.

### 5. **Understand Bootstrap vs Incremental Behavior**

- **First sync** with a database = Bootstrap (full data transfer)
- **Subsequent syncs** = Incremental (only changes)
- **No manual state management** needed

### 6. **Secure Your Private Keys**

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
