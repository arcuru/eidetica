# User Guide

Welcome to the EideticaDB User Guide. This guide will help you understand and use EideticaDB effectively in your applications.

## What is EideticaDB?

EideticaDB is a Rust library for managing structured data with built-in history tracking. It combines concepts from distributed systems, Merkle-CRDTs, and traditional databases to provide a unique approach to data management:

- **Efficient data storage** with customizable [Backends](concepts/backends.md)
- **History tracking** for all changes via immutable [Entries](concepts/entries_trees.md) forming a DAG
- **Structured data types** via named, typed [Subtrees](concepts/subtrees.md) within logical [Trees](concepts/entries_trees.md)
- **Atomic changes** across multiple data structures using [Operations](operations.md)
- **Designed for distribution** (future capability)

## How to Use This Guide

This user guide is structured to guide you from basic setup to advanced concepts:

1.  [**Getting Started**](getting_started.md): Installation, basic setup, and your first steps.
2.  [**Basic Usage Pattern**](#basic-usage-pattern): A quick look at the typical workflow.
3.  [**Core Concepts**](core_concepts.md): Understand the fundamental building blocks:
    - [Entries & Trees](concepts/entries_trees.md): The core DAG structure.
    - [Backends](concepts/backends.md): How data is stored.
    - [Subtrees](concepts/subtrees.md): Where structured data lives (`KVStore`, `RowStore`).
    - [Operations](operations.md): How atomic changes are made.
4.  [**Tutorial: Todo App**](tutorial_todo_app.md): A step-by-step walkthrough using a simple application.
5.  [**Code Examples**](examples_snippets.md): Focused code snippets for common tasks.

## Quick Overview: The Core Flow

EideticaDB revolves around a few key components working together:

1.  **`Backend`**: You start by choosing or creating a storage `Backend` (e.g., `InMemoryBackend`).
2.  **`BaseDB`**: You create a `BaseDB` instance, providing it the `Backend`. This is your main database handle.
3.  **`Tree`**: Using the `BaseDB`, you create or load a `Tree`, which acts as a logical container for related data and tracks its history.
4.  **`Operation`**: To **read or write** data, you start an `Operation` from the `Tree`. This ensures atomicity and consistent views.
5.  **`Subtree`**: Within an `Operation`, you get handles to named `Subtree`s (like `KVStore` or `RowStore<YourData>`). These provide methods (`set`, `get`, `insert`, `remove`, etc.) to interact with your structured data.
6.  **`Commit`**: Changes made via `Subtree` handles within the `Operation` are staged. Calling `commit()` on the `Operation` finalizes these changes atomically, creating a new historical `Entry` in the `Tree`.

## Basic Usage Pattern (Conceptual Code)

```rust
use eideticadb::{BaseDB, Tree, Error};
use eideticadb::backend::InMemoryBackend;
use eideticadb::subtree::{KVStore, RowStore};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Clone)]
struct MyData { /* fields */ }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create Backend
    let backend = InMemoryBackend::new();
    // 2. Create BaseDB
    let db = BaseDB::new(Box::new(backend));

    // 3. Create/Load Tree (e.g., named "my_tree")
    let tree = match db.find_tree("my_tree") {
        Ok(mut trees) => trees.pop().unwrap(), // Found existing
        Err(Error::NotFound) => {
            let mut settings = eideticadb::data::KVOverWrite::new();
            settings.set("name".to_string(), "my_tree".to_string());
            db.new_tree(settings)? // Create new
        }
        Err(e) => return Err(e.into()),
    };

    // --- Writing Data ---
    // 4. Start an Operation
    let op_write = tree.new_operation()?;
    { // Scope for subtree handles
        // 5. Get Subtree handles
        let config = op_write.get_subtree::<KVStore>("config")?;
        let items = op_write.get_subtree::<RowStore<MyData>>("items")?;

        // 6. Use Subtree methods
        config.set("version", "1.0")?;
        items.insert(MyData { /* ... */ })?;
    } // Handles drop, changes are staged in op_write
    // 7. Commit changes
    let new_entry_id = op_write.commit()?;
    println!("Committed changes, new entry ID: {}", new_entry_id);

    // --- Reading Data ---
    // Use Tree::get_subtree_viewer for reads outside an Operation
    let items_viewer = tree.get_subtree_viewer::<RowStore<MyData>>("items")?;
    if let Some(item) = items_viewer.get(&some_id)? {
       println!("Read item: {:?}", item);
    }

    Ok(())
}
```

See [Operations](operations.md) and [Code Examples](examples_snippets.md) for more details.

## Project Status

EideticaDB is currently under active development. The core functionality is working, but APIs are considered **experimental** and may change in future releases. It is suitable for evaluation and prototyping, but not yet recommended for production systems requiring long-term API stability.

<!-- TODO: Add links to versioning policy or release notes once available -->
