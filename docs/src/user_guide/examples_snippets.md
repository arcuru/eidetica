# Code Examples

This page provides focused code snippets for common tasks in EideticaDB.

_Assumes basic setup like `use eideticadb::{BaseDB, Tree, Error, ...};` and error handling (`?`) for brevity._

## 1. Initializing the Database (`BaseDB`)

```rust
use eideticadb::backend::InMemoryBackend;
use eideticadb::basedb::BaseDB;
use std::path::PathBuf;

// Option A: Create a new, empty in-memory database
let backend_new = InMemoryBackend::new();
let db_new = BaseDB::new(Box::new(backend_new));

// Option B: Load from a previously saved file
let db_path = PathBuf::from("my_database.json");
if db_path.exists() {
    match InMemoryBackend::load_from_file(&db_path) {
        Ok(backend_loaded) => {
            let db_loaded = BaseDB::new(Box::new(backend_loaded));
            println!("Database loaded successfully.");
            // Use db_loaded
        }
        Err(e) => {
            eprintln!("Error loading database: {}", e);
            // Handle error, maybe create new
        }
    }
} else {
    println!("Database file not found, creating new.");
    // Use db_new from Option A
}
```

## 2. Creating or Loading a Tree

```rust
use eideticadb::data::KVOverWrite;

let db: BaseDB = /* obtained from step 1 */;
let tree_name = "my_app_data";

let tree = match db.find_tree(tree_name) {
    Ok(mut trees) => {
        println!("Found existing tree: {}", tree_name);
        trees.pop().unwrap() // Assume first one is correct
    }
    Err(Error::NotFound) => {
        println!("Creating new tree: {}", tree_name);
        let mut settings = KVOverWrite::new();
        settings.set("name".to_string(), tree_name.to_string());
        db.new_tree(settings)?
    }
    Err(e) => return Err(e.into()), // Propagate other errors
};

println!("Using Tree with root ID: {}", tree.root_id());
```

## 3. Writing Data (KVStore Example)

```rust
use eideticadb::subtree::KVStore;

let tree: Tree = /* obtained from step 2 */;

// Start an operation
let op = tree.new_operation()?;

{
    // Get the KVStore subtree handle (scoped)
    let config_store = op.get_subtree::<KVStore>("configuration")?;

    // Set some values
    config_store.set("api_key", "secret-key-123")?;
    config_store.set("retry_count", "3")?;

    // Overwrite a value
    config_store.set("api_key", "new-secret-456")?;

    // Remove a value
    config_store.remove("old_setting")?; // Ok if it doesn't exist
}

// Commit the changes atomically
let entry_id = op.commit()?;
println!("KVStore changes committed in entry: {}", entry_id);
```

## 4. Writing Data (RowStore Example)

```rust
use eideticadb::subtree::RowStore;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Task {
    description: String,
    completed: bool,
}

let tree: Tree = /* obtained from step 2 */;

// Start an operation
let op = tree.new_operation()?;
let inserted_id;

{
    // Get the RowStore handle
    let tasks_store = op.get_subtree::<RowStore<Task>>("tasks")?;

    // Insert a new task
    let task1 = Task { description: "Buy milk".to_string(), completed: false };
    inserted_id = tasks_store.insert(task1)?;
    println!("Inserted task with ID: {}", inserted_id);

    // Insert another task
    let task2 = Task { description: "Write docs".to_string(), completed: false };
    tasks_store.insert(task2)?;

    // Update the first task (requires getting it first if you only have the ID)
    if let Ok(mut task_to_update) = tasks_store.get(&inserted_id) {
        task_to_update.completed = true;
        tasks_store.set(&inserted_id, task_to_update)?;
        println!("Updated task {}", inserted_id);
    } else {
        eprintln!("Task {} not found for update?", inserted_id);
    }

    // Remove a task (if you knew its ID)
    // tasks_store.remove(&some_other_id)?;
}

// Commit all inserts/updates/removes
let entry_id = op.commit()?;
println!("RowStore changes committed in entry: {}", entry_id);
```

## 5. Reading Data (KVStore Viewer)

```rust
use eideticadb::subtree::KVStore;

let tree: Tree = /* obtained from step 2 */;

// Get a read-only viewer for the latest state
let config_viewer = tree.get_subtree_viewer::<KVStore>("configuration")?;

match config_viewer.get("api_key") {
    Ok(api_key) => println!("Current API Key: {}", api_key),
    Err(Error::NotFound) => println!("API Key not set."),
    Err(e) => return Err(e.into()),
}

match config_viewer.get("retry_count") {
    Ok(count_str) => {
        // Note: KVStore values are strings
        let count: u32 = count_str.parse().unwrap_or(0);
        println!("Retry Count: {}", count);
    }
    Err(_) => println!("Retry count not set or invalid."),
}
```

## 6. Reading Data (RowStore Viewer)

```rust
use eideticadb::subtree::RowStore;
// Assume Task struct from example 4

let tree: Tree = /* obtained from step 2 */;

// Get a read-only viewer
let tasks_viewer = tree.get_subtree_viewer::<RowStore<Task>>("tasks")?;

// Get a specific task by ID
let id_to_find = /* obtained previously, e.g., inserted_id */;
match tasks_viewer.get(&id_to_find) {
    Ok(task) => println!("Found task {}: {:?}", id_to_find, task),
    Err(Error::NotFound) => println!("Task {} not found.", id_to_find),
    Err(e) => return Err(e.into()),
}

// Iterate over all tasks
println!("\nAll Tasks:");
match tasks_viewer.iter() {
    Ok(iter) => {
        for result in iter {
            match result {
                Ok((id, task)) => println!("  ID: {}, Task: {:?}", id, task),
                Err(e) => eprintln!("Error reading task during iteration: {}", e),
            }
        }
    }
    Err(e) => eprintln!("Error creating iterator: {}", e),
}
```

## 7. Saving the Database (InMemoryBackend)

```rust
use eideticadb::backend::InMemoryBackend;
use std::path::PathBuf;

let db: BaseDB = /* database instance */;
let db_path = PathBuf::from("my_database.json");

// Lock the backend mutex
let backend_guard = db.backend().lock().map_err(|_| anyhow::anyhow!("Failed to lock backend mutex"))?;

// Downcast to the concrete InMemoryBackend type
if let Some(in_memory_backend) = backend_guard.as_any().downcast_ref::<InMemoryBackend>() {
    match in_memory_backend.save_to_file(&db_path) {
        Ok(_) => println!("Database saved successfully to {:?}", db_path),
        Err(e) => eprintln!("Error saving database: {}", e),
    }
} else {
    eprintln!("Backend is not InMemoryBackend, cannot save to file this way.");
}
```
