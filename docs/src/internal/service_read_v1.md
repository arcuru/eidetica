# Read-side service v1

The read adapter uses `eidetica.service.v1.Reads` from `crates/lib/proto/read_v1.proto` over gRPC on a Unix domain socket. Try it with `eidetica daemon --read-v1-socket /path/to/read.sock` or `service::read_v1::ReadServer` on a local `Instance`. It uses the existing socket parent, lock and permission rules. It is opt-in and uses a different socket from `Instance::connect`; the legacy API still owns writes, watches and credential commands. There is no TCP listener, browser gateway, credential release or peer bootstrap. In particular the native `_users` database cannot be read through the projection or whole-Entry paths on this socket.

`ReadClient` in `eidetica::service::read_v1` takes a caller-held `DatabaseKey` on each operation. It does not hold a login or enroll the key. `capture` needs current Read and returns a concrete Verified root cut. `read_state` takes an explicit cut and requires Read there **or** at the serving replica's current Verified frontier. `history` takes an explicit Verified cut and requires current Read. The above rules do not grant other actions or expose a later Snapshot to a caller who only has historical State access. Delegated historical authorization follows each committed pointer from the requested cut; live authorization uses the retained current authority. Arbitrary computation refusal remains independent of authorization.

## Wire and proof

`GetCapabilities` reports revision 1, the serving incarnation, and chunk, object and history limits. It does not authorize a read. The baseline accepts no required extensions; an unknown required flag, version, operation, mandatory field or terminal outcome cannot silently become a broader operation. The SDK validates the revision and uses the smaller of its own and the server's advertised limits.

The first `ReadInput` is a `Query`. The server returns a one-shot `Challenge` on **that RPC**, containing a random 32-byte nonce and its 16-byte producer ID. The client replies with a raw Ed25519 signature and half-closes. No other input is accepted. A fresh RPC gets a fresh challenge, including after reconnect; there is no bearer token or shared login state. The client pins the producer ID obtained from the same local peer's capability reply. Producer ID identifies a server incarnation within the trust of the socket path, not a certificate or an independently authenticated remote.

Sign the following normalized bytes, not Protobuf serialization. `u32` is big-endian, and `field(x)` is `u32(byte_len(x))` followed by `x`. Strings are UTF-8. Database IDs and tips are nonempty canonical base32lower DAG-CBOR CIDv1 strings. Normalize a tip set to sorted unique CIDs in CID tuple order, not order of the rendered strings. Delegation paths are ordered lists of tree roots; caller-chosen authority tips are not sent.

1. Literal `eidetica.service.v1/Reads/Read` followed by one NUL byte, then `u32(1)`.
2. `field(producer)`, `field(nonce)`, `field(database)`, `field(public_key)`. The key is `ed25519:` plus standard padded base64 of the raw 32-byte key.
3. The identity discriminator byte: `1` for the proven key, `2` followed by `field(name)` for a name, or `3` for global. Then `u32(number_of_delegation_roots)` and one `field(root)` per hop. Global has no delegation path.
4. The operation byte: `1` for Capture, `2` for State, `3` for History. State then includes `field(store)` and `field(projection)`. State and History end with `u32(number_of_tips)` and one `field(tip)` per normalized tip. Capture has no caller-supplied tips.

Proof vector: revision `1`, database `bafyr4iac6a3ogzhcoaxeyretr36jxbzuulsmniwbgqh34livdmfphskawq`, State at the single tip `bafyr4ibjn2smxkt2elzmt77eyz7ghe2xularxakepduwve3rml5kgjc4nu`, store `data`, projection `eidetica/doc:v0`. Use direct-key hint (`1`), zero delegation roots, key `ed25519:+AzM3OSuHAeuIIoq35mjEK5CB+Awb6AjYRCwaCe7uNA=`, producer hex `000102030405060708090a0b0c0d0e0f`, and nonce hex `101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f`. The expected Ed25519 signature (hex, concatenate the lines):

```text
53323e0f09428c84f06767d1b825837f8a1928a85a834c80aefccac4ccf79bec0f
d475deb5867ddf5722431bb780b84edb26dea9224ef9b2fa28a39194193006
```

## Data and completion

After proof, a successful operation supplies `Start(context)`, optional `Data` frames, then `End(Complete, context)` and gRPC `OK` trailers. The two contexts must match the producer and the request, including the returned concrete cut on capture. Capture has no body. A Doc body carries the native `Doc` codec (JSON bytes, not a Protobuf `Struct`). Even an empty Doc has an encoded body. `eidetica/doc:v0` covers supported built-in plaintext Doc projections, not custom/encrypted execution or a new row query API. It is daemon-built state; it is neither a client cache read nor a proof of correct computation or independently Verified history.

The History body is CARv1: an unsigned-varint-length DAG-CBOR header whose roots are the requested cut, then unsigned-varint-length sections of binary CID plus canonical Entry DAG-CBOR bytes. Frame boundaries do not delimit sections. Each block must have been named by the cut or by a previously delivered child. Common ancestors are delivered once; consumers must not require a particular traversal, display or commit order. The Rust SDK checks the Eidetica CID and canonical encoding, Entry structure, database membership and closure of the delivered cut. A CAR header and matching CIDs alone do not prove coverage or locally verified authority. Entries are candidates for the ordinary signature, dependency and verification path, not automatically accepted history.

A typed `Refused` can end the stream before `Start` or after provisional bytes, with no success context. Denied, invalid boundary, unavailable history, unsupported and resource limit have distinct outcomes. Missing/error trailers, a cut-off body or a missing terminal are incomplete, not a successful empty result or inferred refusal. `StateStream::next_chunk` exposes provisional bytes; `HistoryStream::next_entry` yields owned Entries. `finish` does not drain a stream and needs exhaustion with verified terminal and clean gRPC status. `collect_complete` is an explicit memory-for-convenience choice on a fresh stream. There is no auto-retry, switch to a different cut, trust upgrade or hidden CAR/State fallback.

## Resource limits

Message cap: 128 KiB; data chunks: at most 64 KiB; single Entry or encoded Doc: at most 32 MiB. History is admitted within 4,096 point fetches and 128 MiB, including authority/ancestry work, with separate delivered-body caps. Capture and State share a smaller request budget of 2,048 point fetches and 32 MiB of input. Work can therefore refuse well below a nominal body limit. State retains only selected deltas and folds in the existing order; the current Settings index avoids a database-wide walk on the common capture path. Historical cuts without a usable index may require a bounded walk and be refused even if the requested Store is small. Custom backends without bounded primitives cannot fall back to unbounded bulk loads.

Four concurrent read producers are admitted per endpoint. Capability discovery does not wait behind them. Proof waits and backend work have deadlines; a timed-out or dropped call cannot be treated as complete. Producers pull from bounded backend point reads as the gRPC body is polled; there is no detached unbounded result queue. One synchronous point read/codec step and HTTP/2's in-flight buffers can remain at cancellation. These limits describe work and buffering, not a process RSS, latency or zero-extra-work guarantee.
EOF
sed -i 's/^ d475deb/d475deb/' docs/src/internal/service_read_v1.md
