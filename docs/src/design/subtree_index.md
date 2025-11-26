> ✅ **Status: Implemented**
>
> This design is fully implemented and functional.

# Subtree Index (\_index)

This document describes the `_index` subtree registry system, which maintains metadata about all user-created subtrees in an Eidetica database.

## Table of Contents

- [Overview](#overview)
- [Design Goals](#design-goals)
- [Metadata Travels With Data](#metadata-travels-with-data)
- [How It Works](#how-it-works)
- [API Reference](#api-reference)
- [Examples](#examples)

## Overview

The `_index` subtree is a special system subtree that serves as a registry for all user-created subtrees in a database. It stores metadata about each subtree, including its Store type identifier and configuration data. This enables type discovery, versioning, and configuration management for subtrees.

**Key Features**:

- **Automatic Registration**: Subtrees are automatically registered when first accessed via `get_store()`
- **Type Metadata**: Stores the Store type identifier (e.g., "docstore:v1", "table:v1")
- **Configuration Storage**: Stores Store-specific configuration as JSON
- **Query API**: Provides IndexStore for querying registered subtrees

## Design Goals

The `_index` subtree provides essential metadata capabilities for Eidetica databases:

1. **Type Discovery**: Every subtree has an associated type identifier in `_index`, enabling generic tooling to understand what Store type manages each subtree
2. **Versioning**: Type identifiers include arbitrary version information (e.g., "docstore:v1"), supporting schema migrations and format evolution
3. **Configuration**: Store-specific settings are stored alongside type information, enabling per-subtree customization
4. **Discoverability**: The IndexStore API enables querying all registered subtrees, supporting database browsers and tooling

These capabilities enable:

- Generic database browsers that understand subtree types
- Schema migrations when Store formats evolve
- Tooling that enumerates and understands database structure

## Metadata Travels With Data

> **Subtree metadata is cryptographically verified as part of the same DAG as the subtree data itself—without requiring the full database DAG.**

When you sync a subtree (like `users`) from another peer, you automatically receive all `_index` metadata about that subtree. This is guaranteed by a simple architectural constraint: any Entry that modifies `_index` for a subtree must also include that subtree.

**Why this matters:**

- **No orphaned metadata**: You can't have `_index` entries for subtrees you haven't synced
- **No missing metadata**: When you have a subtree's data, you have its metadata too
- **Cryptographic verification**: The metadata is verified by the same Merkle-DAG that verifies the data
- **Enable Efficient sync**: Sync just the subtrees you need and their metadata comes along automatically

This constraint leverages Eidetica's Merkle-DAG structure: the Entry containing the `_index` update becomes part of the subtree's parent DAG, is verified by the same cryptographic properties, and is automatically included when syncing that subtree.

## How It Works

### The `_index` Subtree

The `_index` subtree is a special system subtree (like `_settings` and `_root`) that uses DocStore to maintain a registry of subtree metadata:

- **Name**: `_index` (reserved system name)
- **Store Type**: DocStore internally
- **Not Self-Registering**: System subtrees (`_index`, `_settings`, `_root`) are excluded from auto-registration to avoid circular dependencies

Each registered subtree has an entry in `_index` with the following structure:

```json
{
  "_index": {
    "users": {
      "type": "table:v1",
      "config": "{}"
    },
    "documents": {
      "type": "ydoc:v1",
      "config": "{\"compression\":\"zstd\"}"
    }
  }
}
```

**Fields**:

- `type`: The Store type identifier from `Store::type_id()` (e.g., "docstore:v1")
- `config`: Store-specific configuration as a JSON string

### Auto-Registration

Subtrees are automatically registered in `_index` when first accessed via `Transaction::get_store()`. The Store's `init()` method handles both creation and registration.

Manual registration via `IndexStore::set_subtree_info()` allows pre-configuring subtrees with custom settings before first access.

### The Index-Subtree Coupling Constraint

**Core Rule**: When `_index` is modified for a subtree, that subtree MUST appear in the same Entry.

This is what enables [metadata to travel with data](#metadata-travels-with-data). The constraint ensures:

1. **DAG Inclusion**: The Entry containing the `_index` update becomes part of the subtree's parent DAG
2. **Verification**: The Entry is verified by the Merkle-DAG properties of the subtree's parent tree
3. **Sync Completeness**: When syncing a subtree's DAG, all Entries pertaining to that subtree are included, including any `_index` metadata about it

To support this constraint, `SubTreeNode.data` is `Option<RawData>`:

- `None`: Subtree participates in this Entry but makes no data changes
- `Some("")`: Explicit empty data (e.g., CRDT tombstone)
- `Some(data)`: Actual serialized data

This allows subtrees to appear in Entries purely to satisfy the constraint without requiring data changes.

## API Reference

### Store Trait Extensions

The `Store` trait provides methods for registry integration:

- **`type_id()`**: Returns unique identifier with version (e.g., "docstore:v1", "table:v1")
- **`default_config()`**: Returns default configuration as JSON string
- **`init()`**: Creates store and registers it in `_index`
- **`get_config()` / `set_config()`**: Read/write configuration in `_index`

### IndexStore API

`IndexStore` provides query and management operations for the registry:

- `get_subtree_info(name)`: Get type and config for a subtree
- `contains_subtree(name)`: Check if registered
- `set_subtree_info(name, type_id, config)`: Register or update
- `list_subtrees()`: Get all registered subtree names

Access via `Transaction::get_index_store()`.

## Examples

### Basic Auto-Registration

<!-- Code block testable: Shows auto-registration during normal store access -->

```rust
# extern crate eidetica;
# use eidetica::{Instance, Transaction, Store, store::DocStore, backend::database::InMemory, crdt::Doc};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let instance = Instance::open(backend)?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let db = user.create_database(settings, &default_key)?;
#
// First access to "config" subtree - will be auto-registered
let txn = db.new_transaction()?;
let config: DocStore = txn.get_store("config")?;
config.set("theme", "dark")?;
txn.commit()?;

// After commit, "config" is registered in _index
let txn = db.new_transaction()?;
let index = txn.get_index_store()?;
assert!(index.contains_subtree("config"));

let info = index.get_subtree_info("config")?;
assert_eq!(info.type_id, "docstore:v1");
assert_eq!(info.config, "{}");
# Ok(())
# }
```

### Manual Registration with Custom Config

<!-- Code block testable: Shows manual registration with custom configuration -->

```rust
# extern crate eidetica;
# use eidetica::{Instance, Transaction, Store, store::DocStore, backend::database::InMemory, crdt::Doc};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let instance = Instance::open(backend)?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let db = user.create_database(settings, &default_key)?;
#
// Pre-register subtree with custom configuration
let txn = db.new_transaction()?;
let index = txn.get_index_store()?;

index.set_subtree_info(
    "documents",
    "ydoc:v1",
    r#"{"compression":"zstd","cache_size":1024}"#
)?;

txn.commit()?;

// Later access uses the registered configuration
let txn = db.new_transaction()?;
let index = txn.get_index_store()?;
let info = index.get_subtree_info("documents")?;
assert_eq!(info.type_id, "ydoc:v1");
assert!(info.config.contains("compression"));
# Ok(())
# }
```

### Querying Registered Subtrees

<!-- Code block testable: Shows querying all registered subtrees -->

```rust
# extern crate eidetica;
# use eidetica::{Instance, Transaction, Store, store::DocStore, backend::database::InMemory, crdt::Doc};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let instance = Instance::open(backend)?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let db = user.create_database(settings, &default_key)?;
#
// Create several subtrees with data
let txn = db.new_transaction()?;
let users: DocStore = txn.get_store("users")?;
users.set("count", "0")?;
let posts: DocStore = txn.get_store("posts")?;
posts.set("count", "0")?;
let comments: DocStore = txn.get_store("comments")?;
comments.set("count", "0")?;
txn.commit()?;

// Query all registered subtrees
let txn = db.new_transaction()?;
let index = txn.get_index_store()?;
let subtrees = index.list_subtrees()?;

// All three subtrees should be registered
assert!(subtrees.contains(&"users".to_string()));
assert!(subtrees.contains(&"posts".to_string()));
assert!(subtrees.contains(&"comments".to_string()));
# Ok(())
# }
```
