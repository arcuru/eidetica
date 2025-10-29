# Bootstrap System

Secure key management and access control for distributed Eidetica databases through a request-approval workflow integrated with the sync module.

## Architecture

### Storage Location

**Bootstrap Request Storage**: Requests are stored in the sync database (`_sync`), not target databases:

- Subtree: `bootstrap_requests`
- Structure: `Table<BootstrapRequest>` with UUID keys
- Persistence: Indefinite for audit trail purposes

**Global Wildcard Permissions**: Databases can enable automatic approval via global `*` permissions in `_settings.auth.*`

### Core Components

#### 1. Bootstrap Request Manager (`bootstrap_request_manager.rs`)

The `BootstrapRequestManager` handles storage and lifecycle of bootstrap requests within the sync database. Key responsibilities:

- **Request Storage**: Persists bootstrap requests as structured documents in the `bootstrap_requests` subtree
- **Status Tracking**: Manages request states (Pending, Approved, Rejected)
- **Request Retrieval**: Provides query APIs to list and filter requests

#### 2. Sync Handler Extensions

The `SyncHandlerImpl` processes bootstrap requests during sync operations:

- **Global Permission Check**: Checks if global `*` wildcard permission satisfies the request
- **Automatic Approval**: Grants access immediately via global permission (no key addition)
- **Manual Queue**: Stores requests for manual review when no global permission exists
- **Response Generation**: Returns appropriate sync responses (BootstrapPending, BootstrapResponse)

#### 3. Sync Module Public API (`sync/mod.rs`)

Request management methods on the `Sync` struct:

| Method                               | Description                     | Returns                              |
| ------------------------------------ | ------------------------------- | ------------------------------------ |
| `pending_bootstrap_requests()`       | Query pending requests          | `Vec<(String, BootstrapRequest)>`    |
| `approved_bootstrap_requests()`      | Query approved requests         | `Vec<(String, BootstrapRequest)>`    |
| `rejected_bootstrap_requests()`      | Query rejected requests         | `Vec<(String, BootstrapRequest)>`    |
| `get_bootstrap_request(id)`          | Retrieve specific request       | `Option<(String, BootstrapRequest)>` |
| `approve_bootstrap_request(id, key)` | Approve and add key to database | `Result<()>`                         |
| `reject_bootstrap_request(id, key)`  | Reject without adding key       | `Result<()>`                         |

### Data Flow

```mermaid
sequenceDiagram
    participant Client
    participant SyncHandler
    participant GlobalPermCheck
    participant PolicyCheck
    participant BootstrapManager
    participant Database
    participant Admin

    Client->>SyncHandler: Bootstrap Request<br/>(key, permission)
    SyncHandler->>GlobalPermCheck: Check global '*' permission

    alt Global Permission Grants Access
        GlobalPermCheck-->>SyncHandler: sufficient
        SyncHandler-->>Client: BootstrapResponse<br/>(approved=true, no key added)
    else Global Permission Insufficient
        GlobalPermCheck-->>SyncHandler: insufficient/missing
        SyncHandler->>BootstrapManager: Store request
        BootstrapManager-->>SyncHandler: Request ID
        SyncHandler-->>Client: BootstrapPending<br/>(request_id)

            Note over Client: Waits for approval

            Admin->>BootstrapManager: approve_request(id)
            BootstrapManager->>Database: Add key
            Database-->>BootstrapManager: Success
            BootstrapManager-->>Admin: Approved

            Note over Client: Next sync gets access
        end
    end
```

## Global Permission Auto-Approval

The bootstrap system supports automatic approval through global '\*' permissions, which provides immediate access without adding new keys to the database.

### How It Works

When a bootstrap request is received, the sync handler first checks if the requesting key already has sufficient permissions through existing auth settings:

1. **Permission Check**: `AuthSettings::can_access()` checks if the requesting public key has sufficient permissions
2. **Global Permission Check**: Includes checking for active global '\*' permission that satisfies the request
3. **Auto-Approval**: If sufficient permission exists (specific or global), approve without adding a new key
4. **Fallback**: If no existing permission, proceed to auto-approval policy or manual approval flow

### Implementation Details

**Key Components** (`handler.rs:check_existing_auth_permission`):

1. Create database instance for target tree
2. Get `AuthSettings` via `SettingsStore`
3. Call `AuthSettings::can_access(requesting_pubkey, requested_permission)`
4. Return approval decision without modifying database if permission exists

**Permission Hierarchy**: Eidetica uses an inverted priority system where **lower numbers = higher permissions**:

