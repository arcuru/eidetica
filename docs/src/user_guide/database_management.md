# Database Sharing and Management

Applications manage a tracked database through `User::manage_database`.
The returned `DatabaseManagement` view works in embedded and service mode while
keeping two operations separate:

1. **User settings** are this user's signed, durable sharing preference.
2. **Ticket lookup** is a point-in-time query of whether the owner can currently
   return a usable database locator.

The settings view does not expose the daemon's private combined configuration,
other users' preferences, peer telemetry, or runtime state.

## Share a Tracked Database

The database must already be tracked. `User::create_database` does that for a
new database; use `track_database` when adopting an existing one.

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, NewUser, crdt::Doc};
# use eidetica::user::PreferenceWriteOutcome;
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let (instance, user) = Instance::connect_or_create(
    "memory://",
    NewUser::passwordless("alice"),
).await?;
instance.enable_sync().await?;
let mut user = user.expect("memory backend is new");
let key = user.get_default_key()?;
let database = user.create_database(Doc::new(), &key).await?;

let management = user.manage_database(database.root_id()).await?;
let initial = management.snapshot().await?;
assert_eq!(initial.source, user.user_database().snapshot().await?);

match management.share().await? {
    PreferenceWriteOutcome::Written(receipt) => {
        println!("sharing preference accepted: {:?}", receipt.entry_id);
    }
    PreferenceWriteOutcome::Unknown { source } => {
        // Submission may have reached the owner. Read back this user's
        // preference or retry the idempotent write.
        eprintln!("sharing outcome unknown: {source}");
    }
}

assert!(management.snapshot().await?.settings.sync_enabled);
println!("share {}", management.ticket().await?);
# Ok(())
# }
```

`Written` acknowledges a durable signed preference entry. It does not say that
the daemon has reconciled its internal combined configuration or that a ticket
is ready. `Unknown` means the write may have reached the owner before the
connection failed. Safely retry it or call `snapshot()` to read back `settings`.

## Snapshot, Watch, and Wait

- `snapshot()` reads this user's settings at one real user-database `Snapshot`.
  The returned `source` is the exact snapshot used for the read.
- `watch()` returns that initial value through `current()` and uses the normal
  database callback path for later user-database changes.
- `wait_for(timeout, predicate)` observes the same stream without changing
  settings and returns `None` on timeout.
- Target-database authorization changes end the watch. Dropping a watch removes
  its normal database callbacks; disconnect cleanup is the same as any other
  service callback.

These methods do not promise cross-database atomicity. Runtime-only changes such
as a transport starting or stopping do not advance the settings watch.

`ticket()` is separate and never changes settings. It first checks this user's
pinned sharing setting and returns an error when that setting is disabled, even
if another user keeps the daemon serving the database. When enabled, it returns
a point-in-time `DatabaseTicket` with the database ID and whatever owner
addresses are currently available. It does not inspect daemon combined settings
or promise serving, reconciliation, or future reachability.

A ticket is a locator with address hints, not an access grant. The receiver still
needs database authorization or the bootstrap and approval flow.

## Stop Sharing

`management.stop_sharing()` withdraws only this user's preference and preserves
their other sync settings. Another user on the same instance can keep the daemon
serving the database. That combined daemon state is intentionally not exposed by
this user-scoped view.

## Migrating from Deprecated User Helpers

| Deprecated method        | Replacement                                       |
| ------------------------ | ------------------------------------------------- |
| `User::enable_sync(id)`  | `User::manage_database(id).await?.share()`        |
| `User::disable_sync(id)` | `User::manage_database(id).await?.stop_sharing()` |
| `User::share(id)`        | `share()`, then separately query `ticket()`       |

Handle `PreferenceWriteOutcome` rather than converting `Unknown` into a definite
failure. Treat the returned ticket as a point-in-time locator, not proof that the
preference was applied or that a peer can connect. `User::is_sync_enabled`,
`track_database`, `untrack_database`, and the lower-level `Sync` APIs remain
available for their existing purposes.
