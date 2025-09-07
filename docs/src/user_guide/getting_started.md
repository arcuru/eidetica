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

```rust
# extern crate eidetica;
use eidetica::{backend::database::InMemory, Instance, crdt::Doc};

fn main() -> eidetica::Result<()> {
    // Create a new in-memory database
    let database = InMemory::new();
    let db = Instance::new(Box::new(database));

    // Add an authentication key (required for all operations)
    db.add_private_key("my_private_key")?;

    // Create a database to store data
    let mut settings = Doc::new();
    settings.set_string("name", "my_database");
    let _database = db.new_database(settings, "my_private_key")?;

    Ok(())
}
```

The database determines how your data is stored. The example above uses `InMemory`, which keeps everything in memory but can save to a file:

```rust
# extern crate eidetica;
use eidetica::{Instance, backend::database::InMemory};
use std::path::PathBuf;

fn save_db(db: &Instance) -> eidetica::Result<()> {
    // Save the database to a file
    let path = PathBuf::from("my_database.json");
    let database_guard = db.backend();
    if let Some(in_memory) = database_guard.as_any().downcast_ref::<InMemory>() {
        in_memory.save_to_file(&path)?;
    }
    Ok(())
}
```

You can load a previously saved database:

```rust
# extern crate eidetica;
use eidetica::{Instance, backend::database::InMemory};
use std::path::PathBuf;

fn load_instance() -> eidetica::Result<Instance> {
    let path = PathBuf::from("my_database.json");
    let database = InMemory::load_from_file(&path)?;
    // Note: Authentication keys are automatically loaded with the database if they exist
    Ok(Instance::new(Box::new(database)))
}
```

## Authentication Requirements

**Important:** All operations in Eidetica require authentication. Every entry created in the database must be cryptographically signed with a valid Ed25519 private key. This ensures data integrity and provides a consistent security model.

## Working with Data

Eidetica uses **Stores** to organize data within a database. One common store type is `Table`, which maintains a collection of items with unique IDs.

### Defining Your Data

Any data you store must be serializable with `serde`:

### Basic Operations

All operations in Eidetica happen within an atomic **Operation**:

**Inserting Data:**

```rust
# extern crate eidetica;
# extern crate serde;
use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::Table, Database};
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Person {
    name: String,
    age: u32,
}

fn insert_alice(database: &Database) -> eidetica::Result<()> {
    // Start an authenticated operation
    let op = database.new_transaction()?;

    // Get or create a Table store
    let people = op.get_store::<Table<Person>>("people")?;

    // Insert a person and get their ID
    let person = Person { name: "Alice".to_string(), age: 30 };
    let _id = people.insert(person)?;

    // Commit the changes (automatically signed with the database's default key)
    op.commit()?;
    Ok(())
}
```

**Reading Data:**

```rust
# extern crate eidetica;
# extern crate serde;
use eidetica::{Database, store::Table};
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Person {
    name: String,
    age: u32,
}

fn read(database: &Database, id: &str) -> eidetica::Result<()> {
    let op = database.new_transaction()?;
    let people = op.get_store::<Table<Person>>("people")?;

    // Get a single person by ID
    if let Ok(person) = people.get(id) {
        println!("Found: {} ({})", person.name, person.age);
    }

    // Search for all people (using a predicate that always returns true)
    let all_people = people.search(|_| true)?;
    for (id, person) in all_people {
        println!("ID: {}, Name: {}, Age: {}", id, person.name, person.age);
    }
    Ok(())
}
```

**Updating Data:**

```rust
# extern crate eidetica;
# extern crate serde;
use eidetica::{Database, store::Table};
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Person {
    name: String,
    age: u32,
}

fn update(database: &Database, id: &str) -> eidetica::Result<()> {
    let op = database.new_transaction()?;
    let people = op.get_store::<Table<Person>>("people")?;

    // Get, modify, and update
    if let Ok(mut person) = people.get(id) {
        person.age += 1;
        people.set(id, person)?;
    }

    op.commit()?;
    Ok(())
}
```

**Deleting Data:**

```rust
# extern crate eidetica;
# extern crate serde;
use eidetica::{Database, store::Table};
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Person {
    name: String,
    age: u32,
}

fn delete(database: &Database, id: &str) -> eidetica::Result<()> {
    let op = database.new_transaction()?;
    let people = op.get_store::<Table<Person>>("people")?;

    // Note: Table doesn't currently support deletion
    // You can overwrite with a "deleted" marker or use other approaches

    op.commit()?;
    Ok(())
}
```

## A Complete Example

For a complete working example, see the [Todo Example](../../examples/todo/README.md) included in the repository.

## Next Steps

After getting familiar with the basics, you might want to explore:

- [Core Concepts](core_concepts.md) to understand Eidetica's unique features
- Advanced operations like querying and filtering
- Using different store types for various data patterns
- Configuring and optimizing your database
