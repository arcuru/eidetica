# Backends

Backends in EideticaDB define how and where data is physically stored.

## The Backend Abstraction

The Backend trait abstracts the underlying storage mechanism for EideticaDB entries. This separation of concerns allows the core database logic to remain independent of the specific storage details.

Key responsibilities of a Backend:

- Storing and retrieving entries by their unique IDs
- Tracking relationships between entries
- Calculating tips (latest entries) for trees and subtrees
- Managing the graph-like structure of entry history

## Available Backends

### InMemoryBackend

The `InMemoryBackend` is currently the primary backend implementation:

- Stores all entries in memory
- Can load from and save to a JSON file
- Well-suited for development, testing, and applications with moderate data volumes
- Simple to use and requires no external dependencies

Example usage:

```rust
// Create a new in-memory backend
let backend = InMemoryBackend::new();
let db = BaseDB::new(Box::new(backend));

// ... use the database ...

// Save to a file (optional)
let path = PathBuf::from("my_database.json");
let backend_guard = db.backend().lock().unwrap();
if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemoryBackend>() {
    in_memory.save_to_file(&path)?;
}

// Load from a file
let backend = InMemoryBackend::load_from_file(&path)?;
let db = BaseDB::new(Box::new(backend));
```

**Note:** The `InMemoryBackend` is the only backend implementation currently provided with EideticaDB.

<!-- TODO: Document other backend implementations when available (e.g., persistent storage, distributed backends) -->

## Backend Trait Responsibilities

The `Backend` trait (`eideticadb::backend::Backend`) defines the core interface required for storage. Beyond simple `get` and `put` for entries, it includes methods crucial for navigating the database's history and structure:

- `get_tips(tree_id)`: Finds the latest entries in a specific `Tree`.
- `get_subtree_tips(tree_id, subtree_name)`: Finds the latest entries _for a specific `Subtree`_ within a `Tree`.
- `all_roots()`: Finds all top-level `Tree` roots stored in the backend.
- `get_tree(tree_id)` / `get_subtree(...)`: Retrieve all entries for a tree/subtree, typically sorted topologically (required for some history operations, potentially expensive).

Implementing these methods efficiently often requires the backend to understand the DAG structure, making the backend more than just a simple key-value store.

## Backend Performance Considerations

The Backend implementation significantly impacts database performance:

- **Entry Retrieval**: How quickly entries can be accessed by ID
- **Graph Traversal**: Efficiency of history traversal and tip calculation
- **Memory Usage**: How entries are stored and whether they're kept in memory
- **Concurrency**: How concurrent operations are handled
