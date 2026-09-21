# Database Sharing

A `Database` opened or created through a `User` carries that caller's private
sharing capability. Its sharing methods work in embedded and service mode while
keeping two kinds of state separate:

1. Database settings are replicated database-owned state, accessed through
   `Database::get_settings()`.
2. `SyncSettings` are this caller's signed, durable private preferences.

Plain `Database` handles remain valid for ordinary reads and transactions, but
cannot manage a user's private sharing preference. Their sharing methods return
an explicit missing-capability error.

## Share a Database

`User::create_database` returns a capability-backed handle. For an existing
tracked database, use `User::open_database`.

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

match database.share().await? {
    PreferenceWriteOutcome::Written(receipt) => {
        println!("sharing preference accepted: {:?}", receipt.entry_id);
    }
    PreferenceWriteOutcome::Unknown { source } => {
        // Submission may have reached the owner. Read back the setting or
        // retry the idempotent write.
        eprintln!("sharing outcome unknown: {source}");
    }
}

assert!(database.is_shared().await?);
println!("share {}", database.ticket().await?);
# Ok(())
# }
```

`Written` acknowledges a durable signed preference entry. It does not say that
the daemon has reconciled its internal configuration or is advertising an address.
`Unknown` means the write may have reached the owner before the connection
failed. Safely retry it or read `sync_settings()`.

## Settings and Tickets

`sync_settings()` reads only this caller's private preference. It does not
expose the daemon's configuration, another user's preferences, peer telemetry,
or runtime state. `is_shared()` returns its `sync_enabled` field.

`ticket()` is a separate point-in-time query and never changes settings. It
requires this caller's sharing setting to be enabled, even if another user keeps
the daemon serving the database. When enabled, it returns a `DatabaseTicket`
with the database ID and currently available owner addresses. It does not
inspect daemon configuration or promise serving, reconciliation, or future
reachability.

A ticket is a locator with address hints, not an access grant. The receiver
still needs database authorization or the bootstrap and approval flow.

## Stop Sharing

`database.stop_sharing()` withdraws only this caller's preference and preserves
their other sync settings. Another user on the same instance can keep the daemon
serving the database. The combined daemon state is intentionally not exposed.

## Migrating from Deprecated User Helpers

| Deprecated method        | Replacement                                           |
| ------------------------ | ----------------------------------------------------- |
| `User::enable_sync(id)`  | `user.open_database(id).await?.share().await?`        |
| `User::disable_sync(id)` | `user.open_database(id).await?.stop_sharing().await?` |
| `User::share(id)`        | `share()`, then separately call `ticket()`            |

Handle `PreferenceWriteOutcome` rather than converting `Unknown` into a
definite failure. Treat the returned ticket as a point-in-time locator, not
proof that the preference was applied or that a peer can connect.
