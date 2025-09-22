# Authentication

Comprehensive Ed25519-based cryptographic authentication system that ensures data integrity and access control across Eidetica's distributed architecture.

## Overview

Eidetica provides **flexible authentication** supporting both unsigned and signed modes, although signed databases are the default. Databases that lack authentication are used for specialized purposes, such as local-only databases or 'overlays'.

Once authentication is configured, all operations require valid Ed25519 signatures, providing strong guarantees about data authenticity and enabling access control in decentralized environments.

The authentication system is deeply integrated with the core database, not merely a consumer of the API. This tight integration enables efficient validation, deterministic conflict resolution during network partitions, and preservation of historical validity.

## Authentication States

Databases operate in one of four authentication configuration states:

| State             | `_settings.auth` Value      | Unsigned Ops | Authenticated Ops | Transition  | Error Type                   |
| ----------------- | --------------------------- | ------------ | ----------------- | ----------- | ---------------------------- |
| **Unsigned Mode** | Missing or `{}` (empty Doc) | ✓ Allowed    | ✓ Bootstrap       | → Signed    | N/A                          |
| **Signed Mode**   | Valid keys configured       | ✗ Rejected   | ✓ Validated       | Permanent   | `AuthenticationRequired`     |
| **Corrupted**     | Wrong type (String, etc.)   | ✗ Rejected   | ✗ Rejected        | → Fail-safe | `CorruptedAuthConfiguration` |
| **Deleted**       | Tombstone (was deleted)     | ✗ Rejected   | ✗ Rejected        | → Fail-safe | `CorruptedAuthConfiguration` |

**State Semantics**:

- **Unsigned Mode**: Database has no authentication configured (missing or empty `_settings.auth`). Both missing and empty `{}` are equivalent. Unsigned operations succeed, authenticated operations trigger automatic bootstrap.

- **Signed Mode**: Database has at least one key configured in `_settings.auth`. All operations require valid signatures. This is a permanent state - cannot return to unsigned mode.

- **Corrupted**: Authentication configuration exists but has wrong type (not a Doc). **Fail-safe behavior**: ALL operations rejected to prevent security bypass through corruption.

- **Deleted**: Authentication configuration was explicitly deleted (CRDT tombstone). **Fail-safe behavior**: ALL operations rejected since this indicates invalid security state.

**Fail-Safe Principle**: When auth configuration is corrupted or deleted, the system rejects ALL operations rather than guessing or bypassing security. This prevents exploits through auth configuration manipulation. When the state is detected, those Entries are invalid and will be rejected by Instances that try to validate them.

For complete behavioral details, see [Authentication Behavior Reference](../auth_behavior_reference.md).

## Architecture

**Storage Location**: Authentication configuration resides in the special `_settings.auth` store of each Database, using Doc CRDT for deterministic conflict resolution.

**Validation Component**: The AuthValidator provides centralized entry validation with performance-optimized caching.

**Signature Format**: All entries include authentication information in their structure:

```json
{
  "auth": {
    "sig": "ed25519_signature_base64_encoded",
    "key": "KEY_NAME_OR_DELEGATION_PATH"
  }
}
```

## Permission Hierarchy

Three-tier permission model with integrated priority system:

| Permission | Settings Access | Key Management | Data Write | Data Read | Priority |
| ---------- | --------------- | -------------- | ---------- | --------- | -------- |
| **Admin**  | ✓               | ✓              | ✓          | ✓         | 0-2^32   |
| **Write**  | ✗               | ✗              | ✓          | ✓         | 0-2^32   |
| **Read**   | ✗               | ✗              | ✗          | ✓         | None     |

**Priority Semantics**:

- Lower numbers = higher priority (0 is highest)
- Admin/Write permissions include u32 priority value
- Keys can only modify other keys with equal or lower priority
- Priority affects administrative operations, NOT CRDT merge resolution

## Key Management

### Key Management API

The authentication system provides three methods for managing keys with different safety guarantees:

**`add_key(key_name, auth_key)`**: Adds a new key, fails if key already exists

- Prevents accidental overwrites during operations like bootstrap sync
- Recommended for new key creation to avoid conflicts between devices
- Returns `KeyAlreadyExists` error if key name is already in use

**`overwrite_key(key_name, auth_key)`**: Explicitly replaces an existing key

- Use when intentionally updating or replacing a key
- Provides clear intent for key replacement operations
- Always succeeds regardless of whether key exists

**`can_access(pubkey, requested_permission)`**: Check if a public key has sufficient access

- Checks both specific key permissions and global '\*' permissions
- Returns true if the key has sufficient permission (either specific or global)
- Used internally by bootstrap approval system to avoid unnecessary key additions
- Supports the flexible access control patterns enabled by wildcard permissions

### Key Conflict Prevention

During multi-device synchronization, the system prevents key conflicts:

- If adding a key that exists with the **same public key**: Operation succeeds silently (idempotent)
- If adding a key that exists with a **different public key**: Operation fails with detailed error
- This prevents devices from accidentally overwriting each other's authentication keys

### Direct Keys

Ed25519 public keys stored directly in the database's `_settings.auth`:

