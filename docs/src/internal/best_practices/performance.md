# Performance Best Practices

This document outlines performance optimization patterns and practices used throughout the Eidetica codebase, focusing on hot paths, memory efficiency, and scalable algorithms.

## Core Performance Principles

### 1. **Hot Path Optimization**

Identify and optimize performance-critical code paths:

**Common Hot Paths in Eidetica**:

- CRDT state computation and caching
- Entry storage and retrieval operations
- Authentication signature verification
- Bulk data operations (inserts, updates, merges)
- String parameter conversion and handling

### 2. **Memory Efficiency**

Minimize allocations and optimize memory usage patterns:

- Use appropriate string parameter types (`Into<String>` vs `AsRef<str>`)
- Pre-allocate collections with known capacity
- Prefer stack allocation over heap when possible
- Implement efficient caching strategies

### 3. **Algorithmic Efficiency**

Choose algorithms that scale well with data size:

- Use efficient data structures (BTreeMap, HashMap appropriately)
- Implement caching for expensive computations
- Prefer direct iteration over complex iterator chains in hot paths

## String Parameter Optimization

### 1. **Parameter Type Selection**

Follow the storage vs lookup pattern for optimal performance:

```rust
// ✅ GOOD: Use Into<String> for stored parameters
pub fn set_value(&mut self, key: impl Into<String>, value: impl Into<String>) {
    self.data.insert(key.into(), value.into());  // Direct conversion
}

// ✅ GOOD: Use AsRef<str> for lookup operations
pub fn get_value(&self, key: impl AsRef<str>) -> Option<&Value> {
    self.data.get(key.as_ref())  // No allocation
}

// ❌ BAD: Double conversion overhead
pub fn inefficient_set(&mut self, key: impl AsRef<str>, value: impl AsRef<str>) {
    self.data.insert(
        key.as_ref().to_string(),    // AsRef<str> -> &str -> String
        value.as_ref().to_string()   // Double conversion cost
    );
}
```

### 2. **Bulk Operation Optimization**

Convert parameters upfront for bulk operations:

```rust
// ✅ GOOD: Convert once, use many times
pub fn get_multiple_entries<I, T>(&self, entry_ids: I) -> Result<Vec<Entry>>
where
    I: IntoIterator<Item = T>,
    T: Into<ID>,
{
    // Convert all IDs upfront
    let ids: Vec<ID> = entry_ids.into_iter().map(Into::into).collect();
    let mut entries = Vec::with_capacity(ids.len());  // Pre-allocate

    for id in ids {  // No conversion overhead in loop
        entries.push(self.backend.get(&id)?);
    }

    Ok(entries)
}

// ❌ BAD: Per-iteration conversion
pub fn inefficient_get_multiple<I, T>(&self, entry_ids: I) -> Result<Vec<Entry>>
where
    I: IntoIterator<Item = T>,
    T: Into<ID>,
{
    let mut entries = Vec::new();

    for entry_id in entry_ids {
        let id = entry_id.into();  // Conversion on every iteration
        entries.push(self.backend.get(&id)?);
    }

    Ok(entries)
}
```

## Memory Allocation Patterns

### 1. **Pre-allocation Strategies**

Allocate collections with known or estimated capacity:

```rust
// ✅ GOOD: Pre-allocate with capacity
pub fn build_result_map(&self, entries: &[Entry]) -> HashMap<String, Value> {
    let mut result = HashMap::with_capacity(entries.len());

    for entry in entries {
        result.insert(entry.id().to_string(), entry.compute_value());
    }

    result
}

// ✅ GOOD: String building with capacity
pub fn create_cache_key(entry_id: &ID, subtree: &str) -> String {
    let mut key = String::with_capacity(5 + entry_id.len() + 1 + subtree.len());
    key.push_str("crdt:");
    key.push_str(entry_id);
    key.push(':');
    key.push_str(subtree);
    key
}
```

### 2. **Memory-Efficient Data Structures**

Choose appropriate data structures for access patterns:

