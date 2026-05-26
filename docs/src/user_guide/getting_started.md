# Getting Started

This guide will walk you through the basics of using Eidetica in your Rust applications. We'll cover the essential steps to set up and interact with the database.

For contributing to Eidetica itself, see the [Contributing guide](../internal/contributing.md).

## Installation

<!-- TODO: Add proper installation instructions once published -->

Add Eidetica to your project dependencies:

```toml
[dependencies]
eidetica = "0.1.0"  # Update version as appropriate
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
# Or if using from a local workspace:
# eidetica = { path = "path/to/eidetica/crates/lib" }
```

Eidetica uses an async-first API built on Tokio. All database operations are async and must be called within a Tokio runtime.

## Setting up the Database

To start using Eidetica, you need to:

1. Choose and initialize a **Backend** (storage mechanism)
2. Create an **Instance** (the infrastructure manager)
3. **Create and login a User** (authentication and session)
4. Create or access a **Database** through the User (logical container for data)

Here's a simple example:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, NewUser, crdt::Doc};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
    // Open or initialise an Instance from a URL. `memory://` is an
    // ephemeral in-process backend — great for tests and embedded use.
    // The first user created on a fresh instance is automatically granted
    // Admin on the system databases.
    let (_instance, maybe_user) = Instance::connect_or_create(
        "memory://",
        NewUser::passwordless("alice"),
    ).await?;
    let mut user = maybe_user.expect("memory:// is always fresh");

    // Create a database in the user's context
    let mut settings = Doc::new();
    settings.set("name", "my_database");

    // Get the default key (earliest created key)
    let default_key = user.get_default_key()?;
    let _database = user.create_database(settings, &default_key).await?;

    Ok(())
}
```

**Note**: This example uses a passwordless user for simplicity, which is perfect for embedded applications and CLI tools. For multi-user scenarios, create password-protected users with `NewUser::with_password("alice", "pwd")` instead.

The URL passed to [`Instance::connect_or_create`](https://docs.rs/eidetica/latest/eidetica/struct.Instance.html#method.connect_or_create) selects the backend. Some other common forms:

- `memory://` — ephemeral in-process backend (above).
- `memory:///abs/path/snap.json` — in-process backend with a JSON snapshot file (load-on-start, writes via [`Instance::flush`](https://docs.rs/eidetica/latest/eidetica/struct.Instance.html#method.flush)).
- `sqlite://./my_data.db` — embedded SQLite database, persisted automatically. The URL is passed through to `sqlx`, so any sqlx-accepted form works (`?journal_mode=WAL`, `?busy_timeout=5000`, etc.).
- `postgres://user:pwd@host/db` — embedded PostgreSQL backend.
- `unix:///run/eidetica/service.sock` — connect to a running [daemon](service.md).

For persistent storage, swap the URL:

<!-- Code block ignored: Requires file system access during testing -->

```rust,ignore
use eidetica::{Instance, NewUser, crdt::Doc};

#[tokio::main]
async fn main() -> eidetica::Result<()> {
    // First run: bootstrap a fresh persistent instance.
    // Subsequent runs: load the existing instance (Option<User> is None).
    let (instance, maybe_user) = Instance::connect_or_create(
        "sqlite://./my_data.db",
        NewUser::passwordless("alice"),
    ).await?;

    let mut user = match maybe_user {
        Some(u) => u,
        None => instance.login_user("alice", None).await?,
    };
    // ... all changes are automatically persisted to my_data.db

    Ok(())
}
```

When you only want to load an already-initialised database (strict — errors if missing), use [`Instance::connect`](https://docs.rs/eidetica/latest/eidetica/struct.Instance.html#method.connect):

<!-- Code block ignored: Requires file system access during testing -->

```rust,ignore
use eidetica::Instance;

#[tokio::main]
async fn main() -> eidetica::Result<()> {
    // Strict load — errors if the database isn't initialised yet.
    let instance = Instance::connect("sqlite://./my_data.db").await?;
    let user = instance.login_user("alice", None).await?;
    // ... data persists across restarts

    Ok(())
}
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
let admin = instance.login_user("admin", None).await?;
admin.admin().await?.create_user(eidetica::NewUser::passwordless("alice")).await?;
let user = instance.login_user("alice", None).await?;
```

**Password-Protected Users** (multi-user apps):

```rust,ignore
let admin = instance.login_user("admin", None).await?;
admin.admin().await?.create_user(eidetica::NewUser::with_password("bob", "password123")).await?;
let user = instance.login_user("bob", Some("password123")).await?;
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
# extern crate tokio;
# extern crate serde;
# use eidetica::{backend::database::Sqlite, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct Person {
#     name: String,
#     age: u32,
# }
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewUser::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
#
// Start an authenticated transaction
let txn = database.new_transaction().await?;

// Get or create a Table store
let people = txn.get_store::<Table<Person>>("people").await?;

// Insert a person and get their ID
let person = Person { name: "Alice".to_string(), age: 30 };
let _id = people.insert(person).await?;

// Commit the changes (automatically signed with the user's key)
txn.commit().await?;
# Ok(())
# }
```

**Reading Data:**

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{backend::database::Sqlite, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct Person {
#     name: String,
#     age: u32,
# }
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewUser::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Insert some test data
# let txn = database.new_transaction().await?;
# let people = txn.get_store::<Table<Person>>("people").await?;
# let test_id = people.insert(Person { name: "Alice".to_string(), age: 30 }).await?;
# txn.commit().await?;
# let id = &test_id;
#
let txn = database.new_transaction().await?;
let people = txn.get_store::<Table<Person>>("people").await?;

// Get a single person by ID
if let Ok(person) = people.get(id).await {
    println!("Found: {} ({})", person.name, person.age);
}

// Search for all people (using a predicate that always returns true)
let all_people = people.search(|_| true).await?;
for (id, person) in all_people {
    println!("ID: {}, Name: {}, Age: {}", id, person.name, person.age);
}
# Ok(())
# }
```

**Updating Data:**

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{backend::database::Sqlite, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct Person {
#     name: String,
#     age: u32,
# }
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewUser::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Insert some test data
# let txn_setup = database.new_transaction().await?;
# let people_setup = txn_setup.get_store::<Table<Person>>("people").await?;
# let test_id = people_setup.insert(Person { name: "Alice".to_string(), age: 30 }).await?;
# txn_setup.commit().await?;
# let id = &test_id;
#
let txn = database.new_transaction().await?;
let people = txn.get_store::<Table<Person>>("people").await?;

// Get, modify, and update
if let Ok(mut person) = people.get(id).await {
    person.age += 1;
    people.set(id, person).await?;
}

txn.commit().await?;
# Ok(())
# }
```

**Deleting Data:**

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{backend::database::Sqlite, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct Person {
#     name: String,
#     age: u32,
# }
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewUser::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let _id = "test_id";
#
let txn = database.new_transaction().await?;
let people = txn.get_store::<Table<Person>>("people").await?;

// FIXME: Table doesn't currently support deletion
// You can overwrite with a "deleted" marker or use other approaches

txn.commit().await?;
# Ok(())
# }
```

## Complete Examples

For complete working examples, see:

- **[Chat Example](https://github.com/arcuru/eidetica/blob/main/examples/chat/README.md)** - Multi-user chat application demonstrating:
  - User accounts and authentication
  - Real-time synchronization with HTTP and Iroh transports
  - Bootstrap protocol for joining rooms
  - TUI interface with Ratatui

- **[Todo Example](https://github.com/arcuru/eidetica/blob/main/examples/todo/README.md)** - Task management application

## Next Steps

After getting familiar with the basics, you might want to explore:

- [Core Concepts](core_concepts.md) to understand Eidetica's unique features
- [Synchronization Guide](synchronization_guide.md) to set up peer-to-peer data sync
- [Authentication Guide](authentication_guide.md) for secure multi-user applications
- [Service (Daemon) Mode](service.md) to share an Instance across multiple processes
- Advanced operations like querying and filtering
- Using different store types for various data patterns