```json
{
  "_settings": {
    "auth": {
      "KEY_LAPTOP": {
        "pubkey": "ed25519:BASE64_PUBLIC_KEY",
        "permissions": "write:10",
        "status": "active"
      }
    }
  }
}
```

### Key Lifecycle

Keys transition between two states:

- **Active**: Can create new entries, all operations permitted
- **Revoked**: Cannot create new entries, historical entries remain valid

This design preserves the integrity of historical data while preventing future use of compromised keys.

### Wildcard Keys

Special `*` key enables public access:

- Can grant any permission level (read, write, or admin)
- Commonly used for world-readable databases
- Subject to same revocation mechanisms as regular keys

## Delegation System

Databases can delegate authentication to other databases, enabling powerful authentication patterns without granting administrative privileges on the delegating database.

### Core Concepts

**Delegated Database References**: Any database can reference another database as an authentication source:

```json
{
  "_settings": {
    "auth": {
      "user@example.com": {
        "permission-bounds": {
          "max": "write:15",
          "min": "read" // optional
        },
        "database": {
          "root": "TREE_ROOT_ID",
          "tips": ["TIP_ID_1", "TIP_ID_2"]
        }
      }
    }
  }
}
```

### Permission Clamping

Delegated permissions are constrained by bounds:

- **max**: Maximum permission level (required)
- **min**: Minimum permission level (optional)
- Effective permission = clamp(delegated_permission, min, max)
- Priority derives from the effective permission after clamping

### Delegation Chains

Multi-level delegation supported with permission clamping at each level:

```json
{
  "auth": {
    "key": [
      { "key": "org_tree", "tips": ["tip1"] },
      { "key": "team_tree", "tips": ["tip2"] },
      { "key": "ACTUAL_KEY" }
    ]
  }
}
```

### Tip Tracking

"Latest known tips" mechanism ensures key revocations are respected:

1. Entries include delegated database tips at signing time
2. Database tracks these as "latest known tips"
3. Future entries must use equal or newer tips
4. Prevents using old database states where revoked keys were valid

## Authentication Flow

1. **Entry Creation**: Application creates entry with auth field
2. **Signing**: Entry signed with Ed25519 private key
3. **Resolution**: AuthValidator resolves key (direct or delegated)
4. **Status Check**: Verify key is Active (not Revoked)
5. **Tip Validation**: For delegated keys, validate against latest known tips
6. **Permission Clamping**: Apply bounds for delegated permissions
7. **Signature Verification**: Cryptographically verify Ed25519 signature
8. **Permission Check**: Ensure key has sufficient permissions
9. **Storage**: Entry stored if all validations pass

## Bootstrap Authentication Flow

For new devices joining existing databases without prior state:

1. **Bootstrap Request**: Device sends SyncTreeRequest with empty tips + auth info
2. **Key Validation**: Server validates requesting device's public key
3. **Permission Evaluation**: Server checks requested permission level
4. **Key Conflict Check**: System checks if key name already exists:
   - If key exists with same public key: Bootstrap continues (idempotent)
   - If key exists with different public key: Bootstrap fails with error
   - If key doesn't exist: Key is added to database
5. **Auto-Approval**: Server automatically approves key (configurable)
6. **Database Update**: Server safely adds key using conflict-safe `add_key()` method
7. **Bootstrap Response**: Complete database sent with key approval confirmation
8. **Local Setup**: Device stores database and gains authenticated access

**Key Components**:

- `sync_with_peer_for_bootstrap()`: API for authenticated bootstrap
- `add_key_to_database()`: Server-side key approval with conflict handling
- Protocol extensions in SyncTreeRequest/BootstrapResponse
- Key conflict resolution during multi-device bootstrap scenarios

## Conflict Resolution

Authentication changes use **Last-Write-Wins (LWW)** semantics based on the DAG structure:

- Settings conflicts resolved deterministically by Doc CRDT
- Priority determines who CAN make changes
- LWW determines WHICH change wins in a conflict
- Historical entries remain valid even after permission changes
- Revoked status prevents new entries but preserves existing content

### Network Partition Handling

During network splits:

1. Both sides may modify authentication settings
2. Upon reconnection, LWW resolves conflicts
3. Most recent change (by DAG timestamp) takes precedence
4. All historical entries remain valid
5. Future operations follow merged authentication state

## Security Considerations

### Protected Against

- Unauthorized entry creation (mandatory signatures)
- Permission escalation (permission clamping)
- Historical tampering (immutable DAG)
- Replay attacks (content-addressable IDs)
- Administrative hierarchy violations (priority system)

### Requires Manual Recovery

- Admin key compromise when no higher-priority key exists
- Conflicting administrative changes during partitions

## Implementation Components

**AuthValidator** (`auth/validation.rs`): Core validation logic with caching

**Crypto Module** (`auth/crypto.rs`): Ed25519 operations and signature verification

**AuthSettings** (`auth/settings.rs`): Settings management and conflict-safe key operations

**Permission Module** (`auth/permission.rs`): Permission checking and clamping logic

## See Also

- [Database](database.md) - How Databases integrate with authentication
- [Entry](entry.md) - Authentication data in entry structure
- [Authentication Design](../../design/authentication.md) - Full design specification
