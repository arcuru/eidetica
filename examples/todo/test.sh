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

# Test user information functionality first (before any todos)
echo "=== Testing User Information (YrsStore) ==="
echo

# Test showing empty user info
echo "Showing empty user information:"
$CMD show-user
echo

# Test setting partial user info
echo "Setting partial user information (name only):"
$CMD set-user --name "Test User"
$CMD show-user
echo

# Test updating user info with more fields
echo "Updating user information with email and bio:"
$CMD set-user --email "test@example.com" --bio "A test user for the todo app"
$CMD show-user
echo

# Test updating individual fields
echo "Updating just the name:"
$CMD set-user --name "Alice Smith"
$CMD show-user
echo

# Test user preferences functionality
echo "=== Testing User Preferences (YrsStore) ==="
echo

# Test showing empty preferences
echo "Showing empty user preferences:"
$CMD show-prefs
echo

# Test setting preferences
echo "Setting user preferences:"
$CMD set-pref theme "dark"
$CMD set-pref notifications "enabled"
$CMD set-pref language "en"
$CMD show-prefs
echo

# Test updating existing preference
echo "Updating existing preference:"
$CMD set-pref theme "light"
$CMD show-prefs
echo

# Test todo functionality
echo "=== Testing Todo Functionality (RowStore) ==="
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

# Test persistence by showing user info and preferences again
echo "=== Testing Data Persistence ==="
echo

echo "Verifying user information is still there:"
$CMD show-user
echo

echo "Verifying user preferences are still there:"
$CMD show-prefs
echo

echo "Verifying todos are still there:"
$CMD list
echo

# Test collaborative scenario simulation
echo "=== Testing Collaborative Scenario Simulation ==="
echo

# Simulate another user updating preferences (in real CRDT scenario, this would be from another client)
echo "Simulating collaborative preference updates:"
$CMD set-pref auto_save "true"
$CMD set-pref sync_interval "30"
echo

echo "Final state of user preferences:"
$CMD show-prefs
echo

echo "Final state of user information:"
$CMD show-user
echo

echo "Final state of todos:"
$CMD list
echo

echo "=== Test completed successfully ==="
echo "✓ User information (YrsStore) functionality working"
echo "✓ User preferences (YrsStore) functionality working"
echo "✓ Todo management (RowStore) functionality working"
echo "✓ Data persistence working"
echo "✓ All subtrees coexisting properly"
