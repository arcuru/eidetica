# Database Sharing

A `Database` opened or created through an `AccountSession` carries that caller's private
sharing capability. Its sharing methods work in embedded and service mode while
keeping two kinds of state separate:

1. Database settings are replicated database-owned state, accessed through
   `Database::get_settings()`.
2. `SyncSettings` are this caller's durable installation-local preferences; they are not signed Account Entries.

Plain `Database` handles remain valid for ordinary reads and transactions, but
cannot manage a user's private sharing preference. Their sharing methods return
an explicit missing-capability error.

## Share a Database

`AccountSession::create_database` returns a capability-backed handle. For an existing
tracked database, use `AccountSession::open_database`.

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, NewAccount, crdt::Doc};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let (instance, user) = Instance::connect_or_create(
    "memory://",
    NewAccount::passwordless("alice"),
).await?;
instance.enable_sync().await?;
let mut user = user.expect("memory backend is new");
let key = user.get_default_key()?;
let database = user.create_database(Doc::new(), &key).await?;

database.share().await?;

assert!(database.is_shared().await?);
println!("share {}", database.ticket().await?);
# Ok(())
# }
```

A successful call acknowledges durable native preference state. It does not say
that the daemon has reconciled its internal configuration or is advertising an
address. If the connection fails while committing, the write may have reached
the owner. Safely retry it or read `sync_settings()`.

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

## Inspect a Tracking Preference

Use `AccountSession::tracked_database(&database_id).await` to inspect one
tracking row without opening or tracking the target database. `Ok(Some(row))`
contains this account's preferences, including `row.sync_settings.sync_enabled`;
`Ok(None)` means the row is absent. Authorization, storage read and decoding
failures remain errors, not an absent row or a disabled preference.

This read works in embedded and service mode through the session's existing
Account access. It does not enumerate tracked databases, change keys or settings,
or grant access to the target database. A cold read may populate disposable
Store-state caches. As with other service reads, daemon authorization failures
use the existing service error representation.

```rust
# use eidetica::{AccountSession, ID};
async fn local_sync_preference(
    session: &AccountSession,
    database_id: &ID,
) -> eidetica::Result<Option<bool>> {
    Ok(session.tracked_database(database_id).await?
        .map(|row| row.sync_settings.sync_enabled))
}
```

## Stop Sharing

`database.stop_sharing()` withdraws only this caller's preference and preserves
their other sync settings. Another user on the same instance can keep the daemon
serving the database. The combined daemon state is intentionally not exposed.

## Database-Centric Sharing

Sharing controls live on the `Database` handle returned by `AccountSession`. Use
`share`, `stop_sharing`, `sync_settings`, and `ticket` on that handle; `AccountSession`
does not expose ID-based sharing helpers.

Treat the returned ticket as a point-in-time locator, not proof that the
preference was applied or that a peer can connect.
