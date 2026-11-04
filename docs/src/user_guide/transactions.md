# Transactions: Atomic Changes

In Eidetica, all modifications to the data stored within a `Database`'s `Store`s happen through an **`Transaction`**. This is a fundamental concept ensuring atomicity and providing a consistent mechanism for interacting with your data.

**Authentication Note**: All transactions in Eidetica are authenticated by default. Every transaction uses the database's default signing key to ensure that all changes are cryptographically verified and can be traced to their source.

A `Transaction` bundles multiple Store operations (which affect individual subtrees) into a single atomic Entry that gets committed to the database.

## Why Transactions?

Transactions provide several key benefits:

- **Atomicity**: Changes made to multiple `Store`s within a single `Transaction` are encoded together in one signed Entry. A failure before persistence writes nothing, but storage, verification, callbacks and acknowledgement are separate steps: an error or lost acknowledgement can follow persistence. Read back verified state before retrying; a failed `commit()` does not imply rollback.
- **Consistency**: A `Transaction` captures a snapshot of the `Database`'s state (specifically, the tips of the relevant `Store`s) when it's created or when a `Store` is first accessed within it. All reads and writes within that `Transaction` occur relative to this consistent state.
- **Change Staging**: Modifications made via `Store` handles are staged within the `Transaction` object itself, not written directly to the database until `commit()` is called.
- **Authentication**: All transactions are automatically authenticated using the database's default signing key, ensuring data integrity and access control.
- **History Creation**: A successful `commit()` results in the creation of a _new `Entry`_ in the `Database`, containing the staged changes and linked to the previous state (the tips the `Transaction` was based on). This is how history is built.

## The Transaction Lifecycle

Using a `Transaction` follows a distinct lifecycle:

1.  **Creation**: Start an authenticated transaction from a `Database` instance.

    ```rust
    # extern crate eidetica;
    # extern crate tokio;
    # use eidetica::{backend::database::Sqlite, Instance, crdt::Doc};
    #
    # #[tokio::main]
    # async fn main() -> eidetica::Result<()> {
    # // Setup database
    # let backend = Sqlite::in_memory().await?;
    # let (instance, mut user) = Instance::create_backend(
    #     Box::new(backend),
    #     eidetica::NewAccount::passwordless("alice"),
    # ).await?;
    # let mut settings = Doc::new();
    # settings.set("name", "test");
    # let default_key = user.get_default_key()?;
    # let database = user.create_database(settings, &default_key).await?;
    #
    let _txn = database.new_transaction().await?; // Automatically uses the database's default signing key
    # Ok(())
    # }
    ```

2.  **Store Access**: Get handles to the specific `Store`s you want to interact with. This implicitly loads the current state (tips) of that store into the transaction if accessed for the first time.

    ```rust
    # extern crate eidetica;
    # extern crate tokio;
    # extern crate serde;
    # use eidetica::{backend::database::Sqlite, Instance, crdt::Doc, store::{Table, DocStore, SettingsStore}, Database};
    # use serde::{Serialize, Deserialize};
    #
    # #[derive(Clone, Debug, Serialize, Deserialize)]
    # struct User {
    #     name: String,
    # }
    #
    # #[tokio::main]
    # async fn main() -> eidetica::Result<()> {
    # // Setup database and transaction
    # let backend = Sqlite::in_memory().await?;
    # let (instance, mut user) = Instance::create_backend(
    #     Box::new(backend),
    #     eidetica::NewAccount::passwordless("alice"),
    # ).await?;
    # let mut settings = Doc::new();
    # settings.set("name", "test");
    # let default_key = user.get_default_key()?;
    # let database = user.create_database(settings, &default_key).await?;
    let txn = database.new_transaction().await?;

    // Get handles within a scope or manage their lifetime
    let _users_store = txn.get_store::<Table<User>>("users").await?;
    let _config_store = txn.get_store::<DocStore>("config").await?;
    let _settings_store = txn.get_settings()?;  // For database settings

    txn.commit().await?;
    # Ok(())
    # }
    ```