- `Write(5)` has **higher** permission than `Write(10)`
- Global `Write(10)` allows bootstrap requests for `Read`, `Write(11)`, `Write(15)`, etc.
- Global `Write(10)` **rejects** bootstrap requests for `Write(5)`, `Write(1)`, `Admin(*)`

### Precedence Rules

1. **Global permissions checked first** - Before manual approval queue
2. **Global permissions provide immediate access** - No admin approval required
3. **No key storage** - Global permission grants don't add keys to auth settings
4. **Insufficient global permission** - Falls back to manual approval queue

### Global Permissions for Ongoing Operations

Once bootstrapped with global permissions, devices use the global "\*" key for all subsequent operations:

- **Transaction commits**: `AuthSettings::resolve_sig_key_for_operation()` resolves to global "\*" when device's specific key is not in auth settings
- **Entry validation**: `KeyResolver::resolve_direct_key_with_pubkey()` falls back to global "\*" permission during signature verification
- **Permission checks**: All operations use the same permission hierarchy and validation rules

This unified approach ensures consistent behavior whether a device has a specific key or relies on global permissions.

### Use Cases

- **Public databases**: Set global `Read` permission for open access
- **Collaborative workspaces**: Set global `Write(*)` for team environments
- **Development environments**: Reduce friction while maintaining some permission control

## Data Structures

### BootstrapRequest

Stored in sync database's `bootstrap_requests` subtree using `Table<BootstrapRequest>`.

**Key Structure**: Request ID (UUID string) is the table key, not a struct field.

<!-- Code block ignored: Internal data structure definition not meant for compilation -->

```rust,ignore
pub struct BootstrapRequest {
    /// Target database/tree ID
    pub tree_id: ID,

    /// Public key of requesting device (ed25519:...)
    pub requesting_pubkey: String,

    /// Key name for the requesting device
    pub requesting_key_name: String,

    /// Permission level requested (Admin, Write, Read)
    pub requested_permission: Permission,

    /// ISO 8601 timestamp of request
    pub timestamp: String,

    /// Current processing status
    pub status: RequestStatus,

    /// Network address for future notifications
    pub peer_address: Address,
}
```

### RequestStatus Enum

```rust,ignore
pub enum RequestStatus {
    Pending,
    Approved {
        approved_by: String,
        approval_time: String,
    },
    Rejected {
        rejected_by: String,
        rejection_time: String,
    },
}
```

## Implementation Details

### Request Lifecycle

#### 1. Request Creation

When a client attempts bootstrap with authentication:

- Sync handler checks if tree exists
- Evaluates bootstrap policy in database settings
- If auto-approval disabled, creates bootstrap request
- Stores request in sync database's `bootstrap_requests` subtree

#### 2. Manual Review

Admin query operations:

- `pending_bootstrap_requests()` - Filter by status enum discriminant
- `get_bootstrap_request(id)` - Direct table lookup
- Decision criteria: pubkey, permission level, timestamp, out-of-band verification

#### 3. Approval Process

When approving a request:

1. Load request from sync database
2. Validate request is still pending
3. Create transaction on target database
4. Add requesting key with specified permissions
5. Update request status to "Approved"
6. Record approver and timestamp

#### 4. Rejection Process

When rejecting a request:

1. Load request from sync database
2. Validate request is still pending
3. Update status to "Rejected"
4. Record rejector and timestamp
5. No keys added to target database

### Authentication Integration

**Key Addition Flow** (`handler.rs:add_key_to_database`):

1. Load target database via `Database::open_readonly()`
2. Create transaction with device key auth
3. Get `SettingsStore` and `AuthSettings`
4. Create `AuthKey::active()` with requested permission
5. Call `settings_store.set_auth_key()`
6. Commit transaction

**Global Permission Check** (`handler.rs:check_existing_auth_permission`):

1. Load database settings via `SettingsStore`
2. Check if global `*` key exists with sufficient permissions
3. Approve immediately if global permission satisfies request

### Audit Trail

Request immutability provides forensic capability:

- Original request parameters preserved
- Approval/rejection metadata includes actor and timestamp
- Complete history of all bootstrap attempts maintained

### Concurrency and Persistence

**Persistence**: No automatic cleanup - requests remain indefinitely for audit trail

**Concurrency**:

- Multiple pending requests per database supported
- UUID keys prevent ID conflicts
- Status transitions use standard CRDT merge semantics

**Duplicate Detection**: Not currently implemented - identical requests from same client create separate entries. Future enhancement may consolidate by (tree_id, pubkey) tuple.

### Error Handling

Key error scenarios:

- `RequestNotFound`: Invalid request ID
- `RequestAlreadyExists`: Duplicate request ID
- `InvalidRequestState`: Request not in expected state
- `InsufficientPermissions`: Approver lacks required permissions
