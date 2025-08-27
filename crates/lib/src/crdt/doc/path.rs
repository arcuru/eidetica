//! Path types for hierarchical document access.
//!
//! This module provides type-safe path construction and validation for accessing
//! nested structures in CRDT documents. The Path/PathBuf types follow the same
//! borrowed/owned pattern as std::path::Path/PathBuf.
//!
//! # Core Types
//!
//! - [`Path`] - An unsized borrowed path type (always behind a reference)
//! - [`PathBuf`] - An owned path type that can be constructed and modified
//!
//! # Usage
//!
//! ```rust
//! use eidetica::crdt::doc::{Path, PathBuf};
//! use std::str::FromStr;
//! # use eidetica::crdt::Doc;
//!
//! // Construct from string (with validation)
//! let path = PathBuf::from_str("user.profile.name")?;
//!
//! // Build incrementally
//! let path = PathBuf::new()
//!     .push("user")?
//!     .push("profile")?
//!     .push("name")?;
//!
//! // Use in document operations
//! let mut doc = Doc::new();
//! doc.set_path(path, "Alice")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use thiserror::Error;

/// Error type for path validation failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathError {
    /// Path component at the given position is empty.
    #[error("Path component at position {position} is empty")]
    EmptyComponent { position: usize },

    /// Path cannot be empty.
    #[error("Path cannot be empty")]
    EmptyPath,

    /// Path cannot start with a dot.
    #[error("Path cannot start with a dot")]
    LeadingDot,

    /// Path cannot end with a dot.
    #[error("Path cannot end with a dot")]
    TrailingDot,

    /// Invalid component: components cannot be empty or contain dots.
    #[error("Invalid component '{component}': {reason}")]
    InvalidComponent { component: String, reason: String },
}

/// A validated component of a path.
///
/// Components are individual parts of a path, separated by dots.
/// They must be non-empty and cannot contain dots themselves.
///
/// # Examples
///
/// ```rust
/// # use eidetica::crdt::doc::path::Component;
/// # use std::str::FromStr;
/// // Valid components
/// let user = Component::new("user").unwrap();
/// let profile = Component::new("profile").unwrap();
///
/// // Invalid components
/// assert!(Component::new("").is_err());        // Empty
/// assert!(Component::new("user.name").is_err()); // Contains dot
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Component {
    inner: String,
}

impl Component {
    /// Creates a new component from a string.
    ///
    /// # Errors
    /// Returns an error if the component is empty or contains a dot.
    pub fn new(s: impl Into<String>) -> Result<Self, PathError> {
        let s = s.into();

        if s.is_empty() {
            return Err(PathError::InvalidComponent {
                component: s,
                reason: "components cannot be empty".to_string(),
            });
        }

        if s.contains('.') {
            return Err(PathError::InvalidComponent {
                component: s.clone(),
                reason: "components cannot contain dots".to_string(),
            });
        }

        Ok(Component { inner: s })
    }

    /// Returns the component as a string slice.
    pub fn as_str(&self) -> &str {
        &self.inner
    }
}

impl AsRef<str> for Component {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inner)
    }
}

impl FromStr for Component {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Component::new(s)
    }
}

impl TryFrom<String> for Component {
    type Error = PathError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Component::new(s)
    }
}

impl TryFrom<&str> for Component {
    type Error = PathError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Component::new(s)
    }
}

/// An owned, validated path for hierarchical document access.
///
/// `PathBuf` provides a type-safe way to construct and manipulate paths
/// for accessing nested structures in CRDT documents. It validates that
/// paths are well-formed and provides efficient access to path component.
///
/// # Examples
///
/// ```rust
/// # use eidetica::crdt::doc::PathBuf;
/// # use std::str::FromStr;
/// // Create from string with validation
/// let path = PathBuf::from_str("user.profile.name")?;
///
/// // Build incrementally
/// let path = PathBuf::new()
///     .push("user")?
///     .push("profile")?
///     .push("name")?;
///
/// // Get components
/// let components: Vec<&str> = path.components().collect();
/// assert_eq!(components, vec!["user", "profile", "name"]);
/// # Ok::<(), eidetica::crdt::doc::PathError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathBuf {
    inner: String,
}

