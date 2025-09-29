# Transactions: Atomic Changes

In Eidetica, all modifications to the data stored within a `Database`'s `Store`s happen through an **`Transaction`**. This is a fundamental concept ensuring atomicity and providing a consistent mechanism for interacting with your data.

**Authentication Note**: All transactions in Eidetica are authenticated by default. Every transaction uses the database's default signing key to ensure that all changes are cryptographically verified and can be traced to their source.

A `Transaction` bundles multiple Store operations (which affect individual subtrees) into a single atomic Entry that gets committed to the database.

## Why Transactions?

Transactions provide several key benefits:

- **Atomicity**: Changes made to multiple `Store`s within a single `Transaction` are committed together as one atomic unit. If the `commit()` fails, no changes are persisted. This is similar to transactions in traditional databases.
- **Consistency**: A `Transaction` captures a snapshot of the `Database`'s state (specifically, the tips of the relevant `Store`s) when it's created or when a `Store` is first accessed within it. All reads and writes within that `Transaction` occur relative to this consistent state.
- **Change Staging**: Modifications made via `Store` handles are staged within the `Transaction` object itself, not written directly to the database until `commit()` is called.
- **Authentication**: All transactions are automatically authenticated using the database's default signing key, ensuring data integrity and access control.
- **History Creation**: A successful `commit()` results in the creation of a _new `Entry`_ in the `Database`, containing the staged changes and linked to the previous state (the tips the `Transaction` was based on). This is how history is built.

## The Transaction Lifecycle

Using a `Transaction` follows a distinct lifecycle:

1.  **Creation**: Start an authenticated transaction from a `Database` instance.

    ```rust
    # extern crate eidetica;
    # use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
    #
    # fn main() -> eidetica::Result<()> {
    # // Setup database
    # let backend = InMemory::new();
    # let db = Instance::new(Box::new(backend));
    # db.add_private_key("key")?;
    # let mut settings = Doc::new();
    # settings.set_string("name", "test");
    # let database = db.new_database(settings, "key")?;
    #
    let _txn = database.new_transaction()?; // Automatically uses the database's default signing key
    # Ok(())
    # }
    ```

2.  **Store Access**: Get handles to the specific `Store`s you want to interact with. This implicitly loads the current state (tips) of that store into the transaction if accessed for the first time.

    ```rust
    # extern crate eidetica;
    # extern crate serde;
    # use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::{Table, DocStore, SettingsStore}, Database};
    # use serde::{Serialize, Deserialize};
    #
    # #[derive(Clone, Debug, Serialize, Deserialize)]
    # struct User {
    #     name: String,
    # }
    #
    # fn main() -> eidetica::Result<()> {
    # // Setup database and transaction
    # let backend = InMemory::new();
    # let db = Instance::new(Box::new(backend));
    # db.add_private_key("key")?;
    # let mut settings = Doc::new();
    # settings.set_string("name", "test");
    # let database = db.new_database(settings, "key")?;
    let txn = database.new_transaction()?;

    // Get handles within a scope or manage their lifetime
    let _users_store = txn.get_store::<Table<User>>("users")?;
    let _config_store = txn.get_store::<DocStore>("config")?;
    let _settings_store = SettingsStore::new(&txn)?;  // For database settings

    txn.commit()?;
    # Ok(())
    # }
    ```

3.  **Staging Changes**: Use the methods provided by the `Store` handles (`set`, `insert`, `get`, `remove`, etc.). These methods interact with the data staged _within the `Transaction`_.

    ```rust
    # extern crate eidetica;
    # extern crate serde;
    # use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::{Table, DocStore, SettingsStore}};
    # use serde::{Serialize, Deserialize};
    #
    # #[derive(Clone, Debug, Serialize, Deserialize)]
    # struct User {
    #     name: String,
    # }
    #
    # fn main() -> eidetica::Result<()> {
    # // Setup database and transaction
    # let backend = InMemory::new();
    # let db = Instance::new(Box::new(backend));
    # db.add_private_key("key")?;
    # let mut settings = Doc::new();
    # settings.set_string("name", "test");
    # let database = db.new_database(settings, "key")?;
    # let txn = database.new_transaction()?;
    # let users_store = txn.get_store::<Table<User>>("users")?;
    # let config_store = txn.get_store::<DocStore>("config")?;
    # let settings_store = SettingsStore::new(&txn)?;
    #
    // Insert a new user and get their ID
    let user_id = users_store.insert(User { name: "Alice".to_string() })?;
    let _current_user = users_store.get(&user_id)?;
    config_store.set("last_updated", "2024-01-15T10:30:00Z")?;
    settings_store.set_name("Updated Database Name")?;  // Manage database settings
    #
    # txn.commit()?;
    # Ok(())
    # }
    ```

    _Note: `get` methods within a transaction read from the staged state, reflecting any changes already made within the same transaction._

