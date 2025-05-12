# Developer Walkthrough: Building with EideticaDB

This guide provides a practical walkthrough for developers starting with EideticaDB, using the simple command-line [Todo Example](../../examples/todo/) to illustrate core concepts.

## Core Concepts

EideticaDB organizes data differently from traditional relational databases. Here's a breakdown of the key components you'll interact with, illustrated by the Todo example (`examples/todo/src/main.rs`).

### 1. The Database (`BaseDB`)

The `BaseDB` is your main entry point to interacting with an EideticaDB database instance. It manages the underlying storage (the "backend") and provides access to data structures called Trees.

In the Todo example, we initialize or load the database using an `InMemoryBackend`, which can be persisted to a file:

```rust
use eideticadb::backend::InMemoryBackend;
use eideticadb::basedb::BaseDB;
use std::path::PathBuf;
use anyhow::Result;

fn load_or_create_db(path: &PathBuf) -> Result<BaseDB> {
    if path.exists() {
        // Load existing DB from file
        let backend = InMemoryBackend::load_from_file(path)?;
        Ok(BaseDB::new(Box::new(backend)))
    } else {
        // Create a new in-memory backend
        let backend = InMemoryBackend::new();
        Ok(BaseDB::new(Box::new(backend)))
    }
}

fn save_db(db: &BaseDB, path: &PathBuf) -> Result<()> {
    let backend = db.backend();
    let backend_guard = backend.lock().unwrap();

    // Cast is needed to call backend-specific methods like save_to_file
    let in_memory_backend = backend_guard
        .as_any()
        .downcast_ref::<InMemoryBackend>()
        .ok_or(anyhow::anyhow!("Failed to downcast backend"))?; // Simplified error

    in_memory_backend.save_to_file(path)?;
    Ok(())
}

// Usage in main:
// let db = load_or_create_db(&cli.database_path)?;
// save_db(&db, &cli.database_path)?;
```

### 2. Trees (`Tree`)

A `Tree` is a primary organizational unit within a `BaseDB`. Think of it somewhat like a schema or a logical database within a larger instance. It acts as a container for related data, managed through `Subtrees`. Trees provide versioning and history tracking for the data they contain.

The Todo example uses a single Tree named "todo":

```rust
use eideticadb::basedb::BaseDB;
use eideticadb::Tree;
use anyhow::Result;

fn load_or_create_todo_tree(db: &BaseDB) -> Result<Tree> {
    let tree_name = "todo";

    // Attempt to find an existing tree by name using find_tree
    match db.find_tree(tree_name) {
        Ok(mut trees) => {
            // Found one or more trees with the name.
            // We arbitrarily take the first one found.
            // In a real app, you might want specific logic for duplicates.
            println!("Found existing todo tree.");
            Ok(trees.pop().unwrap()) // Safe unwrap as find_tree errors if empty
        }
        Err(eideticadb::Error::NotFound) => {
            // If not found, create a new one
            println!("No existing todo tree found, creating a new one...");
            let mut settings = eideticadb::data::KVOverWrite::new(); // Tree settings
            settings.set("name", tree_name);
            let tree = db.new_tree(settings)?;

            // No initial commit needed here as subtrees like RowStore handle
            // their creation upon first access within an operation.

            Ok(tree)
        }
        Err(e) => {
            // Handle other potential errors from find_tree
            Err(e.into())
        }
    }
}

// Usage in main:
// let todo_tree = load_or_create_todo_tree(&db)?;
```

### 3. Operations (`Operation`)

All modifications to a `Tree`'s data happen within an `Operation`. Operations ensure atomicity – similar to transactions in traditional databases. Changes made within an operation are only applied to the Tree when the operation is successfully committed.

```rust
use eideticadb::Tree;
use anyhow::Result;

fn some_data_modification(tree: &Tree) -> Result<()> {
    // Start an atomic operation
    let op = tree.new_operation()?;

    // ... perform data changes using the 'op' handle ...

    // Commit the changes atomically
    op.commit()?;

    Ok(())
}
```

Read-only access also typically uses an `Operation` to ensure a consistent view of the data at a specific point in time.

### 4. Subtrees (`Subtree`)

`Subtrees` are the heart of data storage within a `Tree`. Unlike rigid tables in SQL databases, Subtrees are highly flexible containers.

- **Analogy:** You can think of a Subtree _loosely_ like a table or a collection within a Tree.
- **Flexibility:** Subtrees aren't tied to a single data type or structure. They are generic containers identified by a name (e.g., "todos").
- **Implementations:** EideticaDB provides several `Subtree` implementations for common data patterns. The Todo example uses `RowStore<T>`, which is specialized for storing collections of structured data (like rows) where each item has a unique ID. Other implementations might exist for key-value pairs, lists, etc.
- **Extensibility:** You can implement your own `Subtree` types to model complex or domain-specific data structures.

The Todo example uses a `RowStore` to store `Todo` structs:

```rust
use eideticadb::{Tree, Error};
use eideticadb::subtree::RowStore;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use anyhow::{anyhow, Result};

// Define the data structure (must be Serializable + Deserializable)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub title: String,
    pub completed: bool,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
// ... impl Todo ...

fn add_todo(tree: &Tree, title: String) -> Result<()> {
    let op = tree.new_operation()?;
    // Get a handle to the 'todos' subtree, specifying its type is RowStore<Todo>
    let todos_store = op.get_subtree::<RowStore<Todo>>("todos")?;
    let todo = Todo::new(title);
    // Insert the data - RowStore assigns an ID
    let todo_id = todos_store.insert(todo)?;
    op.commit()?;
    println!("Added todo with ID: {}", todo_id);
    Ok(())
}

fn complete_todo(tree: &Tree, id: &str) -> Result<()> {
    let op = tree.new_operation()?;
    let todos_store = op.get_subtree::<RowStore<Todo>>("todos")?;
    // Get data by ID
    let mut todo = todos_store.get(id).map_err(|e| anyhow!("Get failed: {}", e))?;
    todo.complete();
    // Update data by ID
    todos_store.set(id, todo)?;
    op.commit()?;
    Ok(())
}

fn list_todos(tree: &Tree) -> Result<()> {
    let op = tree.new_operation()?; // Read operation
    let todos_store = op.get_subtree::<RowStore<Todo>>("todos")?;
    // Search/scan the subtree
    let todos_with_ids = todos_store.search(|_| true)?; // Get all
    // ... print todos ...
    Ok(())
}
```

### 5. Data Modeling (`Serialize`, `Deserialize`)

EideticaDB leverages the `serde` framework for data serialization. Any data structure you want to store needs to implement `serde::Serialize` and `serde::Deserialize`. This allows you to store complex Rust types directly.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)] // Serde traits
pub struct Todo {
    pub title: String,
    pub completed: bool,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
```

## Running the Todo Example

To see these concepts in action, you can run the Todo example:

```bash
# Navigate to the example directory
cd examples/todo

# Build the example
cargo build

# Run commands (this will create todo_db.json)
cargo run -- add "Learn EideticaDB"
cargo run -- list
# Note the ID printed
cargo run -- complete <id_from_list>
cargo run -- list
```

Refer to the example's [README.md](../../examples/todo/README.md) and [test.sh](../../examples/todo/test.sh) for more usage details.

This walkthrough provides a starting point. Explore the EideticaDB documentation and other examples to learn about more advanced features like different subtree types, history traversal, and distributed capabilities.
