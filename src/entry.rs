//!
//! Defines the fundamental data unit (`Entry`) and related types.
//!
//! An `Entry` is the core, content-addressable building block of the database,
//! representing a snapshot of data in the main tree and potentially multiple named subtrees.
//! This module also defines the `ID` type and `RawData` type.

use crate::Error;
use crate::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A content-addressable identifier for an `Entry` or other database object.
///
/// Currently represented as a hex-encoded SHA-256 hash string.
pub type ID = String;

/// Represents serialized data, typically JSON, provided by the user.
///
/// This allows users to manage their own data structures and serialization formats.
pub type RawData = String;

/// Internal representation of the main tree node within an `Entry`.
#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct TreeNode {
    /// The ID of the root `Entry` of the tree this node belongs to.
    pub root: ID,
    /// IDs of the parent `Entry`s in the main tree history.
    /// The vector is kept sorted alphabetically.
    pub parents: Vec<ID>,
    /// Serialized data associated with this `Entry` in the main tree.
    pub data: RawData,
}

/// Internal representation of a named subtree node within an `Entry`.
#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SubTreeNode {
    /// The name of the subtree, analogous to a table name.
    /// Subtrees are _named_, and not identified by an ID.
    pub name: String,
    /// IDs of the parent `Entry`s specific to this subtree's history.
    /// The vector is kept sorted alphabetically.
    pub parents: Vec<ID>,
    /// Serialized data specific to this `Entry` within this named subtree.
    pub data: RawData,
}

/// The fundamental unit of data in EideticaDB.
///
/// An `Entry` represents a snapshot of data within a `Tree` and potentially one or more named `SubTree`s.
/// It is content-addressable, meaning its `ID` is a cryptographic hash of its contents.
/// Entries form a Merkle-DAG (Directed Acyclic Graph) structure through parent references.
///
/// Internal consistency is maintained by automatically sorting parent ID vectors and the
/// `subtrees` vector (by subtree name). This ensures deterministic hashing for content addressing.
#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    /// The main tree node data, including the root ID, parents in the main tree, and associated data.
    tree: TreeNode,
    /// A collection of named subtrees this entry contains data for.
    /// The vector is kept sorted alphabetically by subtree name.
    subtrees: Vec<SubTreeNode>,
    // TODO: Security
    // The ID of the key that was used to sign the entry.
    // This is an Entry ID pointing to the entry that allows the key used to sign this.
    // key: String,
    // The signature of the entry.
    // signature: String,
}

impl Entry {
    /// Create a new entry associated with a specific tree root.
    ///
    /// # Arguments
    /// * `root` - The `ID` of the root `Entry` of the tree this entry belongs to.
    /// * `data` - `RawData` (serialized string) for the main tree node (`tree.data`).
    pub fn new(root: String, data: RawData) -> Self {
        let mut entry = Entry::default();
        entry.tree.root = root;
        entry.tree.data = data;
        entry
    }

    /// Creates a new top-level (root) entry for a new tree.
    ///
    /// Root entries have an empty string as their `root` ID and include a special "root" subtree marker.
    ///
    /// # Arguments
    /// * `data` - `RawData` (serialized string) for the root entry's main data (`tree.data`), often tree settings.
    pub fn new_top_level(data: RawData) -> Self {
        let mut entry = Entry::default();
        entry.tree.root = "".to_string();
        entry.tree.data = data;
        // Add a subtree with the name "root" to mark this as a root entry
        entry
            .set_subtree_data("root".to_string(), "{}".to_string())
            .unwrap();
        entry
    }

