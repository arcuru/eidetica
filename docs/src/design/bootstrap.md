> ✅ **Status: Implemented**
>
> This design is fully implemented and functional.

# Bootstrap and Access Control

This design document describes the bootstrap mechanism for requesting access to databases and the global permission system for open access.

## Overview

Bootstrap provides a "knocking" mechanism for clients to request access to databases they don't have permissions for. Global permissions provide an alternative for databases that want to allow open access without requiring bootstrap requests.

## Problem Statement

When a client wants to sync a database they don't have access to:

1. **No Direct Access**: Client's key is not in the database's auth settings
2. **Need Permission Grant**: Requires an admin to add the client's key
3. **Coordination Challenge**: Client and admin need a way to coordinate the access grant
4. **Public Databases**: Some databases should be openly accessible without coordination

## Proposed Solution

Two complementary mechanisms:

1. **Global Permissions**: For databases that want open access
2. **Bootstrap Protocol**: For databases that want controlled access grants

## Global Permissions

### Global Permission

A database can grant universal permissions by setting a global permission in its auth settings. The global permission is stored in the `"global"` sub-object of `_settings.auth`, separate from per-key entries in `"keys"` and delegations in `"delegations"`:

<!-- Code block ignored: Simplified view of AuthSettings structure -->

```text
// AuthSettings stores data in a Doc with three sub-objects:
//   "keys"        - per-key auth entries (SigKey → AuthKey)
//   "delegations" - delegated tree references
//   "global"      - global permission (applies to all clients)
```

### How It Works

When a client attempts to sync a database:

1. **Check for global permission**: If a global permission exists in `_settings.auth`, grant the specified permission to any client
2. **No key required**: Client doesn't need their key in the database's auth settings
3. **Immediate access**: No bootstrap request or approval needed

### Use Cases

**Public Read Access**: Set global permission to Read to allow anyone to read the database. Clients can sync immediately without bootstrap.

**Open Collaboration**: Set global permission to Write to allow anyone to write (use carefully).

**Hybrid Model**: Combine global Read permission with specific Write/Admin permissions for named keys. This allows public read access while restricting modifications to specific users.

### Security Considerations

- **Read-only common**: Most appropriate for public data
- **Write carefully**: Global write allows any client to modify the database
- **Per-database**: Each database controls its own global permission settings

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
2. If auth is required, client calls `user.request_database_access()`
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

### Approval by User with Admin Permission

Any logged-in user who has a key with Admin permission for the database can approve the request:

1. User logs in with `instance.login_user()`
2. Lists pending requests with `user.pending_bootstrap_requests(&sync)`
3. User selects a key they own that has Admin permission on the target database
4. Calls `user.approve_bootstrap_request(&mut sync, request_id, approving_key_id)`
5. System validates the user owns the specified key
6. System retrieves the signing key from the user's key manager
7. System **explicitly validates** the key has Admin permission on the target database
8. Creates transaction using the user's signing key
9. Adds requesting key to database's auth settings
10. Updates request status to Approved in the sync database
11. Broadcasts the newly committed auth entry to all of the database's peers (see [Broadcast on Approval](#broadcast-on-approval))

### Permission Validation Strategy

Bootstrap approval and rejection use **explicit permission validation**:

- **Approval**: The system explicitly checks that the approving user has Admin permission on the target database before adding the requesting key. This provides clear error messages (`InsufficientPermission`) and fails fast if the user lacks the required permission.

- **Rejection**: The system explicitly checks that the rejecting user has Admin permission on the target database before allowing rejection. Since rejection only modifies the sync database (not the target database), explicit validation is necessary to enforce the Admin permission requirement.

**Rationale**: Explicit validation provides:

- Clear, informative error messages for users
- Fast failure before attempting database modifications
- Consistent permission checking across both operations
- Better debugging experience when permission issues occur

### Broadcast on Approval

Approval commits the new auth key as an entry in the target database, then
**broadcasts that entry to all of the database's peers** through the normal
outbound send queue. This happens regardless of the database's `sync_on_commit`
setting — approval always broadcasts.

The requesting peer is registered as a peer of the database during its sync
request, so it is included in the broadcast. Receiving the approval entry is how
it learns access was granted, without waiting on a fixed poll interval. In
fluctuating-network scenarios this meaningfully improves the time-to-visibility
for the client: as soon as any path to the peer succeeds, the entry arrives.

Broadcasting to _all_ peers (not only the requester) is intentional — the auth
change is relevant to every replica of the database, and reusing the general
send path keeps the mechanism uniform. Delivery reuses the standard send queue,
so an unreachable peer falls through to the existing retry/backoff path. The
broadcast is best-effort: a failure to enqueue is logged and does not undo the
committed approval.

