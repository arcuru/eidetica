# Subtree Parent Relationships in Eidetica

## Overview

Subtree parent relationships are a critical aspect of Eidetica's Merkle-CRDT architecture. Each entry in the database can contain multiple subtrees (like "messages", "\_settings", etc.), and these subtrees maintain their own parent-child relationships within the larger DAG structure.

## How Subtree Parents Work

### Subtree Root Entries

**Subtree root entries** are entries that establish the beginning of a named subtree. They have these characteristics:

- **Contains the subtree**: The entry has a `SubTreeNode` for the named subtree
- **Empty subtree parents**: The subtree's `parents` field is empty (`[]`)
- **Normal main tree parents**: The entry still has normal parent relationships in the main tree

Example structure:

```rust,ignore
Entry {
    tree: TreeNode {
        root: "tree_id",
        parents: ["main_parent_1", "main_parent_2"], // Normal main tree parents
    },
    subtrees: [
        SubTreeNode {
            name: "messages",
            parents: [], // EMPTY - this makes it a subtree root
            data: "first_message_data",
        }
    ],
}
```

### Non-Root Subtree Entries

Subsequent entries in the subtree have the previous subtree entries as parents:

```rust,ignore
Entry {
    tree: TreeNode {
        root: "tree_id",
        parents: ["main_parent_3"],
    },
    subtrees: [
        SubTreeNode {
            name: "messages",
            parents: ["previous_messages_entry_id"], // Points to previous subtree entry
            data: "second_message_data",
        }
    ],
}
```

## Multi-Layer Validation System

The system uses multi-layer validation to ensure DAG integrity:

### 1. Entry Layer: Structural Validation

The `Entry::validate()` method enforces critical invariants:

```rust,ignore
/// CRITICAL VALIDATION RULES:
/// 1. Root entries (with "_root" subtree): May have empty parents
/// 2. Non-root entries: MUST have at least one parent in main tree
/// 3. Empty parent IDs: Always rejected

pub fn validate(&self) -> Result<()> {
    // Non-root entries MUST have main tree parents
    if !self.is_root() && self.parents()?.is_empty() {
        return Err(ValidationError::NonRootEntryWithoutParents);
    }

    // Reject empty string parent IDs
    for parent in self.parents()? {
        if parent.is_empty() {
            return Err(ValidationError::EmptyParentId);
        }
    }
    // ... additional validation
}
```

This prevents the creation of orphaned nodes that break DAG traversal.

### 2. Transaction Layer: Lazy Parent Discovery

When a transaction accesses a subtree for the first time, only then does it determine the correct subtree parents:

```rust,ignore
// Get subtree tips based on transaction context
let tips = if main_parents == current_database_tips {
    // Using current database tips - get all current subtree tips
    self.db.backend().get_store_tips(self.db.root_id(), &subtree_name)?
} else {
    // Using custom parent tips - get subtree tips reachable from those parents
    self.db.backend().get_store_tips_up_to_entries(
        self.db.root_id(),
        &subtree_name,
        &main_parents,
    )?
};

// Use the tips directly as subtree parents
builder.set_subtree_parents_mut(&subtree_name, tips);
```

The transaction system handles:

- **Normal operations**: Uses current subtree tips from the database
- **Custom parent scenarios**: Finds subtree tips reachable from specific main parents
- **First subtree entry**: Returns empty tips, creating a subtree root

### 3. Backend Storage: Final Validation Gate

The backend `put()` method serves as the **final validation gate** before persistence:

```rust,ignore
/// CRITICAL VALIDATION GATE: Final check before persistence
pub(crate) fn put(
    backend: &InMemory,
    verification_status: VerificationStatus,
    entry: Entry,
) -> Result<()> {
    // Validate entry structure before storing
    entry.validate()?;  // HARD FAILURE on invalid entries

    // ... storage operations
}
```

### 4. LCA Traversal: Subtree Root Detection

During LCA (Lowest Common Ancestor) calculations, the system correctly identifies subtree roots:

```rust,ignore
match entry.subtree_parents(subtree) {
    Ok(parents) => {
        if parents.is_empty() {
            // This entry is a subtree root - don't traverse further up this subtree
        } else {
            // Entry has parents in the subtree, add them to traversal queue
            for parent in parents {
                queue.push_back(parent);
            }
        }
    }
    Err(_) => {
        // Entry doesn't contain this subtree - ERROR, should not happen in LCA
        return Err(BackendError::EntryNotInSubtree { ... });
    }
}
```

## Common Scenarios

### Scenario 1: Normal Sequential Operations

```text
Entry 1 (root)
  └─ Entry 2 (messages subtree, parents: [])  // First message (subtree root)
      └─ Entry 3 (messages subtree, parents: [2])  // Second message
```

### Scenario 2: Bidirectional Sync

```text
Device 1: Entry 1 (root) → Entry 2 (message A, subtree parents: [])
Device 2: Syncs, gets Entry 1 & 2
Device 2: Entry 3 (message B, subtree parents: [2])
Device 1: Syncs back, creates Entry 4 (message C, subtree parents: [3])
```

### Scenario 3: Diamond Pattern

```text
        Entry 1 (root)
       /              \
   Entry 2A         Entry 2B
       \              /
        Entry 3 (merge)
```

The transaction system correctly handles finding subtree parents in diamond patterns using `get_store_tips_up_to_entries`.

## API Usage

### Creating Entries Through Transactions (Recommended)

```rust,ignore
// The transaction automatically handles subtree parent discovery
let op = database.new_transaction()?;
let store = op.get_store::<DocStore>("messages")?;
store.set("content", "Hello world")?;
let entry_id = op.commit()?; // Parents automatically determined
```

### Manual Entry Creation (Internal Only)

```rust,ignore
// ✅ CORRECT: Root entry (doesn't need parents)
let entry = Entry::root_builder()
    .set_subtree_data("data", "content")
    .build();

// ✅ CORRECT: Non-root entry with parents
let entry = Entry::builder("tree_id")
    .set_parents(vec!["parent_id"])
    .set_subtree_data("messages", "data")
    .set_subtree_parents("messages", vec!["subtree_parent_id"])
    .build();

// ❌ WRONG: Non-root entry without parents (WILL FAIL VALIDATION)
let entry = Entry::builder("tree_id").build();
```

## Debugging Tips

### Identifying Subtree Root Entries

Look for entries where:

- `entry.subtree_parents(subtree_name)` returns `Ok(vec![])` (empty parents)
- The entry contains the subtree in question
- This indicates the entry is the starting point for that subtree

### Common Error Messages

- `"Entry is subtree root (empty parents)"` - Normal operation, entry starts a new subtree
- `"Entry encountered in subtree LCA that doesn't contain the subtree"` - Invalid state, entry should not be in subtree operations
- `"Non-root entry has empty main tree parents"` - Validation failure, entry missing required parents

### Validation Points

1. **Entry validation**: Check that entries have proper main tree parents and valid parent IDs
2. **Transaction commit**: Subtree parents are automatically discovered and set
3. **Backend storage**: Final validation before persistence
4. **LCA operations**: Proper subtree traversal based on subtree parent relationships

## Best Practices

1. **Use transactions** for all entry creation - they handle parent discovery automatically
2. **Validate entries** before manual storage or transmission
3. **Test subtree scenarios** thoroughly, especially with custom parent relationships
4. **Monitor debug logs** for subtree parent discovery during development

## Implementation Details

The subtree parent system is implemented across:

- `crates/lib/src/entry/mod.rs`: Entry structure and validation
- `crates/lib/src/transaction/mod.rs`: Automatic parent discovery
- `crates/lib/src/backend/database/in_memory/storage.rs`: Final validation gate
- `crates/lib/src/backend/database/in_memory/traversal.rs`: LCA operations with subtree awareness

Each layer ensures proper subtree parent relationships and DAG integrity.
