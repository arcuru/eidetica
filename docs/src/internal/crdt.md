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
Neither provides a built-in `Codec`, so neither selects a durable format or
changes an existing Store. Conditional Serde implementations expose their logical
structure without adding encoding bounds to the algebra. Their payloads need not
implement `Default` or encoding traits.

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

## Structured DAG-CBOR Persistence

`eidetica::crdt::dag_cbor::{encode, decode}` is an opt-in boundary for **new**
structured formats. Serde describes the complete value; the helper produces one
canonical plaintext object, not a sequence of separately encoded child payloads.
It does not implement `Codec` automatically or select a Store/schema version.

### Logical wire representation

The conditional Serde implementations pin these shapes independently of private
Rust fields:

| Type              | Logical representation                                                  |
| ----------------- | ----------------------------------------------------------------------- |
| `Map<K, V>`       | A map from serialized keys to structured `V`, with no `entries` wrapper |
| `Lww::NoOp`       | The text string `"NoOp"`                                                |
| `Lww::Delete`     | The text string `"Delete"`                                              |
| `Lww::Set(value)` | A single-entry map `{"Set": value}`, with structured `value`            |

The DAG-CBOR boundary requires unique text keys. `Map`'s algebra still permits
other ordered key types, but their serialization must expose text to use this
helper. Keys are literal: empty, dotted and Unicode strings are not normalized.
An absent entry, an explicit `NoOp`, a `Delete`, and `Set` of empty bytes remain
distinct. Payloads use their own declared Serde schema, not a recursive `Codec`.
There is no per-Map/Lww version header, payload-byte envelope or Table-specific
version protocol.

A logical map has the same bytes regardless of insertion/iteration order,
`BTreeMap` versus `HashMap`, or a custom string type exposing the same Serde text.
DAG-CBOR orders keys by encoded length and then bytes; this is **not** ordinary
Rust string scan order (`"b"` precedes `"aa"` on the wire). Arrays retain order.
A change to a Rust container alone need not change the format, but changing its
logical equality, key normalization, field/variant names or merge interpretation
is a schema/compatibility change.

```rust
use eidetica::crdt::{Map, Lww, dag_cbor};

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Record {
    #[serde(rename = "name")]
    name: String,
    #[serde(rename = "data", with = "serde_bytes")]
    data: Vec<u8>,
}

let mut operations = Map::new();
operations.insert("literal.key".to_owned(), Lww::Set(Record {
    name: "example".to_owned(),
    data: vec![0, 255],
}));
operations.insert("gone".to_owned(), Lww::Delete);
let bytes = dag_cbor::encode(&operations)?;
let decoded: Map<String, Lww<Record>> = dag_cbor::decode(&bytes)?;
assert_eq!(decoded, operations);
# Ok::<(), eidetica::Error>(())
```

### Supported domain and strict admission

The helper follows the [DAG-CBOR encoding rules](https://ipld.io/specs/codecs/dag-cbor/spec/):

- Null, booleans, UTF-8 text, arrays, unique text-keyed maps, byte strings and CID
  links are supported. Native integer range is `[-2^64, 2^64-1]`, including the
  full `i64` and `u64` ranges. Out-of-range `i128`/`u128` values fail; schemas that
  need them must choose an explicit representation, not an implicit JSON fallback.
- Floats must be finite. They use 64-bit encoding, even when exactly representable
  at a narrower width. New values normalize negative zero to positive zero;
  negative-zero bytes are rejected on admission.
- Binary leaves require `serde_bytes` (as above) or an equivalent byte adapter.
  Plain `Vec<u8>` is an integer array, not a byte string. Opaque native bytes are
  canonical as those exact bytes, not as all equivalent Yjs histories.
- `decode` requires both `DeserializeOwned` and `Serialize`. It rejects malformed,
  trailing, duplicate-key, non-text-key, nonminimal-number/length, wrong-order and
  wrong-float-width inputs. It also requires typed re-encoding to reproduce the
  input exactly: ignored fields, redundant defaults, unknown variants and lossy
  schema normalization are not silently accepted.

The pinned `serde_ipld_dagcbor` adapter sorts map keys and rejects nonfinite float
encoding, but its raw decoder accepts some noncanonical representations, its
serializer permits non-text/duplicate keys, and it preserves negative zero.
The helper therefore validates through `ipld_core::Ipld`, normalizes zero on
encoding, and compares canonical re-encoding on admission. This uses the library
parser, not a separate handwritten CBOR parser. Typed re-encoding is necessary
in addition: Doc List's existing decoder can collapse repeated positions even
when the surrounding CBOR is canonical.

This validation allocates a complete IPLD value and performs multiple passes;
one persisted object does not mean one CPU pass or streaming encoding. Serde
implementations must preserve complete state and be deterministic under their
schema. A display/visible-row serialization that omits tombstones cannot meet
that contract. Custom types remain responsible for their own semantic invariants.

Nested Doc uses its existing structural Serde representation, retaining atomic
flags, child tombstones and ordered List position/value pairs, including List
tombstones and UUIDs. The helper does not establish Doc v1 or repair Doc's separate
batching defect involving delete/scalar replacement followed by descendant writes.
The continuation tests qualify only their lawful fixtures, not arbitrary Doc
operation regrouping.

Existing Doc/List/Value/Yrs JSON `Codec` implementations, Table, Entry bytes/IDs,
signature inputs, encryption and legacy decoder semantics remain unchanged.
This helper is not routed into any of those persistence paths. A future Store
must explicitly own its whole format and interpretation/schema identity; this
module does not decide where that identity is stored. Canonical plaintext does
not imply identical ciphertext or equivalence of different histories.

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
