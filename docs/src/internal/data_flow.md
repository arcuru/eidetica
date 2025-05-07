## Data Flow

This section illustrates a typical sequence of interactions between the user and the [core components](core_components/index.md).

1. User creates a BaseDB with a specific backend implementation
2. User creates one or more Trees within the database
3. Operations on the database involve an `EntryBuilder` to construct new, immutable `Entry` objects, which are then added to the appropriate Tree.
4. Each new Entry references its parent entries, forming a directed acyclic graph
5. Entries are stored and retrieved through the Backend interface

```mermaid
sequenceDiagram
    participant User
    participant BaseDB
    participant Tree
    participant Operation
    participant EntryBuilder
    participant RowStore_Todo_
    participant Backend

    User->>BaseDB: Create with backend
    User->>BaseDB: Create new tree ("todo")
    BaseDB->>Tree: Initialize with settings
    Tree->>Backend: Store root entry
    User->>Tree: Add Todo "Buy Milk"
    Tree->>Operation: new_operation()
    Operation->>RowStore_Todo_: get_subtree("todos")
    RowStore_Todo_->>Backend: Load relevant entries
    Backend->>RowStore_Todo_: Return entries/data
    User->>Operation: (via RowStore handle) insert(Todo{title:"Buy Milk"})
    Operation->>RowStore_Todo_: Serialize Todo, generate ID
    Operation->>EntryBuilder: Initialize with updated RowStore data & parents
    EntryBuilder->>Operation: Return built Entry
    User->>Operation: commit()
    Operation->>Backend: Store new Entry
    User->>Tree: List Todos
    Tree->>Operation: new_operation()
    Operation->>RowStore_Todo_: get_subtree("todos")
    RowStore_Todo_->>Backend: Load relevant entries
    Backend->>RowStore_Todo_: Return entries/data
    User->>Operation: (via RowStore handle) search(...)
    RowStore_Todo_->>User: Return Vec<(ID, Todo)>
```
