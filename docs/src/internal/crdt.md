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
Neither provides a built-in `Codec` or Serde implementation, so neither selects
a durable format or changes an existing Store. Their payloads need not implement
`Default` or encoding traits.

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

Map and Lww do not interpret row lifecycle. Use the standalone `RowLifecycle`
wrapper below when composing row operations. Table behavior and persisted bytes
are unchanged.

## Standalone Row Lifecycle

`RowLifecycle<V>` adds ordered row operations over any associative application
payload, including non-LWW CRDTs. Compose rows with `Map<K, RowLifecycle<V>>`;
Map retains the wrapper, including tombstones and reset boundaries.

- `NoOp` is the default and identity, not a row-creation policy.
- `Update(V)` composes ordinary live payloads with `V::merge`. After `NoOp`,
  it supplies the initial payload without creating a default `V`.
- `Reset(V)` replaces all prior payload and lifecycle state, even a deleted row.
- `Delete` retains a whole-row tombstone. Subsequent ordinary updates are
  suppressed; only a reset can replace it.

Order is the caller's deterministic left-to-right order, not wall-clock or
causal observed-delete order. `Delete + Reset(v)` yields `Reset(v)`, whereas
`Reset(v) + Delete` yields `Delete`. Reset followed by update remains a reset:
`Reset(a) + Update(b) = Reset(a.merge(b))`. Keeping that boundary is necessary
for associativity: composing this result after an older row must discard the
older payload rather than merge it back in.

```rust
use eidetica::{crdt::{CRDT, RowLifecycle}, Result};

#[derive(Clone, Debug, PartialEq)]
struct Events(Vec<u8>); // No Default, Codec or Serde.
impl CRDT for Events {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self([self.0.as_slice(), other.0.as_slice()].concat()))
    }
}

let old = RowLifecycle::Update(Events(vec![1]));
let fresh = RowLifecycle::Reset(Events(vec![2]));
let later = RowLifecycle::Update(Events(vec![3]));
let grouped = fresh.merge(&later)?;
assert_eq!(grouped, RowLifecycle::Reset(Events(vec![2, 3])));
assert_eq!(old.merge(&grouped)?, old.merge(&fresh)?.merge(&later)?);
let deleted = old.merge(&RowLifecycle::Delete)?;
assert_eq!(deleted.merge(&later)?, RowLifecycle::Delete);
assert_eq!(deleted.merge(&fresh)?, fresh);
# Ok::<(), eidetica::Error>(())
```

This merge is replicated composition, not local-write validation. The future
Table API must reject ordinary writes when the transaction observes deletion;
that check is not implemented here. History replay suppresses updates following
deletion instead of aborting with a deleted-row error. An inner `Lww::Delete`
removes the payload's visible value but does not delete the row:
`Update(Lww::Delete) + Update(Lww::Set(v))` is still a live update. In contrast,
`RowLifecycle::Delete + Update(Lww::Set(v))` remains a whole-row tombstone.

Construction and default require no payload traits; merge requires
`V: CRDT + Clone`, never `V: Default`. There is no implicit `From<V>` conversion
because the caller must choose update or reset. The wrapper provides no built-in
Codec or Serde implementation and makes no persistence or Table API changes.

Associativity holds on a closed payload domain where every `V::merge`, including
intermediate results, succeeds and is associative. Actual payload merge errors
propagate unchanged, but arbitrary fallible evaluation paths are not
associative. For example, `(Update(a) + Update(b)) + Delete` fails if
`a.merge(b)` fails; `Update(a) + (Update(b) + Delete)` returns `Delete` without
calling that merge. Reset can mask earlier failures in the same way, and
`Delete + (Update(a) + Update(b))` can evaluate a failing merge that the left
fold would suppress. Callers cannot assume regrouping preserves errors outside
the valid successful payload domain.

### Non-exhaustive operation enums

Both `RowLifecycle<V>` and `Lww<T>` are `#[non_exhaustive]`. This intentionally
changes source compatibility for downstream exhaustive matches on Lww: add a
wildcard arm. Construction, Lww's `From<T>`, and its merge behavior are unchanged.

```rust
use eidetica::crdt::{Lww, RowLifecycle};
let row = RowLifecycle::<()>::Delete;
assert!(match row {
    RowLifecycle::Delete => true,
    _ => false,
});
let value = Lww::<()>::Delete;
assert!(match value {
    Lww::Delete => true,
    _ => false,
});
```

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
