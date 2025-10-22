# Instance & Database Relationship

## Architecture

Instance and Database have a parent-child relationship where Database holds a weak reference to Instance:

- **Instance**: Manages storage backend, system databases, and user coordination
- **Database**: Represents a single database, holds `WeakInstance` for storage access
- **Weak References**: Prevents circular dependencies and memory leaks

This layering ensures Instance is the central coordinator while Database remains focused on database-specific operations.

## Storage Access Pattern

Database accesses storage through Instance:

1. Database holds `WeakInstance` reference
2. When storage needed, `instance()` method upgrades weak reference
3. Operations use `instance.backend()` for storage access
4. Weak reference allows Database to be dropped without preventing Instance cleanup

This pattern provides clear ownership hierarchy while avoiding reference cycles.