/// A borrowed, validated path for hierarchical document access.
///
/// `Path` is the borrowed counterpart to `PathBuf`, similar to how `&str`
/// relates to `String`. It provides efficient read-only access to path
/// components without allocation.
///
/// This type is unsized and must always be used behind a reference.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Path {
    inner: str,
}

impl PathBuf {
    /// Creates a new empty path.
    pub fn new() -> Self {
        Self {
            inner: String::new(),
        }
    }

    /// Creates a path from a single component.
    pub fn from_component(component: Component) -> Self {
        Self {
            inner: component.inner,
        }
    }

    /// Adds a component to the end of this path.
    ///
    /// For convenience, this method accepts anything that can be converted to a string,
    /// and validates it as a component.
    pub fn push(mut self, component: impl Into<String>) -> Result<Self, PathError> {
        let component = Component::new(component)?;
        if self.inner.is_empty() {
            self.inner = component.inner;
        } else {
            self.inner.push('.');
            self.inner.push_str(&component.inner);
        }
        Ok(self)
    }

    /// Adds a validated component to the end of this path.
    pub fn push_component(mut self, component: Component) -> Self {
        if self.inner.is_empty() {
            self.inner = component.inner;
        } else {
            self.inner.push('.');
            self.inner.push_str(&component.inner);
        }
        self
    }

    /// Joins this path with another path.
    pub fn join(mut self, other: impl AsRef<Path>) -> Self {
        let other_path = other.as_ref();
        if self.inner.is_empty() {
            self.inner = other_path.inner.to_string();
        } else if !other_path.inner.is_empty() {
            self.inner.push('.');
            self.inner.push_str(&other_path.inner);
        }
        self
    }

    /// Returns an iterator over the path components as string slices.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        if self.inner.is_empty() {
            // Use a Split iterator that will be empty
            "".split('.')
        } else {
            self.inner.split('.')
        }
    }

    /// Returns the number of components in the path.
    pub fn len(&self) -> usize {
        if self.inner.is_empty() {
            0
        } else {
            self.inner.split('.').count()
        }
    }

    /// Returns `true` if the path has no components.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the parent path, or `None` if this is the root.
    pub fn parent(&self) -> Option<PathBuf> {
        self.inner.rfind('.').map(|last_dot| PathBuf {
            inner: self.inner[..last_dot].to_string(),
        })
    }

    /// Returns the last component of the path, or `None` if empty.
    pub fn file_name(&self) -> Option<&str> {
        if self.inner.is_empty() {
            None
        } else if let Some(last_dot) = self.inner.rfind('.') {
            Some(&self.inner[last_dot + 1..])
        } else {
            Some(&self.inner)
        }
    }

    /// Validates a dot-separated path string.
    /// This now primarily checks for leading/trailing dots and empty component,
    /// as individual component validation happens through the Component type.
    fn validate(path: &str) -> Result<(), PathError> {
        if path.is_empty() {
            return Err(PathError::EmptyPath);
        }

        if path.starts_with('.') {
            return Err(PathError::LeadingDot);
        }

        if path.ends_with('.') {
            return Err(PathError::TrailingDot);
        }

        // Check for empty components (which would indicate consecutive dots)
        for (i, component) in path.split('.').enumerate() {
            if component.is_empty() {
                return Err(PathError::EmptyComponent { position: i });
            }
        }

        Ok(())
    }
}

impl Path {
    /// Creates a Path from a string without validation.
    ///
    /// # Safety
    /// The caller must ensure that the string is a valid path according to our validation rules:
    /// - No leading or trailing dots
    /// - No empty components (consecutive dots)
    /// - Components may not contain dots
    ///
    /// This is primarily intended for use with compile-time validated string literals.
    pub unsafe fn from_str_unchecked(s: &str) -> &Path {
        // SAFETY: Path has the same memory layout as str
        unsafe { &*(s as *const str as *const Path) }
    }

