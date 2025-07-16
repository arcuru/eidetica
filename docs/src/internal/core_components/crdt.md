# CRDT Implementation

Trait-based system for Conflict-free Replicated Data Types enabling deterministic conflict resolution.

## Core Concepts

**CRDT Trait**: Defines merge operation for resolving conflicts between divergent states. Requires Serialize, Deserialize, and Default implementations.

**Merkle-CRDT Principles**: CRDT state stored in Entry's RawData for deterministic merging across distributed systems.

**Multiple CRDT Support**: Different CRDT types can be used for different subtrees within the same tree.

## Map CRDT Types

**Simple Map**: Key-value CRDT using last-write-wins strategy

- HashMap with optional string values for tombstone support
- Tombstones track deletions without removing keys
- Last-write-wins merge resolution

**Nested Map**: Supports arbitrary nesting of maps and values

- Value enum: String, Map, or Deleted (tombstone)
- Recursive merging for nested structures
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

1. Identify parent entries (tips) for subtree
2. Find LCA if multiple parents exist
3. Merge all paths from LCA to parent tips
4. Cache results for performance

**Caching**: Automatic caching of computed states with (Entry_ID, Subtree) keys for dramatic performance improvements.

## Custom CRDT Implementation

Requirements:

1. Struct implementing Default, Serialize, Deserialize
2. Data marker trait implementation
3. CRDT trait with deterministic merge logic
4. Optional SubTree handle for user-friendly API
