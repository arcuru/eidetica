# Subtrees

Subtrees provide structured, type-safe access to different kinds of data within a Tree.

## The Subtree Concept

In Eidetica, Subtrees extend the Merkle-CRDT concept by explicitly partitioning data within each Entry. A Subtree:

- Represents a specific type of data structure (like a key-value store or a collection of records)
- Has a unique name within its parent Tree
- Maintains its own history tracking
- Is strongly typed (via Rust generics)

Subtrees are what make Eidetica practical for real applications, as they provide high-level, data-structure-aware interfaces on top of the core Entry and Tree concepts.

## Why Subtrees?

Subtrees offer several advantages:

- **Type Safety**: Each subtree implementation provides appropriate methods for its data type
- **Isolation**: Changes to different subtrees can be tracked separately
- **Composition**: Multiple data structures can exist within a single Tree
- **Efficiency**: Only relevant subtrees need to be loaded or synchronized
- **Atomic Operations**: Changes across multiple subtrees can be committed atomically

## Available Subtree Types

### Dict (Key-Value Store)

The `Dict` subtree implements a flexible key-value store that supports both simple string values and nested hierarchical data structures. It uses the `Map` CRDT implementation internally, which includes support for tombstones to properly track deletions across distributed systems.

#### Basic Usage

```rust
// Get a Dict subtree
let op = tree.new_operation()?;
let config = op.get_subtree::<Dict>("config")?;

// Set simple string values
config.set("api_url", "https://api.example.com")?;
config.set("max_connections", "100")?;

// Get values
let url = config.get("api_url")?; // Returns a Value
let url_string = config.get_string("api_url")?; // Returns a String directly

// Remove values
config.delete("temporary_setting")?; // Creates a tombstone
// Even if temporary_setting doesn't exist, it will be marked as deleted
// This ensures the deletion propagates during synchronization

op.commit()?;
```

#### Working with Nested Structures

`Dict` can handle nested map structures, allowing you to build hierarchical data:

```rust
// Create nested structures
let mut preferences = Map::new();
preferences.set_string("theme", "dark");
preferences.set_string("language", "en");

// Set this map as a value in the Dict
config.set_value("user_prefs", Value::Map(preferences))?;

// Later retrieve and modify the nested data
if let Value::Map(mut prefs) = config.get("user_prefs")? {
    // Modify the map
    prefs.set_string("theme", "light");

    // Update the value in the store
    config.set_value("user_prefs", Value::Map(prefs))?;
}
```

#### Using ValueEditor for Fluent API

The `ValueEditor` provides a more ergonomic way to work with nested data structures in `Dict`. It allows you to navigate and modify nested values without having to manually extract and reinsert the intermediate maps:

```rust
// Get an editor for a specific key
let prefs_editor = config.get_value_mut("user_prefs");

// Read nested values
match prefs_editor.get_value("theme")? {
    Value::String(theme) => println!("Current theme: {}", theme),
    _ => println!("Theme not found or not a string"),
}

// Set nested values directly
prefs_editor
    .get_value_mut("theme")
    .set(Value::String("light".to_string()))?;

// Navigate deep structures with method chaining
config
    .get_value_mut("user")
    .get_value_mut("profile")
    .get_value_mut("display_name")
    .set(Value::String("Alice Smith".to_string()))?;

// Even if intermediate paths don't exist yet, they'll be created automatically
// The line above will work even if "user", "profile", or "display_name" don't exist

// Delete operations
prefs_editor.delete_child("deprecated_setting")?; // Delete a child key
prefs_editor.delete_self()?; // Delete the entire user_prefs object

// Working with the root of the subtree
let root_editor = config.get_root_mut();
match root_editor.get()? {
    Value::Map(root_map) => {
        // Access all top-level keys
        for (key, value) in root_map.as_hashmap() {
            println!("Key: {}, Value type: {}", key, value.type_name());
        }
    },
    _ => unreachable!("Root should always be a map"),
}

// Don't forget to commit changes!
op.commit()?;
```

