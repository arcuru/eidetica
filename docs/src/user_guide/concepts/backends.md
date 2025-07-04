# Database Storage

Database storage implementations in Eidetica define how and where data is physically stored.

## The Database Abstraction

The Database trait abstracts the underlying storage mechanism for Eidetica entries. This separation of concerns allows the core database logic to remain independent of the specific storage details.

Key responsibilities of a Database:

- Storing and retrieving entries by their unique IDs
- Tracking relationships between entries
- Calculating tips (latest entries) for trees and subtrees
- Managing the graph-like structure of entry history

## Available Database Implementations

### InMemory

The `InMemory` database is currently the primary storage implementation:

- Stores all entries in memory
- Can load from and save to a JSON file
- Well-suited for development, testing, and applications with moderate data volumes
- Simple to use and requires no external dependencies

Example usage:

```rust
// Create a new in-memory database
use eidetica::backend::database::InMemory;
let database = InMemory::new();
let db = BaseDB::new(Box::new(database));

// ... use the database ...

// Save to a file (optional)
let path = PathBuf::from("my_database.json");
let database_guard = db.backend().lock().unwrap();
if let Some(in_memory) = database_guard.as_any().downcast_ref::<InMemory>() {
    in_memory.save_to_file(&path)?;
}

// Load from a file
let database = InMemory::load_from_file(&path)?;
let db = BaseDB::new(Box::new(database));
```

**Note:** The `InMemory` database is the only storage implementation currently provided with Eidetica.

<!-- TODO: Document other database implementations when available (e.g., persistent storage, distributed databases) -->

## Database Trait Responsibilities

The `Database` trait (`eidetica::backend::Database`) defines the core interface required for storage. Beyond simple `get` and `put` for entries, it includes methods crucial for navigating the database's history and structure:

- `get_tips(tree_id)`: Finds the latest entries in a specific `Tree`.
- `get_subtree_tips(tree_id, subtree_name)`: Finds the latest entries _for a specific `Subtree`_ within a `Tree`.
- `all_roots()`: Finds all top-level `Tree` roots stored in the database.
- `get_tree(tree_id)` / `get_subtree(...)`: Retrieve all entries for a tree/subtree, typically sorted topologically (required for some history operations, potentially expensive).

Implementing these methods efficiently often requires the database to understand the DAG structure, making the database more than just a simple key-value store.

## Database Performance Considerations

The Database implementation significantly impacts database performance:

- **Entry Retrieval**: How quickly entries can be accessed by ID
- **Graph Traversal**: Efficiency of history traversal and tip calculation
- **Memory Usage**: How entries are stored and whether they're kept in memory
- **Concurrency**: How concurrent operations are handled
