# Database Sharing and Management

Applications manage a tracked database through `User::manage_database`. The
returned `DatabaseManagement` view works in both embedded and service mode and
keeps four separate facts explicit:

1. **Desired state** is this user's signed, durable sharing preference.
2. **Applied state** says whether the owner has reconciled that preference.
3. **Observed state** describes the current owner run, addresses, and peers.
4. **Ticket readiness** says whether a ticket can currently name a live address.

These are not one atomic operation. A durable preference can be accepted before
the owner applies it, and the owner can apply it before a transport has a live
address.

## Share a Tracked Database

The database must already be tracked. `User::create_database` does that for a
new database; use `track_database` when adopting an existing one.

```rust
# extern crate eidetica;
# extern crate tokio;
# use std::time::Duration;
# use eidetica::{Instance, NewUser, crdt::Doc};
# use eidetica::user::{AppliedState, PreferenceWriteOutcome, TicketStatus};
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
let snapshot = management.snapshot().await?;
let watch = management.watch().await?;
assert_eq!(
    watch.current().desired.sync_enabled,
    snapshot.desired.sync_enabled
);
match management.share().await? {
    PreferenceWriteOutcome::Written(receipt) => {
        println!("sharing preference accepted: {:?}", receipt.entry_id);
    }
    PreferenceWriteOutcome::Unknown { source } => {
        // The connection failed after submission may have reached the owner.
        // Read back the preference or retry this idempotent write.
        eprintln!("sharing outcome unknown: {source}");
    }
}

let applied = management.wait_for(Duration::from_secs(1), |snapshot| {
    snapshot.applied == AppliedState::Current
}).await?;

if applied.is_some() {
    match management.ticket().await? {
        TicketStatus::Ready(ticket) => println!("share {ticket}"),
        TicketStatus::NotReady(reason) => {
            eprintln!("preference applied, but ticket not ready: {reason:?}");
        }
    }
}
# Ok(())
# }
```

`Written` acknowledges a durable signed preference entry. It does not say that
the owner has applied the setting or that a ticket is ready. `Unknown` means the
write may have reached the owner before the connection failed. Because the write
is idempotent, safely retry it or call `snapshot()` to read back `desired`.

## Snapshot, Watch, and Wait

- `snapshot()` returns the current desired, effective, applied, and observed
  state after checking the caller still has Read access.
- `watch()` returns an immediate snapshot through `current()` and coalesces
  later database, preference, and runtime changes through `changed()`.
- `wait_for(timeout, predicate)` waits without changing preferences and returns
  `None` on timeout.
- `ticket()` never changes preferences. `TicketStatus::Ready` contains the
  database ID and addresses advertised by the current owner run.

Treat `observed.owner_run` as opaque. Generations are monotonic only within that
run. `RuntimeFreshness::Stale` or `Unknown` means cached runtime fields must not
be treated as current. `TicketStatus::NotReady` explains whether the blocker is
owner availability, pending application, disabled effective sharing, no live
address, or unknown runtime state.

A ticket is a database locator with address hints. It does not grant access.
The receiving peer still needs the database's authorization or bootstrap and
approval flow.

## Stop Sharing

`management.stop_sharing()` withdraws only this user's preference. Another user
on the same instance can keep the owner serving the database, so confirm the
result through `snapshot().effective` when host-wide state matters.

## Migrating from Deprecated User Helpers

The compatibility methods still work, but their single return values hide the
separate stages above:

| Deprecated method        | Replacement                                            |
| ------------------------ | ------------------------------------------------------ |
| `User::enable_sync(id)`  | `User::manage_database(id).await?.share()`             |
| `User::disable_sync(id)` | `User::manage_database(id).await?.stop_sharing()`      |
| `User::share(id)`        | `share()`, then `wait_for` or `watch`, then `ticket()` |

Handle `PreferenceWriteOutcome` rather than converting `Unknown` into a definite
failure. Handle `TicketStatus::NotReady` as a current readiness result rather
than assuming the preference was rolled back. `User::is_sync_enabled`,
`track_database`, `untrack_database`, and the lower-level `Sync` APIs remain
available for their existing purposes.