    /// Sort the subtrees vector by subtree name to ensure consistent ordering.
    /// This is called internally whenever the subtrees collection is modified.
    fn sort_subtrees(&mut self) {
        self.subtrees.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Sort parent IDs to ensure consistent ordering.
    /// This is called internally whenever parent vectors are modified.
    fn sort_parents(parents: &mut [ID]) {
        parents.sort();
    }

    /// Sets data for a named subtree to this entry, creating it if it doesn't exist.
    ///
    /// If an entry contributes data to a specific domain or table, it's added via a `SubTreeNode`.
    /// Subtrees are automatically kept sorted by name.
    ///
    /// # Arguments
    /// * `name` - The name of the subtree (e.g., "users", "products").
    /// * `data` - `RawData` (serialized string) specific to this entry for the named subtree.
    pub fn set_subtree_data(&mut self, name: String, data: RawData) -> Result<()> {
        if let Some(node) = self.subtrees.iter_mut().find(|node| node.name == name) {
            // Update data in existing SubTreeNode while preserving parents
            node.data = data;
        } else {
            // Create new SubTreeNode if it doesn't exist
            self.subtrees.push(SubTreeNode {
                name,
                data,
                parents: vec![],
            });
            // Sort subtrees by name
            self.sort_subtrees();
        }
        Ok(())
    }

    /// Calculate the content-addressable ID (SHA-256 hash) of the entry.
    ///
    /// The hash includes the root ID, main tree data, and data from all subtrees.
    /// Parent vectors and the subtree vector are implicitly sorted before serialization for hashing,
    /// ensuring that any change to the entry results in a different ID and that the ID is deterministic
    /// regardless of the order parents or subtrees were added.
    pub fn id(&self) -> String {
        // Convert the entry to JSON. Serde will serialize fields in the order they are defined.
        // Since `parents` within TreeNode and SubTreeNode, and `subtrees` within Entry are kept sorted,
        // the resulting JSON string is deterministic.
        let json = serde_json::to_string(self).unwrap();
        // hash the json
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        // convert the hash to a string
        let hash = hasher.finalize();
        // convert the hash to a hex string
        format!("{:x}", hash)
    }

    /// Get the ID of the root `Entry` of the tree this entry belongs to.
    pub fn root(&self) -> &str {
        &self.tree.root
    }

    /// Check if this entry is a root entry of a tree.
    ///
    /// Determined by the presence of a special "root" subtree.
    pub fn is_root(&self) -> bool {
        // TODO: Roots are a case that requires special handling.
        self.subtrees.iter().any(|node| node.name == "root")
    }

    /// Check if this entry is the absolute top-level root entry (has no parent tree).
    pub fn is_toplevel_root(&self) -> bool {
        self.root().is_empty() && self.is_root()
    }

    /// Check if this entry contains data for a specific named subtree.
    pub fn in_subtree(&self, subtree: &str) -> bool {
        self.subtrees.iter().any(|node| node.name == subtree)
    }

    /// Check if this entry belongs to a specific tree, identified by its root ID.
    pub fn in_tree(&self, tree: &str) -> bool {
        // Entries that are roots exist in both trees
        self.root() == tree || (self.is_root() && (self.id() == tree))
    }

    /// Get the names of all subtrees this entry contains data for.
    /// The names are returned in alphabetical order.
    pub fn subtrees(&self) -> Vec<String> {
        self.subtrees
            .iter()
            .map(|subtree| subtree.name.clone())
            .collect()
    }

    /// Remove subtrees that do not have any data.
    pub fn remove_empty_subtrees(&mut self) -> Result<()> {
        self.subtrees.retain(|subtree| !subtree.data.is_empty());
        Ok(())
    }

    /// Get the `RawData` associated with the main tree node (`tree.data`).
    /// This is not the same as the "settings" subtree data.
    pub fn get_settings(&self) -> Result<RawData> {
        Ok(self.tree.data.clone())
    }

    /// Get the `RawData` for a specific named subtree within this entry.
    pub fn data(&self, subtree: &str) -> Result<&RawData> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree)
            .map(|node| &node.data)
            .ok_or(Error::NotFound)
    }

    /// Get the IDs of the parent entries in the main tree history.
    /// The parent IDs are returned in alphabetical order.
    pub fn parents(&self) -> Result<Vec<ID>> {
        Ok(self.tree.parents.clone())
    }

    /// Get the IDs of the parent entries specific to a named subtree's history.
    /// The parent IDs are returned in alphabetical order.
    pub fn subtree_parents(&self, subtree: &str) -> Result<Vec<ID>> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree)
            .map(|node| node.parents.clone())
            .ok_or(Error::NotFound)
    }

    /// Set the root ID for this entry.
    /// Typically used internally by `Tree::insert`.
    pub fn set_root(&mut self, root: ID) {
        self.tree.root = root;
    }

    /// Set the parent IDs for the main tree history.
    /// The provided vector will be sorted alphabetically internally.
    /// Typically used internally by `Tree::insert`.
    pub fn set_parents(&mut self, parents: Vec<ID>) {
        self.tree.parents = parents;
        Self::sort_parents(&mut self.tree.parents);
    }

    /// Set the parent IDs for a specific named subtree's history.
    /// The provided vector will be sorted alphabetically internally.
    /// If the subtree does not exist, this operation has no effect.
    /// Typically used internally by `Tree::insert`.
    pub fn set_subtree_parents(&mut self, subtree: &str, parents: Vec<ID>) {
        if let Some(node) = self.subtrees.iter_mut().find(|node| node.name == subtree) {
            node.parents = parents;
            Self::sort_parents(&mut node.parents);
        }
    }
}
