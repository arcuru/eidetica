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

## Synthetic verified-frontier probe

The ignored `frontier_probe` integration test uses a temporary SQLite database and
its own Unix-socket service, never existing data or a production daemon:

```sh
PROBE_ENTRIES=100 PROBE_KEYS=1 cargo test -p eidetica --release --all-features --test frontier_probe -- --ignored --nocapture
```

Compare independently varied history (for example 100 and 1000 commits) and current
materialized Store size (1 and 100 keys). Population seeds the requested keys once
and subsequent commits overwrite them, so history does not imply a larger Store.
Each fixture reports 30 warm local/service snapshot and settings reads, a cold
settings sample after clearing disposable materializations (not OS page caches),
and 10 local/service write transactions. Writes have no performance target.
Record exact source commits, build mode, host load and all raw samples/summaries;
run both revisions with identical parameters, separately built release artifacts,
and no concurrent compilation or gate. A single cold sample is descriptive, not a
statistical speedup claim. Report uncertainty and regressions rather than assigning
causes from this timing probe alone.
