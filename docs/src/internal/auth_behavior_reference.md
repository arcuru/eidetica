# Authentication Behavior Reference

This document provides a comprehensive behavioral reference for authentication configuration states in Eidetica. It complements the [Authentication Design](../design/authentication.md) by documenting the exact behavior of each authentication state with implementation details.

## Table of Contents

- [Overview](#overview)
- [State Definitions](#state-definitions)
  - [Unsigned Mode](#unsigned-mode)
  - [Signed Mode](#signed-mode)
  - [Corrupted/Deleted State](#corrupteddeleted-state)
- [Operation Behavior by State](#operation-behavior-by-state)

## Overview

Eidetica's authentication system operates in two valid modes with **proactive corruption prevention**:

1. **Unsigned Mode**: No authentication configured (missing or empty `_settings.auth`)
2. **Signed Mode**: Valid authentication configuration with at least one key

**Corruption Prevention**: The system uses two-layer validation to prevent invalid auth states:

- **Proactive Prevention** (Layer 1): Transactions that would corrupt or delete auth configuration fail immediately during `commit()`, before the entry enters the Merkle DAG
- **Reactive Fail-Safe** (Layer 2): If auth is already corrupted (from older code or external manipulation), all operations fail with `CorruptedAuthConfiguration`

**Theoretical States** (prevented by validation): 3. **Corrupted State**: Auth configuration has wrong type (PREVENTED - cannot be created) 4. **Deleted State**: Auth configuration was deleted (PREVENTED - cannot be created)

The system enforces aggressive fail-safe behavior: any attempt to corrupt or delete authentication fails immediately, preventing security bypass exploits.

## State Definitions

### Unsigned Mode

**CRDT State**: `_settings.auth` is either:

- Missing entirely (key doesn't exist in Doc)
- Contains empty Doc: `{"auth": {}}`

Both states are **equivalent** - the system treats missing and empty identically.

**Behavior**:

- ✓ **Unsigned operations succeed**: Transactions without signatures commit normally
- ⚡ **No validation overhead**: Authentication validation is skipped entirely
- 🔒 **Not a security weakness**: Intended for only specialized databases

**Use Cases**:

- Development and testing environments
- Local-only computation that never syncs
- Temporary scratch databases
- Future "overlay" databases for local work

### Signed Mode

**CRDT State**: `_settings.auth` contains a `Doc` with at least one key configuration:

```json
{
  "_settings": {
    "auth": {
      "KEY_NAME": {
        "pubkey": "ed25519:...",
        "permissions": "admin:0",
        "status": "active"
      }
    }
  }
}
```

**Behavior**:

- ✗ **Unsigned operations rejected**: All operations must have valid signatures
- ✓ **Authenticated operations validated**: Signature verification and permission checks
- 🔒 **Mandatory authentication**: Security enforced for all future operations
- ⚠️ **Permanent state**: Cannot return to unsigned mode without creating new database

### Corrupted/Deleted State

**Status**: **This state is prevented** by proactive validation and can no longer be created through normal operations.

**Theoretical CRDT State**: `_settings.auth` exists but contains **wrong type** (not a Doc):

- String value: `{"auth": "corrupted_string"}`
- Number value: `{"auth": 42}`
- Array value: `{"auth": [1, 2, 3]}`
- Tombstone value: `{"auth": null}`
- Any non-Doc type

**How It's Prevented**:

- **Layer 1** (Proactive): Commits that would create wrong-type auth fail before entry creation
- **Layer 2** (Reactive): If somehow corrupted, all subsequent operations fail

**If It Existed, Behavior Would Be**:

- ✗ **ALL operations rejected**: Both unsigned and authenticated operations fail
- 💥 **Fail-safe enforcement**: Prevents security bypass through corruption
- 🚨 **Error**: `TransactionError::CorruptedAuthConfiguration`

**Rationale for Fail-Safe**:
If auth configuration is corrupted, the system cannot determine:

- Whether authentication should be required
- What keys are valid
- What permissions exist

Rather than guess or bypass security, ignore the corrupted Entry.

## Operation Behavior by State

Complete behavior matrix for all combinations:

| Auth State                     | Unsigned Transaction       | Authenticated Transaction | Behavior                |
| ------------------------------ | -------------------------- | ------------------------- | ----------------------- |
| **Unsigned Mode** (missing)    | ✓ Succeeds                 | ✓ Triggers bootstrap      | Normal operation        |
| **Unsigned Mode** (empty `{}`) | ✓ Succeeds                 | ✓ Triggers bootstrap      | Equivalent to missing   |
| **Signed Mode**                | ✗ Rejected (auth required) | ✓ Validated normally      | Security enforced       |
| **Corrupted** (wrong type)     | ✗ Rejected                 | ✗ Rejected                | Fail-safe: All ops fail |
| **Deleted** (tombstone)        | ✗ Rejected                 | ✗ Rejected                | Fail-safe: All ops fail |

**Error Messages**:

- Unsigned op in signed mode: `AuthenticationRequired` or `NoAuthConfiguration`
- Corrupted state: `CorruptedAuthConfiguration`
- Deleted state: `CorruptedAuthConfiguration`

## Related Documentation

- [Authentication Design](../design/authentication.md) - High-level design and goals
- [Transaction Implementation](core_components/transaction.md) - Transaction and validation details
- [Authentication Components](core_components/authentication.md) - Auth module architecture
- [Error Handling](error_handling.md) - Error types and handling patterns
