## Data Flow

The data flow in Eidetica follows a structured sequence of interactions between core components.

### Basic Flow

1. User creates an Instance with a storage backend
2. User creates Databases within the Instance
3. Database holds a weak reference to Instance for storage access
4. Operations construct immutable Entry objects through EntryBuilder
5. Entries reference parent entries, forming a directed acyclic graph
6. Database accesses storage through Instance.backend() via weak reference upgrade
7. Entries are stored and retrieved through the Instance's backend interface
8. Authentication validates and signs entries when configured

### Authentication Flow

When authentication is enabled, additional steps occur during commit:

- Entry signing with cryptographic signatures
- Permission validation for the operation type
- Bootstrap handling for initial admin configuration
- Verification status assignment based on validation results

This ensures data integrity and access control while maintaining compatibility with unsigned entries.

### CRDT Caching Flow

The system uses an efficient caching layer for CRDT state computation:

- Cache lookup using Entry ID and Store as the key
- On cache miss, recursive LCA algorithm computes state and caches the result
- Cache hits return instantly for subsequent queries
- Performance scales well due to immutable entries and high cache hit rates
