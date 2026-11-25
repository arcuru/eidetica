# Structured Stores

`StructuredStore<D, C>` is an opt-in whole-state Store for applications that supply a Serde value type `D`, its CRDT merge, and one exact `StoreContract` implementation `C`.
Existing `DocStore`, `Table`, and their persisted formats are unchanged.

```rust,no_run
use eidetica::{Database, Result, Store, crdt::Lww};
use eidetica::store::{StructuredStore, schema::{ContractId, StoreContract}};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Message {
    body: String,
}

struct Messages;
impl StoreContract for Messages {
    const ID: ContractId = ContractId {
        store: "structured:v0.2",
        data: "eidetica/lww:v0.1",
        schema: "example/messages:v0.1",
    };
}

type MessageStore = StructuredStore<Lww<Message>, Messages>;

# async fn example(db: Database) -> Result<()> {
let tx = db.new_transaction().await?;
let messages = MessageStore::open(&tx, "messages").await?;
messages.apply(Lww::Set(Message { body: "Hello".into() })).await?;
let id = tx.commit().await?;

let view = db.get_store_viewer::<MessageStore>("messages").await?;
assert!(view.read().await?.has_source());
assert!(view.inspect(&id).await?.unwrap().override_id.is_none());
# Ok(())
# }
```

## Interpretation is historical

The contract identifies three separate things:

| Field    | What it identifies                                        |
| -------- | --------------------------------------------------------- |
| `store`  | The Store's durable framing and interpretation            |
| `data`   | The whole value/operation grammar and its merge semantics |
| `schema` | The application's value schema                            |

Use nonempty, stable names. Names are matched byte for byte, not as ranges, and Rust type names are not part of the format.
A single Entry can carry different contracts in different Stores. The Entry format, service protocol, and cache layout are separate from all three names.

Registration writes a default into the Store's `_index` row. Each contribution uses the unique default in **its own signed main ancestry, including its Entry's own `_index` update**.
If the typed value uses that default, its wire object contains just `value`. Otherwise it contains `value` and the explicit `override` descriptor.
An override changes only that contribution. It cannot resolve an absent or conflicting default.
`declare_default` stages metadata without adding a value. If used in the same transaction as `apply`, the commit uses the new default regardless of call order.

For example, a Store with only A values can advance its default to B without changing those values.
An A writer can still add A against the B default; it writes an explicit A override.
If actual A and B values are both present, neither exact-only reader can merge the mixed history.
Incomparable declarations must agree, or a later explicit declaration must resolve them before contributing again. That later declaration cannot repair an earlier ambiguous contribution.
`inspect` reports the historical declaration origins, default, and override of an actual contribution after the same access and, if applicable, unlock checks. It does not require application support for the reported data or schema name.

An Entry with no Store value is not a contribution. An empty view reports `has_source() == false` and `into_value()` returns `D::default()`.
An explicitly written empty value or `Lww::NoOp` is a real contribution. A zero-length payload is malformed, not an empty value.

## Value encoding and compatibility

Each new value is structural Serde data inside a canonical DAG-CBOR record. Nested `Map`, `Lww`, and application values do not need their own byte wrappers or version headers.
Use stable field and variant names and preserve all state needed by later merges. Canonical encoding covers admitted wire values, not arbitrary histories that happen to produce the same application result.

The shared [DAG-CBOR domain](../internal/crdt.md) includes full `i64` and `u64` values. Use `serde_bytes` for byte strings.
`schema::u128_bytes` and `schema::i128_bytes` encode full-width integers as exactly 16 big-endian bytes; `schema::KeyedPairs` encodes non-text-keyed maps as sorted pairs and rejects duplicate encoded keys.

There is no implicit conversion, backward-compatible guess, or JSON fallback. An unsupported type, data contract, or schema fails before application decoding or merge.
Use the original reader for untagged legacy Stores; a structured writer cannot adopt them merely by choosing the same name.
Older readers can retain and relay the opaque Entry, but cannot interpret this Store's value. Enable new-format writers only when the readers that need typed access support the new format. Reverting a binary does not remove Entries it wrote.

## PasswordStore and cache

For `PasswordStore<StructuredStore<…>>`, initialize or unlock as usual and use `inner()` for typed reads and writes.
Use `declare_structured_default` on the unlocked wrapper to advance the encrypted declaration. An unlocked current config does not make a historical config readable if its key parameters differ. This path does not rotate keys.

The new encrypted format limits the **complete canonical encoded descriptor** to 512 bytes, including all three fields. Plaintext Stores have no such cap.
Wrapped defaults, contribution overrides (including omission), and cached descriptors use fixed-size private regions. Equal-size application values and otherwise equal metadata have the same encrypted byte footprint regardless of descriptor length or override use. The ciphertext is still randomized. This does not conceal whether an Entry has a Store value or metadata update, the size of the application value, or the existing public Store name and DAG shape. JSON transport may expand the randomized ciphertext by different amounts.

Materialization checks the captured, verified history before using a cached value and keeps cached layout separate from the authoritative Entry format.
The cache can skip the application fold but not the historical attribution check; this is a whole-state API, not a bounded row lookup.
The service relays Entries and stores opaque cache bytes under its existing login boundary; it does not decrypt or execute application code. Cache coordinates alone do not prove that a client computed a value correctly.
