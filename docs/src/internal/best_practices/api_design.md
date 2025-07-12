# API Design Patterns

This document outlines established patterns for API design within the Eidetica codebase, with particular emphasis on string parameter handling, conversion patterns, and performance considerations.

## String Parameter Guidelines

One of the most important API design decisions in Rust is choosing the right parameter types for string data. Eidetica follows specific patterns to optimize performance while maintaining ergonomic APIs.

### Core Principle: Storage vs Lookup Pattern

The fundamental rule for string parameters in Eidetica:

- **Use `Into<String>`** for parameters that will be **stored** (converted to owned `String`)
- **Use `AsRef<str>`** for parameters that are only **accessed** temporarily (lookup, comparison)

### When to Use `Into<String>`

Use `impl Into<String>` when the function will store the parameter as an owned `String`:

```rust
// ✅ GOOD: Parameter is stored as String
pub fn set_default_auth_key(&mut self, key_id: impl Into<String>) {
    self.default_auth_key = Some(key_id.into());
}

// ✅ GOOD: Parameter becomes part of stored data
impl SubTree for Dict {
    fn new(op: &AtomicOp, subtree_name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            name: subtree_name.into(), // Stored as String
            atomic_op: op.clone(),
        })
    }
}
```

**Benefits:**

- Avoids double conversion (`AsRef<str>` → `&str` → `String`)
- Still accepts `&str`, `String`, and `&String` transparently
- More efficient for storage operations

### When to Use `AsRef<str>`

Use `impl AsRef<str>` when the function only needs to read the string temporarily:

```rust
// ✅ GOOD: Parameter is only used for lookup/comparison
pub fn get(&self, key: impl AsRef<str>) -> Option<&Value> {
    self.children.get(key.as_ref()) // Only accessed, not stored
}

// ✅ GOOD: Parameter used for validation/checking
pub fn in_subtree(&self, subtree_name: impl AsRef<str>) -> bool {
    self.subtrees.iter().any(|node| node.name == subtree_name.as_ref())
}
```

**Benefits:**

- Maximum flexibility for callers
- No unnecessary allocations
- Clear intent that parameter is not stored

### Anti-Patterns to Avoid

❌ **BAD: `AsRef<str>` followed by immediate `.to_string()`**

```rust
// DON'T DO THIS
pub fn set_key(&mut self, key_id: impl AsRef<str>) {
    self.key = key_id.as_ref().to_string(); // Double conversion!
}
```

✅ **GOOD: Use `Into<String>` instead**

```rust
pub fn set_key(&mut self, key_id: impl Into<String>) {
    self.key = key_id.into(); // Direct conversion
}
```

## Common Conversion Patterns

### ID Types

For ID parameters, prefer `Into<ID>` when working with ID-typed fields:

```rust
// ✅ GOOD: Clear intent and type safety
pub fn get_entry(&self, entry_id: impl Into<ID>) -> Result<Entry> {
    let id = entry_id.into();
    self.backend.get(&id)
}
```

### Path Segments

For path operations, use `Into<String>` with `Clone` bounds when segments will be stored:

```rust
// ✅ GOOD: Path segments that will be stored as keys
pub fn set_at_path<S, P>(&self, path: P, value: Value) -> Result<()>
where
    S: Into<String> + Clone,
    P: AsRef<[S]>,
{
    for segment in path.as_ref() {
        let key = segment.clone().into(); // Stored as String key
        // ... use key
    }
}
```

## Performance Guidelines

### Hot Path Optimizations

For performance-critical operations:

1. **Bulk Operations**: Convert all parameters upfront to avoid per-iteration conversions

```rust
// ✅ GOOD: Convert once, use many times
pub fn get_entries<I, T>(&self, entry_ids: I) -> Result<Vec<Entry>>
where
    I: IntoIterator<Item = T>,
    T: Into<ID>,
{
    let ids: Vec<ID> = entry_ids.into_iter().map(Into::into).collect();
    let mut entries = Vec::with_capacity(ids.len());

    for id in ids { // No conversion overhead in loop
        entries.push(self.backend.get(&id)?);
    }

    Ok(entries)
}
```

2. **Iterator Chains**: Prefer direct loops over complex iterator chains in hot paths

```rust
// ✅ GOOD: Direct loop for hot path
let mut result = Vec::new();
for (id, entry) in entries.iter() {
    if entry.in_tree(tree) {
        result.push(id.clone());
    }
}

// ❌ LESS EFFICIENT: Complex iterator chain
let result: Vec<_> = entries
    .keys()
    .filter(|&id| entries.get(id).is_some_and(|entry| entry.in_tree(tree)))
    .cloned()
    .collect();
```

## API Documentation Standards

### Parameter Documentation

Always document the expected usage pattern for string parameters:

````rust
/// Sets the default authentication key for this tree.
///
/// # Parameters
/// * `key_id` - Authentication key identifier that will be stored.
///   Accepts any string type (`&str`, `String`, `&String`).
///
/// # Example
/// ```rust
/// tree.set_default_auth_key("my_key");           // &str
/// tree.set_default_auth_key(key_string);         // String
/// tree.set_default_auth_key(&owned_string);      // &String
/// ```
pub fn set_default_auth_key(&mut self, key_id: impl Into<String>) {
    self.default_auth_key = Some(key_id.into());
}
````

## Testing Patterns

### Conversion Compatibility Tests

Test that APIs work with all string types:

```rust
#[test]
fn test_string_parameter_compatibility() {
    let mut dict = create_test_dict();

    // Test all string types work
    dict.set("key1", value1);                    // &str
    dict.set(String::from("key2"), value2);      // String
    dict.set(&String::from("key3"), value3);     // &String

    // All should work identically
    assert!(dict.contains_key("key1"));
    assert!(dict.contains_key("key2"));
    assert!(dict.contains_key("key3"));
}
```

## API Evolution Guidelines

During development, APIs can be freely changed to follow best practices:

1. **Update existing methods** directly with improved parameter types
2. **Add comprehensive tests** to cover new parameter types
3. **Update documentation** to reflect the changes
4. **Consider performance impact** of the changes

Note: Backward compatibility is not required during the development phase. Breaking changes are acceptable when they improve performance, ergonomics, or consistency.

## Summary

Following these patterns ensures:

- **Optimal performance** through minimal conversions
- **Consistent APIs** across the codebase
- **Clear intent** about parameter usage
- **Maximum flexibility** for API consumers
- **Maintainable code** for future development

When in doubt, ask: "Is this parameter stored or just accessed?" The answer determines whether to use `Into<String>` or `AsRef<str>`.
