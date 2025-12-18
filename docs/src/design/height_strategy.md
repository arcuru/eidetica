> ✅ **Status: Implemented**
>
> Height strategies are fully implemented with database-level and per-subtree configuration.

# Height Strategy Design

This document describes the design of configurable height strategies in Eidetica, allowing applications to choose how entry heights are calculated. Heights provide deterministic ordering when merging DAG branches after network splits. Entries are processed in height order, with ties broken by entry hash.

## Problem Statement

Entries in Eidetica form a Merkle-DAG where each entry references its parents. When merging DAG branches after a network split, entries must be processed in a deterministic order—height provides this ordering, with ties broken by entry hash. Different applications have different ordering requirements:

- **Offline-first apps**: Need simple sequential ordering
- **Time-series data**: Need timestamp-based ordering for accurate event sequencing
- **Audit logs**: May need independent counters separate from main data

A one-size-fits-all approach limits the applicability of the database.

## Goals

1. Allow applications to configure height calculation strategy at the database level
2. Support per-subtree overrides for mixed-strategy use cases
3. Keep the common case simple (defaults work for most applications)

## Non-Goals

1. Custom height calculation algorithms (only predefined strategies)
2. Dynamic strategy switching based on content
3. Per-entry strategy selection

## Solution Overview

A two-level configuration system:

1. **Database-level strategy**: Stored in `_settings`, applies to the main tree and all subtrees by default
2. **Per-subtree overrides**: Stored in `_index` registry, allows individual subtrees to use independent height tracking

### Height Strategies

| Strategy      | Calculation                     | Storage                       |
| ------------- | ------------------------------- | ----------------------------- |
| `Incremental` | `max(parent_heights) + 1`       | Small integers (0, 1, 2, ...) |
| `Timestamp`   | `max(timestamp_ms, parent + 1)` | Milliseconds since Unix epoch |

### Inheritance Model

<!-- Code block ignored: Diagram showing data structure, not compilable code -->

```text
Entry
├── tree.height         ← Database HeightStrategy (from _settings)
└── subtrees[]
    └── height          ← None = inherit tree height (not serialized)
                        ← Some(h) = independent (per _index settings)
```

Subtree height is `Option<u64>`: `None` means inherit from the tree, `Some(h)` is an independent height. When `None`, the field is omitted from serialization entirely.

## Implementation Details

### Storage Locations

**Database-level strategy** (`_settings`):

```json
{
  "height_strategy": "timestamp",
  "name": "events_db",
  "auth": { ... }
}
```

**Per-subtree settings** (`_index`):

```json
{
  "messages": {
    "type": "docstore:v0",
    "config": "{}"
  },
  "audit_log": {
    "type": "docstore:v0",
    "config": "{}",
    "settings": { "height_strategy": "incremental" }
  }
}
```

### Height Calculation in Transaction.commit()

During commit, heights are calculated as follows:

1. **Tree height**: Use database-level strategy from `_settings`
2. **Subtree heights**:
   - System subtrees (`_settings`, `_index`, `_root`): Always inherit (height = None)
   - User subtrees: Check `_index` for strategy override
     - Override found: Calculate independent height as `Some(h)`
     - No override: Leave height as `None` (inherit)

### Entry.subtree_height() Accessor

<!-- Code block ignored: Simplified pseudocode showing accessor logic -->

```rust,ignore
pub fn subtree_height(&self, name: &str) -> Result<u64> {
    let subtree = self.find_subtree(name)?;
    Ok(subtree.height.unwrap_or_else(|| self.height()))
}
```

This provides transparent inheritance at read time.

## Alternative Approaches Considered

### Sentinel Value (0 = Inherit)

Use 0 as a sentinel value instead of Option:

<!-- Code block ignored: Illustrative struct showing rejected approach -->

```rust,ignore
struct SubTreeNode {
    height: u64,  // 0 means inherit
}
```

**Rejected**: Overloads the meaning of 0, preventing independent subtrees from having height 0. Option is semantically clearer.

### Strategy Stored in Each Entry

Store the strategy used alongside the height:

<!-- Code block ignored: Illustrative struct showing rejected approach -->

```rust,ignore
struct SubTreeNode {
    height: u64,
    height_strategy: Option<HeightStrategy>,
}
```

**Rejected**: Bloats entry size and complicates serialization. Strategy configuration is a database/subtree property, not an entry property.

### No Per-Subtree Override

Only support database-level strategy:

**Rejected**: Limits use cases. For example, an app with timestamp-ordered messages might want incremental audit logs for simpler debugging.

## Future Considerations

1. **Additional strategies**: Could add `Hybrid` (timestamp with sub-millisecond counter) for high-throughput scenarios
2. **Strategy versioning**: If strategy semantics change, version identifiers allow migration
3. **Query optimization**: Height values enable efficient range queries and pagination
