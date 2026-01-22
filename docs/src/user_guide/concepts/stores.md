# Stores

Stores provide structured, type-safe access to different kinds of data within a Database.

## The Store Concept

In Eidetica, Stores extend the Merkle-CRDT concept by explicitly partitioning data within each Entry. A Store:

- Represents a specific type of data structure (like a key-value store or a collection of records)
- Has a unique name within its parent Database
- Maintains its own history tracking
- Is strongly typed (via Rust generics)

Stores are what make Eidetica practical for real applications, as they provide high-level, data-structure-aware interfaces on top of the core Entry and Database concepts.

## Why Stores?

Stores offer several advantages:

- **Type Safety**: Each store implementation provides appropriate methods for its data type
- **Isolation**: Changes to different stores can be tracked separately
- **Composition**: Multiple data structures can exist within a single Database
- **Efficiency**: Only relevant stores need to be loaded or synchronized
- **Atomic Operations**: Changes across multiple stores can be committed atomically

## Available Store Types

Eidetica provides several store types, each optimized for different data patterns:

| Type              | Purpose               | Key Features                               | Best For                                     |
| ----------------- | --------------------- | ------------------------------------------ | -------------------------------------------- |
| **DocStore**      | Document storage      | Path-based operations, nested structures   | Configuration, metadata, structured docs     |
| **Table\<T>**     | Record collections    | Auto-generated UUIDs, type safety, search  | User lists, products, any structured records |
| **SettingsStore** | Database settings     | Type-safe settings API, auth management    | Database configuration, authentication       |
| **YDoc**          | Collaborative editing | Y-CRDT integration, real-time sync         | Shared documents, collaborative text editing |
| **PasswordStore** | Encrypted wrapper     | Password-based encryption, wraps any store | Sensitive data, secrets, credentials         |

### DocStore (Document-Oriented Storage)

The `DocStore` store provides a document-oriented interface for storing and retrieving structured data. It wraps the `crdt::Doc` type to provide ergonomic access patterns with both simple key-value operations and path-based operations for nested data structures.

#### Basic Usage

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::DocStore, path};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
// Get a DocStore store
let op = database.new_transaction().await?;
let store = op.get_store::<DocStore>("app_data").await?;

// Set simple values
store.set("version", "1.0.0").await?;
store.set("author", "Alice").await?;

// Path-based operations for nested structures
// This creates nested maps: {"database": {"host": "localhost", "port": "5432"}}
store.set_path(path!("database.host"), "localhost").await?;
store.set_path(path!("database.port"), "5432").await?;

// Retrieve values
let version = store.get("version").await?; // Returns a Value
let host = store.get_path(path!("database.host")).await?; // Returns Value

op.commit().await?;
# Ok(())
# }
```

#### Important: Path Operations Create Nested Structures

When using `set_path("a.b.c", value)`, DocStore creates **nested maps**, not flat keys with dots:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::DocStore, path};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let op = database.new_transaction().await?;
# let store = op.get_store::<DocStore>("app_data").await?;
// This code:
store.set_path(path!("user.profile.name"), "Bob").await?;

// Creates this structure:
// {
//   "user": {
//     "profile": {
//       "name": "Bob"
//     }
//   }
// }

// NOT: { "user.profile.name": "Bob" } ❌
# op.commit().await?;
# Ok(())
# }
```

Use cases for `DocStore`:

- Application configuration
- Metadata storage
- Structured documents
- Settings management
- Any data requiring path-based access

### Table

The `Table<T>` store manages collections of serializable items, similar to a table in a database:

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::Table};
# use serde::{Serialize, Deserialize};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
// Define a struct for your data
#[derive(Serialize, Deserialize, Clone)]
struct User {
    name: String,
    email: String,
    active: bool,
}

// Get a Table store
let op = database.new_transaction().await?;
let users = op.get_store::<Table<User>>("users").await?;

// Insert items (returns a generated UUID)
let user = User {
    name: "Alice".to_string(),
    email: "alice@example.com".to_string(),
    active: true,
};
let id = users.insert(user).await?;

// Get an item by ID
if let Ok(user) = users.get(&id).await {
    println!("Found user: {}", user.name);
}

