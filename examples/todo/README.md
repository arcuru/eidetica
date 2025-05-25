# Todo CLI App

A simple command-line Todo application that demonstrates the usage of Eidetica with multiple subtree types.

## Overview

This Todo CLI application demonstrates how Eidetica can be used to build a simple database-backed application with multiple data structures. The app allows you to:

### Todo Management (RowStore)

- Add new tasks to your todo list
- Mark tasks as complete
- List all tasks with their completion status

### User Information Management (YrsStore)

- Set and update user information (name, email, bio)
- View current user information
- Set and update user preferences
- View all user preferences

All data is persisted to a local file using Eidetica's InMemoryBackend with file serialization.
To specify which file it persists into, pass the option `--database-path /path/to/file.json`.

## Usage

```
cargo run -- <COMMAND>
```

Available commands:

### Todo Commands

- `add <TITLE>` - Add a new task
- `complete <ID>` - Mark a task as complete
- `list` - List all tasks

### User Information Commands

- `set-user [--name NAME] [--email EMAIL] [--bio BIO]` - Set or update user information
- `show-user` - Display current user information
- `set-pref <KEY> <VALUE>` - Set a user preference
- `show-prefs` - Display all user preferences

## Example Usage Script

You can try the following commands to test the application:

```bash
# Set user information
eidetica-todo set-user --name "Alice Johnson" --email "alice@example.com" --bio "Software developer"
eidetica-todo show-user

# Set user preferences
eidetica-todo set-pref theme "dark"
eidetica-todo set-pref notifications "enabled"
eidetica-todo show-prefs

# Create todos
eidetica-todo add "Buy groceries"
eidetica-todo add "Finish project report"
eidetica-todo add "Call mom"

# List all todos
eidetica-todo list

# Complete a task (replace ABC123 with an actual ID from your list)
eidetica-todo complete ABC123

# List all todos again to see the completed task
eidetica-todo list

# Update user preferences
eidetica-todo set-pref theme "light"
eidetica-todo show-prefs
```

## How It Works

This app demonstrates several Eidetica features and subtree types:

- **BaseDB**: The main database interface
- **InMemoryBackend**: Memory storage with JSON file persistence
- **Tree**: A hierarchical data structure containing multiple subtrees
- **RowStore**: A subtree type for storing record-like data (used for todos)
- **YrsStore**: A Y-CRDT based subtree for collaborative data structures (used for user info and preferences)

### Data Organization

The application uses three separate subtrees within a single tree:

1. **"todos"** (RowStore<Todo>): Stores todo items with automatic ID generation
2. **"user_info"** (YrsStore): Stores user profile information using Y-CRDT Maps
3. **"user_prefs"** (YrsStore): Stores user preferences using Y-CRDT Maps

This demonstrates how different subtree types can coexist within the same Eidetica tree, each optimized for their specific use case.

## Data Model

### Todo Items (RowStore)

Each todo item has:

- A title
- A completion status
- Creation timestamp
- Completion timestamp (when completed)

The unique ID for each task is provided by the Eidetica RowStore, which automatically generates and manages primary keys.

### User Information (YrsStore)

User information is stored in Y-CRDT Maps within the YrsStore subtree:

- **user_info**: Contains name, email, bio fields
- **preferences**: Contains user preference key-value pairs

The Y-CRDT structure allows for eventual consistency and conflict-free merging in collaborative scenarios.

## Automated Test Script

A comprehensive automated test script is included that demonstrates:

### User Information Testing

- Setting partial user information
- Updating user information with additional fields
- Setting and updating user preferences
- Displaying user information and preferences

### Todo Functionality Testing

- Creating multiple tasks
- Listing all tasks
- Automatically extracting and completing a task
- Showing updated task status

### Persistence and Coexistence Testing

- Verifying data persistence across operations
- Demonstrating multiple subtree types working together
- Simulating collaborative updates

You can run the comprehensive test with:

```bash
chmod +x test.sh
./test.sh
```

The test script provides a complete demonstration of both RowStore and YrsStore functionality, showing how different CRDT types can be used together in a single application.