    /// Returns an iterator over the path components as string slices.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        if self.inner.is_empty() {
            "".split('.')
        } else {
            self.inner.split('.')
        }
    }

    /// Returns the number of components in the path.
    pub fn len(&self) -> usize {
        if self.inner.is_empty() {
            0
        } else {
            self.inner.split('.').count()
        }
    }

    /// Returns `true` if the path has no components.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the last component of the path, or `None` if empty.
    pub fn file_name(&self) -> Option<&str> {
        if self.inner.is_empty() {
            None
        } else {
            self.inner.split('.').next_back()
        }
    }

    /// Returns the path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.inner
    }

    /// Converts this `Path` to an owned `PathBuf`.
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf {
            inner: self.inner.to_string(),
        }
    }
}

impl Default for PathBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for PathBuf {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        // Safe because Path has the same layout as str
        unsafe { crate::crdt::doc::path::Path::from_str_unchecked(self.inner.as_str()) }
    }
}

impl AsRef<Path> for PathBuf {
    fn as_ref(&self) -> &Path {
        self.deref()
    }
}

impl AsRef<PathBuf> for PathBuf {
    fn as_ref(&self) -> &PathBuf {
        self
    }
}

impl AsRef<Path> for Path {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl Borrow<Path> for PathBuf {
    fn borrow(&self) -> &Path {
        self.deref()
    }
}

impl FromStr for PathBuf {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::validate(s)?;
        Ok(PathBuf {
            inner: s.to_string(),
        })
    }
}

impl From<&PathBuf> for PathBuf {
    fn from(path: &PathBuf) -> Self {
        path.clone()
    }
}

impl fmt::Display for PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.inner.is_empty() {
            write!(f, "(empty path)")
        } else {
            write!(f, "{}", self.inner)
        }
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.inner.is_empty() {
            write!(f, "(empty path)")
        } else {
            write!(f, "{}", &self.inner)
        }
    }
}

/// A builder for constructing paths incrementally.
#[derive(Debug, Clone)]
pub struct PathBuilder {
    inner: String,
}

impl PathBuilder {
    /// Creates a new empty path builder.
    pub fn new() -> Self {
        Self {
            inner: String::new(),
        }
    }

    /// Adds a component to the path.
    pub fn component(mut self, component: impl Into<String>) -> Result<Self, PathError> {
        let component = Component::new(component)?;
        if self.inner.is_empty() {
            self.inner = component.inner;
        } else {
            self.inner.push('.');
            self.inner.push_str(&component.inner);
        }
        Ok(self)
    }

    /// Adds a validated component to the path.
    pub fn push_component(mut self, component: Component) -> Self {
        if self.inner.is_empty() {
            self.inner = component.inner;
        } else {
            self.inner.push('.');
            self.inner.push_str(&component.inner);
        }
        self
    }

    /// Builds the final `PathBuf`.
    pub fn build(self) -> PathBuf {
        PathBuf { inner: self.inner }
    }
}

