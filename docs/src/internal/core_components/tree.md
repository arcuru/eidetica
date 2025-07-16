# Tree

Analogous to tables in traditional databases. Collections of related entries identified by root entry ID.

## Key Features

- Identified by root entry's content-addressable ID
- Default authentication key for all operations
- Settings stored using Map CRDT
- Access through atomic operations

## Integration

**Entry Collections**: Groups related entries under a single root

**Authentication**: Uses default signing key for operations

**CRDT Support**: Enables conflict-free collaborative editing

**Operation Access**: Provides interface for atomic modifications
