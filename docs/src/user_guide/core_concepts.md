# Core Concepts

Understanding the fundamental ideas behind Eidetica will help you use it effectively and appreciate its unique capabilities.

## Architectural Foundation

Eidetica builds on several powerful concepts from distributed systems and database design:

1. **Content-addressable storage**: Data is identified by the hash of its content, similar to Git and IPFS
2. **Directed acyclic graphs (DAGs)**: Changes form a graph structure rather than a linear history
3. **Conflict-free replicated data types (CRDTs)**: Data structures that can merge concurrent changes automatically
4. **Immutable data structures**: Once created, data is never modified, only new versions are added

These foundations enable Eidetica's key features: robust history tracking, efficient synchronization, and eventual consistency in distributed environments.

## Merkle-CRDTs

Eidetica is inspired by the Merkle-CRDT concept from OrbitDB, which combines:

- **Merkle DAGs**: A data structure where each node contains a cryptographic hash of its children, creating a tamper-evident history
- **CRDTs**: Data types designed to resolve conflicts automatically when concurrent changes occur

In a Merkle-CRDT, each update creates a new node in the graph, containing:

1. References to parent nodes (previous versions)
2. The updated data
3. Metadata for conflict resolution

This approach allows for:

- Strong auditability of all changes
- Automatic conflict resolution
- Efficient synchronization between replicas

## Data Model Layers

Eidetica organizes data in a layered architecture:

```
+-----------------------+
| User Application      |
+-----------------------+
| BaseDB                |
+-----------------------+
| Trees                 |
+----------+------------+
| Subtrees | Operations |
+----------+------------+
| Entries (DAG)         |
+-----------------------+
| Backend Storage       |
+-----------------------+
```

Each layer builds on the ones below, providing progressively higher-level abstractions:

1. **Backend Storage**: Physical storage of data (currently InMemory with file persistence)
2. **Entries**: Immutable, content-addressed objects forming the database's history
3. **Trees & Subtrees**: Logical organization and typed access to data
4. **Operations**: Atomic transactions across multiple subtrees
5. **BaseDB**: The top-level database container and API entry point

## Entries and the DAG

At the core of Eidetica is a directed acyclic graph (DAG) of immutable Entry objects:

- Each Entry represents a point-in-time snapshot of data and has:

  - A unique ID derived from its content (making it content-addressable)
  - Links to parent entries (forming the graph structure)
  - Data payloads organized by subtree
  - Metadata for tree and subtree relationships

- The DAG enables:
  - Full history tracking (nothing is ever deleted)
  - Efficient verification of data integrity
  - Conflict resolution when merging concurrent changes

## IPFS Inspiration and Future Direction

While Eidetica draws inspiration from IPFS (InterPlanetary File System), it currently uses its own implementation patterns:

- IPFS is a content-addressed, distributed storage system where data is identified by cryptographic hashes
- OrbitDB (which inspired Eidetica) uses IPFS for backend storage and distribution

Eidetica's future plans include:

- Developing efficient internal APIs for transferring objects between Eidetica instances
- Potential IPFS-compatible addressing for distributed storage
- More efficient synchronization mechanisms than traditional IPFS

## Subtrees: A Core Innovation

Eidetica extends the Merkle-CRDT concept with Subtrees, which partition data within each Entry:

- Each subtree is a named, typed data structure within a Tree
- Subtrees can use different data models and conflict resolution strategies
- Subtrees maintain their own history tracking within the larger Tree

This innovation enables:

- Type-safe, structure-specific APIs for data access
- Efficient partial synchronization (only needed subtrees)
- Modular features through pluggable subtrees
- Atomic operations across different data structures

Planned future subtrees include:

- Object Storage: Efficiently handling large objects with content-addressable hashing
- Backup: Archiving tree history for space efficiency
- Encrypted Subtree: Transparent encrypted data storage

## Atomic Operations and Transactions

All changes in Eidetica happen through atomic Operations:

1. An Operation is created from a Tree
2. Subtrees are accessed and modified through the Operation
3. When committed, all changes across all subtrees become a single new Entry
4. If the Operation fails, no changes are applied

This transaction-like model ensures data consistency while allowing complex operations across multiple subtrees.

## Settings as Subtrees

In Eidetica, even configuration is stored as a subtree:

- A Tree's settings are stored in a special "settings" KV Store subtree
- This approach unifies the data model and allows settings to participate in history tracking
- It also enables future distributed synchronization of settings

## CRDT Properties and Eventual Consistency

Eidetica is designed with distributed systems in mind:

- All data structures have CRDT properties for automatic conflict resolution
- Different subtree types implement appropriate CRDT strategies:
  - KVStore uses last-writer-wins (LWW) with implicit timestamps
  - RowStore preserves all items, with LWW for updates to the same item

These properties ensure that when Eidetica instances synchronize, they eventually reach a consistent state regardless of the order in which updates are received.

## History Tracking and Time Travel

One of Eidetica's most powerful features is comprehensive history tracking:

- All changes are preserved in the Entry DAG
- "Tips" represent the latest state of a Tree or Subtree
- Historical states can be reconstructed by traversing the DAG

This design allows for future capabilities like:

- Point-in-time recovery
- Auditing and change tracking
- Historical queries and analysis
- Branching and versioning

<!-- TODO: Document history access APIs when they are more fully developed -->

## Current Status and Roadmap

Eidetica is under active development, and some features mentioned in this documentation are still in planning or development stages. Here's a summary of the current status:

### Implemented Features

- Core Entry and Tree structure
- In-memory backend with file persistence
- KVStore and RowStore subtree implementations
- CRDT functionality:
  - KVOverWrite (simple key-value with tombstone support for deletions)
  - KVNested (hierarchical nested key-value structure with recursive merging)
- Atomic operations across subtrees
- Tombstone support for proper deletion handling in distributed environments

### Planned Features

- Object Storage subtree for efficient handling of large objects
- Backup subtree for archiving tree history
- Encrypted subtree for transparent encrypted data storage
- IPFS-compatible addressing for distributed storage
- Enhanced synchronization mechanisms
- Point-in-time recovery

This roadmap is subject to change as development progresses. Check the project repository for the most up-to-date information on feature availability.