// Update an item
if let Ok(mut user) = users.get(&id).await {
    user.active = false;
    users.set(&id, user).await?;
}

// Delete an item
let was_deleted = users.delete(&id).await?;
if was_deleted {
    println!("User deleted successfully");
}

// Search for items matching a condition
let active_users = users.search(|user| user.active).await?;
for (id, user) in active_users {
    println!("Active user: {} (ID: {})", user.name, id);
}
# op.commit().await?;
# Ok(())
# }
```

Use cases for `Table`:

- Collections of structured objects
- Record storage (users, products, todos, etc.)
- Any data where individual items need unique IDs
- When you need to search across records with custom predicates

### SettingsStore (Database Settings Management)

The `SettingsStore` provides a specialized, type-safe interface for managing database settings and authentication configuration. It wraps the internal `_settings` subtree to provide convenient methods for common settings operations.

#### Basic Usage

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
// Get a SettingsStore for the current transaction
let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Set database name
settings_store.set_name("My Application Database").await?;

// Get database name
let name = settings_store.get_name().await?;
println!("Database name: {}", name);

transaction.commit().await?;
# Ok(())
# }
```

#### Authentication Management

`SettingsStore` provides convenient methods for managing authentication keys:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "stores_auth_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Generate a keypair for the new user
# let (_alice_signing_key, alice_verifying_key) = generate_keypair();
# let alice_public_key = format_public_key(&alice_verifying_key);
let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Add a new authentication key
let auth_key = AuthKey::active(
    &alice_public_key,
    Permission::Write(10),
)?;
settings_store.set_auth_key("alice", auth_key).await?;

// Get an authentication key
let key = settings_store.get_auth_key("alice").await?;
println!("Alice's key: {}", key.pubkey());

// Revoke a key
settings_store.revoke_auth_key("alice").await?;

transaction.commit().await?;
# Ok(())
# }
```

#### Complex Updates with Closures

For complex operations that need to be atomic, use the `update_auth_settings` method:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "complex_auth_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Generate keypairs for multiple users
# let (_bob_signing_key, bob_verifying_key) = generate_keypair();
# let bob_public_key = format_public_key(&bob_verifying_key);
# let bob_key = AuthKey::active(&bob_public_key, Permission::Write(20))?;
# let (_charlie_signing_key, charlie_verifying_key) = generate_keypair();
# let charlie_public_key = format_public_key(&charlie_verifying_key);
# let charlie_key = AuthKey::active(&charlie_public_key, Permission::Admin(15))?;
# let (_old_user_signing_key, old_user_verifying_key) = generate_keypair();
# let old_user_public_key = format_public_key(&old_user_verifying_key);
# let old_user_key = AuthKey::active(&old_user_public_key, Permission::Write(30))?;
# // Add old_user first so we can revoke it
# let setup_txn = database.new_transaction().await?;
# let setup_store = setup_txn.get_settings()?;
# setup_store.set_auth_key("old_user", old_user_key).await?;
# setup_txn.commit().await?;
let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Perform multiple auth operations atomically
settings_store.update_auth_settings(|auth| {
    // Add multiple keys
    auth.overwrite_key("bob", bob_key)?;
    auth.overwrite_key("charlie", charlie_key)?;

    // Revoke an old key
    auth.revoke_key("old_user")?;

    Ok(())
}).await?;

transaction.commit().await?;
# Ok(())
# }
```

#### Advanced Usage

<!-- Code block ignored: Demonstrates advanced API patterns rather than compilable code -->

For operations not covered by the convenience methods, access the underlying DocStore:

<!-- Code block ignored: Demonstrates advanced API patterns rather than compilable code -->

```rust,ignore
let transaction = database.new_transaction()?;
let settings_store = transaction.get_settings()?;

// Access underlying DocStore for advanced operations
let doc_store = settings_store.as_doc_store();
doc_store.set_path(path!("custom.config.option"), "value")?;

transaction.commit()?;
```

Use cases for `SettingsStore`:

- Database configuration and metadata
- Authentication key management
- User permission management
- Bootstrap and sync policies
- Any settings that need type-safe, validated access

