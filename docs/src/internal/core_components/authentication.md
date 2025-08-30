# Authentication

Comprehensive Ed25519-based cryptographic authentication system that ensures data integrity and access control across Eidetica's distributed architecture.

## Overview

Eidetica implements **mandatory authentication** for all entries - there are no unsigned entries in the system. Every operation requires valid Ed25519 signatures, providing strong guarantees about data authenticity and enabling sophisticated access control in decentralized environments.

The authentication system is deeply integrated with the core database, not merely a consumer of the API. This tight integration enables efficient validation, deterministic conflict resolution during network partitions, and preservation of historical validity.

## Architecture

**Storage Location**: Authentication configuration resides in the special `_settings.auth` subtree of each Tree, using Doc CRDT for deterministic conflict resolution.

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

### Direct Keys

Ed25519 public keys stored directly in the tree's `_settings.auth`:

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
- Commonly used for world-readable trees
- Subject to same revocation mechanisms as regular keys

## Delegation System

Trees can delegate authentication to other trees, enabling powerful authentication patterns without granting administrative privileges on the delegating tree.

### Core Concepts

**Delegated Tree References**: Any tree can reference another tree as an authentication source:

```json
{
  "_settings": {
    "auth": {
      "user@example.com": {
        "permission-bounds": {
          "max": "write:15",
          "min": "read" // optional
        },
        "tree": {
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

1. Entries include delegated tree tips at signing time
2. Tree tracks these as "latest known tips"
3. Future entries must use equal or newer tips
4. Prevents using old tree states where revoked keys were valid

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

**AuthSettings** (`auth/settings.rs`): Settings management and key operations

**Permission Module** (`auth/permission.rs`): Permission checking and clamping logic

## See Also

- [Tree](tree.md) - How Trees integrate with authentication
- [Entry](entry.md) - Authentication data in entry structure
- [Authentication Design](../../design/authentication.md) - Full design specification
