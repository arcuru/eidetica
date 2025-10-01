//! Node type alias for CRDT documents.
//!
//! This module now provides a type alias where Node = Doc. The Node type
//! has been folded into the Doc type to simplify the API. All Node functionality
//! is now available directly on Doc.

/// Node type alias - Node is now the same as Doc.
///
/// The Node type has been folded into the Doc type to simplify the CRDT API.
/// All Node functionality is now available directly on Doc.
///
/// # Migration
///
/// Code that used Node should now use Doc instead:
/// - `Node::new()` becomes `Doc::new()`
/// - `Value::Doc(node)` becomes `Value::Doc(doc)`
/// - All Node methods are now available on Doc
///
/// This type alias maintains backward compatibility during the transition.
pub type Node = super::Doc;
