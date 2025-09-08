# Code Examples

This page provides focused code snippets for common tasks in Eidetica.

_Assumes basic setup like `use eidetica::{Instance, Database, Error, ...};` and error handling (`?`) for brevity._

## 1. Initializing the Database (`Instance`)

```rust,ignore
use eidetica::backend::database::InMemory;
use eidetica::Instance;
use std::path::PathBuf;

// Option A: Create a new, empty in-memory database
let database_new = InMemory::new();
let db_new = Instance::new(Box::new(database_new));

// Option B: Load from a previously saved file
let db_path = PathBuf::from("my_database.json");
if db_path.exists() {
    match InMemory::load_from_file(&db_path) {
        Ok(database_loaded) => {
            let db_loaded = Instance::new(Box::new(database_loaded));
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

## 2. Creating or Loading a Database

```rust,ignore
use eidetica::crdt::Doc;

let db: Instance = /* obtained from step 1 */;
let tree_name = "my_app_data";
let auth_key = "my_key"; // Must match a key added to the database

let database = match db.find_database(tree_name) {
    Ok(mut databases) => {
        println!("Found existing database: {}", tree_name);
        databases.pop().unwrap() // Assume first one is correct
    }
    Err(e) if e.is_not_found() => {
        println!("Creating new database: {}", tree_name);
        let mut doc = Doc::new();
        doc.set("name", tree_name);
        db.new_database(doc, auth_key)? // All databases require authentication
    }
    Err(e) => return Err(e.into()), // Propagate other errors
};

println!("Using Database with root ID: {}", database.root_id());
```

## 3. Writing Data (DocStore Example)

```rust,ignore
use eidetica::store::DocStore;

let database: Database = /* obtained from step 2 */;

// Start an authenticated transaction (automatically uses the database's default key)
let op = database.new_transaction()?;

