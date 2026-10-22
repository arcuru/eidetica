# CRDT Merging

Eidetica implements a Merkle-CRDT using content-addressable entries organized in a Merkle DAG structure. Entries store data and maintain parent references to form a distributed version history that supports deterministic merging.

## Core Concepts

- **Content-Addressable Entries**: Immutable data units forming a directed acyclic graph
- **CRDT Trait**: Enables deterministic merging of concurrent changes
- **Parent References**: Maintain history and define DAG structure
- **Tips Tracking**: Identifies current heads for efficient synchronization

## Encoding and the Rust API

`Codec` and `CRDT` are independent traits exported from `eidetica::crdt`:

- `Codec: Sized` provides `encode(&self) -> Result<Vec<u8>>` and
  `decode(bytes: &[u8]) -> Result<Self>` at persistence boundaries.
- `CRDT: Sized` provides only `merge(&self, other: &Self) -> Result<Self>`.
  Its existing merge semantics are unchanged.
- `Store::Data` requires `CRDT + Codec + Default + 'static`. History
  reconstruction needs a default state and encoding; merge-only code does not.
- `Transaction::get_local_data<T>` requires only `T: Codec`.

Neither trait requires `Clone` or Serde. There is no blanket `Codec`
implementation: deriving Serde does not choose a persistence format.

### Migrating implementers from `Data`

This is an intentional source-level Rust API change. Replace the old empty
`impl Data for MyData {}` with an explicit `Codec` implementation. For an
existing JSON-backed type, retain the same Serde representation: implement
`encode` with `Ok(serde_json::to_vec(self)?)` and `decode` with
`Ok(serde_json::from_slice(bytes)?)`, using the existing `serde_json` dependency.
The [Codec API reference](../rustdoc/eidetica/crdt/traits/trait.Codec.html) contains a
complete implementation example.

The existing `Doc`, `Value`, `List` and `YrsBinary` types implement `Codec`;
import it to call their persistence methods:

```rust
use eidetica::crdt::{Codec, Doc};

let mut value = Doc::atomic();
value.set("name", "example");
let bytes = value.encode()?;
assert_eq!(bytes, br#"{"_a":true,"children":{"name":{"Text":"example"}}}"#);
assert_eq!(Doc::decode(&bytes)?, value);
# Ok::<(), eidetica::Error>(())
```

Keep the existing `CRDT::merge` implementation. Generic code that previously
inherited serialization, cloning or a default value from `CRDT` must request
only the additional traits it actually uses. Existing concrete types retain
their Serde and Clone implementations; those capabilities are no longer
requirements of either trait.

A codec must preserve tombstones and all other merge-relevant state. Decode one
complete value and report malformed or trailing data as an error, not as a
default state. Staged reads still treat zero bytes as absent; whitespace-only
JSON remains a decode error. Custom codecs may use non-JSON bytes, but switching
an existing Store's durable format is a separate compatibility change.

No storage migration is needed for the built-in types. `Doc`, `Value` and
`List` use their existing JSON representations, including atomic flags,
tombstones and list positions. `YrsBinary` retains the JSON `{"data":[...]}`
wrapper around Yjs v1 update bytes, rather than writing raw updates. Settings,
Table rows, Entry hashing, encryption and service formats are unchanged. This
does not introduce canonical JSON ordering or a new merge algebra.

## Standalone Map and Lww Algebra

