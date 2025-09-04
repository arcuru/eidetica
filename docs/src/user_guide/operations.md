# Operations: Atomic Changes

In Eidetica, all modifications to the data stored within a `Database`'s `Store`s happen through an **`Operation`**. This is a fundamental concept ensuring atomicity and providing a consistent mechanism for interacting with your data.

**Authentication Note**: All operations in Eidetica are authenticated by default. Every operation uses the database's default signing key to ensure that all changes are cryptographically verified and can be traced to their source.

Internally, the `Operation` corresponds to the `Transaction` struct.

## Why Operations?

Operations provide several key benefits:

- **Atomicity**: Changes made to multiple `Store`s within a single `Operation` are committed together as one atomic unit. If the `commit()` fails, no changes are persisted. This is similar to transactions in traditional databases.
- **Consistency**: An `Operation` captures a snapshot of the `Database`'s state (specifically, the tips of the relevant `Store`s) when it's created or when a `Store` is first accessed within it. All reads and writes within that `Operation` occur relative to this consistent state.
- **Change Staging**: Modifications made via `Store` handles are staged within the `Operation` object itself, not written directly to the database until `commit()` is called.
- **Authentication**: All operations are automatically authenticated using the database's default signing key, ensuring data integrity and access control.
- **History Creation**: A successful `commit()` results in the creation of a _new `Entry`_ in the `Database`, containing the staged changes and linked to the previous state (the tips the `Operation` was based on). This is how history is built.

## The Operation Lifecycle

Using an `Operation` follows a distinct lifecycle:

1.  **Creation**: Start an authenticated operation from a `Database` instance.
    ```rust,ignore
    let database: Database = /* obtain Database instance */;
    let op = database.new_operation()?; // Automatically uses the database's default signing key
    ```
2.  **Store Access**: Get handles to the specific `Store`s you want to interact with. This implicitly loads the current state (tips) of that store into the operation if accessed for the first time.
    ```rust,ignore
    // Get handles within a scope or manage their lifetime
    let users_store = op.get_subtree::<Table<User>>("users")?;
    let config_store = op.get_subtree::<DocStore>("config")?;
    ```
3.  **Staging Changes**: Use the methods provided by the `Store` handles (`set`, `insert`, `get`, `remove`, etc.). These methods interact with the data staged _within the `Operation`_.
    ```rust,ignore
    users_store.insert(User { /* ... */ })?;
    let current_name = users_store.get(&user_id)?;
    config_store.set("last_updated", Utc::now().to_rfc3339())?;
    ```
    _Note: `get` methods within an operation read from the staged state, reflecting any changes already made within the same operation._
4.  **Commit**: Finalize the changes. This consumes the `Operation` object, calculates the final `Entry` content based on staged changes, cryptographically signs the entry, writes the new `Entry` to the `Database`, and returns the `ID` of the newly created `Entry`.
    ```rust,ignore
    let new_entry_id = op.commit()?;
    println!("Changes committed. New state represented by Entry: {}", new_entry_id);
    ```
    _After `commit()`, the `op` variable is no longer valid._

## Read-Only Access

While `Operation`s are essential for writes, you can perform reads without an explicit `Operation` using `Database::get_subtree_viewer`:

```rust,ignore
let users_viewer = database.get_subtree_viewer::<Table<User>>("users")?;
if let Some(user) = users_viewer.get(&user_id)? {
    // Read data based on the current tips of the 'users' store
}
```

A `SubtreeViewer` provides read-only access based on the latest committed state (tips) of that specific store at the time the viewer is created. It does _not_ allow modifications and does not require a `commit()`.

Choose `Operation` when you need to make changes or require a transaction-like boundary for multiple reads/writes. Choose `SubtreeViewer` for simple, read-only access to the latest state.
