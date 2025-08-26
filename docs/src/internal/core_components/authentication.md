# Authentication

Mandatory Ed25519 cryptographic authentication system for all entries.

## Core Concepts

**Mandatory Signing**: Every entry requires a valid Ed25519 signature for data integrity and access control.

**AuthValidator**: Central component that validates entries with caching for performance.

**Permission Hierarchy**: Three-tier system with integrated priority levels:

- **Admin(priority)**: Full access including key management
- **Write(priority)**: Data read/write access
- **Read**: Read-only access

**Key Management**: Authentication keys stored in `_settings.auth` subtree using Doc CRDT for conflict resolution.

## Authentication Flow

1. Entry created with signature information
2. Entry signed with Ed25519 private key
3. AuthValidator resolves key from settings
4. Key status verified (Active/Revoked)
5. Signature cryptographically verified
6. Permissions checked for operation
7. Entry stored if valid

## Key Types

**Direct Keys**: Keys stored directly in tree settings

**Delegated Trees**: Cross-tree authentication references with permission clamping

**Wildcard Keys**: Public read access using "\*" key

## Key Lifecycle

**Active**: Key can be used for authentication

**Revoked**: Key disabled but preserved for audit trail

## Priority System

Priority controls administrative operations (who can modify which keys). Lower numbers indicate higher priority. Does not affect CRDT merge resolution.
