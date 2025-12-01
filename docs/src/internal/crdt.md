# CRDT Merging

Eidetica implements a Merkle-CRDT using content-addressable entries organized in a Merkle DAG structure. Entries store data and maintain parent references to form a distributed version history that supports deterministic merging.

## Core Concepts

- **Content-Addressable Entries**: Immutable data units forming a directed acyclic graph
- **CRDT Trait**: Enables deterministic merging of concurrent changes
- **Parent References**: Maintain history and define DAG structure
- **Tips Tracking**: Identifies current heads for efficient synchronization

## Fork and Merge

The system supports branching and merging through parent-child relationships:

- **Forking**: Multiple entries can share parents, creating divergent branches
- **Merging**: Entries with multiple parents merge separate branches
- **Deterministic Ordering**: Entries sorted by height then ID for consistent results

## Merge Algorithm

Uses a recursive LCA-based approach for computing CRDT states:

- **Cache Check**: Avoids redundant computation through automatic caching
- **LCA Computation**: Finds lowest common ancestor for multi-parent entries
- **Recursive Building**: Computes ancestor states recursively
- **Path Merging**: Merges all entries from LCA to parents with proper ordering
- **Local Integration**: Applies current entry's data to final state

## Key Properties

- **Correctness**: Consistent state computation regardless of access patterns
- **Performance**: Caching eliminates redundant work
- **Deterministic**: Maintains ordering through proper LCA computation
- **Immutable Caching**: Entry immutability ensures cache validity
