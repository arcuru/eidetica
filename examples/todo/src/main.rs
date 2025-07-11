use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eidetica::backend::database::InMemory;
use eidetica::basedb::BaseDB;
use eidetica::crdt::Node;
use eidetica::subtree::RowStore;
use eidetica::subtree::YrsStore;
use eidetica::y_crdt::{Map, Transact};
use eidetica::{Result, Tree};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Default authentication key ID for the todo app
const TODO_APP_KEY_ID: &str = "TODO_APP_USER";

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to the database file to use
    #[arg(short, long, default_value = "todo_db.json")]
    database_path: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a new task to the todo list
    Add {
        /// The title of the task
        #[arg(required = true)]
        title: String,
    },
    /// Mark a task as complete
    Complete {
        /// The ID of the task to mark as complete
        #[arg(required = true)]
        id: String,
    },
    /// List all tasks
    List,
    /// Set user information
    SetUser {
        /// The user's name
        #[arg(short, long)]
        name: Option<String>,
        /// The user's email
        #[arg(short, long)]
        email: Option<String>,
        /// The user's bio
        #[arg(short, long)]
        bio: Option<String>,
    },
    /// Show user information
    ShowUser,
    /// Set user preference
    SetPref {
        /// Preference key
        #[arg(required = true)]
        key: String,
        /// Preference value
        #[arg(required = true)]
        value: String,
    },
    /// Show user preferences
    ShowPrefs,
}

///  A very basic todo list item
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load or create the database
    let db = load_or_create_db(&cli.database_path)?;

    // Load or create the todo tree
    let todo_tree = load_or_create_todo_tree(&db)?;

    // Handle the command with proper error context
    let result = match &cli.command {
        Commands::Add { title } => {
            add_todo(&todo_tree, title.clone()).map(|_| println!("✓ Task added: {title}"))
        }
        Commands::Complete { id } => {
            complete_todo(&todo_tree, id).map(|_| println!("✓ Task completed: {id}"))
        }
        Commands::List => list_todos(&todo_tree),
        Commands::SetUser { name, email, bio } => {
            set_user_info(&todo_tree, name.as_ref(), email.as_ref(), bio.as_ref())
                .map(|_| println!("✓ User information updated"))
        }
        Commands::ShowUser => show_user_info(&todo_tree),
        Commands::SetPref { key, value } => {
            set_user_preference(&todo_tree, key.clone(), value.clone())
                .map(|_| println!("✓ User preference set"))
        }
        Commands::ShowPrefs => show_user_preferences(&todo_tree),
    };

    // Handle command errors with specific error messages
    if let Err(e) = result {
        // Check if it's an authentication error
        if e.is_authentication_error() {
            eprintln!("Authentication error: {e}");
            eprintln!("Make sure you have the necessary permissions for this operation.");
            return Err(e);
        } else if e.is_operation_error() {
            eprintln!("Operation error: {e}");
            eprintln!("The operation could not be completed. The database may be in use.");
            return Err(e);
        }
        // For other errors, just propagate
        return Err(e);
    }

    // Save the database
    save_db(&db, &cli.database_path)
}

fn load_or_create_db(path: &PathBuf) -> Result<BaseDB> {
    let db = if path.exists() {
        let backend = InMemory::load_from_file(path)?;
        BaseDB::new(Box::new(backend))
    } else {
        let backend = InMemory::new();
        BaseDB::new(Box::new(backend))
    };

    // Ensure the todo app authentication key exists
    // First check if the key already exists
    let existing_keys = db.list_private_keys()?;

    if !existing_keys.contains(&TODO_APP_KEY_ID.to_string()) {
        // Add the key if it doesn't exist
        match db.add_private_key(TODO_APP_KEY_ID) {
            Ok(_) => {
                println!("✓ New authentication key created");
            }
            Err(e) if e.is_conflict() => {
                // Key was created concurrently, this is fine
                println!("✓ Authentication key already exists");
            }
            Err(e) => {
                // Authentication is required, so we must fail if key creation fails
                eprintln!("✗ Failed to create authentication key: {e}");
                return Err(e);
            }
        }
    } else {
        println!("✓ Authentication key loaded from database");
    }

    Ok(db)
}

fn save_db(db: &BaseDB, path: &PathBuf) -> Result<()> {
    let database = db.backend();

    // Cast the database to InMemory to access save_to_file
    let in_memory_database = database
        .as_any()
        .downcast_ref::<InMemory>()
        .ok_or_else(|| {
            eidetica::Error::Io(std::io::Error::other(
                "Failed to downcast database to InMemory"
            ))
        })?;

    in_memory_database.save_to_file(path)?;
    Ok(())
}

