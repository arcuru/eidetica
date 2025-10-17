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

1. Choose and initialize a **Backend** (storage mechanism)
2. Create an **Instance** (the infrastructure manager)
3. **Create and login a User** (authentication and session)
4. Create or access a **Database** through the User (logical container for data)

Here's a simple example:

```rust
# extern crate eidetica;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
#
# fn main() -> eidetica::Result<()> {
    // Create a new in-memory backend
    let backend = InMemory::new();

    // Create the Instance
    let instance = Instance::open(Box::new(backend))?;

    // Create a passwordless user (perfect for embedded/single-user apps)
    instance.create_user("alice", None)?;

    // Login to get a User session
    let mut user = instance.login_user("alice", None)?;

    // Create a database in the user's context
    let mut settings = Doc::new();
    settings.set_string("name", "my_database");

    // Get the default key (earliest created key)
    let default_key = user.get_default_key()?;
    let _database = user.new_database(settings, &default_key)?;

    Ok(())
}
```

**Note**: This example uses a passwordless user (password is `None`) for simplicity, which is perfect for embedded applications and CLI tools. For multi-user scenarios, you can create password-protected users by passing `Some("password")` instead.

The backend determines how your data is stored. The example above uses `InMemory`, which keeps everything in memory but can save to a file:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
# use std::path::PathBuf;
#
# fn main() -> eidetica::Result<()> {
# // Create instance and user
# let backend = InMemory::new();
# let instance = Instance::open(Box::new(backend))?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set_string("name", "test_db");
# let default_key = user.get_default_key()?;
# let _database = user.new_database(settings, &default_key)?;
#
# // Use a temporary file path for testing
# let temp_dir = std::env::temp_dir();
# let path = temp_dir.join("eidetica_test_save.json");
#
// Save the backend to a file
let backend_guard = instance.backend();
if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
    in_memory.save_to_file(&path)?;
}
#
# // Clean up the temporary file
# if path.exists() {
#     std::fs::remove_file(&path).ok();
# }
# Ok(())
# }
```

You can load a previously saved backend:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
# use std::path::PathBuf;
#
# fn main() -> eidetica::Result<()> {
# // First create and save a test backend
# let backend = InMemory::new();
# let instance = Instance::open(Box::new(backend))?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set_string("name", "test_db");
# let default_key = user.get_default_key()?;
# let _database = user.new_database(settings, &default_key)?;
#
# // Use a temporary file path for testing
# let temp_dir = std::env::temp_dir();
# let path = temp_dir.join("eidetica_test_load.json");
#
# // Save the backend first
# let backend_guard = instance.backend();
# if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
#     in_memory.save_to_file(&path)?;
# }
#
// Load a previously saved backend
let backend = InMemory::load_from_file(&path)?;

// Load instance (automatically detects existing system state)
let instance = Instance::open(Box::new(backend))?;

// Login to existing user
let user = instance.login_user("alice", None)?;
#
# // Clean up the temporary file
# if path.exists() {
#     std::fs::remove_file(&path).ok();
# }
# Ok(())
# }
```

## User-Centric Architecture

Eidetica uses a user-centric architecture:

- **Instance**: Manages infrastructure (user accounts, backend, system databases)
- **User**: Handles all contextual operations (database creation, key management)

All database and key operations happen through a User session after login. This provides:

- **Clear separation**: Infrastructure management vs. contextual operations
- **Strong isolation**: Each user has separate keys and preferences
- **Flexible authentication**: Users can have passwords or not (passwordless mode)

**Passwordless Users** (embedded/single-user apps):

```rust,ignore
instance.create_user("alice", None)?;
let user = instance.login_user("alice", None)?;
```

**Password-Protected Users** (multi-user apps):

```rust,ignore
instance.create_user("bob", Some("password123"))?;
let user = instance.login_user("bob", Some("password123"))?;
```

The downside of password protection is a slow login. `instance.login_user` needs to verify the password and decrypt keys, which by design is a relatively slow operation.

## Working with Data

Eidetica uses **Stores** to organize data within a database. One common store type is `Table`, which maintains a collection of items with unique IDs.

### Defining Your Data

Any data you store must be serializable with `serde`:

### Basic Operations

All operations in Eidetica happen within an atomic **Transaction**:

**Inserting Data:**

```rust
# extern crate eidetica;
# extern crate serde;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct Person {
#     name: String,
#     age: u32,
# }
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set_string("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.new_database(settings, &default_key)?;
#
// Start an authenticated transaction
let op = database.new_transaction()?;

// Get or create a Table store
let people = op.get_store::<Table<Person>>("people")?;

// Insert a person and get their ID
let person = Person { name: "Alice".to_string(), age: 30 };
let _id = people.insert(person)?;

// Commit the changes (automatically signed with the user's key)
op.commit()?;
# Ok(())
# }
```

**Reading Data:**

```rust
# extern crate eidetica;
# extern crate serde;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct Person {
#     name: String,
#     age: u32,
# }
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set_string("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.new_database(settings, &default_key)?;
# // Insert some test data
# let op = database.new_transaction()?;
# let people = op.get_store::<Table<Person>>("people")?;
# let test_id = people.insert(Person { name: "Alice".to_string(), age: 30 })?;
# op.commit()?;
# let id = &test_id;
#
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
# Ok(())
# }
```

**Updating Data:**

```rust
# extern crate eidetica;
# extern crate serde;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct Person {
#     name: String,
#     age: u32,
# }
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set_string("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.new_database(settings, &default_key)?;
# // Insert some test data
# let op_setup = database.new_transaction()?;
# let people_setup = op_setup.get_store::<Table<Person>>("people")?;
# let test_id = people_setup.insert(Person { name: "Alice".to_string(), age: 30 })?;
# op_setup.commit()?;
# let id = &test_id;
#
let op = database.new_transaction()?;
let people = op.get_store::<Table<Person>>("people")?;

// Get, modify, and update
if let Ok(mut person) = people.get(id) {
    person.age += 1;
    people.set(id, person)?;
}

op.commit()?;
# Ok(())
# }
```

**Deleting Data:**

```rust
# extern crate eidetica;
# extern crate serde;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct Person {
#     name: String,
#     age: u32,
# }
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set_string("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.new_database(settings, &default_key)?;
# let _id = "test_id";
#
let op = database.new_transaction()?;
let people = op.get_store::<Table<Person>>("people")?;

// FIXME: Table doesn't currently support deletion
// You can overwrite with a "deleted" marker or use other approaches

op.commit()?;
# Ok(())
# }
```

## A Complete Example

For a complete working example, see the [Todo Example](../../examples/todo/README.md) included in the repository.

## Next Steps

After getting familiar with the basics, you might want to explore:

- [Core Concepts](core_concepts.md) to understand Eidetica's unique features
- Advanced operations like querying and filtering
- Using different store types for various data patterns
- Configuring and optimizing your database
