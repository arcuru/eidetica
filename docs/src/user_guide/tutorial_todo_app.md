# Developer Walkthrough: Building with Eidetica

This guide walks through the [Todo Example](https://github.com/arcuru/eidetica/tree/main/examples/todo) (`examples/todo/src/main.rs`) to explain Eidetica's core concepts. The example is a simple command-line todo app that demonstrates databases, transactions, stores, and Y-CRDT integration.

## Core Concepts

The Todo example demonstrates Eidetica's key components working together in a real application.

### 1. The Database Backend (`Instance`)

The `Instance` is your main entry point. It wraps a storage backend and manages users and databases.

The Todo example loads or initialises an instance by URL. `connect_or_create` handles both the first-run and reopen cases:

```rust,ignore
async fn load_or_create_instance(path: &Path) -> Result<Instance> {
    // SQLite handles both creation and loading automatically.
    // The URL is passed through to sqlx — `mode=rwc` tells sqlx to
    // create the database file on first run if it doesn't exist.
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let (instance, _maybe_user) =
        Instance::connect_or_create(&url, NewAccount::passwordless("todo-user")).await?;

    println!("✓ Instance initialized");
    Ok(instance)
}
```

Data is automatically saved to the SQLite file. Authentication is managed through the AccountSession system (see below).

### 2. Users (`AccountSession`)

Users provide authenticated access to databases. An `AccountSession` manages signing keys and database access. The Todo example creates a passwordless user for simplicity, bootstrapping it as the initial admin on a fresh data file via `Instance::connect_or_create`:

```rust,ignore
async fn load_or_create_instance_and_user(path: &Path) -> Result<(Instance, AccountSession)> {
    use eidetica::NewAccount;

    let username = "todo-user";
    // SQLite via the URL surface — `sqlite://<path>` is handed through to
    // sqlx, and `mode=rwc` tells sqlx to create the file on first run.
    let url = format!("sqlite://{}?mode=rwc", path.display());

    let (instance, maybe_user) =
        Instance::connect_or_create(&url, NewAccount::passwordless(username)).await?;

    let user = match maybe_user {
        Some(mut session) => {
            // Only the first creator initializes application roots. This embedded
            // SQLite example has exclusive instance storage ownership. Returning
            // logins never repair missing metadata by creating a replacement.
            let key = session.get_default_key()?;
            let mut settings = Doc::new();
            settings.set("name", "todo");
            let todos = session.create_database(settings, &key).await?;
            let tx = session.account().new_transaction().await?;
            tx.get_store::<Table<ID>>("todo_roots").await?
                .set("todos", todos.root_id().clone()).await?;
            tx.commit().await?;
            println!("✓ Initialised new instance and bootstrapped {username} as admin");
            session
        }
        None => {
            let u = instance.login_account(username, None).await?;
            println!("✓ Logged in as passwordless user: {username}");
            u
        }
    };

    Ok((instance, user))
}
```

### 3. Databases (`Database`)

A `Database` is a primary organizational unit within an `Instance`. Think of it somewhat like a schema or a logical database within a larger instance. It acts as a container for related data, managed through `Stores`. Databases provide versioning and history tracking for the data they contain.

The Todo example keeps the todo database ID in its Account root, independent of the target's display name:

```rust,ignore
async fn open_todo_database(session: &AccountSession) -> Result<Database> {
    let roots = session.account().get_store_viewer::<Table<ID>>("todo_roots").await?;
    let id = roots.get("todos").await?;
    session.open_database(&id).await
}
```

Setup runs only for a newly created embedded instance, under exclusive backend ownership.
A returning login never creates a replacement on missing metadata or lookup failure.
Partial setup requires explicit repair; ordinary CRDT writes are not distributed create-if-absent locks.
See [Accounts](../design/accounts.md#initialization-and-concurrency) for the initialization and error contract.

### 4. Transactions and Stores

All data modifications happen within a `Transaction`. Transactions ensure atomicity and are automatically authenticated using the database's default signing key.

Within a transaction, you access `Stores` - flexible containers for different types of data. The Todo example uses `Table<Todo>` to store todo items with unique IDs.

### 5. The Todo Data Structure

The example defines a `Todo` struct that must implement `Serialize` and `Deserialize` to work with Eidetica:

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub title: String,
    pub completed: bool,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Todo {
    pub fn new(title: String) -> Self {
        Self {
            title,
            completed: false,
            created_at: Utc::now(),
            completed_at: None,
        }
    }

    pub fn complete(&mut self) {
        self.completed = true;
        self.completed_at = Some(Utc::now());
    }
}
```