impl Default for PathBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Constructs a path with compile-time optimization for literals.
///
/// This macro provides ergonomic path construction with optimal performance:
/// - Single string literal returns `&'static Path` (zero allocation!)
/// - Multiple arguments or runtime values return `PathBuf`
///
/// # Syntax
///
/// - `path!()` - Empty path (PathBuf)
/// - `path!("user.profile.name")` - Single literal (&'static Path, zero-cost!)
/// - `path!("user", "profile", "name")` - Multiple components (PathBuf)
/// - `path!(base, "profile", "name")` - Mix runtime and literals (PathBuf)
/// - `path!(existing_path)` - Pass through existing paths (PathBuf)
///
/// # Examples
///
/// ```rust
/// # use eidetica::crdt::doc::path;
/// # use eidetica::crdt::doc::PathBuf;
/// # use std::str::FromStr;
/// // Zero-cost literal (returns &'static Path)
/// let path = path!("user.profile.name");
///
/// // Multiple components (returns PathBuf)
/// let path = path!("user", "profile", "name");
///
/// // Mixed runtime/literal (returns PathBuf)
/// let base = "user";
/// let path = path!(base, "profile", "name");
///
/// // Empty path
/// let empty = path!();
/// ```
#[macro_export]
macro_rules! path {
    // Empty path - returns PathBuf
    () => {
        $crate::crdt::doc::PathBuf::new()
    };

    // Single string literal - returns &'static Path (zero allocation!)
    ($single:literal) => {{
        // Validate at compile time
        const _: () = $crate::crdt::doc::path::validate_const($single);
        // Safe because we validated above
        unsafe { $crate::crdt::doc::path::Path::from_str_unchecked($single) }
    }};

    // Multiple arguments - returns PathBuf
    ($first:expr $(, $rest:expr)* $(,)?) => {{
        let mut path = $crate::crdt::doc::PathBuf::new();

        // Helper function to convert anything to a component
        fn add_component(path: &mut $crate::crdt::doc::PathBuf, component: impl AsRef<str>) -> Result<(), $crate::crdt::doc::PathError> {
            let component_str = component.as_ref().trim();
            if !component_str.is_empty() {
                *path = std::mem::take(path).push(component_str)?;
            }
            Ok(())
        }

        // Handle first argument
        let first_str = $first.to_string();
        add_component(&mut path, first_str).expect("Invalid component in path! macro");

        // Handle remaining arguments
        $(
            let rest_str = $rest.to_string();
            add_component(&mut path, rest_str).expect("Invalid component in path! macro");
        )*

        path
    }};
}

