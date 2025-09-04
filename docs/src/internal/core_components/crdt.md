# CRDT Implementation

Trait-based system for Conflict-free Replicated Data Types enabling deterministic conflict resolution.

## Core Concepts

**CRDT Trait**: Defines merge operation for resolving conflicts between divergent states. Requires Serialize, Deserialize, and Default implementations.

**Merkle-CRDT Principles**: CRDT state stored in Entry's RawData for deterministic merging across distributed systems.

**Multiple CRDT Support**: Different CRDT types can be used for different stores within the same database.

## Doc and Node Types

**Doc**: The main CRDT document type that users interact with

- Wraps the internal Node structure for clean API boundaries
- Provides document-level operations (get, set, merge, etc.)
- Handles path-based operations for nested data access
- Separates user-facing API from internal implementation

**Node**: Internal database structure implementing the actual CRDT logic

- Supports nested maps and values via the Value enum
- Value types: Text (string), Node (nested map), List (ordered collection), Deleted (tombstone)
- Recursive merging for nested structures
- Last-write-wins strategy for conflicting values
- Type-aware conflict resolution

## Tombstones

Critical for distributed deletion propagation:

- Mark data as deleted instead of physical removal
- Retained and synchronized between replicas
- Ensure deletions propagate to all nodes
- Prevent resurrection of deleted data

## Merge Algorithm

**LCA-Based Computation**: Uses Lowest Common Ancestor for efficient state calculation

**Process**:

1. Identify parent entries (tips) for store
2. Find LCA if multiple parents exist
3. Merge all paths from LCA to parent tips
4. Cache results for performance

**Caching**: Automatic caching of computed states with (Entry_ID, Store) keys for dramatic performance improvements.

## Custom CRDT Implementation

Requirements:

1. Struct implementing Default, Serialize, Deserialize
2. Data marker trait implementation
3. CRDT trait with deterministic merge logic
4. Optional SubTree handle for user-friendly API
