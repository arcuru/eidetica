# Operations: Atomic Changes

In Eidetica, all modifications to the data stored within a `Tree`'s `Subtree`s happen through an **`Operation`**. This is a fundamental concept ensuring atomicity and providing a consistent mechanism for interacting with your data.

Internally, the `Operation` corresponds to the `AtomicOp` struct.

## Why Operations?

Operations provide several key benefits:

- **Atomicity**: Changes made to multiple `Subtree`s within a single `Operation` are committed together as one atomic unit. If the `commit()` fails, no changes are persisted. This is similar to transactions in traditional databases.
- **Consistency**: An `Operation` captures a snapshot of the `Tree`'s state (specifically, the tips of the relevant `Subtree`s) when it's created or when a `Subtree` is first accessed within it. All reads and writes within that `Operation` occur relative to this consistent state.
- **Change Staging**: Modifications made via `Subtree` handles are staged within the `Operation` object itself, not written directly to the backend until `commit()` is called.
- **History Creation**: A successful `commit()` results in the creation of a _new `Entry`_ in the `Tree`, containing the staged changes and linked to the previous state (the tips the `Operation` was based on). This is how history is built.

## The Operation Lifecycle

Using an `Operation` follows a distinct lifecycle:

1.  **Creation**: Start an operation from a `Tree` instance.
    ```rust
    let tree: Tree = /* obtain Tree instance */;
    let op = tree.new_operation()?;
    ```
2.  **Subtree Access**: Get handles to the specific `Subtree`s you want to interact with. This implicitly loads the current state (tips) of that subtree into the operation if accessed for the first time.
    ```rust
    // Get handles within a scope or manage their lifetime
    let users_store = op.get_subtree::<RowStore<User>>("users")?;
    let config_store = op.get_subtree::<KVStore>("config")?;
    ```
3.  **Staging Changes**: Use the methods provided by the `Subtree` handles (`set`, `insert`, `get`, `remove`, etc.). These methods interact with the data staged _within the `Operation`_.
    ```rust
    users_store.insert(User { /* ... */ })?;
    let current_name = users_store.get(&user_id)?;
    config_store.set("last_updated", Utc::now().to_rfc3339())?;
    ```
    _Note: `get` methods within an operation read from the staged state, reflecting any changes already made within the same operation._
4.  **Commit**: Finalize the changes. This consumes the `Operation` object, calculates the final `Entry` content based on staged changes, writes the new `Entry` to the `Backend`, and returns the `ID` of the newly created `Entry`.
    ```rust
    let new_entry_id = op.commit()?;
    println!("Changes committed. New state represented by Entry: {}", new_entry_id);
    ```
    _After `commit()`, the `op` variable is no longer valid._

## Read-Only Access

While `Operation`s are essential for writes, you can perform reads without an explicit `Operation` using `Tree::get_subtree_viewer`:

```rust
let users_viewer = tree.get_subtree_viewer::<RowStore<User>>("users")?;
if let Some(user) = users_viewer.get(&user_id)? {
    // Read data based on the current tips of the 'users' subtree
}
```

A `SubtreeViewer` provides read-only access based on the latest committed state (tips) of that specific subtree at the time the viewer is created. It does _not_ allow modifications and does not require a `commit()`.

Choose `Operation` when you need to make changes or require a transaction-like boundary for multiple reads/writes. Choose `SubtreeViewer` for simple, read-only access to the latest state.