### 6. Adding a Todo

The `add_todo()` function shows how to insert data into a `Table` store:

```rust,ignore
async fn add_todo(database: &Database, title: String) -> Result<()> {
    // Start an atomic transaction (uses default auth key)
    let txn = database.new_transaction().await?;

    // Get a handle to the 'todos' Table store
    let todos_store = txn.get_store::<Table<Todo>>("todos").await?;

    // Create a new todo
    let todo = Todo::new(title);

    // Insert the todo into the Table
    // The Table will generate a unique ID for it
    let todo_id = todos_store.insert(todo).await?;

    // Commit the transaction
    txn.commit().await?;

    println!("Added todo with ID: {todo_id}");

    Ok(())
}
```

### 7. Updating a Todo

The `complete_todo()` function demonstrates reading and updating data:

```rust,ignore
async fn complete_todo(database: &Database, id: &str) -> Result<()> {
    // Start an atomic transaction (uses default auth key)
    let txn = database.new_transaction().await?;

    // Get a handle to the 'todos' Table store
    let todos_store = txn.get_store::<Table<Todo>>("todos").await?;

    // Get the todo from the Table
    let mut todo = todos_store.get(id).await?;

    // Mark the todo as complete
    todo.complete();

    // Update the todo in the Table
    todos_store.set(id, todo).await?;

    // Commit the transaction
    txn.commit().await?;

    Ok(())
}
```

These examples show the typical pattern: start a transaction, get a store handle, perform operations, and commit.

### 8. Y-CRDT Integration (`YDoc`)

The example also uses `YDoc` stores for user information and preferences. Y-CRDTs are designed for collaborative editing:

```rust,ignore
async fn set_user_info(
    database: &Database,
    name: Option<&String>,
    email: Option<&String>,
    bio: Option<&String>,
) -> Result<()> {
    // Start an atomic transaction (uses default auth key)
    let txn = database.new_transaction().await?;

    // Get a handle to the 'user_info' YDoc store
    let user_info_store = txn.get_store::<YDoc>("user_info").await?;

    // Update user information using the Y-CRDT document
    user_info_store.with_doc_mut(|doc| {
        let user_info_map = doc.get_or_insert_map("user_info");
        let mut txn = doc.transact_mut();

        if let Some(name) = name {
            user_info_map.insert(&mut txn, "name", name.clone());
        }
        if let Some(email) = email {
            user_info_map.insert(&mut txn, "email", email.clone());
        }
        if let Some(bio) = bio {
            user_info_map.insert(&mut txn, "bio", bio.clone());
        }

        Ok(())
    }).await?;

    // Commit the transaction
    txn.commit().await?;
    Ok(())
}
```

The example demonstrates using different store types in one database:

- **"todos"** (`Table<Todo>`): Stores todo items with automatic ID generation
- **"user_info"** (`YDoc`): Stores user profile using Y-CRDT Maps
- **"user_prefs"** (`YDoc`): Stores preferences using Y-CRDT Maps

This shows how you can choose the most appropriate data structure for each type of data.

## Running the Todo Example

To see these concepts in action, you can run the Todo example:

```bash
# Navigate to the example directory
cd examples/todo

# Build the example
cargo build

# Run commands (this will create todo_db.json)
cargo run -- add "Learn Eidetica"
cargo run -- list
# Note the ID printed
cargo run -- complete <id_from_list>
cargo run -- list
```

Refer to the example's [README.md](https://github.com/arcuru/eidetica/blob/main/examples/todo/README.md) and [test.sh](https://github.com/arcuru/eidetica/blob/main/examples/todo/test.sh) for more usage details.

This walkthrough provides a starting point. Explore the Eidetica documentation and other examples to learn about more advanced features like different store types, history traversal, and distributed capabilities.
