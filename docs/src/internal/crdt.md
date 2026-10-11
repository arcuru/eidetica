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

Built-in types already implement `Codec`; import it to call their persistence
methods:

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
