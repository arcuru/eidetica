# Getting Started

This guide will walk you through the basics of using EideticaDB in your Rust applications. We'll cover the essential steps to set up and interact with the database.

## Installation

<!-- TODO: Add proper installation instructions once published -->

Add EideticaDB to your project dependencies:

```toml
[dependencies]
eideticadb = "0.1.0"  # Update version as appropriate
```

## Setting up the Database

To start using EideticaDB, you need to:

1. Choose and initialize a **Backend** (storage mechanism)
2. Create a **BaseDB** instance (the main entry point)
3. Create or access a **Tree** (logical container for data)

Here's a simple example:

```rust
use eideticadb::backend::InMemoryBackend;
use eideticadb::basedb::BaseDB;
use std::path::PathBuf;

// Create a new in-memory database
let backend = InMemoryBackend::new();
let db = BaseDB::new(Box::new(backend));

// Create a tree to store data
let mut settings = eideticadb::data::KVOverWrite::new();
settings.set("name", "my_tree");
let tree = db.new_tree(settings)?;
```

The backend determines how your data is stored. The example above uses `InMemoryBackend`, which keeps everything in memory but can save to a file:

```rust
// Save the database to a file
let path = PathBuf::from("my_database.json");
let backend_guard = db.backend().lock().unwrap();
if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemoryBackend>() {
    in_memory.save_to_file(&path)?;
}
```

You can load a previously saved database:

```rust
let path = PathBuf::from("my_database.json");
let backend = InMemoryBackend::load_from_file(&path)?;
let db = BaseDB::new(Box::new(backend));
```

## Working with Data

EideticaDB uses **Subtrees** to organize data within a tree. One common subtree type is `RowStore`, which maintains a collection of items with unique IDs.

### Defining Your Data

Any data you store must be serializable with `serde`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Person {
    name: String,
    age: u32,
}
```

### Basic Operations

All operations in EideticaDB happen within an atomic **Operation**:

**Inserting Data:**

```rust
// Start an operation
let op = tree.new_operation()?;

// Get or create a RowStore subtree
let people = op.get_subtree::<eideticadb::subtree::RowStore<Person>>("people")?;

// Insert a person and get their ID
let person = Person { name: "Alice".to_string(), age: 30 };
let id = people.insert(person)?;

// Commit the changes
op.commit()?;
```

**Reading Data:**

```rust
let op = tree.new_operation()?;
let people = op.get_subtree::<eideticadb::subtree::RowStore<Person>>("people")?;

// Get a single person by ID
if let Ok(person) = people.get(&id) {
    println!("Found: {} ({})", person.name, person.age);
}

// List all people
for result in people.iter()? {
    if let Ok((id, person)) = result {
        println!("ID: {}, Name: {}, Age: {}", id, person.name, person.age);
    }
}
```

**Updating Data:**

```rust
let op = tree.new_operation()?;
let people = op.get_subtree::<eideticadb::subtree::RowStore<Person>>("people")?;

// Get, modify, and update
if let Ok(mut person) = people.get(&id) {
    person.age += 1;
    people.set(&id, person)?;
}

op.commit()?;
```

**Deleting Data:**

```rust
let op = tree.new_operation()?;
let people = op.get_subtree::<eideticadb::subtree::RowStore<Person>>("people")?;

// Remove a person by ID
people.remove(&id)?;

op.commit()?;
```

## A Complete Example

For a complete working example, see the [Todo Example](../../examples/todo/README.md) included in the repository.

## Next Steps

After getting familiar with the basics, you might want to explore:

- [Core Concepts](core_concepts.md) to understand EideticaDB's unique features
- Advanced operations like querying and filtering
- Using different subtree types for various data patterns
- Configuring and optimizing your database
