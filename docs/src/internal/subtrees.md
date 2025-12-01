# Subtrees

Each entry can contain multiple subtrees (e.g., "messages", "\_settings"). Subtrees maintain independent parent-child relationships within the DAG.

## Subtree Root Entries

A **subtree root** is an entry that starts a named subtree:

- Contains a `SubTreeNode` for the subtree
- Has **empty subtree parents** (`[]`)
- Still has normal main tree parents

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

**Always use transactions** for entry creation - they handle parent discovery automatically.

See `src/entry/mod.rs` and `src/transaction/mod.rs` for implementation.
