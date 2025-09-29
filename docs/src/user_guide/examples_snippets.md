# Code Examples

This page provides focused code snippets for common tasks in Eidetica.

_Assumes basic setup like `use eidetica::{Instance, Database, Error, ...};` and error handling (`?`) for brevity._

## 1. Initializing the Database (`Instance`)

```rust
# extern crate eidetica;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
# use std::path::PathBuf;
#
# fn main() -> eidetica::Result<()> {
# // Use a temporary file for testing
# let temp_dir = std::env::temp_dir();
# let db_path = temp_dir.join("eidetica_example_init.json");
#
# // First create and save a test database to demonstrate loading
# let backend = InMemory::new();
# let test_db = Instance::new(Box::new(backend));
# test_db.add_private_key("test_key")?;
# let mut settings = Doc::new();
# settings.set_string("name", "example_db");
# let _database = test_db.new_database(settings, "test_key")?;
# let database_guard = test_db.backend();
# if let Some(in_memory) = database_guard.as_any().downcast_ref::<InMemory>() {
#     in_memory.save_to_file(&db_path)?;
# }
#
// Option A: Create a new, empty in-memory database
let database_new = InMemory::new();
let _db_new = Instance::new(Box::new(database_new));

// Option B: Load from a previously saved file
if db_path.exists() {
    match InMemory::load_from_file(&db_path) {
        Ok(database_loaded) => {
            let _db_loaded = Instance::new(Box::new(database_loaded));
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
#
# // Clean up the temporary file
# if db_path.exists() {
#     std::fs::remove_file(&db_path).ok();
# }
# Ok(())
# }
```

## 2. Creating or Loading a Database

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
#
# fn main() -> eidetica::Result<()> {
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("my_key")?;
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
# Ok(())
# }
```

## 3. Writing Data (DocStore Example)

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::DocStore};
#
# fn main() -> eidetica::Result<()> {
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("my_key")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "my_key")?;
#
// Start an authenticated transaction (automatically uses the database's default key)
let op = database.new_transaction()?;

{
    // Get the DocStore store handle (scoped)
    let config_store = op.get_store::<DocStore>("configuration")?;

    // Set some values
    config_store.set("api_key", "secret-key-123")?;
    config_store.set("retry_count", "3")?;

    // Overwrite a value
    config_store.set("api_key", "new-secret-456")?;

    // Remove a value
    config_store.delete("old_setting")?; // Ok if it doesn't exist
}

// Commit the changes atomically
let entry_id = op.commit()?;
println!("DocStore changes committed in entry: {}", entry_id);
# Ok(())
# }
```

## 4. Writing Data (Table Example)

```rust
# extern crate eidetica;
# extern crate serde;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::Table};
# use serde::{Serialize, Deserialize};
#
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Task {
    description: String,
    completed: bool,
}

# fn main() -> eidetica::Result<()> {
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("my_key")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "my_key")?;
#
// Start an authenticated transaction (automatically uses the database's default key)
let op = database.new_transaction()?;
let inserted_id;

{
    // Get the Table handle
    let tasks_store = op.get_store::<Table<Task>>("tasks")?;

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
# Ok(())
# }
```

## 5. Reading Data (DocStore Viewer)

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::DocStore};
#
# fn main() -> eidetica::Result<()> {
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("my_key")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "my_key")?;
#
// Get a read-only viewer for the latest state
let config_viewer = database.get_store_viewer::<DocStore>("configuration")?;

match config_viewer.get("api_key") {
    Ok(api_key) => println!("Current API Key: {}", api_key),
    Err(e) if e.is_not_found() => println!("API Key not set."),
    Err(e) => return Err(e.into()),
}

match config_viewer.get("retry_count") {
    Ok(count_str) => {
        // Note: DocStore values can be various types
        if let Some(text) = count_str.as_text() {
            if let Ok(count) = text.parse::<u32>() {
                println!("Retry Count: {}", count);
            }
        }
    }
    Err(_) => println!("Retry count not set or invalid."),
}
# Ok(())
# }
```

## 6. Reading Data (Table Viewer)

```rust
# extern crate eidetica;
# extern crate serde;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::Table};
# use serde::{Serialize, Deserialize};
#
# #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
# struct Task {
#     description: String,
#     completed: bool,
# }
#
# fn main() -> eidetica::Result<()> {
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("my_key")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "my_key")?;
# let op = database.new_transaction()?;
# let tasks_store = op.get_store::<Table<Task>>("tasks")?;
# let id_to_find = tasks_store.insert(Task { description: "Test task".to_string(), completed: false })?;
# op.commit()?;
#
// Get a read-only viewer
let tasks_viewer = database.get_store_viewer::<Table<Task>>("tasks")?;