3.  **Staging Changes**: Use the methods provided by the `Store` handles (`set`, `insert`, `get`, `remove`, etc.). These methods interact with the data staged _within the `Transaction`_.

    ```rust
    # extern crate eidetica;
    # extern crate tokio;
    # extern crate serde;
    # use eidetica::{backend::database::Sqlite, Instance, crdt::Doc, store::{Table, DocStore, SettingsStore}};
    # use serde::{Serialize, Deserialize};
    #
    # #[derive(Clone, Debug, Serialize, Deserialize)]
    # struct User {
    #     name: String,
    # }
    #
    # #[tokio::main]
    # async fn main() -> eidetica::Result<()> {
    # // Setup database and transaction
    # let backend = Sqlite::in_memory().await?;
    # let (instance, mut user) = Instance::create_backend(
    #     Box::new(backend),
    #     eidetica::NewAccount::passwordless("alice"),
    # ).await?;
    # let mut settings = Doc::new();
    # settings.set("name", "test");
    # let default_key = user.get_default_key()?;
    # let database = user.create_database(settings, &default_key).await?;
    # let txn = database.new_transaction().await?;
    # let users_store = txn.get_store::<Table<User>>("users").await?;
    # let config_store = txn.get_store::<DocStore>("config").await?;
    # let settings_store = txn.get_settings()?;
    #
    // Insert a new user and get their ID
    let user_id = users_store.insert(User { name: "Alice".to_string() }).await?;
    let _current_user = users_store.get(&user_id).await?;
    config_store.set("last_updated", "2024-01-15T10:30:00Z").await?;
    settings_store.set_name("Updated Database Name").await?;  // Manage database settings
    #
    # txn.commit().await?;
    # Ok(())
    # }
    ```

    _Note: `get` methods within a transaction read from the staged state, reflecting any changes already made within the same transaction._

4.  **Commit**: Finalize the changes. This consumes the `Transaction` object, calculates the final `Entry` content based on staged changes, cryptographically signs the entry, writes the new `Entry` to the `Database`, and returns the `ID` of the newly created `Entry`.

    ```rust
    # extern crate eidetica;
    # extern crate tokio;
    # use eidetica::{backend::database::Sqlite, Instance, crdt::Doc};
    #
    # #[tokio::main]
    # async fn main() -> eidetica::Result<()> {
    # // Setup database
    # let backend = Sqlite::in_memory().await?;
    # let (instance, mut user) = Instance::create_backend(
    #     Box::new(backend),
    #     eidetica::NewAccount::passwordless("alice"),
    # ).await?;
    # let mut settings = Doc::new();
    # settings.set("name", "test");
    # let default_key = user.get_default_key()?;
    # let database = user.create_database(settings, &default_key).await?;
    #
    // Create transaction and commit
    let txn = database.new_transaction().await?;
    let new_entry_id = txn.commit().await?;
    println!("Changes committed. New state represented by Entry: {}", new_entry_id);
    # Ok(())
    # }
    ```

    _After `commit()`, the `txn` variable is no longer valid._

## Convenience: `with_transaction`

For the common pattern of "create a transaction, do some work, commit," `Database::with_transaction` handles the lifecycle for you:

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{backend::database::Sqlite, Instance, crdt::Doc, store::Table};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct User {
#     name: String,
# }
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Sqlite::in_memory().await?;
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(backend),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "test");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
#
// Insert a record and get its generated key
let user_id = database.with_transaction(|txn| async move {
    let users = txn.get_store::<Table<User>>("users").await?;
    users.insert(User { name: "Alice".into() }).await
}).await?;

