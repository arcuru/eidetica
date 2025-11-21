# Stores

Typed data access patterns within databases providing structured interaction with Entry RawData.

## Core Concepts

**SubTree Trait**: Interface for typed store implementations accessed through Transaction handles.

**Reserved Names**: Store names with underscore prefix (e.g., `_settings`, `_index`, `_root`) reserved for internal use.

**Typed APIs**: Handle serialization/deserialization and provide structured access to raw entry data.

**Type Registration**: All stores have a type identifier (e.g., "docstore:v1") registered in the `_index` subtree.

## Current Implementations

### Table<T>

Record-oriented store for managing collections with unique identifiers.

**Features**:

- Stores user-defined types (T: Serialize + Deserialize)
- Automatic UUID generation for records
- CRUD operations: insert, get, set, delete, search
- Type-safe access via Operation::get_subtree

**Use Cases**: User lists, task management, any collection requiring persistent IDs.

### DocStore

Document-oriented store wrapping `crdt::Doc` for nested structures and path-based access.

**Features**:

- Path-based operations for nested data (set_path, get_path, etc.)
- Simple key-value operations (get, set, delete)
- Support for nested map structures via Value enum
- Tombstone support for distributed deletion propagation
- Last-write-wins merge strategy

**Use Cases**: Configuration data, metadata, structured documents, sync state.

### SettingsStore

Specialized wrapper around DocStore for managing the `_settings` subtree with type-safe authentication operations.

**Features**:

- Type-safe settings management API
- Convenience methods for authentication key operations
- Atomic updates via closure pattern (update_auth_settings)
- Direct access to underlying DocStore for advanced operations
- Built-in validation for authentication configurations

**Architecture**:

- Wraps DocStore instance configured for `_settings` subtree
- Delegates to AuthSettings for authentication-specific operations
- Provides abstraction layer hiding CRDT implementation details
- Maintains proper transaction boundaries for settings modifications

**Operations**:

- Database name management (get_name, set_name)
- Authentication key lifecycle (set_auth_key, get_auth_key, revoke_auth_key)
- Bulk auth operations via update_auth_settings closure
- Auth validation via validate_entry_auth method

**Use Cases**: Database configuration, authentication key management, settings validation, bootstrap policies.

### YDoc (Y-CRDT Integration)

Real-time collaborative editing with sophisticated conflict resolution.

**Features** (requires "y-crdt" feature):

- Y-CRDT algorithms for collaboration
- Differential saving for storage efficiency
- Full Y-CRDT API access
- Caching for performance optimization

**Architecture**:

- YrsBinary wrapper implements CRDT traits
- Differential updates vs full snapshots
- Binary update merging preserves Y-CRDT algorithms

**Operations**:

- Document access with safe closures
- External update application
- Incremental change tracking

**Use Cases**: Collaborative documents, real-time editing, complex conflict resolution.

### IndexStore

Registry interface for the `_index` subtree that tracks subtree type metadata.

**Purpose**: Enables type discovery, schema management, and future dynamic Store loading.

**Architectural Constraint**: When `_index` is modified for a subtree, that subtree MUST appear in the same Entry. This ensures the Entry is part of the subtree's DAG, so metadata is verified and always synced along with the subtree data.

System subtrees (\_settings, \_index, \_root) are excluded from the registry.

## Custom Store Implementation

Custom Stores implement the `Store` trait and use Transaction methods for data access:

- `get_local_data` for staged state
- `get_full_state` for merged historical state
- `update_subtree` for staging changes

The Store trait includes `type_id()` for registry identification and optional `default_config()` for custom configuration.

## Integration

**Transaction Context**: All stores accessed through atomic transactions

**CRDT Support**: Stores can implement CRDT trait for conflict resolution

**Serialization**: Data stored as Option\<RawData> in Entry structure

**Auto-Registration**: First access via get_store() triggers Store::init() which registers the subtree in `_index` with type_id and default_config
