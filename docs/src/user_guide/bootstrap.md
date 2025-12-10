# Bootstrapping

## Overview

The Bootstrap system provides secure key management for Eidetica databases by controlling how new devices gain access to synchronized databases. It supports two approval methods:

1. **Global Wildcard Permissions** - Databases with global '\*' permissions automatically approve bootstrap requests without adding new keys
2. **Manual Approval** - Bootstrap requests are queued for administrator review and explicit approval

## Global Permission Bootstrap

Global '\*' permissions provide the simplest and most flexible approach for collaborative or public databases:

### How It Works

When a database has global permissions configured (e.g., `{"*": {"pubkey": "*", "permissions": "Write: 10"}}`), bootstrap requests are automatically approved if the requested permission level is satisfied by the global permission. No new keys are added to the database.

Devices use the global permission for both bootstrap approval and subsequent operations (transactions, reads, writes). The system automatically resolves to the global "\*" key when a device's specific key is not present in the database's auth settings.

### Advantages

- **No key management**: Devices don't need individual keys added to database
- **Immediate access**: Bootstrap approval happens instantly
- **Simple configuration**: One permission setting controls all devices
- **Flexible permissions**: Set exactly the permission level you want to allow

### Configuration Example

Configure a database with global write permission:

<!-- Code block ignored: Example configuration code for global permissions -->

```rust,ignore
use eidetica::crdt::Doc;

// Create database with global permission
let mut settings = Doc::new();
let mut auth_doc = Doc::new();

// Add admin key for database management
auth_doc.set_json("admin_key", serde_json::json!({
    "pubkey": "ed25519:admin_public_key_here",
    "permissions": {"Admin": 1},
    "status": "Active"
}))?;

// Add global permission for automatic bootstrap approval
auth_doc.set_json("*", serde_json::json!({
    "pubkey": "*",
    "permissions": {"Write": 10},  // Allows Read and Write(11+) requests
    "status": "Active"
}))?;

settings.set("auth", auth_doc);
let database = instance.new_database(settings, "admin_key")?;
```

### Permission Levels

Eidetica uses **lower numbers = higher permissions**:

- Global `Write(10)` **allows**: `Read`, `Write(11)`, `Write(15)`, etc.
- Global `Write(10)` **denies**: `Write(5)`, `Admin(*)`

Choose your global permission level carefully based on your security requirements.

## Client Workflow

From the client's perspective, the bootstrap process follows these steps:

### 1. Initial Bootstrap Attempt

The client initiates a bootstrap request when it needs access to a synchronized database:

<!-- Code block ignored: Example client workflow code demonstrating bootstrap API usage -->

```rust,ignore
client_sync.sync_with_peer_for_bootstrap(
    &server_address,
    &tree_id,
    "client_device_key",     // Client's key name
    Permission::Write(5)     // Requested permission level
).await
```

### 2. Response Handling

The client must handle different response scenarios:

- **Global Wildcard Permission Approved**:
  - Request succeeds immediately
  - Client gains access via global permission
  - No individual key added to database
  - Can proceed with normal operations

- **Manual Approval Required** (default):
  - Request fails with an error
  - Error indicates request is "pending"
  - Bootstrap request is queued for admin review

### 3. Waiting for Approval

While the request is pending, the client has several options:

- **Polling Strategy**: Periodically retry sync operations
- **Event-Based**: Wait for notification from server (future enhancement)
- **User-Triggered**: Let user manually retry when they expect approval

### 4. After Admin Decision

**If Approved:**

- The initial `sync_with_peer_for_bootstrap()` will still return an error
- Client must use normal `sync_with_peer()` to access the database
- Once synced, client can load and use the database normally

**If Rejected:**

- All sync attempts continue to fail
- Client remains unable to access the database
- May submit a new request with different parameters if appropriate

### 5. Retry Logic Example

<!-- Code block ignored: Example retry logic implementation for bootstrap workflow -->

