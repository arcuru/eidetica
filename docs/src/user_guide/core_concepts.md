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

```text
+-----------------------+
| User Application      |
+-----------------------+
| Instance                |
+-----------------------+
| Databases                 |
+----------+------------+
| Stores | Operations |
+----------+------------+
| Entries (DAG)         |
+-----------------------+
| Database Storage      |
+-----------------------+
```

Each layer builds on the ones below, providing progressively higher-level abstractions:

1. **Database Storage**: Physical storage of data (currently InMemory with file persistence)
2. **Entries**: Immutable, content-addressed objects forming the database's history
3. **Databases & Stores**: Logical organization and typed access to data
4. **Operations**: Atomic transactions across multiple stores
5. **Instance**: The top-level database container and API entry point

## Entries and the DAG

At the core of Eidetica is a directed acyclic graph (DAG) of immutable Entry objects:

- Each Entry represents a point-in-time snapshot of data and has:
  - A unique ID derived from its content (making it content-addressable)
  - Links to parent entries (forming the graph structure)
  - Data payloads organized by store
  - Metadata for database and store relationships

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

## Stores: A Core Innovation

Eidetica extends the Merkle-CRDT concept with Stores, which partition data within each Entry:

- Each store is a named, typed data structure within a Database
- Stores can use different data models and conflict resolution strategies
- Stores maintain their own history tracking within the larger Database

This enables:

- Type-safe, structure-specific APIs for data access
- Efficient partial synchronization (only needed stores)
- Modular features through pluggable stores
- Atomic operations across different data structures

Planned future stores include:

- Object Storage: Efficiently handling large objects with content-addressable hashing
- Backup: Archiving database history for space efficiency
- Encrypted Store: Transparent encrypted data storage

## Atomic Operations and Transactions

All changes in Eidetica happen through atomic Transactions:

1. A Transaction is created from a Database
2. Stores are accessed and modified through the Transaction
3. When committed, all changes across all stores become a single new Entry
4. If the Transaction fails, no changes are applied

This model ensures data consistency while allowing complex operations across multiple stores.

## Settings as Stores

In Eidetica, even configuration is stored as a store:

- A Database's settings are stored in a special "settings" Store internally that is hidden from regular usage
- This approach unifies the data model and allows settings to participate in history tracking

## CRDT Properties and Eventual Consistency

Eidetica is designed with distributed systems in mind:

- All data structures have CRDT properties for automatic conflict resolution
- Different store types implement appropriate CRDT strategies:
  - DocStore uses last-writer-wins (LWW) with implicit timestamps
  - Table preserves all items, with LWW for updates to the same item

These properties ensure that when Eidetica instances synchronize, they eventually reach a consistent state regardless of the order in which updates are received.

## History Tracking and Time Travel

One of Eidetica's most powerful features is comprehensive history tracking:

- All changes are preserved in the Entry DAG
- "Tips" represent the latest state of a Database or Store
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

- Core Entry and Database structure
- In-memory database with file persistence
- DocStore and Table store implementations
- CRDT functionality:
  - Doc (hierarchical nested document structure with recursive merging and tombstone support for deletions)
- Atomic operations across stores
- Tombstone support for proper deletion handling in distributed environments

### Planned Features

- Object Storage store for efficient handling of large objects
- Backup store for archiving database history
- Encrypted store for transparent encrypted data storage
- IPFS-compatible addressing for distributed storage
- Enhanced synchronization mechanisms
- Point-in-time recovery

This roadmap is subject to change as development progresses. Check the project repository for the most up-to-date information on feature availability.
