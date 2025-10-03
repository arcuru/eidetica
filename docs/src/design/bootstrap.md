**Implementation Status**: 🔵 Proposed

# Bootstrap and Access Control

This design document describes the bootstrap mechanism for requesting access to databases and the wildcard permission system for open access.

## Overview

Bootstrap provides a "knocking" mechanism for clients to request access to databases they don't have permissions for. Wildcard permissions provide an alternative for databases that want to allow open access without requiring bootstrap requests.

## Problem Statement

When a client wants to sync a database they don't have access to:

1. **No Direct Access**: Client's key is not in the database's auth settings
2. **Need Permission Grant**: Requires an admin to add the client's key
3. **Coordination Challenge**: Client and admin need a way to coordinate the access grant
4. **Public Databases**: Some databases should be openly accessible without coordination

## Proposed Solution

Two complementary mechanisms:

1. **Wildcard Permissions**: For databases that want open access
2. **Bootstrap Protocol**: For databases that want controlled access grants

## Wildcard Permissions

### Wildcard Key

A database can grant universal permissions by setting the special `"*"` key in its auth settings:

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthSettings {
    /// Maps SigKey → AuthKey
    /// Special key "*" grants permissions to all clients
    keys: HashMap<String, AuthKey>,
}
```

### How It Works

When a client attempts to sync a database:

1. **Check for wildcard key**: If `"*"` exists in `_settings.auth`, grant the specified permission to any client
2. **No key required**: Client doesn't need their key in the database's auth settings
3. **Immediate access**: No bootstrap request or approval needed

### Use Cases

**Public Read Access**: Set wildcard key with Read permission to allow anyone to read the database. Clients can sync immediately without bootstrap.

**Open Collaboration**: Set wildcard key with Write permission to allow anyone to write (use carefully).

**Hybrid Model**: Combine wildcard Read permission with specific Write/Admin permissions for named keys. This allows public read access while restricting modifications to specific users.

### Security Considerations

- **Use sparingly**: Wildcard permissions bypass authentication
- **Read-only common**: Most appropriate for public data
- **Write carefully**: Wildcard write allows any client to modify the database
- **Per-database**: Each database controls its own wildcard settings

## Bootstrap Protocol

### Overview

Bootstrap provides a request/approval workflow for controlled access grants:

```text
Client                    Server                     User (with Admin key)
  |                         |                             |
  |-- Sync Request -------→ |                             |
  |                         |-- Check Auth Settings       |
  |                         |   (no matching key)         |
  |                         |                             |
  |←- Auth Required --------| (if no global permissions)  |
  |                         |                             |
  |-- Bootstrap Request --→ |                             |
  |   (with key & perms)    |                             |
  |                         |-- Store in _sync DB -------→|
  |                         |                             |
  |←- Request Pending ------| (Bootstrap ID returned)     |
  |                         |                             |
  |   [Wait for approval]   |                             |
  |                         |                             |
  |                         |           ←-- List Pending -|
  |                         |           --- Pending [] -->|
  |                         |                             |
  |                         |           ←-- Approve ------|
  |                         |←- Add Key to DB Auth -------|
  |                         |   (using user's Admin key)  |
  |                         |                             |
  |-- Retry Normal Sync --→ |                             |
  |                         |-- Check Auth (now has key)  |
  |←- Sync Success ---------| (access granted)            |
```

### Client Bootstrap Request

When a client needs access to a database:

1. Client attempts normal sync
2. If auth is required, client calls `sync_with_peer_for_bootstrap()` with key name and requested permission
3. Server stores bootstrap request in `_sync` database
4. Client receives pending status and waits for approval

### Bootstrap Request Storage

Bootstrap requests are stored in the `_sync` database:

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BootstrapRequest {
    /// Database being requested
    pub tree_id: ID,

    /// Client's public key (for verification)
    pub requesting_pubkey: String,

    /// Client's key name (to add to auth settings)
    pub requesting_key_name: String,

    /// Permission level requested
    pub requested_permission: Permission,

    /// When request was made
    pub timestamp: String,

    /// Current status
    pub status: RequestStatus,

    /// Client's network address
    pub peer_address: Address,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RequestStatus {
    Pending,
    Approved,
    Rejected,
}
```

### Approval by User with Admin Permission

Any logged-in user who has a key with Admin permission for the database can approve the request:

1. User logs in with `instance.login_user()`
2. Lists pending requests with `sync.pending_bootstrap_requests()`
3. Calls `user.approve_bootstrap_request()` with request ID and tree ID
4. System finds user's key with Admin permission for the database
5. Creates transaction using that key's SigKey
6. Adds requesting key to database's auth settings
7. Updates request status to Approved

### Client Retry After Approval

Once approved, the client retries with normal sync after waiting or polling periodically. If access was granted, the sync succeeds and the client can use the database.

### Key Requirements

**For Bootstrap Request:**

- Client must have generated a keypair
- Client specifies the permission level they're requesting

**For Approval:**

- User must be logged in
- User must have a key with Admin permission for the target database
- That key must be in the database's auth settings

**For Rejection:**

- User must be logged in (no specific permissions required for rejection)

## Design Decisions

### No Auto-Approval

Previous designs included auto-approval based on database policy. This has been removed in favor of:

1. **Global Permissions**: Use wildcard `"*"` key for open access
2. **Manual Approval**: All bootstrap requests require explicit approval by a user with Admin permission

**Rationale:**

- Simpler architecture (no policy evaluation)
- Clearer security model (explicit user actions)
- Global permissions handle "open access" use case
- Bootstrap is for controlled access grants by authorized users

Auto-approval will be removed once the new system is completed.

## API Design

### Wildcard Permissions API

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
impl SettingsStore {
    /// Set wildcard permissions for database
    pub fn set_wildcard_permission(&self, permission: Permission) -> Result<()>;

    /// Remove wildcard permissions
    pub fn remove_wildcard_permission(&self) -> Result<()>;

    /// Check if database has wildcard permissions
    pub fn get_wildcard_permission(&self) -> Result<Option<Permission>>;
}
```

### Bootstrap API

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
impl Sync {
    /// List pending bootstrap requests
    pub fn pending_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>>;

    /// Get specific bootstrap request
    pub fn get_bootstrap_request(&self, request_id: &str) -> Result<Option<BootstrapRequest>>;
}

impl User {
    /// Approve a bootstrap request (requires Admin permission)
    pub fn approve_bootstrap_request(
        &self,
        request_id: &str,
        tree_id: &ID,
    ) -> Result<()>;

    /// Reject a bootstrap request
    pub fn reject_bootstrap_request(&self, request_id: &str) -> Result<()>;
}

// Client-side bootstrap request
impl Sync {
    /// Request bootstrap access to a database
    pub async fn sync_with_peer_for_bootstrap(
        &self,
        peer_addr: &Address,
        tree_id: &ID,
        key_name: &str,
        requested_permission: Permission,
    ) -> Result<()>;
}
```

## Security Considerations

### Wildcard Permissions

1. **Public Exposure**: Wildcard permissions make databases publicly accessible
2. **Write Risk**: Wildcard write allows anyone to modify data
3. **Audit Trail**: All modifications still signed by individual keys
4. **Revocation**: Can remove wildcard permission at any time

### Bootstrap Protocol

1. **Request Validation**: Verify requesting public key matches signature
2. **Permission Limits**: Clients request permission, approving user decides what to grant
3. **Admin Permission Required**: Only users with Admin permission on the database can approve
4. **Request Expiry**: Consider implementing request expiration
5. **Rate Limiting**: Prevent spam bootstrap requests

## Implementation Strategy

### Phase 1: Wildcard Permissions

1. Update AuthSettings to support `"*"` key
2. Modify sync protocol to check for wildcard permissions
3. Add SettingsStore API for wildcard management
4. Tests for wildcard permission scenarios

### Phase 2: Bootstrap Request Storage

1. Define BootstrapRequest structure
2. Implement storage in `_sync` database
3. Add request listing and retrieval APIs
4. Tests for request storage and retrieval

### Phase 3: Client Bootstrap Protocol

1. Implement `sync_with_peer_for_bootstrap()` client method
2. Add bootstrap request submission to sync protocol
3. Implement pending status handling
4. Tests for client bootstrap flow

### Phase 4: User Approval

1. Implement `User::approve_bootstrap_request()`
2. Implement `User::reject_bootstrap_request()`
3. Add Admin permission checking and key addition logic
4. Tests for approval workflow

### Phase 5: Integration

1. Update sync protocol to handle bootstrap responses
2. Implement client retry logic
3. End-to-end integration tests
4. Documentation and examples

## Future Enhancements

1. **Request Expiration**: Automatically expire old pending requests
2. **Notification System**: Notify users with Admin permission of new bootstrap requests
3. **Permission Negotiation**: Allow approving user to grant different permission than requested
4. **Batch Approval**: Approve multiple requests at once
5. **Bootstrap Policies**: Configurable rules for auto-rejection (e.g., block certain addresses)
6. **Audit Log**: Track all bootstrap requests and decisions

## Conclusion

The bootstrap and access control system provides:

**Wildcard Permissions:**

- Simple open access for public databases
- Flexible permission levels (Read, Write, Admin)
- Per-database control

**Bootstrap Protocol:**

- Secure request/approval workflow
- User-controlled access grants
- Integration with Users system for authentication

Together, these mechanisms support both open and controlled access patterns for Eidetica databases.