{
    // Get the DocStore store handle (scoped)
    let config_store = op.get_subtree::<DocStore>("configuration")?;

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
println!("DocStore changes committed in entry: {}", entry_id);
```

## 4. Writing Data (Table Example)

```rust,ignore
use eidetica::store::Table;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Task {
    description: String,
    completed: bool,
}

let database: Database = /* obtained from step 2 */;

// Start an authenticated transaction (automatically uses the database's default key)
let op = database.new_transaction()?;
let inserted_id;

{
    // Get the Table handle
    let tasks_store = op.get_subtree::<Table<Task>>("tasks")?;

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
println!("Table changes committed in entry: {}", entry_id);
```

## 5. Reading Data (DocStore Viewer)

```rust,ignore
use eidetica::store::DocStore;

let database: Database = /* obtained from step 2 */;

// Get a read-only viewer for the latest state
let config_viewer = database.get_subtree_viewer::<DocStore>("configuration")?;

match config_viewer.get("api_key") {
    Ok(api_key) => println!("Current API Key: {}", api_key),
    Err(e) if e.is_not_found() => println!("API Key not set."),
    Err(e) => return Err(e.into()),
}

match config_viewer.get("retry_count") {
    Ok(count_str) => {
        // Note: DocStore values can be various types
        let count: u32 = count_str.parse().unwrap_or(0);
        println!("Retry Count: {}", count);
    }
    Err(_) => println!("Retry count not set or invalid."),
}
```

## 6. Reading Data (Table Viewer)

```rust,ignore
use eidetica::store::Table;
// Assume Task struct from example 4

let database: Database = /* obtained from step 2 */;

// Get a read-only viewer
let tasks_viewer = database.get_subtree_viewer::<Table<Task>>("tasks")?;

// Get a specific task by ID
let id_to_find = /* obtained previously, e.g., inserted_id */;
match tasks_viewer.get(&id_to_find) {
    Ok(task) => println!("Found task {}: {:?}", id_to_find, task),
    Err(e) if e.is_not_found() => println!("Task {} not found.", id_to_find),
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

```rust,ignore
use eidetica::store::{DocStore, Value};

let database: Database = /* obtained from step 2 */;

// Start an authenticated transaction (automatically uses the database's default key)
let op = database.new_transaction()?;

// Get the DocStore store handle
let user_store = op.get_subtree::<DocStore>("users")?;

// Using ValueEditor to create and modify nested structures
{
    // Get an editor for a specific user
    let user_editor = user_store.get_value_mut("user123");

    // Set profile information with method chaining - creates paths as needed
    user_editor
        .get_value_mut("profile")
        .get_value_mut("name")
        .set(Value::String("Jane Doe".to_string()))?;

    user_editor
        .get_value_mut("profile")
        .get_value_mut("email")
        .set(Value::String("jane@example.com".to_string()))?;

    // Set preferences as a map
    let mut preferences = Map::new();
    preferences.set_string("theme".to_string(), "dark".to_string());
    preferences.set_string("notifications".to_string(), "enabled".to_string());

    user_editor
        .get_value_mut("preferences")
        .set(Value::Map(preferences))?;

    // Add to preferences using the editor
    user_editor
        .get_value_mut("preferences")
        .get_value_mut("language")
        .set(Value::String("en".to_string()))?;

    // Delete a specific preference
    user_editor
        .get_value_mut("preferences")
        .delete_child("notifications")?;
}

// Commit the changes
let entry_id = op.commit()?;
println!("ValueEditor changes committed in entry: {}", entry_id);

// Read back the nested data
let viewer_op = database.new_transaction()?;
let viewer_store = viewer_op.get_subtree::<DocStore>("users")?;

// Get the user data and navigate through it
if let Ok(user_data) = viewer_store.get("user123") {
    if let Value::Map(user_map) = user_data {
        // Access profile
        if let Some(Value::Map(profile)) = user_map.get("profile") {
            if let Some(Value::String(name)) = profile.get("name") {
                println!("User name: {}", name);
            }
        }

        // Access preferences
        if let Some(Value::Map(prefs)) = user_map.get("preferences") {
            println!("User preferences:");
            for (key, value) in prefs.as_hashmap() {
                match value {
                    Value::String(val) => println!("  {}: {}", key, val),
                    Value::Deleted => println!("  {}: [deleted]", key),
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
        Ok(Value::String(name)) => println!("User name (via editor): {}", name),
        _ => println!("Name not found or not a string"),
    }

    // Check if a preference exists
    match editor.get_value_mut("preferences").get_value("notifications") {
        Ok(_) => println!("Notifications setting exists"),
        Err(e) if e.is_not_found() => println!("Notifications setting was deleted"),
        Err(_) => println!("Error accessing notifications setting"),
    }
}

// Using get_root_mut to access the entire store
{
    let root_editor = viewer_store.get_root_mut();
    println!("\nAll users in store:");

    match root_editor.get() {
        Ok(Value::Map(users)) => {
            for (user_id, _) in users.as_hashmap() {
                println!("  User ID: {}", user_id);
            }
        },
        _ => println!("No users found or error accessing store"),
    }
}
```

## 8. Working with Y-CRDT Documents (YDoc)

The `YDoc` store provides access to Y-CRDT (Yrs) documents for collaborative data structures. This requires the "y-crdt" feature flag.

```rust,ignore
use eidetica::store::YDoc;
use eidetica::y_crdt::{Map as YMap, Transact};

let database: Database = /* obtained from step 2 */;

// Start an authenticated transaction (automatically uses the database's default key)
let op = database.new_transaction()?;

// Get the YDoc store handle
let user_info_store = op.get_subtree::<YDoc>("user_info")?;

// Writing to Y-CRDT document
user_info_store.with_doc_mut(|doc| {
    let user_info_map = doc.get_or_insert_map("user_info");
    let mut txn = doc.transact_mut();

    user_info_map.insert(&mut txn, "name", "Alice Johnson");
    user_info_map.insert(&mut txn, "email", "alice@example.com");
    user_info_map.insert(&mut txn, "bio", "Software developer");

    Ok(())
})?;

// Commit the transaction
let entry_id = op.commit()?;
println!("YDoc changes committed in entry: {}", entry_id);

// Reading from Y-CRDT document
let read_op = database.new_transaction()?;
let reader_store = read_op.get_subtree::<YDoc>("user_info")?;

reader_store.with_doc(|doc| {
    let user_info_map = doc.get_or_insert_map("user_info");
    let txn = doc.transact();

    println!("User Information:");

    if let Some(name) = user_info_map.get(&txn, "name") {
        let name_str = name.to_string(&txn);
        println!("Name: {name_str}");
    }

    if let Some(email) = user_info_map.get(&txn, "email") {
        let email_str = email.to_string(&txn);
        println!("Email: {email_str}");
    }

    if let Some(bio) = user_info_map.get(&txn, "bio") {
        let bio_str = bio.to_string(&txn);
        println!("Bio: {bio_str}");
    }

    Ok(())
})?;

// Working with nested Y-CRDT maps
let prefs_op = database.new_transaction()?;
let prefs_store = prefs_op.get_subtree::<YDoc>("user_prefs")?;

prefs_store.with_doc_mut(|doc| {
    let prefs_map = doc.get_or_insert_map("preferences");
    let mut txn = doc.transact_mut();

    prefs_map.insert(&mut txn, "theme", "dark");
    prefs_map.insert(&mut txn, "notifications", "enabled");
    prefs_map.insert(&mut txn, "language", "en");

    Ok(())
})?;

prefs_op.commit()?;

// Reading preferences
let prefs_read_op = database.new_transaction()?;
let prefs_read_store = prefs_read_op.get_subtree::<YDoc>("user_prefs")?;

prefs_read_store.with_doc(|doc| {
    let prefs_map = doc.get_or_insert_map("preferences");
    let txn = doc.transact();

    println!("User Preferences:");

    // Iterate over all preferences
    for (key, value) in prefs_map.iter(&txn) {
        let value_str = value.to_string(&txn);
        println!("{key}: {value_str}");
    }

    Ok(())
})?;
```

**YDoc Features:**

- **Collaborative Editing**: Y-CRDT documents provide conflict-free merging for concurrent modifications
- **Rich Data Types**: Support for Maps, Arrays, Text, and other Y-CRDT types
- **Functional Interface**: Access via `with_doc()` for reads and `with_doc_mut()` for writes
- **Atomic Integration**: Changes are staged within the Transaction and committed atomically

**Use Cases for YDoc:**

- User profiles and preferences (as shown in the todo example)
- Collaborative documents and shared state
- Real-time data synchronization
- Any scenario requiring conflict-free concurrent updates

## 9. Saving the Database (InMemory)

```rust,ignore
use eidetica::backend::database::InMemory;
use std::path::PathBuf;

let db: Instance = /* database instance */;
let db_path = PathBuf::from("my_database.json");

// Lock the database mutex
let database_guard = db.backend().lock().map_err(|_| anyhow::anyhow!("Failed to lock database mutex"))?;

// Downcast to the concrete InMemory type
if let Some(in_memory_database) = database_guard.as_any().downcast_ref::<InMemory>() {
    match in_memory_database.save_to_file(&db_path) {
        Ok(_) => println!("Database saved successfully to {:?}", db_path),
        Err(e) => eprintln!("Error saving database: {}", e),
    }
} else {
    eprintln!("Database is not InMemory, cannot save to file this way.");
}
```
