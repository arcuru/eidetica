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

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
// Create a database with sync enabled
let db = Instance::open(backend)?;
db.enable_sync()?;

// Add a private key for authentication
db.add_private_key("device_key")?;
# Ok(())
# }
```

### 2. Enable a Transport Protocol

<!-- Code block ignored: Attempts to bind to network port during testing -->

```rust,ignore
// Get access to sync module
let sync = db.sync().unwrap();

// Enable HTTP transport
sync.enable_http_transport()?;

// Start a server to accept connections
sync.start_server_async("127.0.0.1:8080").await?;
```

### 3. Authenticated Bootstrap (Recommended)

For new devices joining existing databases, use authenticated bootstrap to request access:

<!-- Code block ignored: Requires network connectivity and authentication policy setup -->

```rust,ignore
// On another device - connect and bootstrap with authentication
let client_sync = client_db.sync().unwrap();
client_sync.enable_http_transport()?;

// Bootstrap with authentication - automatically requests write permission
client_sync.sync_with_peer_for_bootstrap(
    "127.0.0.1:8080",
    &tree_id,
    "device_key",                          // Your local key name
    eidetica::auth::Permission::Write      // Requested permission level
).await?;
```

### 4. Connecting to Peers

<!-- Code block ignored: Requires network connectivity to peer server -->

```rust,ignore
// Connect and sync with a peer - automatically detects bootstrap vs incremental sync
client_sync.sync_with_peer("127.0.0.1:8080", Some(&tree_id)).await?;
```

That's it! The sync system handles everything automatically:

- Handshake and peer registration
- Bootstrap (full sync) if you don't have the database
- Incremental sync if you already have it
- Bidirectional data transfer

## Transport Protocols

### HTTP Transport

The HTTP transport uses REST APIs for synchronization. Good for simple deployments with fixed IP addresses:

<!-- Code block ignored: Attempts to bind to network port during testing -->

```rust,ignore
// Enable HTTP transport
sync.enable_http_transport()?;

// Start server
sync.start_server_async("127.0.0.1:8080").await?;

// Connect to peer
sync.sync_with_peer("peer.example.com:8080", Some(&tree_id)).await?;
```

### Iroh P2P Transport (Recommended)

Iroh provides direct peer-to-peer connectivity with NAT traversal. Best for production deployments:

<!-- Code block ignored: Complex Iroh setup requiring external relay servers -->

```rust,ignore
// Enable Iroh transport (uses production relay servers by default)
sync.enable_iroh_transport()?;

// Start server
sync.start_server_async("ignored").await?; // Iroh manages its own addressing

// Get address to share with peers
let my_address = sync.get_server_address_async().await?;

// Connect to peer
sync.sync_with_peer(&peer_address_json, Some(&tree_id)).await?;
```

**How it works:**

- Attempts direct connection via NAT hole-punching (~90% success)
- Falls back to relay servers if needed
- Automatically upgrades to direct when possible

**Advanced configuration:** Iroh supports custom relay servers, staging mode, and relay-disabled mode for local testing. See the [Iroh documentation](https://docs.rs/iroh) for details.

## Sync Configuration

### BackgroundSync Architecture

The sync system automatically starts a background thread when transport is enabled. Once configured, all operations are handled automatically:

- **When you commit changes**, they're sent immediately via sync callbacks
- **Failed sends** are retried with exponential backoff (2^attempts seconds, max 64 seconds)
- **Periodic sync** runs every 5 minutes
- **Connection checks** every 60 seconds
- **No manual queue management** needed

## Peer Management

### Simplified Sync API (Recommended)

The new `sync_with_peer()` method handles peer management automatically:

<!-- Code block ignored: Requires network connectivity to peer server -->

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

<!-- Code block ignored: Uses low-level APIs requiring complex peer setup -->

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

<!-- Code block ignored: Requires established peer connections -->

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

## Database Tracking and Preferences

When using Eidetica with user accounts, you can track which databases you want to sync and configure individual sync preferences for each database.

### Adding a Database to Track

To add a database to your user's tracked databases:

<!-- Code block ignored: Requires complete database setup with permissions -->

```rust,ignore
// Configure preferences for a database
let prefs = DatabasePreferences {
    database_id: db_id.clone(),
    key_id: user.get_default_key()?,
    sync_settings: SyncSettings {
        sync_enabled: true,
        sync_on_commit: false,
        interval_seconds: Some(60),  // Sync every 60 seconds
        properties: Default::default(),
    },
};

