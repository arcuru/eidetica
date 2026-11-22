# Subtrees

Each entry can contain multiple subtrees (e.g., "messages", "\_settings"). Subtrees maintain independent parent-child relationships within the DAG.

## Subtree Root Entries

A **subtree root** is an entry that starts a named subtree:

- Contains a `SubTreeNode` for the subtree
- Has **empty subtree parents** (`[]`)
- Still has normal main tree parents

For ordinary Stores this may start another independent branch even when earlier
contributions to the same Store exist in main ancestry. Empty Store parents do
not mean the Entry has no causal history. `_settings` is different: its writes
must consume the complete causal pre-write settings frontier.

```rust,ignore
Entry {
    tree: TreeNode {
        root: "tree_id",
        parents: ["main_parent_id"],  // Normal main tree parents
    },
    subtrees: [
        SubTreeNode {
            name: "messages",
            parents: [],  // Empty = subtree root
            data: "...",
        }
    ],
}
```

Subsequent entries reference previous subtree entries as parents:

```rust,ignore
SubTreeNode {
    name: "messages",
    parents: ["previous_messages_entry_id"],
    data: "...",
}
```

## Automatic Parent Discovery

Transactions automatically determine subtree parents:

1. If using current database tips → get current subtree tips
2. If using custom parents → find subtree tips reachable from those parents
3. If first subtree entry → empty tips (creates subtree root)

**Use transactions** for entry creation; they handle parent discovery automatically.
Discovery includes every unconsumed Store tip reachable through the selected
main ancestry, not just Store contributions at main tips. It is an authoring
default, not a requirement that every ordinary Store write consume that frontier.
Named parents must still target the same Store and database and lie in main-only
ancestry. See the [A/B/C example](../design/verification.md#store-parent-frontiers).

## Subtree Heights

Each subtree can have its own height value. Heights provide deterministic ordering when merging DAG branches after network splits. Entries are processed in height order, with ties broken by entry hash:

- **Height = None**: Subtree inherits the tree (main database) height
- **Height = Some(h)**: Subtree has an independent height value

The `Entry.subtree_height()` accessor handles inheritance transparently:

```rust,ignore
// If subtree.height is None, returns tree.height (inheritance)
// If subtree.height is Some(h), returns the independent value
let height = entry.subtree_height("messages")?;
```

Effective height must strictly increase over every named Store-parent edge;
positive gaps are valid. Main observation alone adds no ordinary Store edge.
Per-subtree height strategies are configured via the `_index` registry. System subtrees (`_settings`, `_index`, `_root`) always inherit from the tree.

See [Height Strategy Design](../design/height_strategy.md) for implementation details.

## Implementation

See `src/entry/mod.rs` and `src/transaction/mod.rs` for implementation.
