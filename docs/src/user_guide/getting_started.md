# Getting Started

This guide will walk you through the basics of using Eidetica in your Rust applications. We'll cover the essential steps to set up and interact with the database.

## Installation

<!-- TODO: Add proper installation instructions once published -->

Add Eidetica to your project dependencies:

```toml
[dependencies]
eidetica = "0.1.0"  # Update version as appropriate
# Or if using from a local workspace:
# eidetica = { path = "path/to/eidetica/crates/lib" }
```

## Setting up the Database

To start using Eidetica, you need to:

1. Choose and initialize a **Database** (storage mechanism)
2. Create a **Instance** instance (the main entry point)
3. **Add authentication keys** (required for all operations)
4. Create or access a **Database** (logical container for data)

Here's a simple example:

```rust,ignore
use eidetica::backend::database::InMemory;
use eidetica::Instance;
use eidetica::crdt::Doc;
use std::path::PathBuf;

// Create a new in-memory database
let database = InMemory::new();
let db = Instance::new(Box::new(database));

// Add an authentication key (required for all operations)
db.add_private_key("my_key")?;

// Create a database to store data
let mut settings = Doc::new();
settings.set("name", "my_database");
let database = db.new_database(settings, "my_key")?;
```

The database determines how your data is stored. The example above uses `InMemory`, which keeps everything in memory but can save to a file:

```rust,ignore
// Save the database to a file
let path = PathBuf::from("my_database.json");
let database_guard = db.backend().lock().unwrap();
if let Some(in_memory) = database_guard.as_any().downcast_ref::<InMemory>() {
    in_memory.save_to_file(&path)?;
}
```

You can load a previously saved database:

```rust,ignore
let path = PathBuf::from("my_database.json");
let database = InMemory::load_from_file(&path)?;
let db = Instance::new(Box::new(database));

// Note: Authentication keys are automatically loaded with the database
```

## Authentication Requirements

**Important:** All operations in Eidetica require authentication. Every entry created in the database must be cryptographically signed with a valid Ed25519 private key. This ensures data integrity and provides a consistent security model.

## Working with Data

Eidetica uses **Stores** to organize data within a database. One common store type is `Table`, which maintains a collection of items with unique IDs.

### Defining Your Data

Any data you store must be serializable with `serde`:

```rust,ignore
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Person {
    name: String,
    age: u32,
}
```

### Basic Operations

All operations in Eidetica happen within an atomic **Operation**:

**Inserting Data:**

```rust,ignore
// Start an authenticated operation
let op = database.new_operation()?;

// Get or create a Table store
let people = op.get_subtree::<eidetica::store::Table<Person>>("people")?;

// Insert a person and get their ID
let person = Person { name: "Alice".to_string(), age: 30 };
let id = people.insert(person)?;

// Commit the changes (automatically signed with the database's default key)
op.commit()?;
```

**Reading Data:**

```rust,ignore
let op = database.new_operation()?;
let people = op.get_subtree::<eidetica::store::Table<Person>>("people")?;

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

```rust,ignore
let op = database.new_operation()?;
let people = op.get_subtree::<eidetica::store::Table<Person>>("people")?;

// Get, modify, and update
if let Ok(mut person) = people.get(&id) {
    person.age += 1;
    people.set(&id, person)?;
}

op.commit()?;
```

**Deleting Data:**

```rust,ignore
let op = database.new_operation()?;
let people = op.get_subtree::<eidetica::store::Table<Person>>("people")?;

// Remove a person by ID
people.remove(&id)?;

op.commit()?;
```

## A Complete Example

For a complete working example, see the [Todo Example](../../examples/todo/README.md) included in the repository.

## Next Steps

After getting familiar with the basics, you might want to explore:

- [Core Concepts](core_concepts.md) to understand Eidetica's unique features
- Advanced operations like querying and filtering
- Using different store types for various data patterns
- Configuring and optimizing your database
