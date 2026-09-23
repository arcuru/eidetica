# CRDT Merging

Eidetica implements a Merkle-CRDT using content-addressable entries organized in a Merkle DAG structure. Entries store data and maintain parent references to form a distributed version history that supports deterministic merging.

## Core Concepts

- **Content-Addressable Entries**: Immutable data units forming a directed acyclic graph
- **CRDT Trait**: Enables deterministic merging of concurrent changes
- **Parent References**: Maintain history and define DAG structure
- **Tips Tracking**: Identifies current heads for efficient synchronization

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

## Ordered map operations

`Lww<T>` is a right-biased `NoOp`/`Set`/`Delete` register. “Last” means
last in deterministic Entry reduction order (height, then Entry ID), **not**
wall-clock timestamp. `NoOp` is its identity; a delete remains a tombstone.

`Map<K, V: CRDT>` merges per key, leaving keys present on only one side
untouched. It does not define deletion or assume that merging a value is a
replacement. A general `Map<String, Counter>` therefore cannot use the cheap
row projection of `LwwMap<K, V>`, which composes `Map<K, Lww<V>>` and presents
only live values through its ordinary iteration API. Both maps serialize as
key-ordered sequences of unique key/operation pairs, not JSON objects.

For example, folding `Set("a", 1)`, `Delete("a")`, then `Set("a", 2)`
leaves `a = 2`; an unrelated key `b` is unaffected. The operation order
comes from Entries, whereas serialization sorts map keys.

`CanonicalJson` holds RFC 8785 canonical row bytes (`canonical-json:v0`). It
rejects duplicate member names and invalid JSON number inputs. A typed reader
may deserialize a row, but its schema never rewrites the canonical row bytes.
Table now reduces `LwwMap<String, CanonicalJson>` Entry deltas in deterministic Entry order.
Its `table:v0` type ID is retained, but the old Doc-backed wire format is incompatible;
no mixed old/new history or migration is supported.

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