```rust,ignore
async fn bootstrap_with_retry(
    client_sync: &mut Sync,
    server_addr: &str,
    tree_id: &ID,
    key_name: &str,
) -> Result<()> {
    // Initial bootstrap request
    if let Err(_) = client_sync.sync_with_peer_for_bootstrap(
        server_addr, tree_id, key_name, Permission::Write(5)
    ).await {
        println!("Bootstrap request pending approval...");

        // Poll for approval (with backoff)
        for attempt in 0..10 {
            tokio::time::sleep(Duration::from_secs(30 * (attempt + 1))).await;

            // Try normal sync after potential approval
            if client_sync.sync_with_peer(server_addr, Some(tree_id)).await.is_ok() {
                println!("Access granted!");
                return Ok(());
            }
        }

        return Err("Bootstrap request timeout or rejected".into());
    }

    Ok(()) // Auto-approved
}
```

## Usage Examples

### Manual Approval Workflow

For administrators managing bootstrap requests:

<!-- Code block ignored: Example admin workflow for manual approval process -->

```rust,ignore
// 1. List pending requests
let pending = sync.pending_bootstrap_requests()?;
for (request_id, request) in pending {
    println!("Request {}: {} wants {} access to tree {}",
        request_id,
        request.requesting_key_name,
        request.requested_permission,
        request.tree_id
    );
}

// 2. Approve a request
sync.approve_bootstrap_request(
    "bootstrap_a1b2c3d4...",
    "admin_key"  // Your admin key name
)?;

// 3. Or reject a request
sync.reject_bootstrap_request(
    "bootstrap_e5f6g7h8...",
    "admin_key"
)?;
```

### Complete Client Bootstrap Example

<!-- Code block ignored: Complete client bootstrap workflow example -->

```rust,ignore
// Step 1: Initial bootstrap attempt with authentication
let bootstrap_result = client_sync.sync_with_peer_for_bootstrap(
    &server_address,
    &tree_id,
    "my_device_key",
    Permission::Write(5)
).await;

// Step 2: Handle the response based on approval method
match bootstrap_result {
    Ok(_) => {
        // Global wildcard permission granted immediate access
        println!("Bootstrap approved via global permission! Access granted immediately.");
    },
    Err(e) => {
        // Manual approval required
        // The error indicates the request is pending
        println!("Bootstrap request submitted, awaiting admin approval...");

        // Step 3: Wait for admin to review and approve
        // Options:
        // a) Poll periodically
        // b) Wait for out-of-band notification
        // c) User-triggered retry

        // Step 4: After admin approval, retry with normal sync
        // (bootstrap sync will still fail, use regular sync instead)
        tokio::time::sleep(Duration::from_secs(30)).await;

        // After approval, normal sync will succeed
        match client_sync.sync_with_peer(&server_address, Some(&tree_id)).await {
            Ok(_) => {
                println!("Access granted! Database synchronized.");
                // Client can now load and use the database
                let db = client_instance.load_database(&tree_id)?;
            },
            Err(_) => {
                println!("Still pending or rejected. Check with admin.");
            }
        }
    }
}

// Handling rejection scenario
// If the request was rejected, all sync attempts will continue to fail
// The client will need to submit a new bootstrap request if appropriate
```

## Security Considerations

### Trust Model

- **Global Wildcard Permissions**: Trusts any device that can reach the sync endpoint
  - Suitable for: Development, collaborative projects, public databases
  - Risk: Any device can gain the configured global permissions
  - Benefit: Simple, immediate access for authorized scenarios

- **Manual Approval**: Requires explicit admin action for each device
  - Suitable for: Production, sensitive data, controlled access scenarios
  - Benefit: Complete control over which devices gain access
  - Risk: Administrative overhead for each new device

## Troubleshooting

### Common Issues

1. **"Authentication required but not configured"**
   - Cause: Sync handler cannot authenticate with target database
   - Solution: Ensure proper key configuration for database operations

2. **"Invalid request state"**
   - Cause: Attempting to approve/reject non-pending request
   - Solution: Check request status before operation

### Performance Considerations

- Sync database grows linearly with request count
- Request queries are indexed by ID
