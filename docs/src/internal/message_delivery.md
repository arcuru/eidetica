# Message Delivery

One logical message is one request or response. The Unix service socket, Iroh and HTTP all deliver messages the same way, in `message_io`, below the service and sync operations. Callers send and receive ordinary values and never split data to fit a transport. Entry-aware history paging is a separate concern and is not how bytes are delivered.

## Wire Format

Every message is an envelope on a stream the transport already keeps reliable and ordered:

```text
┌──────────────────────────┬──────────────────────────────┐
│ Length (8 bytes)         │ Exactly that many bytes of   │
│ big-endian u64           │ noncanonical CBOR            │
└──────────────────────────┴──────────────────────────────┘
```

The length is an admission and boundary declaration, not an I/O chunk size or a rule about what is valid. A zero length, a truncated body and bytes left after the value are errors. There is no packetization, acknowledgement or retransmission. Stored Entry encoding, IDs and signatures do not pass through this codec, so they are unchanged.

| Transport | Boundary                                                                             | Version check                                                           |
| --------- | ------------------------------------------------------------------------------------ | ----------------------------------------------------------------------- |
| Service   | Consecutive envelopes on the socket; a clean EOF is allowed only between messages    | Small JSON `Handshake` with `PROTOCOL_VERSION = 1` before any envelope  |
| Iroh      | One envelope each way on one bidirectional stream; `FIN` must follow it              | ALPN `eidetica/v1`                                                      |
| HTTP      | One envelope per body; the body must end with it and any `Content-Length` must agree | Path `/api/v1` and media type `application/vnd.eidetica.cbor;version=1` |

### Compatibility

This is a deliberate wire break. A v0 peer is refused before any operation runs: by the handshake, the ALPN, or the missing route and media type. Nothing falls back to v0, and a refusal is never a reason to resend an operation another way. HTTP redirects and transparent content encodings are off, because the first could replay a request and the second would hide the bytes admission accounts for. Unknown service operations, responses and notifications keep the semantics in [Service](service.md#wire-protocol).

## Memory

The value being sent and the value being received are owned, and stay proportional to their contents. What this module bounds is the **additional** memory used to move them. Above 64 KiB a blocking worker encodes or decodes, joined to the async stream by a queue of two 16 KiB chunks per direction. A sender runs the encoder twice over the same owned value, first to count bytes for the envelope and then to stream them, so no encoded copy exists. Messages of 64 KiB or less go through one buffer without a worker.

`message_memory` measures the production send paths through all three transports in both directions at 16, 48 and 96 MiB. The opposite peer drains bytes without decoding a result. The meter records the peak above the initial owned source, with no subtraction for future result capacity. It detects an exact-capacity whole encoded copy even if that copy is dropped before streaming begins. These are requested-heap observations, not RSS or an end-to-end decoder-overhead measurement.

On receive, inspection establishes the fixed extra buffers: a queue of two 16 KiB chunks, the decoder's current chunk and the configured bounded unknown-value capture. The native CBOR decoder grows byte strings into the owned result, and the wire adapters move those buffers into Entry/credential fields rather than copying a whole envelope. End-to-end round-trip and backpressure tests cover those paths. Neither this inspection nor the sender meter bounds the following costs:

- A decoded byte string's `Vec` can hold up to about twice its length, because a declared length is untrusted and cannot size an allocation.
- Borrowed APIs such as `SyncTransport::send_request` and `Sync::send_entries` clone their input. Use `send_request_owned`, `send_owned_entries` or `Sync::send_owned_entries` to move it.
- Retry queues, caches, materialization, hashing and storage keep their own copies.
- QUIC and HTTP stacks have their own finite windows and read buffers, which are independent of message size.

## Resource Policy

`TransferPolicy` sets the limits and `TransferBudget` enforces them. A constructed client, server or transport shares one budget across its connections; clone a budget into several endpoints to share a limit. Use `RemoteConnection::connect_with_budget`, `ServiceServer::bind_with_budget` or the `transfer_budget` builder methods. Independent budgets are independent, so this is not a process-wide ceiling.

| Policy                                            | Default   |
| ------------------------------------------------- | --------- |
| `max_message_bytes`                               | 256 MiB   |
| `max_reserved_bytes` (sum admitted at once)       | 512 MiB   |
| `max_inbound_messages`, `max_outbound_messages`   | 4 each    |
| `max_nodes` (decoded values per message)          | 1,048,576 |
| `max_depth`                                       | 128       |
| `max_identifier_bytes`                            | 4 KiB     |
| `max_ignored_bytes` (one discarded unknown value) | 64 KiB    |
| `service_queue_slots`                             | 4         |
| `notification_queue_slots`                        | 1024      |

These are resource-policy limits, not Entry validity rules. A refusal is `SyncError::ResourceLimit` (see `Error::is_resource_limit`), says nothing about the data, and may succeed under different load or configuration. Admission fails fast rather than waiting, so transfers cannot form a wait cycle. The active-message limits apply above the inline size, and the byte reservation covers all messages. Byte accounting admits encoded sizes: it is not exact heap accounting and does not bound storage, hashing or caller code. A scalar-heavy message can reach `max_nodes` before `max_message_bytes`.

A receiver reserves the declared length before reading the body, but never allocates from a declaration. Unknown values are skipped under `max_ignored_bytes`, so an extension larger than that is refused rather than ignored.

A refused message gets a small explicit answer outside the data budgets where the stream allows it: a service `Error`, `SyncResponse::ResourceRefused` on Iroh, or HTTP 413. The receiver then stops reading. A sender treats a correlated refusal as the outcome even if its upload failed, and otherwise reports the real transport failure.

## Progress and Cancellation

A healthy transfer may take as long as it needs, so a body has no total deadline of its own:

| Policy                                | Default       | Meaning                                                              |
| ------------------------------------- | ------------- | -------------------------------------------------------------------- |
| `stall_timeout`                       | 30 s          | Longest silence inside a body                                        |
| `progress_window`, `min_window_bytes` | 10 s, 640 KiB | Every complete active window must move this much (64 KiB/s)          |
| `first_response_timeout`              | 30 s          | Wait for a response to begin                                         |
| `preparation_timeout`                 | 30 s          | Sizing a message before its first byte                               |
| `overall_timeout`                     | None          | Caller-selected bound on a whole exchange, never renewed by progress |

For service connections, the first-response wait starts only for the fully uploaded request at the front of the response FIFO. Later requests do not time out behind an active bulk response. A frame that has started uses the body progress policy; intervening notifications do not renew the front request's deadline. Idle connections are untimed. A first-response timeout fails the front request and retires the connection; canceled response slots remain in order until consumed. An explicit overall deadline still covers the whole call, including queueing.

Connecting has its own transport bound. Windows carry no credit, so an early burst does not prepay later ones and a trickle cannot keep a transfer alive. A body that finishes within one window does not have to fill it, and timing stops once the body is complete. The defaults are engineering limits rather than a throughput promise: a link that cannot sustain 64 KiB/s needs a lower `min_window_bytes`.

Bytes count when the native transport accepts or yields them. That is acceptance by the transport, not proof a packet was acknowledged. Serializer output and queue pushes do not count.

Dropping a transfer cancels its worker, and admission is released only when the worker has really stopped. Nothing is accepted until the whole message and any required stream end have arrived, so an incomplete Entry never reaches a handler. Any failure on a persistent service connection retires it, because a partial message cannot be skipped without shifting FIFO association. An interrupted write has an uncertain outcome, which is surfaced and never replayed.
