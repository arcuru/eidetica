# Developer Walkthrough: Building with Eidetica

This guide walks through the [Todo Example](../../examples/todo/) (`examples/todo/src/main.rs`) to explain Eidetica's core concepts. The example is a simple command-line todo app that demonstrates databases, transactions, stores, and Y-CRDT integration.

## Core Concepts

The Todo example demonstrates Eidetica's key components working together in a real application.

### 1. The Database Backend (`Instance`)

The `Instance` is your main entry point. It wraps a storage backend and provides access to your databases.

The Todo example implements `load_or_create_db()` to handle loading existing databases or creating new ones:

```rust,ignore
fn load_or_create_db(path: &PathBuf) -> Result<Instance> {
    let db = if path.exists() {
        let backend = InMemory::load_from_file(path)?;
        Instance::open(Box::new(backend))?
    } else {
        let backend = InMemory::new();
        Instance::open(Box::new(backend))?
    };

    // Ensure the todo app authentication key exists
    let existing_keys = db.list_private_keys()?;

    if !existing_keys.contains(&TODO_APP_KEY_NAME.to_string()) {
        db.add_private_key(TODO_APP_KEY_NAME)?;
        println!("✓ New authentication key created");
    }

    Ok(db)
}
```

This shows how the `InMemory` backend can persist to disk and how authentication keys are managed.

### 2. Databases (`Database`)

A `Database` is a primary organizational unit within a `Instance`. Think of it somewhat like a schema or a logical database within a larger instance. It acts as a container for related data, managed through `Stores`. Databases provide versioning and history tracking for the data they contain.

The Todo example uses a single Database named "todo":

```rust,ignore
fn load_or_create_todo_database(db: &Instance) -> Result<Database> {
    let database_name = "todo";

    // Try to find the database by name
    let mut database = match db.find_database(database_name) {
        Ok(mut databases) => {
            databases.pop().unwrap() // unwrap is safe because find_database errors if empty
        }
        Err(e) if e.is_not_found() => {
            // If not found, create a new one
            println!("No existing todo database found, creating a new one...");
            let mut settings = Doc::new();
            settings.set_string("name", database_name);

            db.new_database(settings, TODO_APP_KEY_NAME)?
        }
        Err(e) => return Err(e),
    };

    // Set the default authentication key for this database
    database.set_default_auth_key(TODO_APP_KEY_NAME);

    Ok(database)
}
```

This shows how `find_database()` searches for existing databases by name, and `set_default_auth_key()` configures automatic authentication for all transactions.

### 3. Transactions and Stores

All data modifications happen within a `Transaction`. Transactions ensure atomicity and are automatically authenticated using the database's default signing key.

Within a transaction, you access `Stores` - flexible containers for different types of data. The Todo example uses `Table<Todo>` to store todo items with unique IDs.

### 4. The Todo Data Structure

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

### 5. Adding a Todo

The `add_todo()` function shows how to insert data into a `Table` store:

```rust,ignore
fn add_todo(database: &Database, title: String) -> Result<()> {
    // Start an atomic transaction (uses default auth key)
    let op = database.new_transaction()?;

    // Get a handle to the 'todos' Table store
    let todos_store = op.get_store::<Table<Todo>>("todos")?;

    // Create a new todo
    let todo = Todo::new(title);

    // Insert the todo into the Table
    // The Table will generate a unique ID for it
    let todo_id = todos_store.insert(todo)?;

    // Commit the transaction
    op.commit()?;

    println!("Added todo with ID: {todo_id}");

    Ok(())
}
```

### 6. Updating a Todo

The `complete_todo()` function demonstrates reading and updating data:

```rust,ignore
fn complete_todo(database: &Database, id: &str) -> Result<()> {
    // Start an atomic transaction (uses default auth key)
    let op = database.new_transaction()?;

    // Get a handle to the 'todos' Table store
    let todos_store = op.get_store::<Table<Todo>>("todos")?;

    // Get the todo from the Table
    let mut todo = todos_store.get(id)?;

    // Mark the todo as complete
    todo.complete();

    // Update the todo in the Table
    todos_store.set(id, todo)?;

    // Commit the transaction
    op.commit()?;

    Ok(())
}
```

These examples show the typical pattern: start a transaction, get a store handle, perform operations, and commit.

### 7. Y-CRDT Integration (`YDoc`)

The example also uses `YDoc` stores for user information and preferences. Y-CRDTs are designed for collaborative editing:

```rust,ignore
fn set_user_info(
    database: &Database,
    name: Option<&String>,
    email: Option<&String>,
    bio: Option<&String>,
) -> Result<()> {
    // Start an atomic transaction (uses default auth key)
    let op = database.new_transaction()?;

    // Get a handle to the 'user_info' YDoc store
    let user_info_store = op.get_store::<YDoc>("user_info")?;

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
    })?;

    // Commit the transaction
    op.commit()?;
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

Refer to the example's [README.md](../../examples/todo/README.md) and [test.sh](../../examples/todo/test.sh) for more usage details.

This walkthrough provides a starting point. Explore the Eidetica documentation and other examples to learn about more advanced features like different store types, history traversal, and distributed capabilities.
