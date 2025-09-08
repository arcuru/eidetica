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
    extern crate eidetica;
    use eidetica::{backend::database::InMemory, Instance, crdt::Doc};

    fn main() -> eidetica::Result<()> {
        // Setup database
        let backend = InMemory::new();
        let db = Instance::new(Box::new(backend));
        db.add_private_key("key")?;
        let mut settings = Doc::new();
        settings.set_string("name", "test");
        let database = db.new_database(settings, "key")?;

        let _txn = database.new_transaction()?; // Automatically uses the database's default signing key
        Ok(())
    }
    ```

2.  **Store Access**: Get handles to the specific `Store`s you want to interact with. This implicitly loads the current state (tips) of that store into the transaction if accessed for the first time.

    ```rust
    extern crate eidetica;
    extern crate serde;
    use eidetica::{backend::database::InMemory, Instance, crdt::Doc, store::{Table, DocStore}, Database};
    use serde::{Serialize, Deserialize};

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct User {
        name: String,
    }

    fn main() -> eidetica::Result<()> {
        // Setup database and transaction
        let backend = InMemory::new();
        let db = Instance::new(Box::new(backend));
        db.add_private_key("key")?;
        let mut settings = Doc::new();
        settings.set_string("name", "test");
        let database = db.new_database(settings, "key")?;
        let txn = database.new_transaction()?;

        // Get handles within a scope or manage their lifetime
        let _users_store = txn.get_store::<Table<User>>("users")?;
        let _config_store = txn.get_store::<DocStore>("config")?;

        txn.commit()?;
        Ok(())
    }
    ```

3.  **Staging Changes**: Use the methods provided by the `Store` handles (`set`, `insert`, `get`, `remove`, etc.). These methods interact with the data staged _within the `Transaction`_.

    ```rust
    extern crate eidetica;
    extern crate serde;
    extern crate chrono;
    use eidetica::store::{Table, DocStore};
    use serde::{Serialize, Deserialize};

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct User {
        name: String,
    }

    fn example(users_store: &Table<User>, config_store: &DocStore, user_id: &str) -> eidetica::Result<()> {
        users_store.insert(User { name: "Alice".to_string() })?;
        let _current_name = users_store.get(user_id)?;
        config_store.set("last_updated", chrono::Utc::now().to_rfc3339())?;
        Ok(())
    }
    ```

    _Note: `get` methods within a transaction read from the staged state, reflecting any changes already made within the same transaction._

4.  **Commit**: Finalize the changes. This consumes the `Transaction` object, calculates the final `Entry` content based on staged changes, cryptographically signs the entry, writes the new `Entry` to the `Database`, and returns the `ID` of the newly created `Entry`.

    ```rust
    extern crate eidetica;
    use eidetica::{backend::database::InMemory, Instance, crdt::Doc};

    fn main() -> eidetica::Result<()> {
        // Setup database
        let backend = InMemory::new();
        let db = Instance::new(Box::new(backend));
        db.add_private_key("key")?;
        let mut settings = Doc::new();
        settings.set_string("name", "test");
        let database = db.new_database(settings, "key")?;

        // Create transaction and commit
        let txn = database.new_transaction()?;
        let new_entry_id = txn.commit()?;
        println!("Changes committed. New state represented by Entry: {}", new_entry_id);
        Ok(())
    }
    ```

    _After `commit()`, the `txn` variable is no longer valid._

## Read-Only Access

While `Transaction`s are essential for writes, you can perform reads without an explicit `Transaction` using `Database::get_subtree_viewer`:

```rust
extern crate eidetica;
extern crate serde;
use eidetica::{Database, store::Table};
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct User {
    name: String,
}

fn example(database: Database, user_id: &str) -> eidetica::Result<()> {
    let users_viewer = database.get_store_viewer::<Table<User>>("users")?;
    if let Ok(_user) = users_viewer.get(user_id) {
        // Read data based on the current tips of the 'users' store
    }
    Ok(())
}
```

A `SubtreeViewer` provides read-only access based on the latest committed state (tips) of that specific store at the time the viewer is created. It does _not_ allow modifications and does not require a `commit()`.

Choose `Transaction` when you need to make changes or require a transaction-like boundary for multiple reads/writes. Choose `SubtreeViewer` for simple, read-only access to the latest state.