```rust
// ✅ GOOD: BTreeMap for ordered iteration and range queries
pub struct OrderedCache {
    entries: BTreeMap<ID, Entry>,  // Maintains order
}

impl OrderedCache {
    pub fn get_range(&self, start: &ID, end: &ID) -> impl Iterator<Item = &Entry> {
        self.entries.range(start..=end).map(|(_, entry)| entry)
    }
}

// ✅ GOOD: HashMap for fast lookup
pub struct FastLookupCache {
    entries: HashMap<ID, Entry>,  // O(1) average lookup
}

// ✅ GOOD: Vec for dense, indexed access
pub struct IndexedEntries {
    entries: Vec<Entry>,          // Cache-friendly, dense storage
    index_map: HashMap<ID, usize>, // ID -> index mapping
}
```

### 3. **Avoiding Unnecessary Clones**

Use references and borrowing effectively:

```rust
// ✅ GOOD: Work with references when possible
pub fn process_entries(&self, processor: impl Fn(&Entry) -> bool) -> Vec<&Entry> {
    self.entries
        .values()
        .filter(|entry| processor(entry))  // No cloning
        .collect()
}

// ✅ GOOD: Clone only when necessary for ownership
pub fn extract_matching_entries(&self, predicate: impl Fn(&Entry) -> bool) -> Vec<Entry> {
    self.entries
        .values()
        .filter(|entry| predicate(entry))
        .cloned()  // Clone only the matches
        .collect()
}

// ❌ BAD: Unnecessary cloning
pub fn inefficient_process(&self) -> Vec<Entry> {
    self.entries
        .values()
        .cloned()  // Clones everything upfront
        .filter(|entry| expensive_predicate(entry))
        .collect()
}
```

## CRDT Performance Patterns

### 1. **State Computation Caching**

Cache expensive CRDT state computations:

```rust
pub struct CachedCRDTState {
    cache: HashMap<(ID, String), Arc<Map>>,  // (entry_id, subtree_name) -> state
}

impl CachedCRDTState {
    pub fn get_or_compute(&mut self, entry_id: &ID, subtree_name: &str) -> Arc<Map> {
        let cache_key = (entry_id.clone(), subtree_name.to_string());

        if let Some(cached) = self.cache.get(&cache_key) {
            return cached.clone();  // Return cached result
        }

        // Compute expensive state
        let state = self.compute_crdt_state(entry_id, subtree_name);
        let arc_state = Arc::new(state);

        self.cache.insert(cache_key, arc_state.clone());
        arc_state
    }

    fn compute_crdt_state(&self, entry_id: &ID, subtree_name: &str) -> Map {
        // Expensive computation here
        todo!("Implement LCA-based state computation")
    }
}
```

### 2. **Efficient Merge Operations**

Optimize CRDT merge algorithms:

```rust
impl Map {
    /// Optimized merge that minimizes allocations
    pub fn merge_optimized(&mut self, other: &Map) {
        // Pre-allocate for expected merge size
        let estimated_size = self.children.len() + other.children.len();
        if self.children.capacity() < estimated_size {
            self.children.reserve(estimated_size - self.children.len());
        }

        for (key, other_value) in &other.children {
            match self.children.get_mut(key) {
                Some(existing_value) => {
                    // In-place merge when possible
                    existing_value.merge_in_place(other_value);
                }
                None => {
                    // Clone only when adding new keys
                    self.children.insert(key.clone(), other_value.clone());
                }
            }
        }
    }
}
```

### 3. **Lazy Computation Patterns**

Defer expensive computations until needed:

```rust
pub struct LazyComputedState {
    entry_id: ID,
    subtree_name: String,
    computed_state: OnceCell<Map>,  // Compute only once
}

impl LazyComputedState {
    pub fn get_state(&self) -> &Map {
        self.computed_state.get_or_init(|| {
            // Expensive computation happens only when first accessed
            self.compute_full_state()
        })
    }

    fn compute_full_state(&self) -> Map {
        // Expensive state computation
        todo!("Implement state computation")
    }
}
```

## Backend Performance Patterns

### 1. **Batch Operations**

Optimize backend operations for bulk access:

