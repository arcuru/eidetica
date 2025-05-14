# Todo CLI App

A simple command-line Todo application that demonstrates the usage of Eidetica.

## Overview

This Todo CLI application demonstrates how Eidetica can be used to build a simple database-backed application. The app allows you to:

- Add new tasks to your todo list
- Mark tasks as complete
- List all tasks with their completion status

All data is persisted to a local file using Eidetica's InMemoryBackend with file serialization.
To specify which file it persists into, pass the option `--database-path /path/to/file.json`.

## Usage

```
cargo run -- <COMMAND>
```

Available commands:

- `add <TITLE>` - Add a new task
- `complete <ID>` - Mark a task as complete
- `list` - List all tasks

## Example Usage Script

You can try the following commands to test the application:

```bash
# Create a new todo database
eidetica-todo add "Buy groceries"
eidetica-todo add "Finish project report"
eidetica-todo add "Call mom"

# List all todos
eidetica-todo list

# Complete a task (replace ABC123 with an actual ID from your list)
eidetica-todo complete ABC123

# List all todos again to see the completed task
eidetica-todo list
```

## How It Works

This app uses several Eidetica features:

- **BaseDB**: The main database interface
- **InMemoryBackend**: Memory storage with JSON file persistence
- **Tree**: A hierarchical data structure
- **RowStore**: A subtree type for storing record-like data

Each todo item is stored as a record in a RowStore subtree named "todos" within the main database tree.

## Data Model

Each todo item has:

- A title
- A completion status
- Creation timestamp
- Completion timestamp (when completed)

The unique ID for each task is provided by the Eidetica RowStore, which automatically generates and manages primary keys.

## Automated Test Script

For a more comprehensive demonstration, an automated test script is included in this repository. This script:

- Creates a fresh database
- Adds multiple tasks
- Lists all tasks
- Automatically extracts a task ID for the "Buy groceries" task
- Completes that task
- Lists the tasks again to show the updated status

You can run it with:

```bash
chmod +x test.sh
./test.sh
```

The test script uses standard shell commands to extract the task ID from the list output, making the demonstration fully automatic without requiring manual intervention.
