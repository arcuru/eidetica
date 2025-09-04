# Core Components

The architectural foundation of Eidetica, implementing the Merkle-CRDT design principles through a carefully orchestrated set of interconnected components.

## Component Overview

These components work together to provide Eidetica's unique combination of features: content-addressable storage, cryptographic authentication, conflict-free synchronization, and flexible data access patterns.

## Architecture Layers

**[Entry](entry.md)**: The fundamental data unit - immutable, content-addressable, and cryptographically signed

**[Database](database.md)**: Collections of related entries with independent history and authentication policies

**[Instance](basedb.md)**: The main database orchestration layer managing databases, authentication, and storage

**[Transaction](atomicop.md)**: Transaction mechanism providing atomic operations across multiple stores

## Data Access and Storage

**[Stores](stores.md)**: Typed data structures (DocStore, Table, YDoc) providing application-friendly interfaces

**[Backend](backend.md)**: Pluggable storage abstraction supporting different persistence strategies

**[CRDT](crdt.md)**: Conflict-free data types enabling distributed merging and synchronization

## Security and Synchronization

**[Authentication](authentication.md)**: Ed25519-based cryptographic system for signing and verification

**[Synchronization](synchronization.md)**: Distributed sync protocols built on the Merkle-DAG foundation
