# User Guide

Welcome to the Eidetica User Guide. This guide will help you understand and use Eidetica effectively in your applications.

## What is Eidetica?

Eidetica is a Rust library for managing structured data with built-in history tracking. It combines concepts from distributed systems, Merkle-CRDTs, and traditional databases to provide a unique approach to data management:

- **Efficient data storage** with customizable [Databases](concepts/backends.md)
- **History tracking** for all changes via immutable [Entries](concepts/entries_trees.md) forming a DAG
- **Structured data types** via named, typed [Stores](concepts/stores.md) within logical [Databases](concepts/entries_trees.md)
- **Atomic changes** across multiple data structures using [Operations](operations.md)
- **Designed for distribution** (future capability)

## How to Use This Guide

This user guide is structured to guide you from basic setup to advanced concepts:

1.  [**Getting Started**](getting_started.md): Installation, basic setup, and your first steps.
2.  [**Basic Usage Pattern**](#basic-usage-pattern): A quick look at the typical workflow.
3.  [**Core Concepts**](core_concepts.md): Understand the fundamental building blocks:
    - [Entries & Databases](concepts/entries_trees.md): The core DAG structure.
    - [Databases](concepts/backends.md): How data is stored.
    - [Stores](concepts/stores.md): Where structured data lives (`DocStore`, `Table`, `YDoc`).
    - [Operations](operations.md): How atomic changes are made.
4.  [**Tutorial: Todo App**](tutorial_todo_app.md): A step-by-step walkthrough using a simple application.
5.  [**Code Examples**](examples_snippets.md): Focused code snippets for common tasks.

## Quick Overview: The Core Flow

Eidetica revolves around a few key components working together:

1.  **`Database`**: You start by choosing or creating a storage `Database` (e.g., `InMemoryDatabase`).
2.  **`Instance`**: You create a `Instance` instance, providing it the `Database`. This is your main database handle.
3.  **`Database`**: Using the `Instance`, you create or load a `Database`, which acts as a logical container for related data and tracks its history.
4.  **`Operation`**: To **read or write** data, you start an `Operation` from the `Database`. This ensures atomicity and consistent views.
5.  **`Store`**: Within an `Operation`, you get handles to named `Store`s (like `DocStore` or `Table<YourData>`). These provide methods (`set`, `get`, `insert`, `remove`, etc.) to interact with your structured data.
6.  **`Commit`**: Changes made via `Store` handles within the `Operation` are staged. Calling `commit()` on the `Operation` finalizes these changes atomically, creating a new historical `Entry` in the `Database`.

## Basic Usage Pattern (Conceptual Code)

```rust,ignore
use eidetica::{Instance, Database, Error};
use eidetica::backend::database::InMemory;
use eidetica::store::{DocStore, Table};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Clone)]
struct MyData { /* fields */ }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create Database
    let database = InMemory::new();
    // 2. Create Instance
    let db = Instance::new(Box::new(database));

    // Add authentication key (required for all operations)
    db.add_private_key("my_key")?;

    // 3. Create/Load Database (e.g., named "my_tree")
    let database = match db.find_tree("my_tree") {
        Ok(mut databases) => databases.pop().unwrap(), // Found existing
        Err(e) if e.is_not_found() => {
            let mut doc = eidetica::crdt::Doc::new();
            doc.set("name", "my_tree");
            db.new_tree(doc, "my_key")? // Create new with auth
        }
        Err(e) => return Err(e.into()),
    };

    // --- Writing Data ---
    // 4. Start an Operation
    let op_write = database.new_operation()?;
    { // Scope for store handles
        // 5. Get Store handles
        let config = op_write.get_subtree::<DocStore>("config")?;
        let items = op_write.get_subtree::<Table<MyData>>("items")?;

        // 6. Use Store methods
        config.set("version", "1.0")?;
        items.insert(MyData { /* ... */ })?;
    } // Handles drop, changes are staged in op_write
    // 7. Commit changes
    let new_entry_id = op_write.commit()?;
    println!("Committed changes, new entry ID: {}", new_entry_id);

    // --- Reading Data ---
    // Use Database::get_subtree_viewer for reads outside an Operation
    let items_viewer = database.get_subtree_viewer::<Table<MyData>>("items")?;
    if let Some(item) = items_viewer.get(&some_id)? {
       println!("Read item: {:?}", item);
    }

    Ok(())
}
```

See [Operations](operations.md) and [Code Examples](examples_snippets.md) for more details.

## Project Status

Eidetica is currently under active development. The core functionality is working, but APIs are considered **experimental** and may change in future releases. It is suitable for evaluation and prototyping, but not yet recommended for production systems requiring long-term API stability.

<!-- TODO: Add links to versioning policy or release notes once available -->
