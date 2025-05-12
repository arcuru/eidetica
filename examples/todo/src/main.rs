use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eideticadb::backend::InMemoryBackend;
use eideticadb::basedb::BaseDB;
use eideticadb::data::KVNested;
use eideticadb::subtree::RowStore;
use eideticadb::Error;
use eideticadb::Tree;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

    // Handle the command
    match &cli.command {
        Commands::Add { title } => {
            add_todo(&todo_tree, title.clone())?;
            println!("Task added: {}", title);
        }
        Commands::Complete { id } => {
            complete_todo(&todo_tree, id)?;
            println!("Task completed: {}", id);
        }
        Commands::List => {
            list_todos(&todo_tree)?;
        }
    }

    // Save the database
    save_db(&db, &cli.database_path)?;

    Ok(())
}

fn load_or_create_db(path: &PathBuf) -> Result<BaseDB> {
    if path.exists() {
        let backend = InMemoryBackend::load_from_file(path)?;
        Ok(BaseDB::new(Box::new(backend)))
    } else {
        let backend = InMemoryBackend::new();
        Ok(BaseDB::new(Box::new(backend)))
    }
}

fn save_db(db: &BaseDB, path: &PathBuf) -> Result<()> {
    let backend = db.backend();
    let backend_guard = backend.lock().unwrap();

    // Cast the backend to InMemoryBackend to access save_to_file
    let in_memory_backend = backend_guard
        .as_any()
        .downcast_ref::<InMemoryBackend>()
        .ok_or(anyhow!("Failed to downcast backend to InMemoryBackend"))?;

    in_memory_backend.save_to_file(path)?;
    Ok(())
}

fn load_or_create_todo_tree(db: &BaseDB) -> Result<Tree> {
    let tree_name = "todo".to_string();

    // Try to find the tree by name
    match db.find_tree(&tree_name) {
        Ok(mut trees) => {
            // If multiple trees with the same name exist, pop will return one arbitrarily.
            // We might want more robust handling later (e.g., error or config option).
            Ok(trees.pop().unwrap()) // unwrap is safe because find_tree errors if empty
        }
        Err(Error::NotFound) => {
            // If not found, create a new one
            println!("No existing todo tree found, creating a new one...");
            let mut settings = KVNested::new();
            settings.set_string("name", tree_name.clone());

            let tree = db.new_tree(settings)?;

            Ok(tree)
        }
        Err(e) => {
            // Propagate other errors
            Err(e.into())
        }
    }
}

fn add_todo(tree: &Tree, title: String) -> Result<()> {
    // Start an atomic operation
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

    println!("Added todo with ID: {}", todo_id);

    Ok(())
}

fn complete_todo(tree: &Tree, id: &str) -> Result<()> {
    // Start an atomic operation
    let op = tree.new_operation()?;

    // Get a handle to the 'todos' RowStore subtree
    let todos_store = op.get_subtree::<RowStore<Todo>>("todos")?;

    // Get the todo from the RowStore
    let mut todo = match todos_store.get(id) {
        Ok(todo) => todo,
        Err(Error::NotFound) => return Err(anyhow!("Todo with ID {} not found", id)),
        Err(e) => return Err(anyhow!("Error retrieving todo: {}", e)),
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
    // Start an atomic operation (for read-only)
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