```rust
pub trait Database {
    /// Efficient batch retrieval
    fn get_multiple(&self, ids: &[ID]) -> Result<Vec<Option<Entry>>> {
        // Default implementation - backends can override for optimization
        ids.iter()
            .map(|id| self.get(id))
            .collect()
    }

    /// Efficient batch storage
    fn store_multiple(&mut self, entries: &[Entry]) -> Result<()> {
        // Default implementation - backends can override for optimization
        for entry in entries {
            self.store(entry)?;
        }
        Ok(())
    }
}

// Optimized implementation example
impl Database for OptimizedBackend {
    fn get_multiple(&self, ids: &[ID]) -> Result<Vec<Option<Entry>>> {
        // Use backend-specific bulk operations (e.g., mget in Redis)
        self.bulk_retrieve(ids)
    }
}
```

### 2. **Connection Pooling and Resource Management**

Manage expensive resources efficiently:

```rust
pub struct PooledBackend {
    connection_pool: Pool<Connection>,
    read_cache: LruCache<ID, Entry>,
}

impl PooledBackend {
    pub fn get_with_cache(&mut self, id: &ID) -> Result<Entry> {
        // Check cache first
        if let Some(entry) = self.read_cache.get(id) {
            return Ok(entry.clone());
        }

        // Use pooled connection for database access
        let conn = self.connection_pool.get()?;
        let entry = conn.retrieve(id)?;

        // Cache for future access
        self.read_cache.put(id.clone(), entry.clone());

        Ok(entry)
    }
}
```

## Algorithm Optimization

### 1. **Direct Loops vs Iterator Chains**

Prefer direct loops in hot paths:

```rust
// ✅ GOOD: Direct loop for hot path
pub fn find_matching_entries_fast(&self, predicate: impl Fn(&Entry) -> bool) -> Vec<ID> {
    let mut result = Vec::new();

    for (id, entry) in &self.entries {
        if predicate(entry) {
            result.push(id.clone());
        }
    }

    result
}

// ❌ LESS EFFICIENT: Complex iterator chain in hot path
pub fn find_matching_entries_slow(&self, predicate: impl Fn(&Entry) -> bool) -> Vec<ID> {
    self.entries
        .iter()
        .filter(|(_, entry)| predicate(*entry))
        .map(|(id, _)| id.clone())
        .collect()
}
```

### 2. **Efficient Graph Traversal**

Optimize tree and DAG traversal algorithms:

```rust
pub struct EfficientTraversal {
    visited: HashSet<ID>,
    stack: Vec<ID>,
}

impl EfficientTraversal {
    pub fn find_lca(&mut self, entries: &[ID], backend: &dyn Database) -> Result<Option<ID>> {
        self.visited.clear();
        self.stack.clear();

        // Use iterative traversal to avoid stack overflow
        for entry_id in entries {
            self.stack.push(entry_id.clone());
        }

        while let Some(current_id) = self.stack.pop() {
            if !self.visited.insert(current_id.clone()) {
                continue;  // Already visited
            }

            let entry = backend.get(&current_id)?;
            for parent_id in entry.parents() {
                if self.visited.contains(parent_id) {
                    return Ok(Some(parent_id.clone()));  // Found LCA
                }
                self.stack.push(parent_id.clone());
            }
        }

        Ok(None)
    }
}
```

## Profiling and Measurement

### 1. **Benchmark-Driven Development**

Use criterion for performance testing:

```rust
// benches/performance.rs
use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};

fn benchmark_crdt_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("crdt_operations");

    for size in [100, 1_000, 10_000].iter() {
        group.bench_with_input(
            BenchmarkId::new("map_merge", size),
            size,
            |b, &size| {
                let map1 = create_test_map(size);
                let map2 = create_test_map(size);

                b.iter(|| {
                    let mut result = map1.clone();
                    result.merge(&map2);
                    result
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, benchmark_crdt_operations);
criterion_main!(benches);
```

### 2. **Performance Monitoring**

Add performance monitoring to critical paths:

```rust
use std::time::Instant;

pub struct PerformanceMetrics {
    operation_times: HashMap<String, Vec<Duration>>,
}

impl PerformanceMetrics {
    pub fn time_operation<T>(&mut self, operation_name: &str, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let result = f();
        let duration = start.elapsed();

        self.operation_times
            .entry(operation_name.to_string())
            .or_default()
            .push(duration);

        result
    }

    pub fn get_average_time(&self, operation_name: &str) -> Option<Duration> {
        let times = self.operation_times.get(operation_name)?;
        if times.is_empty() {
            return None;
        }

        let total: Duration = times.iter().sum();
        Some(total / times.len() as u32)
    }
}
```