// Get a specific task by ID
match tasks_viewer.get(&id_to_find) {
    Ok(task) => println!("Found task {}: {:?}", id_to_find, task),
    Err(e) if e.is_not_found() => println!("Task {} not found.", id_to_find),
    Err(e) => return Err(e.into()),
}

// Search for all tasks
println!("\nAll Tasks:");
match tasks_viewer.search(|_| true) {
    Ok(tasks) => {
        for (id, task) in tasks {
            println!("  ID: {}, Task: {:?}", id, task);
        }
    }
    Err(e) => eprintln!("Error searching tasks: {}", e),
}
# Ok(())
# }
```

## 7. Working with Nested Data (Path-Based Operations)

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::DocStore, path, Database};
#
# fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("test_key")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "test_key")?;
// Start an authenticated transaction (automatically uses the database's default key)
let op = database.new_transaction()?;

// Get the DocStore store handle
let user_store = op.get_store::<DocStore>("users")?;

// Using path-based operations to create and modify nested structures
// Set profile information using paths - creates nested structure automatically
user_store.set_path(path!("user123.profile.name"), "Jane Doe")?;
user_store.set_path(path!("user123.profile.email"), "jane@example.com")?;

// Set preferences using paths
user_store.set_path(path!("user123.preferences.theme"), "dark")?;
user_store.set_path(path!("user123.preferences.notifications"), "enabled")?;
user_store.set_path(path!("user123.preferences.language"), "en")?;

// Set additional nested configuration
user_store.set_path(path!("config.database.host"), "localhost")?;
user_store.set_path(path!("config.database.port"), "5432")?;

// Commit the changes
let entry_id = op.commit()?;
println!("Nested data changes committed in entry: {}", entry_id);

// Read back the nested data using path operations
let viewer_op = database.new_transaction()?;
let viewer_store = viewer_op.get_store::<DocStore>("users")?;

// Get individual values using path operations
let _name_value = viewer_store.get_path(path!("user123.profile.name"))?;
let _email_value = viewer_store.get_path(path!("user123.profile.email"))?;
let _theme_value = viewer_store.get_path(path!("user123.preferences.theme"))?;
let _host_value = viewer_store.get_path(path!("config.database.host"))?;

// Get the entire user object to verify nested structure was created
if let Ok(_user_data) = viewer_store.get("user123") {
    println!("User profile and preferences created successfully");
}

// Get the entire config object to verify nested structure
if let Ok(_config_data) = viewer_store.get("config") {
    println!("Configuration data created successfully");
}

println!("Path-based operations completed successfully");
# Ok(())
# }
```

## 8. Working with Y-CRDT Documents (YDoc)

The `YDoc` store provides access to Y-CRDT (Yrs) documents for collaborative data structures. This requires the "y-crdt" feature flag.

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::YDoc, Database};
# use eidetica::y_crdt::{Map as YMap, Transact};
#
# fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let backend = InMemory::new();
# let db = Instance::new(Box::new(backend));
# db.add_private_key("test_key")?;
# let mut settings = Doc::new();
# settings.set_string("name", "y_crdt_example");
# let database = db.new_database(settings, "test_key")?;
#
// Start an authenticated transaction (automatically uses the database's default key)
let op = database.new_transaction()?;

// Get the YDoc store handle
let user_info_store = op.get_store::<YDoc>("user_info")?;

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
let reader_store = read_op.get_store::<YDoc>("user_info")?;

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
let prefs_store = prefs_op.get_store::<YDoc>("user_prefs")?;

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
let prefs_read_store = prefs_read_op.get_store::<YDoc>("user_prefs")?;

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
# Ok(())
# }
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

```rust
# extern crate eidetica;
# use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
# use std::path::PathBuf;
#
# fn main() -> eidetica::Result<()> {
# // Create a test database
# let backend = InMemory::new();
# let db = Instance::new(Box::new(backend));
# db.add_private_key("test_key")?;
# let mut settings = Doc::new();
# settings.set_string("name", "save_example");
# let _database = db.new_database(settings, "test_key")?;
#
# // Use a temporary file for testing
# let temp_dir = std::env::temp_dir();
# let db_path = temp_dir.join("eidetica_save_example.json");
#
// Save the database to a file
let database_guard = db.backend();

// Downcast to the concrete InMemory type
if let Some(in_memory_database) = database_guard.as_any().downcast_ref::<InMemory>() {
    match in_memory_database.save_to_file(&db_path) {
        Ok(_) => println!("Database saved successfully to {:?}", db_path),
        Err(e) => eprintln!("Error saving database: {}", e),
    }
} else {
    eprintln!("Database is not InMemory, cannot save to file this way.");
}
#
# // Clean up the temporary file
# if db_path.exists() {
#     std::fs::remove_file(&db_path).ok();
# }
# Ok(())
# }
```
