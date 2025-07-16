# Backend

Pluggable storage abstraction layer supporting different storage implementations.

## Database Trait

Abstracts underlying storage to allow different backends without changing core logic.

**Core Operations**:

- Entry storage and retrieval by content-addressable ID
- Verification status tracking for authentication
- Tree and subtree tip calculation
- Topological sorting for consistent entry ordering

## Current Implementation

**InMemory**: HashMap-based storage with JSON file persistence

- Stores entries and verification status
- Includes save/load functionality for state preservation
- Supports all Database trait operations

## Verification Status

**Verified**: Entry cryptographically verified and authorized

**Unverified**: Entry lacks authentication or failed verification

Status determined during commit based on signature validation and permission checking.

## Key Features

**Entry Storage**: Immutable entries with content-addressable IDs

**Tip Calculation**: Identifies entries with no children in trees/subtrees

**Height Calculation**: Computes topological heights for proper ordering

**Graph Traversal**: Efficient DAG navigation for tree operations

## Custom Backend Implementation

Implement Database trait with:

1. Storage-specific logic for all trait methods
2. Verification status tracking support
3. Thread safety (Send + Sync + Any)
4. Performance considerations for graph operations
