## Data Flow

This section illustrates a typical sequence of interactions between the user and the [core components](core_components/index.md).

1. User creates a BaseDB with a specific backend implementation
2. User creates one or more Trees within the database
3. Operations on the database involve an `EntryBuilder` to construct new, immutable `Entry` objects, which are then added to the appropriate Tree.
4. Each new Entry references its parent entries, forming a directed acyclic graph
5. Entries are stored and retrieved through the Backend interface
6. Authentication (if configured) validates and signs entries before storage

```mermaid
sequenceDiagram
    participant User
    participant BaseDB
    participant Tree
    participant Operation
    participant EntryBuilder
    participant RowStore_Todo_
    participant AuthValidator
    participant Backend

    User->>BaseDB: Create with backend
    User->>BaseDB: Create new tree ("todo")
    BaseDB->>Tree: Initialize with settings
    Tree->>Backend: Store root entry (Unverified)
    User->>Tree: Add Todo "Buy Milk"
    Tree->>Operation: new_operation()
    Note over Operation: Optional: with_auth("key_id")
    Operation->>RowStore_Todo_: get_subtree("todos")
    RowStore_Todo_->>Backend: Load relevant entries
    Backend->>RowStore_Todo_: Return entries/data
    User->>Operation: (via RowStore handle) insert(Todo{title:"Buy Milk"})
    Operation->>RowStore_Todo_: Serialize Todo, generate ID
    Operation->>EntryBuilder: Initialize with updated RowStore data & parents
    EntryBuilder->>Operation: Return built Entry
    User->>Operation: commit()

    alt Authentication Configured
        Operation->>Backend: Get signing key
        Backend->>Operation: Return private key
        Operation->>Operation: Sign entry
        Operation->>AuthValidator: Validate entry & permissions
        AuthValidator->>Operation: Return validation result
        Operation->>Backend: Store entry (Verified/Unverified)
    else No Authentication
        Operation->>Backend: Store entry (Unverified)
    end

    User->>Tree: List Todos
    Tree->>Operation: new_operation()
    Operation->>RowStore_Todo_: get_subtree("todos")
    RowStore_Todo_->>Backend: Load relevant entries
    Backend->>RowStore_Todo_: Return entries/data
    User->>Operation: (via RowStore handle) search(...)
    RowStore_Todo_->>User: Return Vec<(ID, Todo)>
```

### Authentication Flow Details

When authentication is enabled, the commit process includes additional steps:

1. **Entry Signing**: If a key ID is configured, the entry is cryptographically signed
2. **Permission Validation**: The system validates that the signing key has appropriate permissions for the operation type
3. **Bootstrap Handling**: First authenticated operation automatically configures the signing key as an admin
4. **Verification Status**: Entries are stored with a verification status (Verified/Unverified) based on validation results

This ensures data integrity and access control while maintaining backward compatibility with unsigned entries.