### Client-Side Completion

When a request against a manual-approval peer comes back pending, the client
records an **outgoing bootstrap request** in its own `_sync` tree (a direct
mirror of the incoming request store used by approvers). The record captures
everything needed to finish the join once access is granted: the target tree,
the addresses to pull from, the requesting key and permission, and the caller's
desired sync settings. The provisional User-layer key mapping is front-loaded on
the same pending path, so once the tree syncs the database is immediately
openable.

Completion is owned entirely by the sync layer — it never calls back into the
User layer — and is driven by two triggers that share one completion path:

- **Broadcast-woken (low latency):** the approval entry arrives via
  `put_remote_entries`, which recognizes that the written tree matches a pending
  outgoing request and drives completion. This fires even before the client
  holds the tree's root (the approval entry lands as an orphan), so completion
  requests a full bootstrap rather than a tip diff in that case.
- **Periodic sweep (correctness / restart-safety):** the background engine
  periodically re-checks every pending outgoing request, so a client that was
  offline when the approval was broadcast, or that restarted, still converges.

On completion the client pulls the now-authorized tree (reusing the existing
`SyncTree` bootstrap path — no new protocol variant), applies the recorded sync
settings, registers the tree/peer relationship, and marks the request hydrated.
The caller does not re-invoke `request_database_access`; the database becomes
openable on its own.

#### Who Is On The Push List

A database's tree-peer set is a **push list**: the `sync_on_commit` fan-out sends
every committed entry to it, and bootstrap approval broadcasts the new auth entry
to it. Membership therefore has to mean something.

The rule is **you are on a tree's push list only once we have served you that
tree**. A peer whose bootstrap came back `Pending`, or was refused outright, is
not registered — otherwise being told "wait" or "no" would still deliver database
contents and the auth key list.

That leaves approval needing a route back to the requester. The requester's
handshake device key is recorded on the stored `BootstrapRequest` instead, and
approval registers it at the moment access is granted, immediately before
broadcasting. A request with no recorded device key simply isn't broadcast to;
the requester's own completion sweep still converges.

Note this is registration-time hygiene, not an authorization filter on the send
path itself — `queue_entry_for_sync` still trusts its peer list. Closing that
requires a device-key-to-auth-key association the peer records do not currently
carry.

#### Retrying Without Amplifying

The sweep re-sends the bootstrap request on every tick until it is answered, so
the request path must be **idempotent per (tree, requesting key, permission)**.
An approver holds at most one record per distinct ask: a re-request reuses the
existing record and returns its id rather than appending a new one. Without this
an honest client appends a row to the approver's `_sync` tree every sweep
interval, growing without bound and burying the real request among duplicates.

The permission belongs in the key. A retry always re-sends the same one, so
amplification still collapses; but asking for `Admin` after a pending `Read` is a
materially different request, and collapsing those would answer the escalation
with the weaker record — approving it would silently grant less than was asked
for. An `Approved` record is likewise not reused: reaching the store path means
the auth check found no live grant, so the approval was revoked and a genuinely
new request is correct.

Rejection is **terminal**. A rejected requester receives `BootstrapRejected`
rather than another `BootstrapPending`, marks its outgoing record `Rejected`, and
drops it from the sweep set. This matters more once retries exist: an
indefinitely-retrying client would otherwise re-queue itself on the approver
forever, effectively undoing the rejection. Getting access after a rejection
requires the approver to act out of band, not the requester to keep asking.

### Key Requirements

**For Bootstrap Request:**

- Client must have generated a keypair
- Client specifies the permission level they're requesting

**For Approval:**

- User must be logged in
- User must have a key with Admin permission for the target database
- That key must be in the database's auth settings

**For Rejection:**

- User must be logged in
- User must have a key with Admin permission for the target database
- That key must be in the database's auth settings
- System explicitly validates Admin permission before allowing rejection

### Verification of Transferred Entries

A successful bootstrap transfers the full database state to the client. Those
entries are **not** trusted because the server sent them: like any synced
data they are stored `Unverified` (the wire protocol carries no way for a
peer to assert a verification status — see the
[service architecture](../internal/service.md)).

Promotion is a local decision on the client:

- `Database::verify()` validates each received entry against the `_settings`
  it pins and promotes it, **prefix-closed** (an entry becomes `Verified`
  only once its whole ancestor history is);
- a normal read also triggers an opportunistic verification pass when a tip
  is still `Unverified` (the access-time hook).