// Multiple operations in one atomic transaction
database.with_transaction(|txn| async move {
    let users = txn.get_store::<Table<User>>("users").await?;
    users.insert(User { name: "Bob".into() }).await?;
    users.insert(User { name: "Charlie".into() }).await?;
    Ok(())
}).await?;
# Ok(())
# }
```

The closure receives the `Transaction`, and `with_transaction` commits it after the closure returns `Ok`. If the closure returns `Err`, the transaction is dropped without committing — no partial writes.

Use `with_transaction` when you don't need the commit ID. If you need the `ID` of the committed entry (e.g., for tracking DAG structure), use `new_transaction()` and `commit()` directly.

## Managing Database Settings

Within transactions, you can manage database settings using `SettingsStore`. This provides type-safe access to database configuration and authentication settings:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::generate_keypair;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "settings_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Generate keypairs for old user and add it first so we can revoke it
# let (_old_user_signing_key, old_user_public_key) = generate_keypair();
# let old_user_key = AuthKey::active(Some("old_device"), Permission::Write(15));
# let setup_txn = database.new_transaction().await?;
# let setup_store = setup_txn.get_settings()?;
# setup_store.set_auth_key(&old_user_public_key, old_user_key).await?;
# setup_txn.commit().await?;
let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Update database name
settings_store.set_name("Production Database").await?;

// Generate keypairs for new users (hidden in production code)
# let (_new_account_signing_key, new_account_public_key) = generate_keypair();
# let (_alice_signing_key, alice_public_key) = generate_keypair();

// Add authentication keys (indexed by pubkey, name is optional metadata)
let new_account_key = AuthKey::active(Some("new_device"), Permission::Write(10));
settings_store.set_auth_key(&new_account_public_key, new_account_key).await?;

// Set auth keys directly
let alice_key = AuthKey::active(Some("alice_laptop"), Permission::Write(5));
settings_store.set_auth_key(&alice_public_key, alice_key).await?;
settings_store.revoke_auth_key(&old_user_public_key).await?;

transaction.commit().await?;
# Ok(())
# }
```

This ensures that settings changes are atomic and properly authenticated alongside other database modifications.

## Height Strategies

Each Entry in Eidetica has a **height** value. When branches diverge and later merge (e.g., after a network split), heights determine the order entries are processed. Lower heights come first; ties are broken by entry hash. The height strategy determines how this value is calculated during commit.

### Available Strategies

| Strategy                | Calculation                     | Use Case                            |
| ----------------------- | ------------------------------- | ----------------------------------- |
| `Incremental` (default) | `max(parent_heights) + 1`       | Offline-first apps, simple ordering |
| `Timestamp`             | `max(timestamp_ms, parent + 1)` | Time-series data, audit logs        |

### Database-Level Strategy

Configure the height strategy for the entire database via `SettingsStore`:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, HeightStrategy};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "height_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
let txn = database.new_transaction().await?;
let settings = txn.get_settings()?;

// Set timestamp-based heights for time-series data
settings.set_height_strategy(HeightStrategy::Timestamp).await?;

txn.commit().await?;
# Ok(())
# }
```

### Per-Subtree Height Strategy

Individual stores can override the database strategy for independent height tracking:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, HeightStrategy, Store, store::DocStore};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "per_subtree_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
let txn = database.new_transaction().await?;

// This store uses its own incremental counter
let audit_log = txn.get_store::<DocStore>("audit_log").await?;
audit_log.set_height_strategy(Some(HeightStrategy::Incremental)).await?;
audit_log.set("event", "user_login").await?;

// This store inherits the database strategy
let messages = txn.get_store::<DocStore>("messages").await?;
messages.set("content", "Hello").await?;

txn.commit().await?;
# Ok(())
# }
```

### How Height Inheritance Works

Subtrees with no explicit height strategy inherit from the database-level strategy:

- **No explicit height** (`None`): The subtree inherits from the tree (database) height
- **Explicit height** (`Some(h)`): The subtree has an independent height value

When querying `Entry.subtree_height()`, the returned value reflects this inheritance transparently.

