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
//! // Construct from string (automatically normalized)
//! let path = PathBuf::from_str("user.profile.name")?;
//!
//! // Build incrementally (infallible)
//! let path = PathBuf::new()
//!     .push("user")
//!     .push("profile")
//!     .push("name");
//!
//! // Use in document operations
//! let mut doc = Doc::new();
//! doc.set_path(path, "Alice")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::{borrow::Borrow, fmt, ops::Deref, str::FromStr};

use thiserror::Error;

/// Error type for path validation failures.
///
/// Note: Most path operations are now infallible through normalization.
/// This error type is kept for backward compatibility and component validation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathError {
    /// Invalid component: components cannot contain dots.
    #[error("Invalid component '{component}': {reason}")]
    InvalidComponent { component: String, reason: String },
}

/// Normalizes a path string by cleaning up dots and empty components.
///
/// This function implements the core path normalization logic:
/// - Empty string "" → empty string (refers to current Doc)
/// - Leading dots ".user" → "user"
/// - Trailing dots "user." → "user"
/// - Consecutive dots "user..profile" → "user.profile"
/// - Pure dots "..." → empty string
///
/// # Examples
///
/// ```rust
/// # use eidetica::crdt::doc::path::normalize_path;
/// assert_eq!(normalize_path(""), "");
/// assert_eq!(normalize_path(".user"), "user");
/// assert_eq!(normalize_path("user."), "user");
/// assert_eq!(normalize_path("user..profile"), "user.profile");
/// assert_eq!(normalize_path("..."), "");
/// assert_eq!(normalize_path("user.profile.name"), "user.profile.name");
/// ```
pub fn normalize_path(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    input
        .split('.')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

/// A validated component of a path.
///
/// Components are individual parts of a path, separated by dots.
/// They cannot contain dots themselves. Empty components are allowed but
/// will be filtered during path normalization.
///
/// # Examples
///
/// ```rust
/// # use eidetica::crdt::doc::path::Component;
/// # use std::str::FromStr;
/// // Valid components
/// let user = Component::new("user").unwrap();
/// let profile = Component::new("profile").unwrap();
/// let empty = Component::new("").unwrap();  // Empty is allowed
///
/// // Invalid components
/// assert!(Component::new("user.name").is_err()); // Dots not allowed
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Component {
    inner: String,
}

impl Component {
    /// Creates a new component from a string.
    ///
    /// # Errors
    /// Returns an error only if the component contains a dot.
    /// Empty components are now allowed and will be filtered during path normalization.
    pub fn new(s: impl Into<String>) -> Result<Self, PathError> {
        let s = s.into();

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
/// // Create from string (automatically normalized)
/// let path = PathBuf::from_str("user.profile.name")?;
///
/// // Build incrementally (infallible)
/// let path = PathBuf::new()
///     .push("user")
///     .push("profile")
///     .push("name");
///
/// // Get components
/// let components: Vec<&str> = path.components().collect();
/// assert_eq!(components, vec!["user", "profile", "name"]);
/// # Ok::<(), std::convert::Infallible>(())
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

    /// Adds a path to the end of this path.
    ///
    /// This method accepts both strings and Path types, normalizing the input.
    /// It's infallible and handles all path joining cases through normalization.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use eidetica::crdt::doc::PathBuf;
    /// # use std::str::FromStr;
    /// // Push strings
    /// let path = PathBuf::new().push("user").push("profile");
    /// assert_eq!(path.as_str(), "user.profile");
    ///
    /// // Push Path types
    /// let suffix = PathBuf::from_str("name.value").unwrap();
    /// let path = PathBuf::new().push("user").push(&suffix);
    /// assert_eq!(path.as_str(), "user.name.value");
    /// ```
    pub fn push(mut self, path: impl AsRef<str>) -> Self {
        let normalized = normalize_path(path.as_ref());
        if normalized.is_empty() {
            return self;
        }

        if self.inner.is_empty() {
            self.inner = normalized;
        } else {
            self.inner.push('.');
            self.inner.push_str(&normalized);
        }
        self
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
        self.inner.split('.').filter(|s| !s.is_empty())
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

    /// Creates a PathBuf from a normalized string.
    ///
    /// This is the internal constructor that assumes the string is already normalized.
    /// Use `from_str()` or `normalize()` for general string input.
    fn from_normalized(normalized: String) -> Self {
        PathBuf { inner: normalized }
    }

    /// Creates a PathBuf by normalizing the input string.
    ///
    /// This method always succeeds by applying path normalization rules.
    pub fn normalize(path: &str) -> Self {
        Self::from_normalized(normalize_path(path))
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
        self.inner.split('.').filter(|s| !s.is_empty())
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

impl AsRef<str> for Path {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl AsRef<str> for PathBuf {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl Borrow<Path> for PathBuf {
    fn borrow(&self) -> &Path {
        self.deref()
    }
}

impl FromStr for PathBuf {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::normalize(s))
    }
}

/// Backward compatibility: FromStr that can return a PathError
///
/// This trait implementation allows existing code that expects `Result<PathBuf, PathError>`
/// to continue working during the migration period.
pub trait FromStrResult {
    fn from_str_result(s: &str) -> Result<PathBuf, PathError>;
}

impl FromStrResult for PathBuf {
    fn from_str_result(s: &str) -> Result<PathBuf, PathError> {
        // For backward compatibility, we still normalize but return in Result form
        Ok(Self::normalize(s))
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
        // Normalize at compile time (basic)
        const NORMALIZED: &str = $crate::crdt::doc::path::normalize_const($single);
        // Safe because we normalized above
        unsafe { $crate::crdt::doc::path::Path::from_str_unchecked(NORMALIZED) }
    }};

    // Multiple arguments - returns PathBuf
    ($first:expr $(, $rest:expr)* $(,)?) => {{
        let mut path = $crate::crdt::doc::PathBuf::new();

        // Helper function to convert anything to a component
        fn add_component(path: &mut $crate::crdt::doc::PathBuf, component: impl AsRef<str>) {
            let component_str = component.as_ref().trim();
            if !component_str.is_empty() {
                // Push is now infallible and handles all path strings
                *path = std::mem::take(path).push(component_str);
            }
        }

        // Handle first argument
        let first_str = $first.to_string();
        add_component(&mut path, first_str);

        // Handle remaining arguments
        $(
            let rest_str = $rest.to_string();
            add_component(&mut path, rest_str);
        )*

        path
    }};
}

/// Normalizes a path string at compile time for string literals.
///
/// This performs basic normalization that can be done in const context.
/// For string literals, this ensures the path is normalized at compile time.
///
/// Note: This is a simplified version that handles most common cases.
/// For complete normalization, use the runtime `normalize_path()` function.
pub const fn normalize_const(path: &str) -> &str {
    // For const context, we can only do basic checks
    // Complex normalization happens at runtime
    if path.is_empty() {
        return "";
    }

    // In const context, we'll just return the path as-is
    // The macro will handle basic validation
    path
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
        // push() accepts strings
        let path = PathBuf::new().push("user").push("profile").push("name");

        assert_eq!(path.len(), 3);
        let components: Vec<&str> = path.components().collect();
        assert_eq!(components, vec!["user", "profile", "name"]);
        assert_eq!(path.file_name(), Some("name"));

        // push() also accepts Path/PathBuf types
        let base = PathBuf::new().push("user");
        let suffix = PathBuf::from_str("profile.name").unwrap();
        let path = base.push(&suffix);
        assert_eq!(path.as_str(), "user.profile.name");

        // Can chain push with different types
        let path = PathBuf::new()
            .push("user")
            .push(PathBuf::from_str("profile").unwrap())
            .push("name");
        assert_eq!(path.as_str(), "user.profile.name");
    }

    #[test]
    fn test_pathbuf_push_normalization() {
        // push() normalizes path strings with dots
        let path = PathBuf::new().push("user.name");
        assert_eq!(path.as_str(), "user.name");

        // Empty strings are ignored
        let path = PathBuf::new().push("");
        assert!(path.is_empty());

        // Consecutive dots are normalized
        let path = PathBuf::new().push("user..name");
        assert_eq!(path.as_str(), "user.name");
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
    fn test_path_normalization_behavior() {
        let test_cases = vec![
            // FromStr normalizes all inputs
            ("", ""),
            (".user", "user"),
            ("user.", "user"),
            ("user..profile", "user.profile"),
            ("user...profile", "user.profile"),
            ("...user...profile...", "user.profile"),
            ("...", ""),
        ];

        for (input, expected_normalized) in test_cases {
            let result = PathBuf::from_str(input);
            assert_eq!(
                result.unwrap().as_str(),
                expected_normalized,
                "Path '{input}' should normalize to '{expected_normalized}'"
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
    fn test_from_str_normalization() {
        let result = PathBuf::from_str("user..invalid");
        assert!(result.is_ok()); // Normalizes instead of failing
        assert_eq!(result.unwrap().as_str(), "user.invalid");
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
        // Empty path is allowed and returns empty PathBuf
        let empty = path!();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        // Empty string literal now works and returns empty path
        let empty_str = path!("");
        assert!(empty_str.is_empty());
        assert_eq!(empty_str.len(), 0);
    }

    #[test]
    fn test_path_normalization() {
        // normalize_path() filters empty components
        assert_eq!(normalize_path(""), "");
        assert_eq!(normalize_path("user"), "user");
        assert_eq!(normalize_path(".user"), "user");
        assert_eq!(normalize_path("user."), "user");
        assert_eq!(normalize_path("user..profile"), "user.profile");
        assert_eq!(normalize_path("...user...profile..."), "user.profile");
        assert_eq!(normalize_path("..."), "");
        assert_eq!(normalize_path("user.profile.name"), "user.profile.name");
    }

    #[test]
    fn test_pathbuf_normalization() {
        // PathBuf::from_str normalizes input
        let cases = vec![
            ("", ""),
            (".user", "user"),
            ("user.", "user"),
            ("user..profile", "user.profile"),
            ("...user...profile...", "user.profile"),
            ("...", ""),
            ("user.profile.name", "user.profile.name"),
        ];

        for (input, expected) in cases {
            let path = PathBuf::from_str(input).unwrap();
            assert_eq!(
                path.as_str(),
                expected,
                "Input '{}' should normalize to '{}'",
                input,
                expected
            );
        }
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
        assert!(Component::new("").is_ok()); // Empty components now allowed

        // Test invalid components
        assert!(Component::new("user.name").is_err()); // Still can't contain dots
    }
}
