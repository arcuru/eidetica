# Entries & Databases

The basic units of data and organization in Eidetica.

## Entries

Entries are the fundamental building blocks in Eidetica. An Entry represents an atomic unit of data with the following characteristics:

- **Content-addressable**: Each entry has a unique ID derived from its content, similar to Git commits.
- **Immutable**: Once created, entries cannot be modified.
- **Parent references**: Entries maintain references to their parent entries, forming a directed acyclic graph (DAG).
- **Database association**: Each entry belongs to a database and can reference parent entries within both the main database and stores.
- **Store data**: Entries can contain data for one or more stores, representing different aspects or types of data.

Entries function similar to commits in Git - they represent a point-in-time snapshot of data with links to previous states, enabling history tracking.

## Databases

A Database in Eidetica is a logical container for related entries, conceptually similar to:

- A traditional database containing multiple tables
- A branch in a version control system
- A collection in a document database

Key characteristics of Databases:

- **Root Entry**: Each database has a root entry that serves as its starting point.
- **Named Identity**: Databases typically have a name stored in their settings store.
- **History Tracking**: Databases maintain the complete history of all changes as a linked graph of entries.
- **Store Organization**: Data within a database is organized into named stores, each potentially using different data structures.
- **Atomic Operations**: All changes to a database happen through transactions, which create new entries.

## Database Operations

You interact with Databases through Operations:

```rust,ignore
// Create a new operation
let op = database.new_operation()?;

// Access stores and perform actions
let settings = op.get_store::<DocStore>("settings")?;
settings.set("version", "1.2.0")?;

// Commit the changes, creating a new Entry
let new_entry_id = op.commit()?;
```

When you commit an operation, Eidetica:

1. Creates a new Entry containing all changes
2. Links it to the appropriate parent entries
3. Adds it to the database's history
4. Returns the ID of the new entry

## Database Settings

Each Database maintains its settings as a key-value store in a special "settings" store:

```rust,ignore
// Get the settings store
let settings = database.get_settings()?;

// Access settings
let name = settings.get("name")?;
let version = settings.get("version")?;
```

Common settings include:

- `name`: The identifier for the database (used by `Instance::find_database`). This is the primary standard setting currently used.
- _Other application-specific settings can be stored here._

<!-- TODO: Define more standard database settings if they emerge, e.g., for schema information or access control -->

## Tips and History

Databases in Eidetica maintain a concept of "tips" - the latest entries in the database's history:

```rust,ignore
// Get the current tip entries
let tips = database.get_tips()?;
```

Tips represent the current state of the database. As new transactions are committed, new tips are created, and the history grows. This historical information remains accessible, allowing you to:

- Track all changes to data over time
- Reconstruct the state at any point in history (requires manual traversal or specific backend support - see [Backends](backends.md))

<!-- TODO: Implement and document high-level history browsing APIs (e.g., `database.get_entry_at_timestamp()`, `database.diff()`) -->

## Database vs. Store

While a Database is the logical container, the actual data is organized into Stores. This separation allows:

- Different types of data structures within a single Database
- Type-safe access to different parts of your data
- Fine-grained history tracking by store
- Efficient partial replication and synchronization

See [Stores](stores.md) for more details on how data is structured within a Database.
