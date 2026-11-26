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
- **`db`** (required): The database ID, parsed as an `ID`
- **`pr`** (optional, repeatable): A self-describing peer address

### Database ID

For a nonempty `db` value, the parser validates the decoded text with
`ID::parse`, which parses a [CID (Content Identifier)][cid-spec]. The ticket
stores the typed `ID`, not the original text. Serialization uses `ID`'s
`Display` representation (base32lower for CIDv1), so it need not reproduce the
input spelling. CID parsing does not verify that the ID names an existing
database root or that anyone can access it.

Every `db` occurrence is parsed in order. If all are accepted, the last one
wins. A malformed nonempty value fails immediately with `Error::Id`, even if
a later `db` is valid. A missing `db` fails with `SyncError::InvalidAddress`
wrapped in `Error::Sync`. A present but empty `db=` is different: `ID::parse`
accepts it as the empty ID, not a CID.

[cid-spec]: https://github.com/multiformats/cid

### Peer Address Hints

Each `pr` parameter contains a peer address prefixed by its transport
name and a colon. The transport's native encoding follows the colon:

- `http:192.168.1.1:8080` — HTTP transport (plain `host:port`)
- `iroh:endpointABC...` — Iroh transport (`EndpointTicket` format)

Multiple `pr` parameters are supported, including multiple addresses for the
same transport type (e.g., different network interfaces).

Parsing splits each decoded `pr` value on the first `:`. Values without that
separator are skipped. Unknown transport names and even empty names or
payloads are retained if the separator is present. Parsing does not check
whether a transport is available, whether it accepts the address, or whether
a peer answers. Bootstrap selects a usable route through a transport and
handshake; a successful handshake does not grant database access.

### Round-trip example

This example uses a sample CID to show the format without creating a database
or contacting a peer. For a real ticket, use the database's root ID.

```rust
use eidetica::{
    ID,
    sync::{Address, DatabaseTicket},
};

let id = ID::from_bytes(b"example ticket ID");
let address = Address::http("192.168.1.1:8080");
let ticket = DatabaseTicket::with_addresses(id.clone(), vec![address.clone()]);
let url = ticket.to_string();
assert_eq!(url, format!("eidetica:?db={id}&pr=http:192.168.1.1:8080"));

let parsed: DatabaseTicket = url.parse().unwrap();
assert_eq!(parsed.database_id(), &id);
assert_eq!(parsed.addresses(), &[address]);
```

## Proposed: `tips` parameter

The `tips` parameter is a proposal, not implemented. Its purpose would be to
reference a database at a specific state using the complete set of tips.
The proposed format puts the set in a single parameter, unlike independent
`pr` values. A count prefix would let a reader detect a missing tip:

```text
tips=<count>:<tip_id>,<tip_id>,...
```

For example, `tips=2:bafyr4i...,bafyr4i...`. Under the proposal, a count
mismatch would discard the parameter; a receiver could use a complete set to
identify the intended state. Today the parser ignores `tips` like any other
unknown parameter, and including it does not affect bootstrap.

## Implementation

### Serialization

The `Display` implementation uses minimal percent-encoding — only characters
that are structurally significant in query strings (`&`, `=`, `#`, `+`, `%`)
are encoded. Colons and other characters pass through verbatim, keeping
tickets human-readable.

Parsing requires the literal `eidetica:?` prefix and uses
`url::form_urlencoded::parse()` on the rest. It accepts more aggressive
percent-encoding and decodes an unescaped `+` as a space.

### `Sync::create_ticket`

`Sync::create_ticket` pairs the supplied ID with addresses reported by running
transport servers. A server that cannot report its address contributes no hint;
the resulting list can be empty. This operation neither reads nor changes an
Account's sharing preference.

### Session-scoped ticket lookup

A `Database` handle with a local login's tracking record and Read permission
can make that database shareable. Writing the preference and obtaining a
locator are separate operations:

1. `Database::share` writes the user's durable installation-local preference.
2. Owner reconciliation applies the daemon's private combined configuration independently.
3. `Database::ticket` checks this user's setting. If enabled, it asks the
   embedded sync instance or the connected daemon for a point-in-time locator.
   Enabling the preference does not guarantee an available sync instance or
   a reachable address.

`Database::sync_settings` exposes only that user's settings. It does not expose
or consult combined settings, other users, or runtime telemetry. A caller whose
own setting is disabled receives an error even if another user keeps the daemon
serving the database. An unknown write acknowledgment is safe to retry or read
back because setting the preference is idempotent.

A `DatabaseTicket` is a locator, not a capability: its database ID and address
hints do not grant database permission. Authorization and bootstrap approval
remain separate.

## Joining through a client or daemon

`AccountSession::join` is the high-level consumer of a ticket. On an embedded instance it
uses the same `Sync::bootstrap_with_ticket` path as `request_database_access`. On a
connected instance the daemon owns transports, while the client retains the user's
private signing key.
Neither a second client sync engine nor daemon custody of that key is needed.

A request proof binds the answering peer, database and advertised tips, so a
connected client cannot sign before the daemon selects a route. Prepare returns
that route and peer with empty tips, without reading local database state. The
client signs, and execution checks session-key possession and the proof, then
re-handshakes the selected route before recording it. Tickets remain locators,
not access grants. See [ticket bootstrap](../internal/service.md#ticket-bootstrap)
for the wire exchange.

The interface composes existing facilities rather than making them one atomic
operation: [bootstrap transfer and approval](../internal/bootstrap.md),
[Account mapping persistence](accounts.md#mapping-persistence-and-retry-safety), and
[service framing](../internal/service.md#cancellation-and-frame-integrity) have
separate failure boundaries. Success returns an opened database after verified
transfer, Account bookkeeping and a final requested-permission check. Pending requires
an explicit retry and writes no Account state; an error does not imply rollback.

This is not device enrollment or a durable completion job. Later daemon sync uses
its device key and needs separate authority or a global grant. The existing
held-tree Read gate and native authority semantics remain unchanged; see the
[recovery limits](../user_guide/bootstrap.md#current-recovery-limits).
