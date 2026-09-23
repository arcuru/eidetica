# Performance

The architecture provides several performance characteristics:

- **Content-addressable storage**: Enables efficient deduplication through BLAKE3 content hashing.
- **Database structure (DAG)**: Supports partial replication and sparse checkouts. Tip calculation complexity depends on parent relationships.
- **SQLite Backend**: Provides excellent performance with automatic persistence. Supports both file-based and in-memory modes.
- **Lock-based concurrency**: May create bottlenecks in high-concurrency write scenarios.
- **Height calculation**: Uses BFS-based topological sorting with O(V + E) complexity.
- **CRDT merge algorithm**: Employs recursive merge-base merging with intelligent caching.

### CRDT Merge Performance

The recursive merge-base algorithm uses caching for performance optimization:

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

### Table projections

`Table<T>` persists RFC 8785 canonical JSON rows in `LwwMap` Entry deltas.
Cold record materialization streams physical row puts/deletes into bounded private
chunks (128 mutations or 1 MiB) before publishing an immutable generation.
This bounds the projected row chunk, **not** the entire rebuild: history retrieval
still returns `Vec<Entry>`, and the transaction's staged delta may hold a batch
of rows. Warm point reads fetch one record; scans request bounded pages.
Encrypted scans sort by keyed physical hashes rather than logical row keys.
See the Table benchmark harness for payload, write, point and scan workloads;
benchmark results depend on backend, history shape and cache state.