Until then, default reads expose only the **Verified frontier**, so a
freshly bootstrapped database may read as empty for the instant before
verification completes; `.allow_unverified()` opts into the pre-verification
view. Because the bootstrap root is genesis/TOFU, verification bottoms out at
a self-authorising root and the rest cascades from there.

## Design Decisions

### Auto-Approval via Global Permissions

Bootstrap requests are auto-approved when the database has a global permission that covers the requested permission level:

1. **Global Permissions**: A database with global permission set to `Write(10)` auto-approves any request for `Write(10)` or lower (including `Read`)
2. **Manual Approval**: Requests exceeding global permissions require explicit approval by a user with Admin permission

**Rationale:**

- Simple model: global permissions define open access boundaries
- Clear security: requests beyond global permissions need explicit approval
- No per-request policy evaluation needed
- Bootstrap combines both open and controlled access patterns

## API Design

### Global Permissions API

Global permissions are managed through the `AuthSettings` API via the `set_global_permission` method:

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
// Set global permission
let mut auth_settings = AuthSettings::new();
auth_settings.set_global_permission(AuthKey::active(None, Permission::Write(10)));
```

### Bootstrap API

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
impl Sync {
    /// List pending bootstrap requests
    pub fn pending_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>>;

    /// Get specific bootstrap request
    pub fn get_bootstrap_request(&self, request_id: &str) -> Result<Option<(String, BootstrapRequest)>>;

    /// Approve a bootstrap request (low-level, requires signing key)
    pub fn approve_bootstrap_request_with_key(
        &self,
        request_id: &str,
        signing_key: SigningKey,
        approving_key_id: &str,
    ) -> Result<()>;

    /// Reject a bootstrap request (low-level, requires signing key)
    pub fn reject_bootstrap_request_with_key(
        &self,
        request_id: &str,
        signing_key: SigningKey,
        rejecting_key_id: &str,
    ) -> Result<()>;

    /// Request bootstrap access (low-level, requires key details)
    pub async fn sync_with_peer_for_bootstrap_with_key(
        &self,
        address: &Address,
        tree_id: &ID,
        public_key: &str,
        key_id: &str,
        requested_permission: Permission,
    ) -> Result<()>;
}

impl User {
    /// Get all pending bootstrap requests from the sync system
    pub fn pending_bootstrap_requests(
        &self,
        sync: &Sync,
    ) -> Result<Vec<(String, BootstrapRequest)>>;

    /// Approve a bootstrap request (requires Admin permission)
    /// The approving_key_id must be owned by this user and have Admin permission on the target database
    pub fn approve_bootstrap_request(
        &self,
        sync: &Sync,
        request_id: &str,
        approving_key_id: &PublicKey,
    ) -> Result<()>;

    /// Reject a bootstrap request (requires Admin permission)
    /// The rejecting_key_id must be owned by this user and have Admin permission on the target database
    pub fn reject_bootstrap_request(
        &self,
        sync: &Sync,
        request_id: &str,
        rejecting_key_id: &PublicKey,
    ) -> Result<()>;

    /// Request database access via bootstrap (client-side with user-managed keys)
    pub async fn request_database_access(
        &self,
        sync: &Sync,
        address: &Address,
        database_id: &ID,
        key_id: &PublicKey,
        requested_permission: Permission,
    ) -> Result<()>;
}
```

## Security Considerations

### Global Permissions

1. **Public Exposure**: Global permissions make databases publicly accessible
2. **Write Risk**: Global write allows anyone to modify data
3. **Audit Trail**: All modifications still signed by individual keys
4. **Revocation**: Admins can remove global permission at any time

### Bootstrap Protocol

1. **Request Validation**: Verify requesting public key matches signature
2. **Permission Limits**: Clients request permission, approving user decides what to grant
3. **Admin Permission Required**: Only users with Admin permission on the database can approve
4. **Request Expiry**: Consider implementing request expiration
5. **Rate Limiting**: Prevent spam bootstrap requests

## Future Enhancements

1. **Request Expiration**: Automatically expire old pending requests
2. **Notification System**: Notify users with Admin permission of new bootstrap requests
3. **Permission Negotiation**: Allow approving user to grant different permission than requested
4. **Batch Approval**: Approve multiple requests at once
5. **Bootstrap Policies**: Configurable rules for auto-rejection (e.g., block certain addresses)
6. **Audit Log**: Track all bootstrap requests and decisions

## Conclusion

The bootstrap and access control system provides:

**Global Permissions:**

- Simple open access for public databases
- Flexible permission levels (Read, Write, Admin)
- Per-database control

**Bootstrap Protocol:**

- Secure request/approval workflow
- User-controlled access grants
- Integration with Users system for authentication

Together, these mechanisms support both open and controlled access patterns for Eidetica databases.