### YDoc (Y-CRDT Integration)

The `YDoc` store provides integration with Y-CRDT (Yjs) for real-time collaborative editing. This requires the "y-crdt" feature:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::YDoc};
# use eidetica::y_crdt::{Map, Text, Transact};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let backend = Sqlite::in_memory().await?;
# let instance = Instance::open(Box::new(backend)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "y_crdt_stores");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
#
// Get a YDoc store
let op = database.new_transaction().await?;
let doc_store = op.get_store::<YDoc>("document").await?;

// Work with Y-CRDT structures
doc_store.with_doc_mut(|doc| {
    let text = doc.get_or_insert_text("content");
    let metadata = doc.get_or_insert_map("meta");

    let mut txn = doc.transact_mut();

    // Collaborative text editing
    text.insert(&mut txn, 0, "Hello, collaborative world!");

    // Set metadata
    metadata.insert(&mut txn, "title", "My Document");
    metadata.insert(&mut txn, "author", "Alice");

    Ok(())
}).await?;

op.commit().await?;
# Ok(())
# }
```

Use cases for `YDoc`:

- Real-time collaborative text editing
- Shared documents with multiple editors
- Conflict-free data synchronization
- Applications requiring sophisticated merge algorithms

### PasswordStore (Encrypted Wrapper)

`PasswordStore` wraps any other store type with transparent password-based encryption. All data is encrypted using AES-256-GCM before being stored, with keys derived from a password using Argon2id.

For detailed usage and examples, see the [Encryption Guide](../encryption_guide.md).

## Subtree Index

Eidetica automatically maintains an index of all user-created subtrees in a special `_index` subtree. This index stores metadata about each subtree, including its Store type and configuration.

### What is the Subtree Index?

The `_index` subtree tracks:

- **Subtree names**: Which subtrees exist in the database
- **Store types**: What type of Store manages each subtree (e.g., "docstore:v0", "table:v0")
- **Configuration**: Store-specific settings for each subtree
- **Subtree settings**: Common settings like height strategy overrides

The index is maintained automatically when you access stores via `get_store()` and is useful for:

- **Discovery**: Finding what subtrees exist in a database
- **Type information**: Understanding what Store type manages each subtree
- **Tooling**: Building generic database browsers and inspectors

The index is accessed via `Transaction::get_index()`, which returns a `Registry` - a general-purpose type for managing `name → {type, config}` mappings.

### Automatic Registration

When you first access a Store using `Transaction::get_store()`, it's automatically registered in the `_index` with its Store type and default configuration:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::DocStore};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
// First access to "app_config" - automatically registered in _index
let txn = database.new_transaction().await?;
let config: DocStore = txn.get_store("app_config").await?;
config.set("version", "1.0.0").await?;
txn.commit().await?;

// The 'app_config' Store is now registered with type "docstore:v0"
# Ok(())
# }
```

Registration happens immediately when `get_store()` is called for a new subtree.

**System Subtrees**: The special system subtrees (`_settings`, `_index`, `_root`) are excluded from the index to avoid circular dependencies.

### Querying the Index

Use `get_index()` to query information about registered subtrees:

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::{DocStore, Table}};
# use serde::{Serialize, Deserialize};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Create some subtrees first
# #[derive(Serialize, Deserialize, Clone)]
# struct User { name: String }
# let setup_txn = database.new_transaction().await?;
# let _config: DocStore = setup_txn.get_store("config").await?;
# let _users: Table<User> = setup_txn.get_store("users").await?;
# setup_txn.commit().await?;
// Query the index to discover subtrees
let txn = database.new_transaction().await?;
let index = txn.get_index().await?;

// List all registered subtrees
let subtrees = index.list().await?;
for name in subtrees {
    println!("Found subtree: {}", name);
}

// Check if a specific subtree exists
if index.contains("config").await {
    // Get metadata about the subtree
    let info = index.get_entry("config").await?;
    println!("Type: {}", info.type_id);  // e.g., "docstore:v0"
    println!("Config: {}", info.config);  // Store-specific configuration
}
# Ok(())
# }
```

### Manual Registration

You can manually register or update subtree metadata using `set_entry()` on the index. This is useful for pre-registering subtrees with custom configuration:

<!-- Code block ignored: Manual registration requires custom config which varies by Store type -->

```rust,ignore
let txn = database.new_transaction()?;
let index = txn.get_index()?;

