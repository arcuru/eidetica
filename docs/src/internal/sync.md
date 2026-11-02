# Sync

Eidetica uses a Merkle-CRDT based sync protocol. Peers exchange tips (current DAG heads) and send only the entries the other is missing.

## Sync Flow

```mermaid
sequenceDiagram
    participant A as Peer A
    participant B as Peer B

    A->>B: SyncTreeRequest (my tips)
    B->>A: Response (entries you're missing, my tips)
    A->>B: SendEntries (entries you're missing)
```

1. Peer A sends its current tips for a Tree
2. Peer B compares DAGs, returns entries A is missing plus B's tips
3. A sends entries B is missing based on the tip comparison

This is **stateless** and **self-correcting** - no tracking of previously synced entries.

These tips describe raw DAG possession, not Verified membership. New received
Entries are stored Unverified; neither peer authentication nor transfer completion
asserts local acceptance. Default Database reads use the retained Verified frontier,
which can lag raw tips until local validation succeeds. Write-event cursors also
remain raw boundaries. See the canonical [verification contract](../design/verification.md)
for promotion, partial-history reads and retry limits.

## Bootstrap vs Incremental

The same protocol handles both cases:

- **Empty tips** (new database): Peer sends complete Tree from root
- **Has tips** (existing database): Peer sends only missing entries

## Transport Options

- **HTTP**: REST API for server-based sync
- **Iroh P2P**: QUIC-based with NAT traversal for peer-to-peer sync

Both transports implement the same `SyncTransport` trait. Multiple transports can be enabled simultaneously.

## Architecture

```mermaid
graph LR
    App[Application] --> Sync[Sync Module]
    Sync --> BG[Background Thread]
    BG --> TM[TransportManager]
    TM --> HTTP[HTTP Transport]
    TM --> Iroh[Iroh Transport]
```

The Sync module queues operations for a background thread, which uses a `TransportManager` to route requests to the appropriate transport based on address type. Failed sends are retried with exponential backoff.

### Multi-Transport Support

The `TransportManager` enables simultaneous use of multiple transports:

- **Address-based routing**: Each transport declares which addresses it can handle via `can_handle_address()`
- **First-match routing**: Requests are routed to the first transport that can handle the address
- **Independent servers**: Each transport runs its own server on different ports/protocols
- **Unified API**: `accept_connections()` starts all registered transports, `get_all_server_addresses_async()` returns all addresses

## Current Limitations

The sync system is currently simple and 1:1. Each peer connection requires manual setup with explicit peer addresses. Planned improvements include:

- Peer discovery
- Address sharing and relay coordination
- Multi-peer sync orchestration

See [Bootstrap System](bootstrap.md) for the key exchange flow when joining a database.

## Shared forward history vocabulary

Native [history deltas](service.md#forward-store-snapshot-deltas-v2) define a
Store-scoped fixed target, known ancestor closure, canonical forward prefix,
target-local covered Snapshot and explicit completion. A future bounded P2P
adapter can map `our_tips` to known and capture the server frontier as target,
but needs a separate covered progress field. `their_tips` currently advertises
the whole server frontier; it is not a delivered-prefix acknowledgement.

Current P2P `collect_ancestors_to_send` stops at literal known tips, collects
Entry bodies in a HashMap and returns unordered Entries. It does not implement
exact target-minus-known subtraction or native v2 bounds. Pull, bootstrap and
outbound SendEntries remain unbounded here. Their wire, authentication and
orchestration are unchanged; protocol migration, sparse-replica reconciliation
and reverse traversal are deferred. In particular, child-first traversal cannot
advance a forward possession Snapshot until its ancestors are covered.
