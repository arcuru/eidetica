# User Guide

Welcome to the Eidetica User Guide. This guide will help you understand and use Eidetica effectively in your applications.

## What is Eidetica?

Eidetica is a Rust library for managing structured data with built-in history tracking. It combines concepts from distributed systems, Merkle-CRDTs, and traditional databases to provide a unique approach to data management:

- **Efficient data storage** with customizable [Databases](concepts/backends.md)
- **History tracking** for all changes via immutable [Entries](concepts/entries_databases.md) forming a DAG
- **Structured data types** via named, typed [Stores](concepts/stores.md) within logical [Databases](concepts/entries_databases.md)
- **Atomic changes** across multiple data structures using [Transactions](transactions.md)
- **Designed for distribution** (future capability)

## How to Use This Guide

This user guide is structured to guide you from basic setup to advanced concepts:

1.  [**Getting Started**](getting_started.md): Installation, basic setup, and your first steps.
2.  [**Basic Usage Pattern**](#basic-usage-pattern): A quick look at the typical workflow.
3.  [**Core Concepts**](core_concepts.md): Understand the fundamental building blocks:
    - [Entries & Databases](concepts/entries_databases.md): The core DAG structure.
    - [Databases](concepts/backends.md): How data is stored.
    - [Stores](concepts/stores.md): Where structured data lives (`DocStore`, `Table`, `YDoc`).
    - [Transactions](transactions.md): How atomic changes are made.
4.  [**Tutorial: Todo App**](tutorial_todo_app.md): A step-by-step walkthrough using a simple application.
5.  [**Code Examples**](examples_snippets.md): Focused code snippets for common tasks.

## Quick Overview: The Core Flow

Eidetica revolves around a few key components working together:

1.  **`Database`**: You start by choosing or creating a storage `Database` (e.g., `InMemoryDatabase`).
2.  **`Instance`**: You create a `Instance` instance, providing it the `Database`. This is your main database handle.
3.  **`Database`**: Using the `Instance`, you create or load a `Database`, which acts as a logical container for related data and tracks its history.
4.  **`Transaction`**: To **read or write** data, you start a `Transaction` from the `Database`. This ensures atomicity and consistent views.
5.  **`Store`**: Within a `Transaction`, you get handles to named `Store`s (like `DocStore` or `Table<YourData>`). These provide methods (`set`, `get`, `insert`, `remove`, etc.) to interact with your structured data.
6.  **`Commit`**: Changes made via `Store` handles within the `Transaction` are staged. Calling `commit()` on the `Transaction` finalizes these changes atomically, creating a new historical `Entry` in the `Database`.

## Basic Usage Pattern

Here's a quick example showing creating a user, database, and writing new data.

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::{DocStore, Table}};
# use serde::{Serialize, Deserialize};
#
# #[derive(Serialize, Deserialize, Clone, Debug)]
# struct MyData {
#     name: String,
# }
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let backend = InMemory::new();
let instance = Instance::open(Box::new(backend)).await?;

// Create and login a passwordless user
instance.create_user("alice", None).await?;
let mut user = instance.login_user("alice", None).await?;

// Create a database
let mut settings = Doc::new();
settings.set("name", "my_database");
let default_key = user.get_default_key()?;
let database = user.create_database(settings, &default_key).await?;

// --- Writing Data ---
// Start a Transaction
let txn = database.new_transaction().await?;
let inserted_id = { // Scope for store handles
    // Get Store handles
    let config = txn.get_store::<DocStore>("config").await?;
    let items = txn.get_store::<Table<MyData>>("items").await?;

    // Use Store methods
    config.set("version", "1.0").await?;
    items.insert(MyData { name: "example".to_string() }).await?
}; // Handles drop, changes are staged in txn
// Commit changes
let new_entry_id = txn.commit().await?;
println!("Committed changes, new entry ID: {}", new_entry_id);

// --- Reading Data ---
// Use Database::get_store_viewer for a read-only view
let items_viewer = database.get_store_viewer::<Table<MyData>>("items").await?;
if let Ok(item) = items_viewer.get(&inserted_id).await {
   println!("Read item: {:?}", item);
}
# Ok(())
# }
```

See [Transactions](transactions.md) and [Code Examples](examples_snippets.md) for more details.

## Project Status

Eidetica is currently under active development. The core functionality is working, but APIs are considered **experimental** and may change in future releases. It is suitable for evaluation and prototyping, but not yet recommended for production systems requiring long-term API stability.

<!-- TODO: Add links to versioning policy or release notes once available -->
