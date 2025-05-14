# Entries & Trees

The basic units of data and organization in Eidetica.

## Entries

Entries are the fundamental building blocks in Eidetica. An Entry represents an atomic unit of data with the following characteristics:

- **Content-addressable**: Each entry has a unique ID derived from its content, similar to Git commits.
- **Immutable**: Once created, entries cannot be modified.
- **Parent references**: Entries maintain references to their parent entries, forming a directed acyclic graph (DAG).
- **Tree association**: Each entry belongs to a tree and can reference parent entries within both the main tree and subtrees.
- **Subtree data**: Entries can contain data for one or more subtrees, representing different aspects or types of data.

Entries function similar to commits in Git - they represent a point-in-time snapshot of data with links to previous states, enabling history tracking.

## Trees

A Tree in Eidetica is a logical container for related entries, conceptually similar to:

- A table in a relational database
- A branch in a version control system
- A collection in a document database

Key characteristics of Trees:

- **Root Entry**: Each tree has a root entry that serves as its starting point.
- **Named Identity**: Trees typically have a name stored in their settings subtree.
- **History Tracking**: Trees maintain the complete history of all changes as a linked graph of entries.
- **Subtree Organization**: Data within a tree is organized into named subtrees, each potentially using different data structures.
- **Atomic Operations**: All changes to a tree happen through atomic operations, which create new entries.

## Tree Operations

You interact with Trees through Operations:

```rust
// Create a new operation
let op = tree.new_operation()?;

// Access subtrees and perform actions
let settings = op.get_subtree::<KVStore>("settings")?;
settings.set("version", "1.2.0")?;

// Commit the changes, creating a new Entry
let new_entry_id = op.commit()?;
```

When you commit an operation, Eidetica:

1. Creates a new Entry containing all changes
2. Links it to the appropriate parent entries
3. Adds it to the tree's history
4. Returns the ID of the new entry

## Tree Settings

Each Tree maintains its settings as a key-value store in a special "settings" subtree:

```rust
// Get the settings subtree
let settings = tree.get_settings()?;

// Access settings
let name = settings.get("name")?;
let version = settings.get("version")?;
```

Common settings include:

- `name`: The identifier for the tree (used by `BaseDB::find_tree`). This is the primary standard setting currently used.
- _Other application-specific settings can be stored here._

<!-- TODO: Define more standard tree settings if they emerge, e.g., for schema information or access control -->

## Tips and History

Trees in Eidetica maintain a concept of "tips" - the latest entries in the tree's history:

```rust
// Get the current tip entries
let tips = tree.get_tips()?;
```

Tips represent the current state of the tree. As new operations are committed, new tips are created, and the history grows. This historical information remains accessible, allowing you to:

- Track all changes to data over time
- Reconstruct the state at any point in history (requires manual traversal or specific backend support - see [Backends](backends.md))

<!-- TODO: Implement and document high-level history browsing APIs (e.g., `tree.get_entry_at_timestamp()`, `tree.diff()`) -->

## Tree vs. Subtree

While a Tree is the logical container, the actual data is organized into Subtrees. This separation allows:

- Different types of data structures within a single Tree
- Type-safe access to different parts of your data
- Fine-grained history tracking by subtree
- Efficient partial replication and synchronization

See [Subtrees](subtrees.md) for more details on how data is structured within a Tree.