// Add to user's tracked databases
user.add_database(prefs)?;
```

When you add a database, the system **automatically discovers** which signing key (SigKey) your user key can use to authenticate with that database. This uses the database's permission system to find the best available access level.

### Managing Tracked Databases

<!-- Code block ignored: Demonstrates full workflow requiring database setup -->

```rust,ignore
// List all tracked databases
let databases = user.list_database_prefs()?;
for db_prefs in databases {
    println!("Database: {}", db_prefs.database_id);
    println!("  Syncing: {}", db_prefs.sync_settings.sync_enabled);
}

// Get preferences for a specific database
let prefs = user.database_prefs(&db_id)?;

// Update sync preferences
let mut updated_prefs = prefs.clone();
updated_prefs.sync_settings.sync_enabled = false;
user.set_prefs(updated_prefs.database_prefs)?;

// Remove a database from tracking
user.remove_database(&db_id)?;
```

### Loading Tracked Databases

Once a database is tracked, you can easily load it:

<!-- Code block ignored: Requires complete user and database setup -->

```rust,ignore
// Load a tracked database
let database = user.open_database(&db_id)?;

// The user's configured key and SigKey are automatically used
// You can now work with the database normally
```

### Sync Preferences vs Sync Status

It's important to understand the distinction:

- **Preferences** (managed by User): What you _want_ to happen (sync enabled, interval, etc.)
- **Status** (managed by Sync module): What is _actually_ happening (last sync time, success/failure, etc.)

The user tracking system manages your preferences. The sync module reads these preferences to determine which databases to sync and when.

### Multi-User Support

Different users can track the same database with different preferences:

<!-- Code block ignored: Demonstrates multi-user scenario requiring complex setup -->

```rust,ignore
// Alice wants to sync this database every minute
alice_user.add_database(DatabasePreferences {
    database_id: shared_db_id.clone(),
    key_id: alice_key.clone(),
    sync_settings: SyncSettings {
        sync_enabled: true,
        interval_seconds: Some(60),
        ..Default::default()
    },
})?;

// Bob wants to sync the same database, but only on commit
bob_user.add_database(DatabasePreferences {
    database_id: shared_db_id.clone(),
    key_id: bob_key.clone(),
    sync_settings: SyncSettings {
        sync_enabled: true,
        sync_on_commit: true,
        interval_seconds: None,
        ..Default::default()
    },
})?;
```

Each user maintains their own tracking list and preferences independently.

## Security

### Authentication

All sync operations use Ed25519 digital signatures:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
#
# fn main() -> eidetica::Result<()> {
# // Setup database instance with sync capability
# let backend = Box::new(InMemory::new());
# let db = Instance::open(backend)?;
# db.enable_sync()?;
#
// The sync system automatically uses your device key for authentication
// First add the primary key
db.add_private_key("main_device_key")?;

// Add additional keys if needed for backup or multiple devices
db.add_private_key("backup_key")?;

// Create a database with default authentication
let mut settings = Doc::new();
settings.set_string("name", "my_sync_database");
let database = db.new_database(settings, "main_device_key")?;

// Set a specific key as default for a database (configuration pattern)
// In production: database.set_default_auth_key("backup_key");
println!("Authentication keys configured for sync operations");
# Ok(())
# }
```

#### Bootstrap Authentication Flow

When joining a new database, the authenticated bootstrap protocol handles permission requests:

1. **Client Request**: Device requests access with its public key and desired permission level
2. **Policy Check**: Server evaluates bootstrap auto-approval policy (secure by default)
3. **Conditional Approval**: Key approved only if policy explicitly allows
4. **Key Addition**: Server adds the requesting key to the database's authentication settings
5. **Database Transfer**: Complete database state transferred to the client
6. **Access Granted**: Client can immediately make authenticated operations

<!-- Code block ignored: Complex authentication flow requiring global permissions setup -->

```rust,ignore
// Configure database with global wildcard permission (server side)
let mut settings = Doc::new();
settings.set_string("name", "Team Database");

let mut auth_doc = Doc::new();

// Add admin key
auth_doc.set_json("admin_key", serde_json::json!({
    "pubkey": admin_pubkey,
    "permissions": {"Admin": 1},
    "status": "Active"
}))?;

// Add global wildcard permission for team collaboration
auth_doc.set_json("*", serde_json::json!({
    "pubkey": "*",
    "permissions": {"Write": 10},
    "status": "Active"
}))?;

settings.set_doc("auth", auth_doc);
let database = instance.new_database(settings, "admin_key")?;

// Bootstrap authentication example (client side)
sync.sync_with_peer_for_bootstrap(
    "peer_address",
    &tree_id,
    "my_device_key",
    Permission::Write(15)  // Request write access
).await?;  // Will succeed via global permission

// After successful bootstrap, the device can write to the database
let op = database.new_authenticated_operation("my_device_key")?;
// ... make changes ...
op.commit()?;
```

