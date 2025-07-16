# Core Components

Main building blocks of Eidetica's architecture.

## Components

- **[Entry](entry.md)**: Immutable content-addressable data units
- **[Authentication](authentication.md)**: Ed25519-based cryptographic authentication
- **[CRDT](crdt.md)**: Conflict-free data structures for distributed merging
- **[BaseDB](basedb.md)**: Primary database implementation
- **[Tree](tree.md)**: Database table analogue for entry collections
- **[AtomicOp](atomicop.md)**: Atomic transaction mechanism
- **[Backend](backend.md)**: Pluggable storage abstraction layer
- **[Subtrees](subtrees.md)**: Typed data access patterns within trees
