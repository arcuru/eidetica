### Backend

The Backend trait abstracts the underlying storage system, allowing for different storage implementations without changing the core database logic.

```mermaid
classDiagram
    class Backend {
        <<interface>>
        +get(id: &ID) Result<&Entry>
        +put(&mut self, entry: Entry) Result<()>
        +get_tips(tree: &ID) Result<Vec<ID>>
        +get_subtree_tips(tree: &ID, subtree: &str) Result<Vec<ID>>
        +all_roots() Result<Vec<ID>>
        +get_tree(tree: &ID) Result<Vec<Entry>>
        +get_subtree(tree: &ID, subtree: &str) Result<Vec<Entry>>
        +as_any() &dyn Any
    }

    class InMemoryBackend {
        -HashMap<ID, Entry> entries
        +new() InMemoryBackend
        +save_to_file(path: P) Result<()>
        +load_from_file(path: P) Result<Self>
        +all_ids() Vec<ID>
        +get_entry(id: &ID) Result<&Entry>
        # Note: Implements all Backend trait methods
        # Additional internal methods for tip/height calculation exist
    }

    Backend <|.. InMemoryBackend : implements
```

Currently, the only implementation is an `InMemoryBackend` which stores entries in a `HashMap`. It includes functionality to save/load its state to/from a JSON file using `serde_json`.

**`InMemoryBackend` Persistence Format:**

- The `save_to_file` method serializes the entire `InMemoryBackend` struct (which contains the `entries: HashMap<ID, Entry>`) to a JSON string.
- The `load_from_file` method reads this JSON string and deserializes it back into an `InMemoryBackend`.
- The format is effectively a JSON object where keys are entry IDs (strings) and values are the JSON representations of the corresponding `Entry` structs.

<!-- TODO: Add a section on how to implement a custom Backend. -->

### Implementing a Custom Backend

To create a custom storage backend:

1.  Define a struct that will hold the backend's state (e.g., connection pools, file handles).
2.  Implement the [`Backend`](../../src/backend/mod.rs) trait for your struct. This requires providing logic for all methods (`get`, `put`, `get_tips`, etc.) specific to your chosen storage.
3.  Ensure your struct implements `Send`, `Sync`, and `Any`.
4.  Consider performance implications, especially for graph traversal operations like `get_tips` and the topological sorting required by `get_tree`/`get_subtree`.
5.  Use your custom backend when creating a `BaseDB` instance: `BaseDB::new(Box::new(MyCustomBackend::new(...)))`.

Key Backend features include:

- **Tip Calculation**: Determines which entries are "tips" (have no children) in a tree or subtree
- **Height Calculation**: Computes topological heights for proper ordering of entries
- **Topological Sorting**: Orders entries based on their position in the DAG for consistent retrieval
