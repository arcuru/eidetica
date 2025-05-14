# Code Examples

This page provides focused code snippets for common tasks in Eidetica.

_Assumes basic setup like `use eidetica::{BaseDB, Tree, Error, ...};` and error handling (`?`) for brevity._

## 1. Initializing the Database (`BaseDB`)

```rust
use eidetica::backend::InMemoryBackend;
use eidetica::basedb::BaseDB;
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
use eidetica::data::KVOverWrite;

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
        settings.set("name", tree_name);
        db.new_tree(settings)?
    }
    Err(e) => return Err(e.into()), // Propagate other errors
};

println!("Using Tree with root ID: {}", tree.root_id());
```

## 3. Writing Data (KVStore Example)

```rust
use eidetica::subtree::KVStore;

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
use eidetica::subtree::RowStore;
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
use eidetica::subtree::KVStore;

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
use eidetica::subtree::RowStore;
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

## 7. Working with Nested Data (ValueEditor)

```rust
use eidetica::subtree::{KVStore, NestedValue, KVNested};

let tree: Tree = /* obtained from step 2 */;

// Start an operation
let op = tree.new_operation()?;

// Get the KVStore subtree handle
let user_store = op.get_subtree::<KVStore>("users")?;

// Using ValueEditor to create and modify nested structures
{
    // Get an editor for a specific user
    let user_editor = user_store.get_value_mut("user123");

    // Set profile information with method chaining - creates paths as needed
    user_editor
        .get_value_mut("profile")
        .get_value_mut("name")
        .set(NestedValue::String("Jane Doe".to_string()))?;

    user_editor
        .get_value_mut("profile")
        .get_value_mut("email")
        .set(NestedValue::String("jane@example.com".to_string()))?;

    // Set preferences as a map
    let mut preferences = KVNested::new();
    preferences.set_string("theme".to_string(), "dark".to_string());
    preferences.set_string("notifications".to_string(), "enabled".to_string());

    user_editor
        .get_value_mut("preferences")
        .set(NestedValue::Map(preferences))?;

    // Add to preferences using the editor
    user_editor
        .get_value_mut("preferences")
        .get_value_mut("language")
        .set(NestedValue::String("en".to_string()))?;

    // Delete a specific preference
    user_editor
        .get_value_mut("preferences")
        .delete_child("notifications")?;
}

// Commit the changes
let entry_id = op.commit()?;
println!("ValueEditor changes committed in entry: {}", entry_id);

// Read back the nested data
let viewer_op = tree.new_operation()?;
let viewer_store = viewer_op.get_subtree::<KVStore>("users")?;

// Get the user data and navigate through it
if let Ok(user_data) = viewer_store.get("user123") {
    if let NestedValue::Map(user_map) = user_data {
        // Access profile
        if let Some(NestedValue::Map(profile)) = user_map.get("profile") {
            if let Some(NestedValue::String(name)) = profile.get("name") {
                println!("User name: {}", name);
            }
        }

        // Access preferences
        if let Some(NestedValue::Map(prefs)) = user_map.get("preferences") {
            println!("User preferences:");
            for (key, value) in prefs.as_hashmap() {
                match value {
                    NestedValue::String(val) => println!("  {}: {}", key, val),
                    NestedValue::Deleted => println!("  {}: [deleted]", key),
                    _ => println!("  {}: [complex value]", key),
                }
            }
        }
    }
}

// Using ValueEditor to read nested data (alternative to manual navigation)
{
    let editor = viewer_store.get_value_mut("user123");

    // Get profile name
    match editor.get_value_mut("profile").get_value("name") {
        Ok(NestedValue::String(name)) => println!("User name (via editor): {}", name),
        _ => println!("Name not found or not a string"),
    }

    // Check if a preference exists
    match editor.get_value_mut("preferences").get_value("notifications") {
        Ok(_) => println!("Notifications setting exists"),
        Err(Error::NotFound) => println!("Notifications setting was deleted"),
        Err(_) => println!("Error accessing notifications setting"),
    }
}

// Using get_root_mut to access the entire store
{
    let root_editor = viewer_store.get_root_mut();
    println!("\nAll users in store:");

    match root_editor.get() {
        Ok(NestedValue::Map(users)) => {
            for (user_id, _) in users.as_hashmap() {
                println!("  User ID: {}", user_id);
            }
        },
        _ => println!("No users found or error accessing store"),
    }
}
```

## 8. Saving the Database (InMemoryBackend)

```rust
use eidetica::backend::InMemoryBackend;
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