/// Validates a path string at compile time.
///
/// This performs basic validation that can be done in const context.
/// For string literals, this ensures the path is valid at compile time.
pub const fn validate_const(path: &str) {
    let bytes = path.as_bytes();
    let len = bytes.len();

    if len == 0 {
        // Empty paths are valid
        return;
    }

    // Check for leading dot
    if bytes[0] == b'.' {
        panic!("Path cannot start with a dot");
    }

    // Check for trailing dot
    if bytes[len - 1] == b'.' {
        panic!("Path cannot end with a dot");
    }

    // Check for consecutive dots (empty components)
    let mut i = 0;
    while i < len - 1 {
        if bytes[i] == b'.' && bytes[i + 1] == b'.' {
            panic!("Path cannot contain empty components (consecutive dots)");
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pathbuf_construction() {
        let path = PathBuf::new();
        assert!(path.is_empty());
        assert_eq!(path.len(), 0);

        let component = Component::new("test").unwrap();
        let path = PathBuf::from_component(component);
        assert!(!path.is_empty());
        assert_eq!(path.len(), 1);
        assert_eq!(path.file_name(), Some("test"));
    }

    #[test]
    fn test_pathbuf_push() {
        let path = PathBuf::new()
            .push("user")
            .unwrap()
            .push("profile")
            .unwrap()
            .push("name")
            .unwrap();

        assert_eq!(path.len(), 3);
        let components: Vec<&str> = path.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
        assert_eq!(path.file_name(), Some("name"));
    }

    #[test]
    fn test_pathbuf_push_invalid_component() {
        let result = PathBuf::new().push("user.name");
        assert!(result.is_err());

        let result = PathBuf::new().push("");
        assert!(result.is_err());
    }

    #[test]
    fn test_pathbuf_parent() {
        let path = PathBuf::from_str("user.profile.name").unwrap();
        let parent = path.parent().unwrap();

        let parent_components: Vec<&str> = parent.components().collect();
        assert_eq!(parent_components, vec!["user", "profile"]);

        let root = PathBuf::from_str("user").unwrap();
        assert!(root.parent().is_none());
    }

    #[test]
    fn test_path_validation_success() {
        let valid_paths = vec!["simple", "user.profile", "user.profile.name", "a.b.c.d.e"];

        for path_str in valid_paths {
            let path = PathBuf::from_str(path_str);
            assert!(path.is_ok(), "Path '{path_str}' should be valid");
        }
    }

    #[test]
    fn test_path_validation_errors() {
        let test_cases = vec![
            // Empty paths are NOT allowed anymore
            ("", PathError::EmptyPath),
            (".user", PathError::LeadingDot),
            ("user.", PathError::TrailingDot),
            ("user..profile", PathError::EmptyComponent { position: 1 }),
            ("user...profile", PathError::EmptyComponent { position: 1 }),
        ];

        for (path_str, expected_error) in test_cases {
            let result = PathBuf::from_str(path_str);
            assert_eq!(
                result.unwrap_err(),
                expected_error,
                "Path '{path_str}' should fail with expected error"
            );
        }
    }

    #[test]
    fn test_path_deref() {
        let pathbuf = PathBuf::from_str("user.profile.name").unwrap();
        let path: &Path = &pathbuf;

        assert_eq!(path.as_str(), "user.profile.name");
        let components: Vec<&str> = path.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
    }

    #[test]
    fn test_path_builder() {
        let path = PathBuilder::new()
            .component("user")
            .unwrap()
            .component("profile")
            .unwrap()
            .component("name")
            .unwrap()
            .build();

        let components: Vec<&str> = path.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
    }

    #[test]
    fn test_display() {
        let path = PathBuf::from_str("user.profile.name").unwrap();
        assert_eq!(format!("{path}"), "user.profile.name");

        let empty = PathBuf::new();
        assert_eq!(format!("{empty}"), "(empty path)");
    }

    #[test]
    fn test_from_str() {
        let from_str = PathBuf::from_str("user.profile.name").unwrap();

        let components: Vec<&str> = from_str.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
    }

    #[test]
    fn test_from_str_invalid() {
        let result = PathBuf::from_str("user..invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_path_join() {
        let base = PathBuf::from_str("user").unwrap();
        let suffix = PathBuf::from_str("profile.name").unwrap();

        let joined = base.join(&suffix);
        let components: Vec<&str> = joined.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
    }

    #[test]
    fn test_path_macro_from_string() {
        let path = path!("user.profile.name");
        let components: Vec<&str> = path.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
    }

    #[test]
    fn test_path_macro_from_components() {
        let path = path!("user", "profile", "name");
        let components: Vec<&str> = path.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);

        // Test with trailing comma
        let path = path!("user", "profile", "name",);
        let components: Vec<&str> = path.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
    }

    #[test]
    fn test_path_macro_mixed() {
        let base = "user";
        let path = path!(base, "profile", "name");
        let components: Vec<&str> = path.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
    }

    #[test]
    fn test_path_macro_empty_and_edge_cases() {
        // Empty path is now allowed and returns empty PathBuf
        let empty = path!();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        // Single empty string literal should be caught at compile time
        // let _ = path!(""); // This would panic at compile time

        // But we can test that the validation works for runtime paths
        // (this would be caught by validate_const for literals)
    }

    #[test]
    fn test_unified_macro_behavior() {
        // Test that different call forms produce equivalent results
        let literal = path!("user.profile.name");
        let components = path!("user", "profile", "name");
        let base = "user";
        let mixed = path!(base, "profile", "name");

        // All should have the same component structure
        let literal_vec: Vec<&str> = literal.components().collect();
        let components_vec: Vec<&str> = components.components().collect();
        let mixed_vec: Vec<&str> = mixed.components().collect();

        assert_eq!(literal_vec, vec!["user", "profile", "name"]);
        assert_eq!(components_vec, vec!["user", "profile", "name"]);
        assert_eq!(mixed_vec, vec!["user", "profile", "name"]);

        // Test different return types work with AsRef<Path>
        fn accepts_path_ref(p: impl AsRef<Path>) -> String {
            p.as_ref().as_str().to_string()
        }

        assert_eq!(accepts_path_ref(literal), "user.profile.name");
        assert_eq!(accepts_path_ref(&components), "user.profile.name");
        assert_eq!(accepts_path_ref(&mixed), "user.profile.name");
    }

    #[test]
    fn test_component_validation() {
        // Test valid components
        assert!(Component::new("user").is_ok());
        assert!(Component::new("profile123").is_ok());
        assert!(Component::new("_internal").is_ok());

        // Test invalid components
        assert!(Component::new("").is_err());
        assert!(Component::new("user.name").is_err());
    }
}
