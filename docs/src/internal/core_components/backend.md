# Backend

Pluggable storage abstraction layer supporting different storage implementations.

## Architecture

The backend system has two layers:

- **BackendImpl trait**: The storage trait that backends implement
- **Backend wrapper**: Instance-level wrapper providing future local/remote dispatch

## BackendImpl Trait

Abstracts underlying storage to allow different backends without changing core logic.

**Core Operations**:

- Entry storage and retrieval by content-addressable ID
- Verification status tracking for authentication
- Database and store tip calculation
- Topological sorting for consistent entry ordering

## Current Implementation

**InMemory**: HashMap-based storage with JSON file persistence

- Stores entries and verification status
- Includes save/load functionality for state preservation
- Supports all BackendImpl trait operations

## Verification Status

**Verified**: Entry cryptographically verified and authorized

**Unverified**: Entry lacks authentication or failed verification

Status determined during commit based on signature validation and permission checking.

## Key Features

**Entry Storage**: Immutable entries with content-addressable IDs

**Tip Calculation**: Identifies entries with no children in databases/stores

**Height Calculation**: Computes topological heights for proper ordering

**Graph Traversal**: Efficient DAG navigation for database operations

## Custom Backend Implementation

Implement BackendImpl trait with:

1. Storage-specific logic for all trait methods
2. Verification status tracking support
3. Thread safety (Send + Sync + Any)
4. Performance considerations for graph operations

The Backend wrapper will automatically delegate operations to your BackendImpl implementation.