**Security Considerations**:

- Bootstrap requests are **rejected by default** for security
- Global wildcard permissions enable automatic approval without per-device key management
- Manual approval queues bootstrap requests for administrator review
- All key additions (in manual approval) are recorded in the immutable database history
- Permission levels are enforced (Read/Write/Admin)

### Peer Verification

During handshake, peers exchange and verify public keys:

<!-- Code block ignored: Requires network connectivity for handshake process -->

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

<!-- Code block ignored: Demonstrates monitoring concepts rather than compilable code -->

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

<!-- Code block ignored: Uses internal APIs requiring sync state setup -->

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

## Bootstrap-First Sync Protocol

Eidetica now supports a **bootstrap-first sync protocol** that enables devices to join existing databases (rooms/channels) without requiring pre-existing local state.

### Key Features

**Unified API:** Single `sync_with_peer()` method handles both bootstrap and incremental sync

<!-- Code block ignored: Requires network connectivity to peer server -->

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

<!-- Code block ignored: Requires network connectivity to chat server -->

```rust,ignore
// Join a chat room by ID
let room_id = "abc123...";
sync.sync_with_peer("chat.example.com", Some(&room_id)).await?;
// Now you have the full chat history and can participate
```

**Document Collaboration:**

<!-- Code block ignored: Requires network connectivity to document server -->

```rust,ignore
// Join a collaborative document
let doc_id = "def456...";
sync.sync_with_peer("docs.example.com", Some(&doc_id)).await?;
// You now have the full document and can make edits
```

**Data Synchronization:**

<!-- Code block ignored: Requires network connectivity to data server -->

```rust,ignore
// Sync application data to a new device
sync.sync_with_peer("my-server.com", Some(&app_data_id)).await?;
// All your application data is now available locally
```

## Best Practices

### 1. **Use the New Simplified API**

Prefer `sync_with_peer()` over manual peer management:

<!-- Code block ignored: Shows API comparison requiring network connections -->

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

<!-- Code block ignored: Makes actual HTTP requests during testing -->

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

### Custom Write Callbacks for Sync

You can use write callbacks to trigger sync operations when entries are committed:

```rust,ignore
use std::sync::Arc;

// Get the sync instance
let sync = instance.sync().expect("Sync not enabled");

// Set up a write callback to trigger sync
let sync_clone = sync.clone();
let peer_pubkey = "peer_public_key".to_string();
database.on_local_write(move |entry, db, _instance| {
    // Queue the entry for sync when it's committed
    sync_clone.queue_entry_for_sync(&peer_pubkey, entry.id(), db.root_id())
})?;
```

This approach allows you to automatically sync entries when they're created, enabling real-time synchronization between peers.

### Multiple Database Instances

You can run multiple sync-enabled databases in the same process:

<!-- Code block ignored: Complex multi-instance setup requiring multiple network ports -->

```rust,ignore
// Database 1
let db1 = Instance::open(Box::new(InMemory::new())?.enable_sync()?;
db1.sync()?.enable_http_transport()?;
db1.sync()?.start_server("127.0.0.1:8080")?;

// Database 2
let db2 = Instance::open(Box::new(InMemory::new())?.enable_sync()?;
db2.sync()?.enable_http_transport()?;
db2.sync()?.start_server("127.0.0.1:8081")?;

// Connect them
let addr = Address::http("127.0.0.1:8080")?;
let peer_key = db2.sync()?.connect_to_peer(&addr).await?;
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

## Complete Synchronization Example

For a full working example that demonstrates real-time synchronization between peers, see the **[Chat Example](../../examples/chat/README.md)** in the repository.

The chat application demonstrates:

- **Multi-Transport Sync**: Both HTTP (simple client-server) and Iroh (P2P with NAT traversal)
- **Bootstrap Protocol**: Automatic access requests when joining existing rooms
- **User API Integration**: User-based authentication with automatic key management
- **Sync Hooks**: Real-time message updates via periodic refresh
- **Peer Discovery**: Server address sharing for easy peer connection
- **Multiple Databases**: Each chat room is a separate synchronized database

### Quick Start with the Chat Example

```bash
# Terminal 1 - Create a room with HTTP transport
cd examples/chat
cargo run -- --username alice --transport http --create-only --room-name "Demo"

# Terminal 2 - Connect to the room
cargo run -- --username bob --transport http --connect "room_id@127.0.0.1:PORT"
```

See the [full chat documentation](../../examples/chat/README.md) for detailed usage, transport options, and troubleshooting.
