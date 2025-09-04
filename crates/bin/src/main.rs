use eidetica::Database;
use eidetica::Entry;
use eidetica::Instance;
use eidetica::backend::database::InMemory;
use signal_hook::flag as signal_flag;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing_subscriber::EnvFilter;

const DB_FILE: &str = "eidetica.json";

// Helper function to save the database
fn save_database(db: &Instance) {
    tracing::info!("Saving database to {DB_FILE}...");
    println!("Saving database to {DB_FILE}...");
    let backend_any = db.backend().as_any();
    if let Some(in_memory_backend) = backend_any.downcast_ref::<InMemory>() {
        match in_memory_backend.save_to_file(DB_FILE) {
            Ok(_) => {
                tracing::info!("Database saved successfully.");
                println!("Database saved successfully.");
            }
            Err(e) => {
                tracing::error!("Failed to save database: {e:?}");
                println!("Failed to save database: {e:?}");
            }
        }
    } else {
        tracing::error!("Failed to downcast database to InMemory for saving.");
        println!("Failed to downcast database to InMemory for saving.");
    }
}

fn main() -> io::Result<()> {
    // Initialize tracing subscriber with environment filter
    // Uses RUST_LOG environment variable to control log level
    // Example: RUST_LOG=info cargo run
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=info".parse().unwrap()),
        )
        .init();

    // Set up signal handling
    // term_signal is a flag that is set to true when a termination signal is received
    let term_signal = Arc::new(AtomicBool::new(false));
    // Register handlers for termination signals
    // The `register` function handles potential errors internally for common cases
    // and returns a Result which we ignore here for simplicity in the REPL context.
    for signal in signal_hook::consts::TERM_SIGNALS {
        let _ = signal_flag::register(*signal, Arc::clone(&term_signal));
    }

    println!("Welcome to Eidetica REPL");
    println!("Database is automatically loaded from and saved to '{DB_FILE}'");
    print_help();

    // Create or load the in-memory backend
    let backend: Box<dyn eidetica::backend::BackendDB> = match InMemory::load_from_file(DB_FILE) {
        Ok(backend) => {
            tracing::info!("Loaded database from {DB_FILE}");
            println!("Loaded database from {DB_FILE}");
            Box::new(backend)
        }
        Err(e) => {
            tracing::warn!("Failed to load database: {e:?}. Creating a new one.");
            println!("Failed to load database: {e:?}. Creating a new one.");
            Box::new(InMemory::new())
        }
    };

    // Initialize Instance with the loaded or new backend
    let db = Instance::new(backend);

    // Add a default key for CLI operations (all entries must now be authenticated)
    const DEFAULT_CLI_KEY: &str = "cli_default_key";
    if db.add_private_key(DEFAULT_CLI_KEY).is_err() {
        // Key might already exist, which is fine
    }

    // Store trees by name
    let mut trees: HashMap<String, Database> = HashMap::new();

    // Restore trees using the new Instance.all_trees method
    match db.all_trees() {
        Ok(loaded_trees) => {
            for tree in loaded_trees {
                match tree.get_name() {
                    Ok(name) => {
                        tracing::info!("Restored tree '{}' with root ID: {}", name, tree.root_id());
                        println!("Restored tree '{}' with root ID: {}", name, tree.root_id());
                        trees.insert(name.clone(), tree);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to get name for tree with root {}: {:?}",
                            tree.root_id(),
                            e
                        );
                        println!(
                            "Warning: Failed to get name for tree with root {}: {:?}",
                            tree.root_id(),
                            e
                        );
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!("Error loading trees from database: {e:?}");
            println!("Error loading trees from database: {e:?}");
        }
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut input = String::new();
    let mut save_on_exit = true;

    loop {
        // Check if a termination signal has been received
        if term_signal.load(Ordering::Relaxed) {
            println!("\nTermination signal received, saving database...");
            // Ensure save happens, even if user typed 'exit-no-save' before signal
            save_on_exit = true;
            break;
        }

        print!("> ");
        stdout.flush()?;

        input.clear();
        stdin.lock().read_line(&mut input)?;

        let args: Vec<&str> = input.split_whitespace().collect();

        if args.is_empty() {
            continue;
        }

        match args[0] {
            "help" => {
                print_help();
            }
            "exit" => {
                break;
            }
            "exit-no-save" => {
                save_on_exit = false;
                println!("Exiting without saving...");
                break;
            }
            "save" => {
                save_database(&db);
            }
            "create-tree" => {
                if args.len() < 3 {
                    println!("Usage: create-tree <name>");
                    continue;
                }

                let name = args[1];

                match db.new_tree_default(DEFAULT_CLI_KEY) {
                    Ok(tree) => {
                        tracing::info!("Created tree '{}' with root ID: {}", name, tree.root_id());
                        println!("Created tree '{}' with root ID: {}", name, tree.root_id());
                        trees.insert(name.to_string(), tree);
                    }
                    Err(e) => {
                        tracing::error!("Error creating tree: {e:?}");
                        println!("Error creating tree: {e:?}");
                    }
                }
            }
            "list-trees" => {
                if trees.is_empty() {
                    println!("No trees created yet");
                } else {
                    println!("Databases:");
                    for (name, tree) in &trees {
                        println!("  {} (root: {})", name, tree.root_id());
                    }
                }
            }
            "get-root" => {
                if args.len() < 2 {
                    println!("Usage: get-root <tree-name>");
                    continue;
                }

                let name = args[1];

                if let Some(tree) = trees.get(name) {
                    println!("Root ID for tree '{}': {}", name, tree.root_id());
                } else {
                    println!("Tree '{name}' not found");
                }
            }
            "get-entry" => {
                if args.len() < 2 {
                    println!("Usage: get-entry <entry-id>");
                    continue;
                }

                let id = args[1];
                let mut found = false;

                for (name, tree) in &trees {
                    if tree.root_id() == id {
                        match tree.get_root() {
                            Ok(entry) => {
                                println!("Entry found in tree '{name}':");
                                print_entry(&entry);
                                found = true;
                                break;
                            }
                            Err(e) => {
                                println!("Error retrieving entry: {e:?}");
                                found = true;
                                break;
                            }
                        }
                    }
                }

                if !found {
                    println!("Entry with ID '{id}' not found");
                }
            }
            _ => println!(
                "Unknown command: {}. Type 'help' for available commands.",
                args[0]
            ),
        }
    }

    // Save the database automatically on exit, unless exit-no-save was used
    if save_on_exit {
        save_database(&db);
        tracing::info!("Exiting Eidetica REPL");
        println!("Exiting Eidetica REPL");
    }

    Ok(())
}

fn print_help() {
    println!("Available commands:");
    println!("  help                  - Show this help message");
    println!("  create-tree <n> <settings> - Create a new tree with the given name and settings");
    println!("  list-trees            - List all created trees");
    println!("  get-root <tree-name>  - Get the root ID of a tree");
    println!("  get-entry <entry-id>  - Get details of an entry by ID");
    println!("  save                  - Save the database to disk");
    println!("  exit                  - Save database and exit the REPL");
    println!("  exit-no-save          - Exit the REPL without saving the database");
}

fn print_entry(entry: &Entry) {
    println!("  ID: {}", entry.id());
    println!("  Root: {}", entry.root());
    for subtree in entry.subtrees() {
        println!("  Subtree: {subtree}");
        println!("    Data:");
        if let Ok(data) = entry.data(&subtree) {
            println!("      {data}");
        } else {
            println!("      <no data>");
        }
    }
    if let Ok(parents) = entry.parents() {
        println!("  Parents: {parents:?}");
    } else {
        println!("  Parents: []");
    }
}
