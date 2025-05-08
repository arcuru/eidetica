use eideticadb::backend::InMemoryBackend;
use eideticadb::basedb::BaseDB;
use eideticadb::constants::SETTINGS;
use eideticadb::data::KVOverWrite;
use eideticadb::entry::Entry;
use eideticadb::Tree;
use signal_hook::flag as signal_flag;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const DB_FILE: &str = "eidetica.json";

// Helper function to save the database
fn save_database(db: &BaseDB) {
    println!("Saving database to {}...", DB_FILE);
    if let Ok(backend_guard) = db.backend().lock() {
        let backend_any = backend_guard.as_any();
        if let Some(in_memory_backend) = backend_any.downcast_ref::<InMemoryBackend>() {
            match in_memory_backend.save_to_file(DB_FILE) {
                Ok(_) => println!("Database saved successfully."),
                Err(e) => println!("Failed to save database: {:?}", e),
            }
        } else {
            println!("Failed to downcast backend to InMemoryBackend for saving.");
        }
    } else {
        println!("Failed to lock backend for saving.");
    }
}

fn main() -> io::Result<()> {
    // Set up signal handling
    // term_signal is a flag that is set to true when a termination signal is received
    let term_signal = Arc::new(AtomicBool::new(false));
    // Register handlers for termination signals
    // The `register` function handles potential errors internally for common cases
    // and returns a Result which we ignore here for simplicity in the REPL context.
    for signal in signal_hook::consts::TERM_SIGNALS {
        let _ = signal_flag::register(*signal, Arc::clone(&term_signal));
    }

    println!("Welcome to EideticaDB REPL");
    println!(
        "Database is automatically loaded from and saved to '{}'",
        DB_FILE
    );
    print_help();

    // Create or load the in-memory backend
    let backend: Box<dyn eideticadb::backend::Backend> =
        match InMemoryBackend::load_from_file(DB_FILE) {
            Ok(backend) => {
                println!("Loaded database from {}", DB_FILE);
                Box::new(backend)
            }
            Err(e) => {
                println!("Failed to load database: {:?}. Creating a new one.", e);
                Box::new(InMemoryBackend::new())
            }
        };

    // Initialize BaseDB with the loaded or new backend
    let db = BaseDB::new(backend);

    // Store trees by name
    let mut trees: HashMap<String, Tree> = HashMap::new();

    // Restore trees using the new BaseDB.all_trees method
    match db.all_trees() {
        Ok(loaded_trees) => {
            for tree in loaded_trees {
                match tree.get_name() {
                    Ok(name) => {
                        println!("Restored tree '{}' with root ID: {}", name, tree.root_id());
                        trees.insert(name.clone(), tree);
                    }
                    Err(e) => {
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
            println!("Error loading trees from database: {:?}", e);
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
                    println!("Usage: create-tree <name> <settings>");
                    continue;
                }

                let name = args[1];
                let settings_str = args[2..].join(" ");
                // Create a map for the initial settings and serialize it to JSON
                let mut settings_map = HashMap::new();
                settings_map.insert(SETTINGS.to_string(), settings_str);
                settings_map.insert("name".to_string(), name.to_string());

                let settings = KVOverWrite::from_hashmap(settings_map);

                match db.new_tree(settings) {
                    Ok(tree) => {
                        println!("Created tree '{}' with root ID: {}", name, tree.root_id());
                        trees.insert(name.to_string(), tree);
                    }
                    Err(e) => println!("Error creating tree: {:?}", e),
                }
            }
            "list-trees" => {
                if trees.is_empty() {
                    println!("No trees created yet");
                } else {
                    println!("Trees:");
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
                    println!("Tree '{}' not found", name);
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
                                println!("Entry found in tree '{}':", name);
                                print_entry(&entry);
                                found = true;
                                break;
                            }
                            Err(e) => {
                                println!("Error retrieving entry: {:?}", e);
                                found = true;
                                break;
                            }
                        }
                    }
                }

                if !found {
                    println!("Entry with ID '{}' not found", id);
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
        println!("Exiting EideticaDB REPL");
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
        println!("  Subtree: {}", subtree);
        println!("    Data:");
        println!("      {}", entry.get_settings().unwrap());
    }
    if let Ok(parents) = entry.parents() {
        println!("  Parents: {:?}", parents);
    } else {
        println!("  Parents: []");
    }
}
