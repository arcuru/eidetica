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
transformation is applied. The ID format (`sha256:hex`, `blake3:hex`) passes
through opaquely. The ticket format does not need to understand the ID's
internal structure.

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
eidetica:?db=sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
```

With transport hints:

```text
eidetica:?db=sha256:e3b0c44...855&pr=iroh:endpoint...&pr=http:192.168.1.1:8080
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

For example, `tips=2:sha256:abc,sha256:def`. If the count doesn't match the
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
the database ID into a `DatabaseTicket`.

## Forward Compatibility

Unknown query parameters are silently ignored during parsing. This allows
future parameters to be added without breaking existing parsers. Malformed
`pr` values (missing `:` separator) are also silently skipped.