#### Path-Based Operations

`Dict` also provides direct path-based access, which the `ValueEditor` uses internally:

```rust
// Set a value using a path array
config.set_at_path(
    &["user".to_string(), "settings".to_string(), "notifications".to_string()],
    Value::String("enabled".to_string())
)?;

// Get a value using a path array
let notification_setting = config.get_at_path(
    &["user".to_string(), "settings".to_string(), "notifications".to_string()]
)?;
```

Use cases for `Dict`:

- Configuration settings
- User preferences
- Hierarchical metadata
- Structured document storage
- Application state

### Table

The `Table<T>` subtree manages collections of serializable items, similar to a table in a database:

```rust
// Define a struct for your data
#[derive(Serialize, Deserialize, Clone)]
struct User {
    name: String,
    email: String,
    active: bool,
}

// Get a Table subtree
let op = tree.new_operation()?;
let users = op.get_subtree::<Table<User>>("users")?;

// Insert items (returns a generated ID)
let user = User {
    name: "Alice".to_string(),
    email: "alice@example.com".to_string(),
    active: true,
};
let id = users.insert(user)?;

// Get an item by ID
if let Ok(user) = users.get(&id) {
    println!("Found user: {}", user.name);
}

// Update an item
if let Ok(mut user) = users.get(&id) {
    user.active = false;
    users.set(&id, user)?;
}

// Remove an item
users.remove(&id)?;

// Iterate over all items
for result in users.iter()? {
    if let Ok((id, user)) = result {
        println!("ID: {}, Name: {}", id, user.name);
    }
}

op.commit()?;
```

Use cases for `Table`:

- Collections of structured objects
- Record storage (users, products, todos, etc.)
- Any data where individual items need unique IDs

## Subtree Implementation Details

Each Subtree implementation in Eidetica:

1. Implements the `SubTree` trait
2. Provides methods appropriate for its data structure
3. Handles serialization/deserialization of data
4. Manages the subtree's history within the Tree

The `SubTree` trait defines the minimal interface:

```rust
pub trait SubTree: Sized {
    fn new(op: &AtomicOp, subtree_name: &str) -> Result<Self>;
    fn name(&self) -> &str;
}
```

Subtree implementations add their own methods on top of this minimal interface.

## Subtree History and Merging (CRDT Aspects)

While Eidetica uses Merkle-DAGs for overall history, the way data _within_ a Subtree is combined when branches merge relies on Conflict-free Replicated Data Type (CRDT) principles. This ensures that even if different replicas of the database have diverged and made concurrent changes, they can be merged back together automatically without conflicts (though the merge _result_ depends on the CRDT strategy).

Each Subtree type implements its own merge logic, typically triggered implicitly when an `Operation` reads the current state of the subtree (which involves finding and merging the tips of that subtree's history):

- **`Dict`**: Implements a **Last-Writer-Wins (LWW)** strategy using `Map`. When merging concurrent writes to the _same key_, the write associated with the later `Entry` "wins", and its value is kept. Writes to different keys are simply combined. Deleted keys (via `remove()`) are tracked with tombstones to ensure deletions propagate properly.

- **`Table<T>`**: Also uses **LWW for updates to the _same row ID_**. If two concurrent operations modify the same row, the later write wins. Inserts of _different_ rows are combined (all inserted rows are kept). Deletions generally take precedence over concurrent updates (though precise semantics might evolve).

**Note:** The CRDT merge logic happens internally when an `Operation` loads the initial state of a Subtree or when a `SubtreeViewer` is created. You typically don't invoke merge logic directly.

<!-- TODO: Add links to specific CRDT literature or more detailed internal docs on merge logic if needed -->

## Future Subtree Types

Eidetica's architecture allows for adding new Subtree implementations. Potential future types include:

- **ObjectStore**: For storing large binary blobs.

These are **not yet implemented**. Development is currently focused on the core API and the existing `Dict` and `Table` types.

<!-- TODO: Update this list if/when new subtree types become available or development starts -->
