### CRDT Implementation

Eidetica includes a trait-based system for Conflict-free Replicated Data Types (CRDTs) that enables conflict resolution. These are typically accessed via [`Operation::get_subtree`](basedb_tree.md) similarly to [`RowStore`](subtrees.md), but implement the `CRDT` trait for merge capabilities.

The goal is to support [Merkle-CRDT principles](../crdt_principles.md) where the CRDT state is stored within an [Entry's](entry.md) `RawData` and can be deterministically merged.

```mermaid
classDiagram
    class Data {
        <<interface>>
        +Serialize
        +Deserialize
    }

    class CRDT {
        <<interface>>
        +Default
        +Data
        +merge(&self, other: &Self) Result<Self>
    }

    class KVOverWrite {
        -HashMap<String, Option<String>> data
        +new() KVOverWrite
        +from_hashmap(data: HashMap<String, String>) KVOverWrite
        +get(key: &str) Option<&String>
        +set(key: String, value: String) &mut Self
        +remove(key: &str) Option<String>
        +as_hashmap() &HashMap<String, Option<String>>
        +merge(&self, other: &Self) Result<Self>
    }

    class NestedValue {
        <<enum>>
        +String(String)
        +Map(KVNested)
        +Deleted
    }

    class KVNested {
        -HashMap<String, NestedValue> data
        +new() KVNested
        +get(key: &str) Option<&NestedValue>
        +set(key: String, value: NestedValue) &mut Self
        +set_string(key: String, value: String) &mut Self
        +set_map(key: String, value: KVNested) &mut Self
        +remove(key: &str) Option<NestedValue>
        +as_hashmap() &HashMap<String, NestedValue>
        +merge(&self, other: &Self) Result<Self>
    }

    CRDT --|> Data : requires
    KVOverWrite ..|> CRDT : implements
    KVOverWrite ..|> Data : implements
    KVNested ..|> CRDT : implements
    KVNested ..|> Data : implements
    KVNested -- NestedValue : uses
```

- **CRDT Trait**: Defines a `merge` operation for resolving conflicts between divergent states. Implementors must also implement `Serialize`, `Deserialize`, and `Default`.

- **KVOverWrite**: A simple key-value CRDT implementation using a last-write-wins strategy:

  - Uses a `HashMap<String, Option<String>>` to store data
  - Supports tombstones via `Option<String>` values where `None` represents a deleted key
  - `get()` returns only non-tombstone values, while `as_hashmap()` returns all keys including tombstones
  - `remove()` doesn't actually remove keys but sets them to `None` (a tombstone) to track deletions
  - During merge, if a key exists in both CRDTs, the `other` value always wins (last-write-wins)
  - Tombstones are preserved during merges to ensure proper deletion propagation

- **KVNested**: A nested key-value CRDT implementation:

  - Supports arbitrary nesting of maps and string values via the `NestedValue` enum
  - `NestedValue` can be a `String`, another `KVNested` map, or `Deleted` (tombstone)
  - Implements recursive merging for nested maps
  - Provides specific methods for setting string values (`set_string`) and map values (`set_map`)
  - Uses tombstones (`NestedValue::Deleted`) to track deletions
  - During merges, if a key exists in both CRDTs:
    - If both have maps at that key, the maps are recursively merged
    - If types differ (map vs string) or one side has a tombstone, the `other` side's value wins
    - Tombstones are preserved during merges

- **Serialization**: CRDTs implementing the trait are serialized to/from JSON (by default) for storage in `Entry`'s `RawData`.
- **Multiple CRDT Support**: The design allows for different CRDT types (each implementing the `CRDT` trait) to be used for different subtrees within the same `Tree`.

### Implementing a Custom CRDT

To add a new CRDT type:

1.  Define your CRDT struct (e.g., `struct MySet { items: HashSet<String> }`).
2.  Implement `Default`, `serde::Serialize`, `serde::Deserialize` for your struct.
3.  Implement the marker trait: `impl Data for MySet {}`.
4.  Implement the `CRDT` trait:
    ```rust
    impl CRDT for MySet {
        fn merge(&self, other: &Self) -> Result<Self> {
            // Implement your deterministic merge logic here
            let merged_items = self.items.union(&other.items).cloned().collect();
            Ok(MySet { items: merged_items })
        }
    }
    ```
5.  **(Optional but Recommended)** Create a corresponding `SubTree` handle (e.g., `MySetHandle`) that implements the `SubTree` trait. This handle provides a user-friendly API and interacts with `AtomicOp` (`get_local_data`, `get_full_state`, `update_subtree`) to manage the CRDT state during operations.

### Using Tombstones

Tombstones are an important concept in CRDTs to ensure proper deletion propagation across distributed systems:

1. Instead of physically removing data, we mark it as deleted with a tombstone
2. Tombstones are retained and synchronized between replicas
3. This ensures that a deletion in one replica eventually propagates to all replicas
4. Both `KVOverWrite` and `KVNested` use tombstones to represent deleted entries