`Map<K, V>` and `Lww<T>` implement merge algebra independently of persistence.
Their payloads need not implement `Default` or encoding traits to merge. Explicit
codecs are available when the payload has one (see
[Operation codecs](#operation-codecs)); there is no Serde implementation, and no
existing Store uses these types or their encoding.

### Ordered register operations

`Lww<T>` has three operations: `NoOp` (the default and identity), `Set(T)` and
`Delete`. Merging chooses the rightmost non-`NoOp` in the caller's deterministic
operation order. There are no timestamps or clocks, and merge is intentionally
not commutative. The payload is replaced whole, not merged, and only needs
`Clone` for `CRDT::merge`. `Lww::from(value)` or `value.into()` wraps a payload
in `Set` without requiring `Clone` or other payload traits.

`Delete` is retained as a tombstone. A later `Set` replaces it at the value
level; this is not a causal delete-wins rule or Table row resurrection policy.

```rust
use eidetica::crdt::{CRDT, Lww};

#[derive(Clone, Debug, PartialEq)]
struct Payload(&'static str); // No Default, CRDT, Codec or Serde.

let old = Lww::Set(Payload("before"));
let deleted = old.merge(&Lww::Delete)?;
assert_eq!(deleted, Lww::Delete);
assert_eq!(deleted.merge(&Lww::NoOp)?, Lww::Delete);
assert_eq!(deleted.merge(&Payload("after").into())?, Lww::Set(Payload("after")));
# Ok::<(), eidetica::Error>(())
```

### Per-key delta composition

`Map<K, V>` stores operations in a `BTreeMap`. The empty map is the identity.
Merge preserves keys present on only one side and invokes `V::merge` exactly
once for each overlapping key. It propagates value merge errors and leaves both
inputs unchanged. Associativity follows from the value algebra; commutativity
and idempotency are not required.

Keys are literal ordered keys, not document paths. `get`, `iter` and `len`
include every stored operation, even `Lww::NoOp` and `Lww::Delete`. Map does not
interpret or discard tombstones, and defines no row deletion/lifecycle policy.
Values can be other Maps, Docs or application-defined CRDTs, not just Lww.

Use `insert_delta` for repeated same-key updates: it composes the new delta
with the stored operation via `V::merge`. If merging fails, the stored operation
is unchanged. The low-level `insert` instead replaces the stored operation
verbatim and returns the previous value. It does not compose deltas.

```rust
use eidetica::{crdt::{CRDT, Map}, Result};

#[derive(Clone, Debug, PartialEq)]
struct Events(Vec<u8>); // An associative, non-idempotent application CRDT.

impl CRDT for Events {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self([self.0.as_slice(), other.0.as_slice()].concat()))
    }
}

let mut updates = Map::new();
updates.insert_delta("events".to_owned(), Events(vec![1]))?;
updates.insert_delta("events".to_owned(), Events(vec![2]))?;
assert_eq!(updates.get("events"), Some(&Events(vec![1, 2])));
assert_eq!(updates.merge(&Map::new())?, updates);
# Ok::<(), eidetica::Error>(())
```

Construction and iteration require no key or value traits. Lookup and literal
insertion require ordered keys; delta insertion additionally requires `V: CRDT`,
without cloning. Map merge requires `K: Ord + Clone` and `V: CRDT + Clone` to
return an owned result while preserving one-sided operations. No `V: Default`
is required.

These primitives do not integrate with Table, add a live-value convenience
layer, or define lifecycle resets. Table behavior and persisted bytes are
unchanged.

### Operation codecs

`Lww<T>` implements `Codec` when `T: Codec`, and `Map<String, V>` implements
`Codec` when `V: Codec`. Neither needs `CRDT`, `Clone` or Serde on the payload,
and encoding never merges values. Both use versioned DAG-CBOR arrays rather than
Serde enum spelling. The leading `0` is the envelope revision:

| Value             | Encoding                         |
| ----------------- | -------------------------------- |
| `Lww::NoOp`       | `[0, 0]`                         |
| `Lww::Set(value)` | `[0, 1, value_bytes]`            |
| `Lww::Delete`     | `[0, 2]`                         |
| `Map<String, V>`  | `[0, [[key, value_bytes], ...]]` |

`value_bytes` is a CBOR byte string holding the payload's own complete `Codec`
output, which may be empty if the payload accepts it. Map keys are literal text
strings in strictly ascending UTF-8 byte order, so equal maps encode to
identical bytes.
Every stored operation is encoded, including `NoOp` and `Delete`, and nested
values such as `Doc` keep their own atomic flags and tombstones.

Decoding rejects other revisions or operation tags, wrong field counts or types,
duplicate or out-of-order keys, trailing bytes, and nested bytes that the payload
codec rejects. These are building blocks: no Store or Table uses them yet.

## Fork and Merge

The system supports branching and merging through parent-child relationships:

- **Forking**: Multiple entries can share parents, creating divergent branches
- **Merging**: Entries with multiple parents merge separate branches
- **Deterministic Ordering**: Entries sorted by height then ID for consistent results

## Merge Algorithm

Uses a recursive merge-base approach for computing CRDT states:

- **Cache Check**: Avoids redundant computation through automatic caching
- **Merge Base Computation**: Finds the common dominator for multi-parent entries (the lowest ancestor through which ALL paths must pass)
- **Recursive Building**: Computes ancestor states recursively
- **Path Merging**: Merges all entries from merge base to parents with proper ordering
- **Local Integration**: Applies current entry's data to final state

## Doc Merge Semantics

The `Doc` type supports two merge modes controlled by an `atomic` flag:

- **Atomic merge** (`other.atomic == true`): The incoming Doc replaces the existing one entirely (LWW). The result is a clone of `other`, including its atomic flag.

- **Structural merge** (default): Fields are merged recursively. For each key present in both sides, values are merged per-field using last-writer-wins. Keys unique to either side are preserved in the result. The result is non-atomic unless `self` was atomic (see below).

### Contagious Atomic Flag

When `self.atomic == true` and `other.atomic == false`, a structural merge occurs (other's fields are merged into self's fields), but the result preserves the atomic flag from `self`. This "contagious" property ensures associativity:

```text
Given entries E1, E2, E3(atomic), E4:

Left fold:   ((E1 ⊕ E2) ⊕ E3) ⊕ E4
Grouped:     (E1 ⊕ E2) ⊕ (E3 ⊕ E4)

E3 ⊕ E4 produces an atomic result (contagious from E3).
When merged with (E1 ⊕ E2), the atomic flag triggers LWW,
correctly overwriting all pre-E3 data.
Both forms produce identical results.
```

### When to Use Atomic Docs

Atomic Docs are appropriate for configuration or typed data that should be treated as a complete unit — replacing the previous value entirely rather than merging individual fields. Store implementations use `Doc::atomic()` when writing config values (e.g., `PasswordStoreConfig`) to ensure the full config is replaced on each write.

## Key Properties

- **Correctness**: Consistent state computation regardless of access patterns
- **Performance**: Caching eliminates redundant work
- **Deterministic**: Maintains ordering through proper merge base computation
- **Immutable Caching**: Entry immutability ensures cache validity
