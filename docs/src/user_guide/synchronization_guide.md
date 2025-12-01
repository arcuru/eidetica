# Synchronization Guide

Eidetica's sync system enables real-time data synchronization between distributed peers.

## Quick Start

### 1. Enable Sync

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
let instance = Instance::open(backend)?;
instance.enable_sync()?;

// Create and login a user (generates authentication key automatically)
instance.create_user("alice", None)?;
let mut user = instance.login_user("alice", None)?;
# Ok(())
# }
```

### 2. Start a Server

```rust,ignore
let sync = instance.sync().unwrap();
sync.enable_http_transport()?;

// Start a server to accept connections
sync.start_server_async("127.0.0.1:8080").await?;
```

### 3. Connect and Sync

```rust,ignore
// Single API handles both bootstrap (new) and incremental (existing) sync
sync.sync_with_peer("127.0.0.1:8080", Some(&tree_id)).await?;
```

That's it. The system automatically detects whether you need full bootstrap or incremental sync.

## Transport Options

### HTTP

Simple REST-based sync. Good for development and fixed-IP deployments.

```rust,ignore
sync.enable_http_transport()?;
sync.start_server_async("127.0.0.1:8080").await?;
```

### Iroh P2P (Recommended for Production)

QUIC-based with NAT traversal. Works through firewalls.

```rust,ignore
sync.enable_iroh_transport()?;
sync.start_server_async("ignored").await?;  // Iroh manages addressing
let my_address = sync.get_server_address_async().await?;  // Share this with peers
```

## Declarative Sync API

For persistent sync relationships:

```rust
# extern crate eidetica;
# use eidetica::sync::{SyncPeerInfo, Address};
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let instance = Instance::open(backend)?;
# instance.enable_sync()?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let default_key = user.get_default_key()?;
# let db = user.create_database(Doc::new(), &default_key)?;
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
})?;
# Ok(())
# }
```

Background sync happens automatically. Check status with `handle.status()?`.

## Sync Settings

Configure per-database sync behavior:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
# use eidetica::user::types::{DatabasePreferences, SyncSettings};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let instance = Instance::open(backend)?;
# instance.enable_sync()?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let key = user.get_default_key()?;
# let db = user.create_database(Doc::new(), &key)?;
# let db_id = db.root_id().clone();
let prefs = DatabasePreferences {
    database_id: db_id,
    key_id: user.get_default_key()?,
    sync_settings: SyncSettings {
        sync_enabled: true,
        sync_on_commit: true,        // Sync immediately on commit
        interval_seconds: Some(60),  // Also sync every 60 seconds
        properties: Default::default(),
    },
};

// Register this database with the User
user.add_database(prefs)?;
# Ok(())
# }
```

## Authenticated Bootstrap

For joining databases that require authentication:

```rust,ignore
sync.sync_with_peer_for_bootstrap(
    "127.0.0.1:8080",
    &tree_id,
    "device_key",
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

| Issue                  | Solution                                                    |
| ---------------------- | ----------------------------------------------------------- |
| "No transport enabled" | Call `enable_http_transport()` or `enable_iroh_transport()` |
| Sync not happening     | Check peer status, network connectivity                     |
| Auth failures          | Verify keys are configured, protocol versions match         |

## Example

See the [Chat Example](../../examples/chat/README.md) for a complete working application demonstrating multi-transport sync, bootstrap, and real-time updates.
