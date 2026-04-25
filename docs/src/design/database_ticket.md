# DatabaseTicket: URL-based Shareable Database Links

## Overview

A `DatabaseTicket` encodes a database ID and optional peer address hints into a
compact, shareable URL. Peers exchange a single link to initiate synchronization
rather than coordinating database IDs and transport addresses separately.

## URL Format

Magnet-style URI with query parameters:

```text
eidetica:?db=<database_id>&pr=<transport_address>&pr=<transport_address>
```

- **Scheme**: `eidetica`
- **`db`** (required): The database ID, passed through as an opaque string
- **`pr`** (optional, repeatable): A self-describing peer address

### Database ID

The database ID is stored as-is in the `db` query parameter, no encoding or
transformation is applied. The ID is a multibase-encoded
[CID (Content Identifier)][cid-spec] string (e.g., `bafyr4i...` for
base32lower). The ticket format does not need to understand the ID's internal
structure.

[cid-spec]: https://github.com/multiformats/cid

### Peer Address Hints

Each `pr` parameter contains a peer address prefixed by its transport
name and a colon. The transport's native encoding follows the colon:

- `http:192.168.1.1:8080` — HTTP transport (plain `host:port`)
- `iroh:endpointABC...` — Iroh transport (`EndpointTicket` format)

The first `:` separates the transport name from the transport-specific
address. Each transport uses its own native encoding — for iroh this is
the standard `EndpointTicket` format (postcard + base32-lower with
`endpoint` prefix).

Multiple `pr` parameters are supported, including multiple addresses for the
same transport type (e.g., different network interfaces).

### Examples

Database ID only (no hints):

```text
eidetica:?db=bafyr4ihkr4ld3m4gqkjf4reryxsy2s5tkbxprqkow6fin2iiyvreuzzab4
```

With transport hints:

```text
eidetica:?db=bafyr4i...&pr=iroh:endpoint...&pr=http:192.168.1.1:8080
```

## Future Features

### `tips` Parameter

An optional `tips` query parameter could reference a database at a specific
state. A database state is defined by its complete set of tips, so tips are
encoded as a single parameter value rather than repeated parameters (unlike
`pr`, where each address is independent). A count prefix makes truncation
detectable:

```text
tips=<count>:<tip_id>,<tip_id>,...
```

For example, `tips=2:bafyr4i...,bafyr4i...`. If the count doesn't match the
number of parsed tip IDs the parameter is discarded as truncated. A receiver
can use the tips to sync to a known state or verify they reached the expected
point. Not needed for normal use.

A set of `tips` would **fully define the database state**.

## Implementation

### `DatabaseTicket` struct

<!-- Code block ignored: API signature illustration, not compilable standalone -->

```rust,ignore
pub struct DatabaseTicket {
    database_id: ID,
    addresses: Vec<Address>,
}
```

`DatabaseTicket` reuses the existing `Address` type from the sync module,
which already bundles a transport type identifier with an address string.

### Serialization

The `Display` implementation uses minimal percent-encoding — only characters
that are structurally significant in query strings (`&`, `=`, `#`, `+`, `%`)
are encoded. Colons and other characters pass through verbatim, keeping
tickets human-readable.

Parsing uses `url::form_urlencoded::parse()` which handles both encoded and
unencoded input, so tickets produced by other implementations with more
aggressive encoding are accepted.

### `Sync::create_ticket`

Generates a ticket for a database using all running transports:

<!-- Code block ignored: API signature illustration, not compilable standalone -->

```rust,ignore
pub async fn create_ticket(&self, database_id: &ID) -> Result<DatabaseTicket>
```

Collects server addresses from all running transports and bundles them with
the database ID into a `DatabaseTicket`. Lower-level helper that does not
touch user-level sync preferences.

### `User::share`

Atomic high-level API for the common "make this database shareable and hand
out a ticket" operation:

<!-- Code block ignored: API signature illustration, not compilable standalone -->

```rust,ignore
pub async fn share(&mut self, database_id: &ID) -> Result<DatabaseTicket>
```

Equivalent to calling [`User::enable_sync`](./users.md) followed by
`Sync::create_ticket`, but as a single call so that a `track_database` →
`enable_sync` → ticket-build sequence can't be accidentally split. This is
the recommended way to produce a shareable ticket.

Preconditions (sync attached to the instance, database tracked by the user)
are checked before any state is mutated, so a failed `share()` leaves the
user's sync preference unchanged. Errors:

- `SyncError::SyncNotEnabled` — sync is not attached to the instance.
- `SyncError::NoTransportEnabled` — sync is attached but no transport
  has been registered yet.
- `UserError::DatabaseNotTracked` — the database is not in the user's
  tracked list.

## Forward Compatibility

Unknown query parameters are silently ignored during parsing. This allows
future parameters to be added without breaking existing parsers. Malformed
`pr` values (missing `:` separator) are also silently skipped.
