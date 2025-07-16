## Data Flow

The data flow in Eidetica follows a structured sequence of interactions between core components.

### Basic Flow

1. User creates a BaseDB with a database backend
2. User creates Trees within the database
3. Operations construct immutable Entry objects through EntryBuilder
4. Entries reference parent entries, forming a directed acyclic graph
5. Entries are stored and retrieved through the database interface
6. Authentication validates and signs entries when configured

### Authentication Flow

When authentication is enabled, additional steps occur during commit:

- Entry signing with cryptographic signatures
- Permission validation for the operation type
- Bootstrap handling for initial admin configuration
- Verification status assignment based on validation results

This ensures data integrity and access control while maintaining compatibility with unsigned entries.

### CRDT Caching Flow

The system uses an efficient caching layer for CRDT state computation:

- Cache lookup using Entry ID and Subtree as the key
- On cache miss, recursive LCA algorithm computes state and caches the result
- Cache hits return instantly for subsequent queries
- Performance scales well due to immutable entries and high cache hit rates
