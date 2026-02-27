# Eidetica Chat

A mostly vibe-coded and human reviewed Chat application demonstrating usage of the API and sync capabilities. It's to show how to use the API and as a test-bed for the ergonomics of Eidetica's API surface.

It's a Terminal User Interface (TUI) chat application with room-based messaging, user accounts, and peer-to-peer synchronization.

## Overview

This example showcases:

- **User accounts**: Automatic passwordless user creation and management
- **Room-based chat**: Create and join multiple chat rooms (each room is a separate database)
- **Multi-transport sync**: Choose between HTTP (simple client-server) or Iroh (P2P with NAT traversal)
- **Connection sharing**: Share room addresses to invite others
- **Automatic sync**: Messages sync in real-time between connected peers
- **Simple TUI**: Clean terminal interface for easy testing

## Quick Start

### Create a New Room

```bash
# From the repo root, run:
cd examples/chat

# Create a new room (default Iroh transport)
cargo run -- --username alice

# Or use HTTP transport
cargo run -- --username alice --transport http
```

When you create a new room, the app will:

1. Display the room address that others can use to join
2. Wait for you to press Enter before starting the chat

Example output:

```
🚀 Eidetica Chat Room Created!
📍 Room Address: eidetica:?db=sha256:abc...&pr=iroh:endpoint...
👤 Username: alice

Share this address with others to invite them to the chat.
Press Enter to start chatting...
```

### Join an Existing Room

```bash
# Connect to a room using its address
cargo run -- <room_address> --username bob
```

The app will connect to the specified room and start the chat interface immediately.

### Chat Interface

Once in a room, you'll see:

- **Room address bar**: Displays the room address at the top
- **Messages**: Chat history in the middle
- **Input field**: Type messages at the bottom

Controls:

- **Type and Enter**: Send a message
- **↑/↓**: Scroll through message history
- **Q or ESC**: Quit the application

## CLI Options

```bash
Usage: eidetica-chat [ROOM_ADDRESS] [OPTIONS]

Arguments:
  [TICKET]        Ticket URL to connect to (eidetica:?db=...&pr=...).
                  If not provided, creates a new room.

Options:
  -u, --username <USERNAME>      Username for the chat session (default: $USER or "Anonymous")
  -v, --verbose                  Enable verbose debug output
      --transport <TRANSPORT>    Transport to use: 'http' or 'iroh' (default: iroh)
  -h, --help                     Print help
  -V, --version                  Print version
```

## Connecting with Others

1. **Create a room** - Run without a room address to create a new room
2. **Copy the room address** - The address is displayed after creation
3. **Share the address** - Send it to others via any communication channel
4. **Others join** - They run the app with your room address as an argument

## Example Workflow

### Two-User Chat Session

**Terminal 1 (Alice - Room Creator):**

```bash
cd examples/chat
cargo run -- --username alice

# App displays:
# 🚀 Eidetica Chat Room Created!
# 📍 Room Address: eidetica:?db=sha256:abc...&pr=iroh:endpoint...
# 👤 Username: alice
#
# Share this address with others to invite them to the chat.
# Press Enter to start chatting...

# Copy the ticket URL, share it with Bob, then press Enter
```

**Terminal 2 (Bob - Room Joiner):**

```bash
cd examples/chat

# Paste Alice's ticket URL as the first argument
cargo run -- 'eidetica:?db=sha256:abc...&pr=iroh:endpoint...' --username bob

# Chat interface starts immediately - start chatting!
```

### Using Different Transports

**HTTP (for local testing):**

```bash
cargo run -- --username alice --transport http
```

**Iroh (for P2P across networks):**

```bash
cargo run -- --username alice --transport iroh
```

## Architecture

The example demonstrates Eidetica's key components:

- **Instance**: Main database system managing storage and sync
- **User**: Passwordless user account with automatic key management
- **Database**: Each room is a separate database with its own authentication
- **Table Store**: Messages stored in a `Table<ChatMessage>` store within each room
- **Sync System**: Automatic peer synchronization with configurable transports
- **Bootstrap Protocol**: Automatic database access requests when joining rooms
