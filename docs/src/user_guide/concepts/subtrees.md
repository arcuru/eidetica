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

Eidetica provides three main subtree types, each optimized for different data patterns:

| Type          | Purpose               | Key Features                              | Best For                                     |
| ------------- | --------------------- | ----------------------------------------- | -------------------------------------------- |
| **DocStore**  | Document storage      | Path-based operations, nested structures  | Configuration, metadata, structured docs     |
| **Table\<T>** | Record collections    | Auto-generated UUIDs, type safety, search | User lists, products, any structured records |
| **YDoc**      | Collaborative editing | Y-CRDT integration, real-time sync        | Shared documents, collaborative text editing |

### DocStore (Document-Oriented Storage)

The `DocStore` subtree provides a document-oriented interface for storing and retrieving structured data. It wraps the `crdt::Doc` type to provide ergonomic access patterns with both simple key-value operations and path-based operations for nested data structures.

#### Basic Usage

```rust
// Get a DocStore subtree
let op = tree.new_operation()?;
let store = op.get_subtree::<DocStore>("app_data")?;

// Set simple values
store.set("version", "1.0.0")?;
store.set("author", "Alice")?;

// Path-based operations for nested structures
// This creates nested maps: {"database": {"host": "localhost", "port": "5432"}}
store.set_path("database.host", "localhost")?;
store.set_path("database.port", "5432")?;

// Retrieve values
let version = store.get("version")?; // Returns a Value
let host = store.get_path("database.host")?; // Navigate nested structure

op.commit()?;
```

#### Important: Path Operations Create Nested Structures

When using `set_path("a.b.c", value)`, DocStore creates **nested maps**, not flat keys with dots:

```rust
// This code:
store.set_path("user.profile.name", "Bob")?;

// Creates this structure:
// {
//   "user": {
//     "profile": {
//       "name": "Bob"
//     }
//   }
// }

// NOT: { "user.profile.name": "Bob" } ❌
```

Use cases for `DocStore`:

- Application configuration
- Metadata storage
- Structured documents
- Settings management
- Any data requiring path-based access

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

// Insert items (returns a generated UUID)
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

// Search for items matching a condition
let active_users = users.search(|user| user.active)?;
for (id, user) in active_users {
    println!("Active user: {} (ID: {})", user.name, id);
}

op.commit()?;
```

Use cases for `Table`:

- Collections of structured objects
- Record storage (users, products, todos, etc.)
- Any data where individual items need unique IDs
- When you need to search across records with custom predicates

### YDoc (Y-CRDT Integration)

The `YDoc` subtree provides integration with Y-CRDT (Yjs) for real-time collaborative editing. This requires the "y-crdt" feature:

```rust
// Enable in Cargo.toml: eidetica = { features = ["y-crdt"] }
use eidetica::subtree::YDoc;
use eidetica::y_crdt::{Map, Text, Transact};

// Get a YDoc subtree
let op = tree.new_operation()?;
let doc_store = op.get_subtree::<YDoc>("document")?;

// Work with Y-CRDT structures
doc_store.with_doc_mut(|doc| {
    let text = doc.get_or_insert_text("content");
    let metadata = doc.get_or_insert_map("meta");

    let mut txn = doc.transact_mut();

    // Collaborative text editing
    text.insert(&mut txn, 0, "Hello, collaborative world!");

    // Set metadata
    metadata.insert(&mut txn, "title", "My Document");
    metadata.insert(&mut txn, "author", "Alice");

    Ok(())
})?;

// Apply updates from other collaborators
let external_update = receive_update_from_network();
doc_store.apply_update(&external_update)?;

// Get updates to send to others
let update = doc_store.get_update()?;
broadcast_update(update);

op.commit()?;
```

Use cases for `YDoc`:

- Real-time collaborative text editing
- Shared documents with multiple editors
- Conflict-free data synchronization
- Applications requiring sophisticated merge algorithms

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

- **`DocStore`**: Implements a **Last-Writer-Wins (LWW)** strategy using the internal `Doc` type. When merging concurrent writes to the _same key_ or path, the write associated with the later `Entry` "wins", and its value is kept. Writes to different keys are simply combined. Deleted keys (via `delete()`) are tracked with tombstones to ensure deletions propagate properly.

- **`Table<T>`**: Also uses **LWW for updates to the _same row ID_**. If two concurrent operations modify the same row, the later write wins. Inserts of _different_ rows are combined (all inserted rows are kept). Deletions generally take precedence over concurrent updates (though precise semantics might evolve).

**Note:** The CRDT merge logic happens internally when an `Operation` loads the initial state of a Subtree or when a `SubtreeViewer` is created. You typically don't invoke merge logic directly.

<!-- TODO: Add links to specific CRDT literature or more detailed internal docs on merge logic if needed -->

## Future Subtree Types

Eidetica's architecture allows for adding new Subtree implementations. Potential future types include:

- **ObjectStore**: For storing large binary blobs.

These are **not yet implemented**. Development is currently focused on the core API and the existing `DocStore` and `Table` types.

<!-- TODO: Update this list if/when new subtree types become available or development starts -->
