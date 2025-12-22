# Eidetica Documentation

Welcome to the official documentation for Eidetica - a decentralized database built on Merkle-CRDT principles with built-in peer-to-peer synchronization.

## Key Features

- **Decentralized Architecture**: No central server required - peers connect directly
- **Conflict-Free Replication**: Automatic merge resolution using CRDT principles
- **Content-Addressable Storage**: Immutable, hash-identified data entries
- **Real-time Synchronization**: Background sync with configurable batching and timing
- **Multiple Transport Protocols**: HTTP and Iroh P2P with NAT traversal
- **Async-First API**: Built on Tokio for efficient, non-blocking operations
- **Authentication & Security**: Ed25519 signatures for all operations
- **Flexible Data Models**: Support for documents, key-value, and structured data

## Project Structure

Eidetica is organized as a Cargo workspace:

- **Library** (`crates/lib/`): The core Eidetica library crate
- **CLI Binary** (`crates/bin/`): Command-line interface using the library
- **Examples** (`examples/`): Standalone applications demonstrating usage

## Quick Links

### Documentation Sections

- [User Guide](user_guide/index.md): Learn how to use the Eidetica library
- [Getting Started](user_guide/getting_started.md): Set up your first Eidetica database
- [Synchronization Guide](user_guide/synchronization_guide.md): Enable peer-to-peer sync
- [Internal Documentation](internal/index.md): Understand the internal design and contribute
- [Design Documents](design/index.md): Architectural documents used for development

### Examples

- **[Chat Application](https://github.com/arcuru/eidetica/blob/main/examples/chat/README.md)** - Complete multi-user chat with P2P sync
- **[Todo Application](https://github.com/arcuru/eidetica/blob/main/examples/todo/README.md)** - Task management example
