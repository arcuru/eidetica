# Performance

The architecture provides several performance characteristics:

- **Content-addressable storage**: Enables efficient deduplication through SHA-256 content hashing.
- **Database structure (DAG)**: Supports partial replication and sparse checkouts. Tip calculation complexity depends on parent relationships.
- **InMemoryDatabase**: Provides high-speed operations but is limited by available RAM.
- **Lock-based concurrency**: May create bottlenecks in high-concurrency write scenarios.
- **Height calculation**: Uses BFS-based topological sorting with O(V + E) complexity.
- **CRDT merge algorithm**: Employs recursive LCA-based merging with intelligent caching.

### CRDT Merge Performance

The recursive LCA-based merge algorithm uses caching for performance optimization:

#### Algorithm Complexity

- Cached states: O(1) amortized performance
- Uncached states: O(D × M) where D is DAG depth and M is merge cost
- Overall performance benefits from high cache hit rates

#### Key Performance Benefits

- Efficient handling of complex DAG structures
- Optimized path finding reduces database calls
- Cache eliminates redundant computations
- Scales well with DAG complexity through memoization
- Memory-computation trade-off favors cached access patterns
