# Instance

## Purpose and Architecture

Instance manages the multi-user infrastructure and system resources. It separates infrastructure management from contextual operations, providing user account management and coordinating with pluggable storage backends. All contextual operations (database creation, key management) run through User sessions after login.

Each Instance maintains a unique device identity (`_device_key`) through an automatically-generated Ed25519 keypair, enabling system database authentication and secure multi-device synchronization.

## Key Responsibilities

**User Management**: Creates and authenticates user accounts with optional password protection.

**System Database Management**: Maintains system databases (`_instance`, `_users`, `_databases`) for infrastructure operations.

**Backend Coordination**: Interfaces with pluggable storage backends (currently just InMemory) while abstracting storage details from higher-level code.

**Device Identity**: Automatically maintains device-specific cryptographic identity (`_device_key`) for system operations and sync.

## Design Principles

- **Infrastructure Focus**: Instance manages infrastructure, User handles operations
- **User-Centric**: All database and key operations run in User context after login
- **Pluggable Storage**: Storage backends can be swapped without affecting application logic
- **Multi-User**: Always multi-user underneath, supporting both passwordless and password-protected users
- **Sync-Ready**: Built-in device identity and hooks for distributed synchronization

## Architecture Layers

Instance provides infrastructure management:

- **User Account Management**: Create users with optional passwords, login to obtain User sessions
- **System Databases**: Maintain `_instance`, `_users`, `_databases` for infrastructure
- **Backend Access**: Coordinate storage operations through pluggable backends

User provides contextual operations (returned from login):

- **Database Operations**: Create, load, and find databases in user context
- **Key Management**: Add private keys, list keys, get signing keys
- **Session Management**: Logout to clear decrypted keys from memory

## Sync Integration

Instance can be extended with synchronization capabilities via `enable_sync()`:

```rust,ignore
// Enable sync on an instance
let instance = Instance::open(backend)?.enable_sync()?;

// Access sync module via Arc (cheap to clone, thread-safe)
let sync = instance.sync().expect("Sync enabled");
```

**Design:**

- **Optional feature**: Sync is opt-in via `enable_sync()` method
- **Arc-based sharing**: `sync()` returns `Option<Arc<Sync>>`
- **Thread-safe**: `Arc<Sync>` can be shared across threads without additional locking
- **Interior mutability**: Sync uses `AtomicBool` and `OnceLock` internally, eliminating need for `Mutex` wrapper
- **Single accessor**: Only `sync()` method (no separate mutable accessor needed)

This design eliminates deadlock risks and simplifies the API by avoiding `MutexGuard` lifetime management.
