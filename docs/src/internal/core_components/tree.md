# Tree

Represents an independent, versioned collection of data entries within Eidetica, analogous to a database in traditional databases.

## Conceptual Model

Trees organize related data entries into a coherent unit with its own history and authentication policies. Each Tree is identified by its root entry's content-addressable ID, making it globally unique and verifiable.

Unlike traditional databases, Trees maintain full historical data through a Merkle DAG structure, enabling features like:

- Conflict-free merging of concurrent changes
- Cryptographic verification of data integrity
- Decentralized synchronization across devices
- Point-in-time queries (unimplemented)

## Architecture and Lifecycle

**Tree Creation**: Initialized with settings (stored as a Doc CRDT) and associated with an authentication key for signing operations.

**Data Access**: Applications interact with Trees through AtomicOp instances, which provide transactional semantics and subtree access.

**Entry History**: Each operation creates new entries that reference their parents, building an immutable history DAG.

**Settings Management**: Tree-level configuration (permissions, sync settings, etc.) is stored as CRDT data, allowing distributed updates.

## Authentication

Each Tree maintains its own authentication configuration in the special `_settings` subtree. All entries must be cryptographically signed with Ed25519 signatures - there are no unsigned entries in Eidetica.

Trees support direct keys, delegation to other trees for flexible cross-project authentication, and a three-tier permission hierarchy (Admin, Write, Read) with priority-based key management. Authentication changes merge deterministically using Last-Write-Wins semantics.

For complete details, see [Authentication](authentication.md).

## Integration Points

**Subtree Access**: Trees provide typed access to different data structures (DocStore, Table, YDoc) through the subtree system.

**Synchronization**: Trees serve as the primary unit of synchronization, with independent merge and conflict resolution.