fn load_or_create_todo_tree(db: &BaseDB) -> Result<Tree> {
    let tree_name = "todo";

    // Try to find the tree by name
    let mut tree = match db.find_tree(tree_name) {
        Ok(mut trees) => {
            // If multiple trees with the same name exist, pop will return one arbitrarily.
            // We might want more robust handling later (e.g., error or config option).
            trees.pop().unwrap() // unwrap is safe because find_tree errors if empty
        }
        Err(e) if e.is_not_found() => {
            // If not found, create a new one
            println!("No existing todo tree found, creating a new one...");
            let mut settings = Node::new();
            settings.set_string("name", tree_name);

            db.new_tree(settings, TODO_APP_KEY_ID)?
        }
        Err(e) => {
            // Propagate other errors
            return Err(e);
        }
    };

    // Set the default authentication key for this tree
    // This means all subsequent new_operation() calls will automatically use this key
    tree.set_default_auth_key(TODO_APP_KEY_ID);

    Ok(tree)
}

fn add_todo(tree: &Tree, title: String) -> Result<()> {
    // Start an atomic operation (uses default auth key)
    let op = tree.new_operation()?;

    // Get a handle to the 'todos' RowStore subtree
    let todos_store = op.get_subtree::<RowStore<Todo>>("todos")?;

    // Create a new todo
    let todo = Todo::new(title);

    // Insert the todo into the RowStore
    // The RowStore will generate a unique ID for it
    let todo_id = todos_store.insert(todo)?;

    // Commit the operation
    op.commit()?;

    println!("Added todo with ID: {todo_id}");

    Ok(())
}

fn complete_todo(tree: &Tree, id: &str) -> Result<()> {
    // Start an atomic operation (uses default auth key)
    let op = tree.new_operation()?;

    // Get a handle to the 'todos' RowStore subtree
    let todos_store = op.get_subtree::<RowStore<Todo>>("todos")?;

    // Get the todo from the RowStore
    let mut todo = match todos_store.get(id) {
        Ok(todo) => todo,
        Err(e) if e.is_not_found() => {
            // Provide a user-friendly error message for not found
            return Err(eidetica::subtree::SubtreeError::KeyNotFound {
                subtree: "todos".to_string(),
                key: id.to_string(),
            }.into());
        }
        Err(e) => {
            // For other errors, just propagate
            return Err(e);
        }
    };

    // Mark the todo as complete
    todo.complete();

    // Update the todo in the RowStore
    todos_store.set(id, todo)?;

    // Commit the operation
    op.commit()?;

    Ok(())
}

fn list_todos(tree: &Tree) -> Result<()> {
    // Start an atomic operation (for read-only, uses default auth key)
    let op = tree.new_operation()?;

    // Get a handle to the 'todos' RowStore subtree
    let todos_store = op.get_subtree::<RowStore<Todo>>("todos")?;

    // Search for all todos (predicate always returns true)
    let todos_with_ids = todos_store.search(|_| true)?;

    // Print the todos
    if todos_with_ids.is_empty() {
        println!("No tasks found.");
    } else {
        println!("Tasks:");
        // Sort todos by creation date
        let mut sorted_todos = todos_with_ids;
        sorted_todos.sort_by(|(_, a), (_, b)| a.created_at.cmp(&b.created_at));

        for (id, todo) in sorted_todos {
            let status = if todo.completed { "✓" } else { " " };
            println!("[{}] {} (ID: {})", status, todo.title, id);
        }
    }

    Ok(())
}

fn set_user_info(
    tree: &Tree,
    name: Option<&String>,
    email: Option<&String>,
    bio: Option<&String>,
) -> Result<()> {
    // Start an atomic operation (uses default auth key)
    let op = tree.new_operation()?;

    // Get a handle to the 'user_info' YrsStore subtree
    let user_info_store = op.get_subtree::<YrsStore>("user_info")?;

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

    // Commit the operation
    op.commit()?;

    Ok(())
}

fn show_user_info(tree: &Tree) -> Result<()> {
    // Start an atomic operation (for read-only, uses default auth key)
    let op = tree.new_operation()?;

    // Get a handle to the 'user_info' YrsStore subtree
    let user_info_store = op.get_subtree::<YrsStore>("user_info")?;

    // Read user information from the Y-CRDT document
    user_info_store.with_doc(|doc| {
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

    Ok(())
}

fn set_user_preference(tree: &Tree, key: String, value: String) -> Result<()> {
    // Start an atomic operation (uses default auth key)
    let op = tree.new_operation()?;

    // Get a handle to the 'user_prefs' YrsStore subtree
    let user_prefs_store = op.get_subtree::<YrsStore>("user_prefs")?;

    // Update user preference using the Y-CRDT document
    user_prefs_store.with_doc_mut(|doc| {
        let prefs_map = doc.get_or_insert_map("preferences");
        let mut txn = doc.transact_mut();
        prefs_map.insert(&mut txn, key, value);
        Ok(())
    })?;

    // Commit the operation
    op.commit()?;

    Ok(())
}

fn show_user_preferences(tree: &Tree) -> Result<()> {
    // Start an atomic operation (for read-only)
    let op = tree.new_operation()?;

    // Get a handle to the 'user_prefs' YrsStore subtree
    let user_prefs_store = op.get_subtree::<YrsStore>("user_prefs")?;

    // Read user preferences from the Y-CRDT document
    user_prefs_store.with_doc(|doc| {
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

    Ok(())
}
