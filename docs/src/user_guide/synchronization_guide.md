# Synchronization Guide

Eidetica's sync system enables real-time data synchronization between distributed peers.

## Quick Start

### 1. Enable Sync

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
let instance = Instance::open(backend).await?;
instance.enable_sync().await?;

// Create and login a user (generates authentication key automatically)
instance.create_user("alice", None).await?;
let mut user = instance.login_user("alice", None).await?;
# Ok(())
# }
```

### 2. Start a Server

```rust,ignore
use eidetica::sync::transports::http::HttpTransport;

let sync = instance.sync().unwrap();

// Register transport with bind address and start accepting connections
sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:8080")).await?;
sync.accept_connections().await?;
```

### 3. Connect and Sync

```rust,ignore
use eidetica::sync::Address;

// Single API handles both bootstrap (new) and incremental (existing) sync
sync.sync_with_peer(&Address::http("127.0.0.1:8080"), Some(&tree_id)).await?;
```

The system automatically detects whether you need full bootstrap or incremental sync.

## Connection Architecture

The sync system separates **outbound** and **inbound** connection handling:

- **Outbound** (`sync_with_peer()`): Works immediately after `register_transport()`. No server needed.
- **Inbound** (`accept_connections()`): Must be called to accept incoming connections.

```text
register_transport()
    │ - Transport ready for outbound requests
    ▼
[OUTBOUND READY]
    │ - sync_with_peer() works
    │ - Push hooks work
    │ - NO incoming connections
    ▼
accept_connections()
    │ - Starts server on registered transports
    ▼
[FULL P2P]
    │ - Outbound works
    │ - Inbound works
```

This separation provides:

- **Security by default**: Nodes don't accept incoming connections unless explicitly enabled
- **Zero-config outbound**: Applications can sync data immediately without server setup
- **Flexible deployment**: Support client-only, server-only, or full peer-to-peer modes

## Transport Options

Eidetica supports multiple transports simultaneously, allowing peers to be reachable via different networks.

### HTTP

Simple REST-based sync. Good for development and fixed-IP deployments.

```rust,ignore
use eidetica::sync::transports::http::HttpTransport;

sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:8080")).await?;
sync.accept_connections().await?;
```

### Iroh P2P (Recommended)

QUIC-based with NAT traversal. Works through firewalls.

```rust,ignore
use eidetica::sync::transports::iroh::IrohTransport;

sync.register_transport("iroh", IrohTransport::builder()).await?;
sync.accept_connections().await?;
let my_address = sync.get_server_address_async().await?;  // Share this with peers
```

### Multiple Transports

Enable multiple transports for maximum connectivity:

<!-- Code block ignored: Requires network connectivity for transport setup -->

```rust,ignore
use eidetica::sync::transports::http::HttpTransport;
use eidetica::sync::transports::iroh::IrohTransport;

// Register both HTTP (for local network) and Iroh (for P2P)
sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0")).await?;
sync.register_transport("iroh", IrohTransport::builder()).await?;

// Start servers on all transports
sync.accept_connections().await?;

// Get all server addresses
let addresses = sync.get_all_server_addresses_async().await?;
for (transport_name, addr) in addresses {
    println!("{}: {}", transport_name, addr);
}
```

Requests are automatically routed to the appropriate transport based on address type.

## Declarative Sync API

For persistent sync relationships:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::sync::{SyncPeerInfo, Address};
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.enable_sync().await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let default_key = user.get_default_key()?;
# let db = user.create_database(Doc::new(), &default_key).await?;
# let tree_id = db.root_id().clone();
# let sync = instance.sync().expect("Sync enabled");
# let peer_pubkey = "ed25519:abc123".to_string();
// Register a peer for automatic background sync
let handle = sync.register_sync_peer(SyncPeerInfo {
    peer_pubkey,
    tree_id,
    addresses: vec![Address {
        transport_type: "http".to_string(),
        address: "http://peer.example.com:8080".to_string(),
    }],
    auth: None,
    display_name: Some("Peer Device".to_string()),
}).await?;
# Ok(())
# }
```

Background sync happens automatically. Check status with `handle.status()?`.

## Sync Settings

Configure per-database sync behavior:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc};
# use eidetica::user::types::{SyncSettings, TrackedDatabase};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.enable_sync().await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let key = user.get_default_key()?;
# let db = user.create_database(Doc::new(), &key).await?;
# let db_id = db.root_id().clone();
let tracked = TrackedDatabase {
    database_id: db_id,
    key_id: user.get_default_key()?,
    sync_settings: SyncSettings {
        sync_enabled: true,
        sync_on_commit: true,        // Sync immediately on commit
        interval_seconds: Some(60),  // Also sync every 60 seconds
        properties: Default::default(),
    },
};

// Track this database with the User
user.track_database(tracked).await?;
# Ok(())
# }
```

## Authenticated Bootstrap

For joining databases that require authentication:

```rust,ignore
use eidetica::sync::Address;

// Request database access through User API
user.request_database_access(
    &sync,
    &Address::http("127.0.0.1:8080"),
    &database_id,
    &key_id,  // User's key ID from user.add_private_key()
    eidetica::auth::Permission::Write,
).await?;
```

See [Bootstrap Guide](bootstrap.md) for approval workflows.

## Automatic Behavior

Once configured, the sync system handles:

- **Immediate sync** on commit (if `sync_on_commit: true`)
- **Periodic sync** at configured intervals
- **Retry** with exponential backoff for failed sends
- **Bidirectional transfer** in each sync operation

## Troubleshooting

| Issue                     | Solution                                                        |
| ------------------------- | --------------------------------------------------------------- |
| "No transport enabled"    | Call `register_transport()` with HTTP or Iroh transport builder |
| Can't receive connections | Call `accept_connections()` to start servers                    |
| Sync not happening        | Check peer status, network connectivity                         |
| Auth failures             | Verify keys are configured, protocol versions match             |

## Example

See the [Chat Example](https://github.com/arcuru/eidetica/blob/main/examples/chat/README.md) for a complete working application demonstrating multi-transport sync, bootstrap, and real-time updates.
