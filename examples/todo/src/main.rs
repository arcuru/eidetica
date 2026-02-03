use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eidetica::{
    Database, Error, Instance, Result,
    backend::database::InMemory,
    crdt::Doc,
    store::{StoreError, Table, YDoc},
    user::User,
    y_crdt::{Map as YMap, Transact},
};
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
    /// Set a user preference
    SetPref {
        /// The preference key
        #[arg(required = true)]
        key: String,
        /// The preference value
        #[arg(required = true)]
        value: String,
    },
    /// Show all user preferences
    ShowPrefs,
}

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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load or create the instance
    let instance = load_or_create_instance(&cli.database_path).await?;

    // Get or create passwordless user
    let mut user = get_or_create_user(&instance).await?;

    // Load or create the todo database
    let todo_database = load_or_create_todo_database(&mut user).await?;

    // Handle the command with proper error context
    let result = match &cli.command {
        Commands::Add { title } => add_todo(&todo_database, title.clone())
            .await
            .map(|_| println!("✓ Task added: {title}")),
        Commands::Complete { id } => complete_todo(&todo_database, id)
            .await
            .map(|_| println!("✓ Task completed: {id}")),
        Commands::List => list_todos(&todo_database).await,
        Commands::SetUser { name, email, bio } => {
            set_user_info(&todo_database, name.as_ref(), email.as_ref(), bio.as_ref())
                .await
                .map(|_| println!("✓ User information updated"))
        }
        Commands::ShowUser => show_user_info(&todo_database).await,
        Commands::SetPref { key, value } => {
            set_user_preference(&todo_database, key.clone(), value.clone())
                .await
                .map(|_| println!("✓ User preference set"))
        }
        Commands::ShowPrefs => show_user_preferences(&todo_database).await,
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

    // Save the instance
    save_instance(&instance, &cli.database_path).await
}

async fn load_or_create_instance(path: &PathBuf) -> Result<Instance> {
    let instance = if path.exists() {
        let backend = InMemory::load_from_file(path).await?;
        Instance::open(Box::new(backend)).await?
    } else {
        let backend = InMemory::new();
        Instance::open(Box::new(backend)).await?
    };

    println!("✓ Instance initialized");

    Ok(instance)
}

async fn get_or_create_user(instance: &Instance) -> Result<User> {
    // Use a fixed username for the single user in this app
    let username = "todo-user";

    // Try to login first
    match instance.login_user(username, None).await {
        Ok(user) => {
            println!("✓ Logged in as passwordless user: {username}");
            Ok(user)
        }
        Err(e) if e.is_not_found() => {
            // User doesn't exist, create it
            println!("Creating new passwordless user: {username}");
            instance.create_user(username, None).await?;
            let user = instance.login_user(username, None).await?;
            println!("✓ Created and logged in as passwordless user: {username}");
            Ok(user)
        }
        Err(e) => Err(e),
    }
}

async fn save_instance(instance: &Instance, path: &PathBuf) -> Result<()> {
    let database = instance.backend();

    // Cast the database to InMemory to access save_to_file
    let in_memory_database = database
        .as_any()
        .downcast_ref::<InMemory>()
        .ok_or_else(|| {
            Error::Io(std::io::Error::other(
                "Failed to downcast database to InMemory",
            ))
        })?;

    in_memory_database.save_to_file(path).await?;
    Ok(())
}

async fn load_or_create_todo_database(user: &mut User) -> Result<Database> {
    let database_name = "todo";

    // Try to find the database by name
    let database = match user.find_database(database_name).await {
        Ok(mut databases) => {
            // If multiple databases with the same name exist, pop will return one arbitrarily.
            // We might want more robust handling later (e.g., error or config option).
            databases.pop().unwrap() // unwrap is safe because find_database errors if empty
        }
        Err(e) if e.is_not_found() => {
            // If not found, create a new one
            println!("No existing todo database found, creating a new one...");
            let mut settings = Doc::new();
            settings.set("name", database_name);

            // Get the default key (earliest created key)
            let default_key = user.get_default_key()?;

            // User API automatically configures the database with user's keys
            user.create_database(settings, &default_key).await?
        }
        Err(e) => {
            // Propagate other errors
            return Err(e);
        }
    };

    Ok(database)
}