## Read-Only Access

While `Transaction`s are essential for writes, you can perform reads without an explicit `Transaction` using `Database::get_store_viewer`:

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{backend::database::Sqlite, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct User {
#     name: String,
# }
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database with some data
# let backend = Sqlite::in_memory().await?;
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(backend),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "test");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Insert test data
# let txn = database.new_transaction().await?;
# let users_store = txn.get_store::<Table<User>>("users").await?;
# let user_id = users_store.insert(User { name: "Alice".to_string() }).await?;
# txn.commit().await?;
#
let users_viewer = database.get_store_viewer::<Table<User>>("users").await?;
if let Ok(_user) = users_viewer.get(&user_id).await {
    // Read data based on the current tips of the 'users' store
}
# Ok(())
# }
```

A `SubtreeViewer` provides read-only access based on the latest committed state (tips) of that specific store at the time the viewer is created. It does _not_ allow modifications and does not require a `commit()`.

Choose `with_transaction` for the common case of writing to one or more stores and committing. Use `new_transaction()` directly when you need the commit ID or complex control flow. Choose `get_store_viewer` for simple, read-only access to the latest state.

## Write Callbacks

`Database::on_write` delivers the registering handle's latest selected Snapshot.
The default view contains only the Verified prefix; `.allow_unverified()` watches the non-Failed frontier instead.
Each registration receives an initial Snapshot, even on a quiet database, atomically ordered with subsequent mutations.
Later notifications occur only when that selected frontier changes.

Keep your own processing checkpoint and advance it only after processing succeeds:

```rust,no_run
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Database, Result, Snapshot};
# use std::sync::Arc;
# async fn example(database: &Database) -> Result<()> {
let checkpoint = Arc::new(tokio::sync::Mutex::new(Snapshot::EMPTY));
let cb = database.on_write(move |latest, db| {
    let latest = latest.clone();
    let db = db.clone();
    let checkpoint = checkpoint.clone();
    async move {
        let mut processed = checkpoint.lock().await;
        let ids = db.ids_added(&processed, &latest).await?;
        println!("{} candidate entries for {}", ids.len(), db.root_id());
        // Advance only after your application's processing succeeds.
        *processed = latest;
        Ok(())
    }
}).await?;
// Drop `cb` to cancel, or call `cb.detach()` to keep the registration
// for the Instance's lifetime.
# Ok(())
# }
```

The payload is a Snapshot only, not a previous/latest bracket or entry stream.
`ids_added` retains its existing ancestry-diff behavior, including possible over-reporting for interior forks; watch delivery does not add an exact paginated diff API.
For an opt-in frontier that moves backward after rejection, rebuild your selected view at the delivered Snapshot rather than treating an added-ID list as a removal stream.
A historical Snapshot still names the same immutable ancestry after verification status changes.

### Timing and coalescing

Both local and service-backed watches run asynchronously to `commit().await`.
The initial Snapshot is delivered first; slow callbacks may skip intermediate frontiers, retaining only the latest pending update per registration.
Callbacks run sequentially within one registration, outside tree locks, including the closure's synchronous prefix.
A slow callback does not hold up commits or another registration on the same tree.
The callback receives a read-only Database with the registering handle's view and service acting identity; capture a keyed handle separately if you need to write.
Errors and panics are logged. They do not acknowledge processing, retry automatically, or move your checkpoint for you.

Dropping the `WriteCallback` cancels local pending and in-flight work.
`.detach()` leaves it registered until the Instance drops.
Connected registrations each have a distinct wire ID, even for the same tree or visibility.
The daemon orders their selected state, but independent coalescing means clients need not receive identical intermediate sequences.
Disconnect releases every daemon registration; dropped handles send deferred cancellation for their own ID.
Core pending watch state is bounded per registration. This does not bound the shared socket writer backlog or the number of subscriptions an application can create.
