# Core Components

The architectural foundation of Eidetica, implementing the Merkle-CRDT design principles through a carefully orchestrated set of interconnected components.

## Component Overview

These components work together to provide Eidetica's unique combination of features: content-addressable storage, cryptographic authentication, conflict-free synchronization, and flexible data access patterns.

## Architecture Layers

**[Entry](entry.md)**: The fundamental data unit containing TreeNode and SubTreeNode structures - immutable, content-addressable, and cryptographically signed

**[Database](database.md)**: User-facing abstraction providing operations over trees of entries with independent history and authentication policies

**[Instance](instance.md)**: The main database orchestration layer managing databases, authentication, and storage

**[User System](user.md)**: Multi-user account management with per-user key storage, database tracking, and sync preferences

**[Transaction](transaction.md)**: Transaction mechanism providing atomic operations across multiple stores

## Data Access and Storage

**[Stores](stores.md)**: User-facing typed data access patterns (DocStore, Table, YDoc) that provide application-friendly interfaces over subtree data

**[Backend](backend.md)**: Pluggable storage abstraction supporting different persistence strategies

**[CRDT](crdt.md)**: Conflict-free data types enabling distributed merging and synchronization

## Security and Synchronization

**[Authentication](authentication.md)**: Ed25519-based cryptographic system for signing and verification

**[Synchronization](synchronization.md)**: Distributed sync protocols built on the Merkle-DAG foundation

## Terminology Note

Eidetica uses a dual terminology system:

- **Internal structures**: TreeNode/SubTreeNode refer to the actual Merkle-DAG data structures within entries
- **User abstractions**: Database/Store refer to the high-level APIs and concepts users interact with

See [Terminology](../terminology.md) for detailed guidelines on when to use each naming scheme.
