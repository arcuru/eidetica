# DocStore

Public store implementation providing document-oriented storage with path-based nested data access.

## Overview

DocStore is a publicly available store type that provides a document-oriented interface for storing and retrieving data. It wraps the `crdt::Doc` type to provide ergonomic access patterns for nested data structures, making it ideal for configuration, metadata, and structured document storage.

## Key Characteristics

**Public API**: DocStore is exposed as part of the public store API and can be used in applications.

**Doc CRDT Based**: Wraps the `crdt::Doc` type which provides deterministic merging of concurrent changes.

**Path-Based Operations**: Supports both flat key-value storage and path-based access to nested structures.

## Important Behavior: Nested Structure Creation

### Path-Based Operations Create Nested Maps

When using `set_path()` with dot-separated paths, DocStore creates **nested map structures**, not flat keys with dots:

<!-- Code block ignored: Conceptual examples and internal API usage not suitable for testing -->

```rust,ignore
// This code:
docstore.set_path("user.profile.name", "Alice")?;

// Creates this structure:
{
  "user": {
    "profile": {
      "name": "Alice"
    }
  }
}

// NOT this:
{ "user.profile.name": "Alice" }  // ❌ This is NOT what happens
```

### Accessing Nested Data

When using `get_all()` to retrieve all data, you get the nested structure and must navigate it accordingly:

<!-- Code block ignored: Conceptual examples and internal API usage not suitable for testing -->

```rust,ignore
let all_data = docstore.get_all()?;

// Wrong way - looking for a flat key with dots
let value = all_data.get("user.profile.name");  // ❌ Returns None

// Correct way - navigate the nested structure
if let Some(Value::Doc(user_doc)) = all_data.get("user") {
    if let Some(Value::Doc(profile_doc)) = user_doc.get("profile") {
        if let Some(Value::Text(name)) = profile_doc.get("name") {
            println!("Name: {}", name);  // ✅ "Alice"
        }
    }
}
```

## API Methods

### Basic Operations

- `set(key, value)` - Set a simple key-value pair
- `get(key)` - Get a value by key
- `get_as<T>(key)` - Get and deserialize a value
- `delete(key)` - Delete a key (creates tombstone)
- `get_all()` - Get all data as a Map

### Path Operations

- `set_path(path, value)` - Set a value at a nested path (creates intermediate maps)
- `get_path(path)` - Get a value from a nested path
- `get_path_as<T>(path)` - Get and deserialize from a path
- `delete_path(path)` - Delete a value at a path

### Path Mutation Operations

- `modify_path<F>(path, f)` - Modify existing value at path
- `get_or_insert_path<F>(path, default)` - Get or insert with default
- `modify_or_insert_path<F, G>(path, modify, default)` - Modify or insert

### Utility Operations

- `contains_key(key)` - Check if a key exists
- `contains_path(path)` - Check if a path exists

## Usage Examples

### Application Configuration

<!-- Code block ignored: Conceptual examples and internal API usage not suitable for testing -->

```rust,ignore
let op = database.new_transaction()?;
let config = op.get_subtree::<DocStore>("app_config")?;

// Set configuration values
config.set("app_name", "MyApp")?;
config.set_path("database.host", "localhost")?;
config.set_path("database.port", "5432")?;
config.set_path("features.auth.enabled", "true")?;

op.commit()?;
```

### Sync State Management

DocStore is used internally for sync state tracking in the sync module:

<!-- Code block ignored: Conceptual examples and internal API usage not suitable for testing -->

```rust,ignore
// Creating nested sync state structure
let sync_state = op.get_subtree::<DocStore>("sync_state")?;

// Store cursor information in nested structure
let cursor_path = format!("cursors.{}.{}", peer_pubkey, tree_id);
sync_state.set_path(cursor_path, cursor_json)?;

// Store metadata in nested structure
let metadata_path = format!("metadata.{}", peer_pubkey);
sync_state.set_path(metadata_path, metadata_json)?;

// Store history in nested structure
let history_path = format!("history.{}", sync_id);
sync_state.set_path(history_path, history_json)?;

// Later, retrieve all data and navigate the structure
let all_data = sync_state.get_all()?;

// Navigate to history entries
if let Some(Value::Doc(history_doc)) = all_data.get("history") {
    for (sync_id, entry_value) in history_doc.iter() {
        // Process each history entry
        if let Value::Text(json_str) = entry_value {
            let entry: SyncHistoryEntry = serde_json::from_str(json_str)?;
            // Use the entry...
        }
    }
}
```

## Common Pitfalls

### Expecting Flat Keys

The most common mistake is expecting `set_path("a.b.c", value)` to create a flat key `"a.b.c"` when it actually creates nested maps.

### Incorrect get_all() Usage

When using `get_all()`, remember that the returned Map contains the nested structure, not flat keys:

<!-- Code block ignored: Conceptual examples and internal API usage not suitable for testing -->

```rust,ignore
// After: docstore.set_path("config.server.port", "8080")

let all = docstore.get_all()?;

// Wrong:
all.get("config.server.port")  // Returns None

// Right:
all.get("config")
   .and_then(|v| v.as_node())
   .and_then(|n| n.get("server"))
   .and_then(|v| v.as_node())
   .and_then(|n| n.get("port"))  // Returns Some(Value::Text("8080"))
```

## Design Rationale

The nested structure approach was chosen because:

1. **Natural Hierarchy**: Represents hierarchical data more naturally
2. **Partial Updates**: Allows updating parts of a structure without rewriting everything
3. **CRDT Compatibility**: Works well with Doc CRDT merge semantics
4. **Query Flexibility**: Enables querying at any level of the hierarchy

## See Also

- [Doc CRDT](../crdt.md) - Underlying CRDT implementation
- [Sync State Management](../../sync/state.md) - Primary use case for DocStore
- [SubTree Trait](./stores.md) - Base trait for all store implementations
