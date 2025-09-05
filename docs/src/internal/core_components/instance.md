# Instance

## Purpose and Architecture

Instance acts as the orchestration layer between application code and the underlying storage systems. It manages multiple independent Databases (analogous to databases), handles cryptographic authentication, and coordinates with pluggable storage backends.

Each Instance instance maintains a unique device identity through an automatically-generated Ed25519 keypair, enabling secure multi-device synchronization.

## Key Responsibilities

**Database Management**: Creates and provides access to Databases, each representing an independent history of data entries.

**Authentication Infrastructure**: Manages Ed25519 private keys for signing operations and validating permissions. All operations require authenticated access.

**Backend Coordination**: Interfaces with pluggable storage backends (currently just InMemory) while abstracting storage details from higher-level code.

**Device Identity**: Automatically maintains device-specific cryptographic identity for sync operations.

## Design Principles

- **Authentication-First**: Every operation requires cryptographic validation
- **Pluggable Storage**: Storage backends can be swapped without affecting application logic
- **Multi-Database**: Supports multiple independent data collections within a single instance
- **Sync-Ready**: Built-in device identity and hooks for distributed synchronization