async fn add_todo(database: &Database, title: String) -> Result<()> {
    // Start an atomic transaction
    let op = database.new_transaction().await?;

    // Get a handle to the 'todos' Table store
    let todos_store = op.get_store::<Table<Todo>>("todos").await?;

    // Create a new todo
    let todo = Todo::new(title);

    // Insert the todo into the Table
    // The Table will generate a unique ID for it
    let todo_id = todos_store.insert(todo).await?;

    // Commit the transaction
    op.commit().await?;

    println!("Added todo with ID: {todo_id}");

    Ok(())
}

async fn complete_todo(database: &Database, id: &str) -> Result<()> {
    // Start an atomic transaction
    let op = database.new_transaction().await?;

    // Get a handle to the 'todos' Table store
    let todos_store = op.get_store::<Table<Todo>>("todos").await?;

    // Get the todo from the Table
    let mut todo = match todos_store.get(id).await {
        Ok(todo) => todo,
        Err(e) if e.is_not_found() => {
            // Provide a user-friendly error message for not found
            return Err(StoreError::KeyNotFound {
                store: "todos".to_string(),
                key: id.to_string(),
            }
            .into());
        }
        Err(e) => {
            // For other errors, just propagate
            return Err(e);
        }
    };

    // Mark the todo as complete
    todo.complete();

    // Update the todo in the Table
    todos_store.set(id, todo).await?;

    // Commit the transaction
    op.commit().await?;

    Ok(())
}

async fn list_todos(database: &Database) -> Result<()> {
    // Start an atomic transaction
    let op = database.new_transaction().await?;

    // Get a handle to the 'todos' Table store
    let todos_store = op.get_store::<Table<Todo>>("todos").await?;

    // Search for all todos (predicate always returns true)
    let todos_with_ids = todos_store.search(|_| true).await?;

    // Print the todos
    if todos_with_ids.is_empty() {
        println!("No tasks found.");
    } else {
        println!("Tasks:");
        // Sort todos by creation date
        let mut sorted_todos = todos_with_ids;
        sorted_todos.sort_by_key(|(_, a)| a.created_at);

        for (id, todo) in sorted_todos {
            let status = if todo.completed { "✓" } else { " " };
            println!("[{}] {} (ID: {})", status, todo.title, id);
        }
    }

    Ok(())
}

async fn set_user_info(
    database: &Database,
    name: Option<&String>,
    email: Option<&String>,
    bio: Option<&String>,
) -> Result<()> {
    // Start an atomic transaction
    let op = database.new_transaction().await?;

    // Get a handle to the 'user_info' YDoc store
    let user_info_store = op.get_store::<YDoc>("user_info").await?;

    // Update user information using the Y-CRDT document
    user_info_store
        .with_doc_mut(|doc| {
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
        })
        .await?;

    // Commit the transaction
    op.commit().await?;

    Ok(())
}

async fn show_user_info(database: &Database) -> Result<()> {
    // Start an atomic transaction
    let op = database.new_transaction().await?;

    // Get a handle to the 'user_info' YDoc store
    let user_info_store = op.get_store::<YDoc>("user_info").await?;

    // Read user information from the Y-CRDT document
    user_info_store
        .with_doc(|doc| {
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
        })
        .await?;

    Ok(())
}

async fn set_user_preference(database: &Database, key: String, value: String) -> Result<()> {
    // Start an atomic transaction
    let op = database.new_transaction().await?;

    // Get a handle to the 'user_prefs' YDoc store
    let user_prefs_store = op.get_store::<YDoc>("user_prefs").await?;

    // Update user preference using the Y-CRDT document
    user_prefs_store
        .with_doc_mut(|doc| {
            let prefs_map = doc.get_or_insert_map("preferences");
            let mut txn = doc.transact_mut();
            prefs_map.insert(&mut txn, key, value);
            Ok(())
        })
        .await?;

    // Commit the transaction
    op.commit().await?;

    Ok(())
}

async fn show_user_preferences(database: &Database) -> Result<()> {
    // Start an atomic transaction (for read-only)
    let op = database.new_transaction().await?;

    // Get a handle to the 'user_prefs' YDoc store
    let user_prefs_store = op.get_store::<YDoc>("user_prefs").await?;

    // Read user preferences from the Y-CRDT document
    user_prefs_store
        .with_doc(|doc| {
            let prefs_map = doc.get_or_insert_map("preferences");
            let txn = doc.transact();

            println!("User Preferences:");

            // Iterate over all preferences
            for (key, value) in prefs_map.iter(&txn) {
                let value_str = value.to_string(&txn);
                println!("{key}: {value_str}");
            }

            Ok(())
        })
        .await?;

    Ok(())
}
