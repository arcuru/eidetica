#!/usr/bin/env bash
set -e

# Configuration
DB_FILE="test_todo.json"
CMD="cargo run --quiet -- --database-path $DB_FILE"

echo "=== Eidetica Todo App Test ==="
echo

# Clean up previous test database
echo "Cleaning up previous test database..."
rm -f $DB_FILE
echo

# Add tasks
echo "Adding tasks..."
$CMD add "Buy groceries"
$CMD add "Write documentation"
$CMD add "Fix bugs"
echo

# List all tasks and save output to a temporary file
echo "Listing all tasks:"
TASK_LIST=$(mktemp)
$CMD list | tee $TASK_LIST
echo

# Get the ID of the first task to complete it
echo "Extracting task ID for completion..."
# Extract the line containing "Buy groceries" and then extract the ID
TASK_LINE=$(grep "Buy groceries" $TASK_LIST || echo "")
echo "Found task line: $TASK_LINE"

if [ -n "$TASK_LINE" ]; then
  # Extract the ID using a more robust pattern matching approach
  TASK_ID=$(echo "$TASK_LINE" | grep -o 'ID: [^ )]*' | cut -d' ' -f2)
  echo "Extracted ID: $TASK_ID"

  if [ -n "$TASK_ID" ]; then
    echo "Completing task with ID: $TASK_ID"
    $CMD complete $TASK_ID
    echo

    echo "Listing tasks after completion:"
    $CMD list
  else
    echo "Failed to extract a valid task ID from line: $TASK_LINE"
  fi
else
  echo "Failed to find task with title 'Buy groceries'. Check the output format."
fi

# Clean up temporary file
rm -f $TASK_LIST

echo
echo "=== Test completed ==="