4.  **Commit**: Finalize the changes. This consumes the `Transaction` object, calculates the final `Entry` content based on staged changes, cryptographically signs the entry, writes the new `Entry` to the `Database`, and returns the `ID` of the newly created `Entry`.

    ```rust
    # extern crate eidetica;
    # use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
    #
    # fn main() -> eidetica::Result<()> {
    # // Setup database
    # let backend = InMemory::new();
    # let db = Instance::new(Box::new(backend));
    # db.add_private_key("key")?;
    # let mut settings = Doc::new();
    # settings.set_string("name", "test");
    # let database = db.new_database(settings, "key")?;
    #
    // Create transaction and commit
    let txn = database.new_transaction()?;
    let new_entry_id = txn.commit()?;
    println!("Changes committed. New state represented by Entry: {}", new_entry_id);
    # Ok(())
    # }
    ```

    _After `commit()`, the `txn` variable is no longer valid._

## Managing Database Settings

Within transactions, you can manage database settings using `SettingsStore`. This provides type-safe access to database configuration and authentication settings:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
#
# fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set_string("name", "settings_example");
# let database = db.new_database(settings, "admin")?;
# // Generate keypairs for old user and add it first so we can revoke it
# let (_old_user_signing_key, old_user_verifying_key) = generate_keypair();
# let old_user_public_key = format_public_key(&old_user_verifying_key);
# let old_user_key = AuthKey::active(&old_user_public_key, Permission::Write(15))?;
# let setup_txn = database.new_transaction()?;
# let setup_store = SettingsStore::new(&setup_txn)?;
# setup_store.set_auth_key("old_user", old_user_key)?;
# setup_txn.commit()?;
let transaction = database.new_transaction()?;
let settings_store = SettingsStore::new(&transaction)?;

// Update database name
settings_store.set_name("Production Database")?;

// Generate keypairs for new users (hidden in production code)
# let (_new_user_signing_key, new_user_verifying_key) = generate_keypair();
# let new_user_public_key = format_public_key(&new_user_verifying_key);
# let (_alice_signing_key, alice_verifying_key) = generate_keypair();
# let alice_public_key = format_public_key(&alice_verifying_key);

// Add authentication keys
let new_user_key = AuthKey::active(
    &new_user_public_key,
    Permission::Write(10),
)?;
settings_store.set_auth_key("new_user", new_user_key)?;

// Complex auth operations atomically
let alice_key = AuthKey::active(&alice_public_key, Permission::Write(5))?;
settings_store.update_auth_settings(|auth| {
    auth.overwrite_key("alice", alice_key)?;
    auth.revoke_key("old_user")?;
    Ok(())
})?;

transaction.commit()?;
# Ok(())
# }
```

This ensures that settings changes are atomic and properly authenticated alongside other database modifications.

## Read-Only Access

While `Transaction`s are essential for writes, you can perform reads without an explicit `Transaction` using `Database::get_store_viewer`:

```rust
# extern crate eidetica;
# extern crate serde;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::Table, Database};
# use serde::{Serialize, Deserialize};
#
# #[derive(Clone, Debug, Serialize, Deserialize)]
# struct User {
#     name: String,
# }
#
# fn main() -> eidetica::Result<()> {
# // Setup database with some data
# let backend = InMemory::new();
# let db = Instance::new(Box::new(backend));
# db.add_private_key("key")?;
# let mut settings = Doc::new();
# settings.set_string("name", "test");
# let database = db.new_database(settings, "key")?;
# // Insert test data
# let txn = database.new_transaction()?;
# let users_store = txn.get_store::<Table<User>>("users")?;
# let user_id = users_store.insert(User { name: "Alice".to_string() })?;
# txn.commit()?;
#
let users_viewer = database.get_store_viewer::<Table<User>>("users")?;
if let Ok(_user) = users_viewer.get(&user_id) {
    // Read data based on the current tips of the 'users' store
}
# Ok(())
# }
```

A `SubtreeViewer` provides read-only access based on the latest committed state (tips) of that specific store at the time the viewer is created. It does _not_ allow modifications and does not require a `commit()`.

Choose `Transaction` when you need to make changes or require a transaction-like boundary for multiple reads/writes. Choose `SubtreeViewer` for simple, read-only access to the latest state.
