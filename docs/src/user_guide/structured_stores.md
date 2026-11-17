# Structured Stores

`StructuredStore<D, C>` is an opt-in whole-state Store for structurally serialized
CRDT data. It leaves existing `DocStore`, `Table`, and `YDoc` formats unchanged.
Reads and updates materialize the whole state: their cost is O(total state), not
an indexed point lookup. This is not the generic Table API.

## Declare the interpretation contract

The application explicitly names three independent contracts:

- `store`: the Store family and durable contribution grammar, such as `structured:v0.1`.
- `data`: the complete operation and merge interpretation, such as `eidetica/lww:v0.1`.
- `schema`: the application's value representation, including field and variant names.

Names are nonempty, byte-exact UTF-8 strings. They are not Rust type names, version
ranges, or a registry that supplies executable code. An incompatible change
advances the affected identity; a new cache layout does not change these identities.
Before a stable v1 contract, use point revisions such as `:v0.1` and `:v0.2`.
Reusing an identity for different semantics violates the application contract.

```rust,no_run
use eidetica::{Database, Result, Store, crdt::Lww};
use eidetica::store::{StructuredStore, schema::{ContractId, StoreContract}};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Message {
    #[serde(rename = "text")]
    text: String,
}

struct Messages;
impl StoreContract for Messages {
    const ID: ContractId = ContractId {
        store: "structured:v0.1",
        data: "eidetica/lww:v0.1",
        schema: "example/messages:v0.1",
    };
}

async fn write(db: Database) -> Result<()> {
    let tx = db.new_transaction().await?;
    let messages = StructuredStore::<Lww<Message>, Messages>::open(&tx, "messages").await?;
    messages.apply(Lww::Set(Message { text: "hello".into() })).await?;
    let view = messages.read().await?;
    assert!(view.has_source());
    assert_eq!(view.descriptor().unwrap().schema, Messages::ID.schema);
    tx.commit().await?;
    Ok(())
}
```

`D` supplies CRDT, Clone, Default and Serde implementations, not a child `Codec`.
`apply` takes a delta, not a cumulative snapshot. `C` is separate from `D`, so a
foreign Serde/CRDT type can be used without implementing an orphan identity trait.
For per-key operations, `D` can be `Map<String, Lww<Message>>`; empty, dotted and
Unicode keys remain literal text. No headers are added to individual Map or Lww values.

Cloned handles share one fixed-parent transaction and serialized staging boundary.
A second handle selecting another complete contract for the same Store name is
refused, even before the first write. Merge, schema validation and encoding finish
before replacing staged bytes. Catching a failed operation and committing retains
prior successful changes. Use the Store's `local_data()` for strict staged access.

## Historical reader and writer behavior

Each new contribution is one canonical DAG-CBOR object with exactly these fields:

```text
{
  "contract": { "store": text, "data": text, "schema": text },
  "value": structured D
}
```

Unknown, missing, duplicated or ill-typed header members are rejected. The complete
tuple must match **before application deserialization or merge**. Typed decoding
must also preserve the complete canonical value; ignored fields and other lossy
decoders are rejected. Present zero-length bytes are invalid, not an empty state.

The descriptor is inside the existing whole-Store encryption boundary and is
covered by the enclosing Entry's existing content identity and signing mechanism.
Each named Store in one Entry can have a different descriptor. Neither the Entry
version nor current winning `_index` metadata relabels historical contributions.
Normal registry type checks still apply, including direct viewers and unlocked
`PasswordStore<StructuredStore<D,C>>` inner handles.

Support is exact-only. A Store history containing A then B, or concurrent A and B
branches, cannot be materialized under either contract. A reader never skips an
unsupported contribution or falls back to older visible data. Historical main-tip
reads of an A-only branch still work. A stale writer may extend its captured valid
A branch; this mechanism does not globally fence concurrent writers. A writer
whose captured history is already mixed refuses before staging a delta.

Missing subtree data is not a contribution. A registry-only history yields an
unattributed view (`has_source() == false`, `descriptor() == None`) and does not
publish a fake schema-bearing cache. `into_value()` then returns `D::default()`.
An explicit default/NoOp write, by contrast, is a real attributed contribution.
Absent nodes never invoke the application's merge with an invented default delta.

Legacy/untagged payloads cannot be guessed from their shape or present configuration.
Keep their explicit legacy reader, or explicitly export/recreate into another
Store. There is no automatic migration, history rewrite, or converter runtime.
Old readers do not gain support for these new contributions; use this opt-in API
only where every reader that must materialize the Store supports its contract.
Raw retention/relay of unsupported data is not typed acceptance.

## Explicit application representations

Canonical DAG-CBOR admits native i64/u64 domains but not full i128/u128 domains.
The shared field adapters encode every wide integer, including zero, as exactly
16 big-endian bytes. Signed fields use two's complement. Other lengths or shapes
are refused; no floating-point or JSON intermediate is used.

```rust
use eidetica::store::schema::{i128_bytes, u128_bytes, KeyedPairs};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Measurement {
    #[serde(with = "u128_bytes")]
    count: u128,
    #[serde(with = "i128_bytes")]
    balance: i128,
    #[serde(with = "serde_bytes")]
    binary: Vec<u8>,
    coordinates: KeyedPairs<(i64, u64), String>,
}
```

`KeyedPairs` represents an unusual-keyed map as a sequence of structured
`[key, value]` pairs. Encoding sorts by canonical key-byte length, then bytes;
decoding requires that order and rejects duplicate encoded keys. Construct its
pair vector from either a HashMap or BTreeMap iterator. Key representations must
be injective: the adapter cannot infer application equality or normalization.
True bytes use byte strings; ordinary `Vec<u8>` without an adapter is an array.
Nonfinite floats require a separately declared explicit representation, not a
fallback codec. Canonical plaintext is not a promise of equal ciphertext or of
identical bytes for arbitrary application-equivalent CRDT histories.

## Caches, service access and inspection

Attributed opaque caches use the separate `eidetica/attributed-opaque` layout,
revision 1. The full tuple stays inside the cached value and is checked on exact
hits, merge-base reuse, and adopted publication winners. Every suffix contribution
is still validated. A wrong-contract cache is an error, not a request to overwrite
it as another schema. Clearing derived caches leaves authoritative Entry bytes,
IDs and signatures unchanged.

Password Stores apply the existing password-layout transformation once. Schema
names or their hashes are not added to service-visible keys or descriptors.
Application/header/merge/encode failures on this encrypted path are redacted.
Local opaque requests retain the existing Shared scope; service client publication
is login-scoped and retains its existing Write permission requirement. A cache
publication permission denial propagates; it is not treated as an optional miss.
Service clients use ordered raw history; the daemon does not run
arbitrary application schemas. Existing access, unlock, verified-frontier and
source-selection rules still apply.

Cache coordinates and a matching header are **not** a proof that a client computed
the cached value from the claimed history. They retain the backend's existing
trust model. Similarly, raw explicit diagnostic tips do not become verified merely
because their descriptors are supported. This API adds no source-proof, AAD,
leakage, or authorization policy.

For authorized inspection, `schema::inspect_contract(plaintext)` validates and
returns the recorded descriptor without invoking application deserialization.
A future export/conversion request can record the database root, Store name,
source Entry ID (or exact source tips), each inspected source descriptor and an
explicit target `ContractId`. Those coordinates supply identity, not conversion
code or permission to execute it. Decrypt first through the existing authorized
boundary; never publish encrypted schema identities in logs or transport metadata.