// Pre-register a subtree with custom configuration
index.set_entry(
    "documents",
    "ydoc:v0",
    r#"{"compression":"zstd","cache_size":1024}"#
)?;

txn.commit()?;

// Future accesses will use the registered configuration
```

### When to Use the Subtree Index

Many applications don't need to interact with the subtree index directly and can let auto-registration handle everything automatically. Use `get_index()` when you need to:

- **List subtrees**: Build a database browser or inspector
- **Query metadata**: Check Store types or configurations
- **Pre-configure**: Set custom configuration before first use
- **Build tooling**: Create generic tools that work with any database structure

For more information on how the index system works internally, see the [Subtree Index Design Document](../../design/subtree_index.md).

## Store Implementation Details

<!-- Code block ignored: Shows trait definition rather than complete implementation -->

Each Store implementation in Eidetica:

1. Implements the `Store` trait
2. Provides methods appropriate for its data structure
3. Handles serialization/deserialization of data
4. Manages the store's history within the Database

The `Store` trait defines the minimal interface:

<!-- Code block ignored: Shows trait definition rather than complete implementation -->

```rust,ignore
pub trait Store: Sized {
    fn new(op: &Transaction, store_name: &str) -> Result<Self>;
    fn name(&self) -> &str;
}
```

Store implementations add their own methods on top of this minimal interface.

## Store Height Strategies

Each store can configure its own height calculation strategy, independent of the database-level strategy. When branches diverge and later merge (e.g., after a network split), heights determine the order entries are processed. Lower heights come first; ties are broken by entry hash.

### Available Methods

All Store types have these height strategy methods available:

- `get_height_strategy()`: Get the current strategy (`None` = inherit from database)
- `set_height_strategy(Option<HeightStrategy>)`: Set or clear an override

### Use Cases

| Scenario                             | Recommended Setup                      |
| ------------------------------------ | -------------------------------------- |
| Global timestamp ordering            | Database: `Timestamp`, stores inherit  |
| Simple sequential ordering           | Database: `Incremental` (default)      |
| Audit logs with independent ordering | Specific store: `Incremental` override |

For detailed usage examples, see [Height Strategies](../transactions.md#height-strategies) in the Transactions guide.

## Store History and Merging (CRDT Aspects)

While Eidetica uses Merkle-DAGs for overall history, the way data _within_ a Store is combined when branches merge relies on Conflict-free Replicated Data Type (CRDT) principles. This ensures that even if different replicas of the database have diverged and made concurrent changes, they can be merged back together automatically without conflicts (though the merge _result_ depends on the CRDT strategy).

Each Store type implements its own merge logic, typically triggered implicitly when an `Transaction` reads the current state of the store (which involves finding and merging the tips of that store's history):

- **`DocStore`**: Implements a **Last-Writer-Wins (LWW)** strategy using the internal `Doc` type. When merging concurrent writes to the _same key_ or path, the write associated with the later `Entry` "wins", and its value is kept. Writes to different keys are simply combined. Deleted keys (via `delete()`) are tracked with tombstones to ensure deletions propagate properly.

- **`Table<T>`**: Also uses **LWW for updates to the _same row ID_**. If two concurrent operations modify the same row, the later write wins. Inserts of _different_ rows are combined (all inserted rows are kept). Deletions generally take precedence over concurrent updates (though precise semantics might evolve).

**Note:** The CRDT merge logic happens internally when an `Transaction` loads the initial state of a Store or when a store viewer is created. You typically don't invoke merge logic directly.

<!-- TODO: Add links to specific CRDT literature or more detailed internal docs on merge logic if needed -->

## Future Store Types

Eidetica's architecture allows for adding new Store implementations. Potential future types include:

- **ObjectStore**: For storing large binary blobs.

These are **not yet implemented**. Development is currently focused on the core API and the existing `DocStore` and `Table` types.

<!-- TODO: Update this list if/when new store types become available or development starts -->