## Memory Profiling

### 1. **Memory Usage Tracking**

Track memory allocation patterns:

```rust
#[cfg(feature = "memory_profiling")]
pub struct MemoryTracker {
    allocations: HashMap<String, usize>,
}

#[cfg(feature = "memory_profiling")]
impl MemoryTracker {
    pub fn track_allocation<T>(&mut self, operation: &str, f: impl FnOnce() -> T) -> T {
        let before = get_memory_usage();
        let result = f();
        let after = get_memory_usage();

        let allocated = after.saturating_sub(before);
        *self.allocations.entry(operation.to_string()).or_default() += allocated;

        result
    }
}

// Platform-specific memory usage function
#[cfg(target_os = "linux")]
fn get_memory_usage() -> usize {
    // Read from /proc/self/status or use system calls
    todo!("Implement Linux memory usage tracking")
}
```

### 2. **Memory-Efficient Collections**

Use appropriate collection types for memory efficiency:

```rust
// For small collections, Vec might be more efficient than HashMap
pub enum SmallMap<K, V> {
    Vec(Vec<(K, V)>),      // For <= 8 items
    HashMap(HashMap<K, V>), // For > 8 items
}

impl<K: Eq + Hash, V> SmallMap<K, V> {
    pub fn insert(&mut self, key: K, value: V) {
        match self {
            Self::Vec(vec) => {
                if vec.len() >= 8 {
                    // Convert to HashMap when growing large
                    let mut map = HashMap::with_capacity(vec.len() + 1);
                    for (k, v) in vec.drain(..) {
                        map.insert(k, v);
                    }
                    map.insert(key, value);
                    *self = Self::HashMap(map);
                } else {
                    vec.push((key, value));
                }
            }
            Self::HashMap(map) => {
                map.insert(key, value);
            }
        }
    }
}
```

## Common Performance Anti-Patterns

### ❌ **Unnecessary String Allocations**

```rust
// DON'T DO THIS
pub fn inefficient_string_ops(&self) -> String {
    let mut result = String::new();
    for item in &self.items {
        result = result + &item.to_string();  // Inefficient concatenation
    }
    result
}
```

### ❌ **Repeated Expensive Computations**

```rust
// DON'T DO THIS
pub fn repeated_computation(&self) -> Vec<ProcessedItem> {
    self.items
        .iter()
        .map(|item| {
            let expensive_result = expensive_computation(item);  // Computed every time
            ProcessedItem::new(expensive_result)
        })
        .collect()
}
```

### ❌ **Memory Leaks with Caching**

```rust
// DON'T DO THIS - unbounded cache growth
pub struct LeakyCache {
    cache: HashMap<String, LargeData>,  // Never cleaned up
}
```

### ✅ **Correct Performance Patterns**

```rust
// DO THIS
pub fn efficient_string_building(&self) -> String {
    let total_len: usize = self.items.iter().map(|item| item.len()).sum();
    let mut result = String::with_capacity(total_len);

    for item in &self.items {
        result.push_str(&item.to_string());
    }

    result
}

// DO THIS
pub struct BoundedCache {
    cache: LruCache<String, ProcessedData>,  // Bounded size
    max_size: usize,
}
```

## Future Performance Improvements

### Planned Optimizations

- **TODO**: Implement zero-copy serialization for entry data
- **TODO**: Add SIMD optimizations for bulk operations where applicable
- **TODO**: Investigate async I/O patterns for backend operations
- **TODO**: Consider memory mapping for large entry storage
- **TODO**: Evaluate compression for stored entry data

### Performance Monitoring Evolution

- **TODO**: Implement distributed tracing for performance analysis
- **TODO**: Add automatic performance regression detection
- **TODO**: Create performance dashboards and alerting
- **TODO**: Design adaptive caching strategies based on access patterns

## Summary

Effective performance optimization in Eidetica focuses on:

- **String parameter optimization** with appropriate type selection
- **Memory-efficient patterns** with pre-allocation and appropriate data structures
- **Hot path optimization** through profiling and measurement
- **CRDT performance** with caching and efficient merge operations
- **Backend optimization** with batch operations and resource pooling
- **Algorithm efficiency** with appropriate complexity and data structure choices

Following these patterns ensures the system maintains good performance characteristics as it scales.
