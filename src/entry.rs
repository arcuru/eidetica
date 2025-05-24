//!
//! Defines the fundamental data unit (`Entry`) and related types.
//!
//! An `Entry` is the core, content-addressable building block of the database,
//! representing a snapshot of data in the main tree and potentially multiple named subtrees.
//! This module also defines the `ID` type and `RawData` type.

use crate::auth::types::AuthInfo;
use crate::constants::ROOT;
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
    /// Serialized metadata associated with this `Entry` in the main tree.
    /// This data is metadata about this specific entry only and is not merged with other entries.
    ///
    /// Metadata is used to improve the efficiency of certain operations and for experimentation.
    ///
    /// Metadata is optional and may not be present in all entries. Future versions
    /// may extend metadata to include additional information.
    pub metadata: Option<RawData>,
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

/// The fundamental unit of data in Eidetica, representing a finalized, immutable Database Entry.
///
/// An `Entry` represents a snapshot of data within a `Tree` and potentially one or more named `SubTree`s.
/// It is content-addressable, meaning its `ID` is a cryptographic hash of its contents.
/// Entries form a Merkle-DAG (Directed Acyclic Graph) structure through parent references.
///
/// # Immutability
///
/// `Entry` instances are designed to be immutable once created. To create or modify entries,
/// use the `EntryBuilder` struct, which provides a mutable API for constructing entries.
/// Once an entry is built, its content cannot be changed, and its ID is deterministic
/// based on its content.
///
/// # Example
///
/// ```
/// # use eidetica::entry::Entry;
///
/// // Create a new entry using Entry::builder()
/// let entry = Entry::builder("tree_root".to_string(), r#"{"settings":true}"#.to_string())
///     .set_subtree_data("users".to_string(), r#"{"user1":"data"}"#.to_string())
///     .build();
///
/// // Access entry data
/// let id = entry.id(); // Calculate content-addressable ID
/// let settings = entry.get_settings().unwrap();
/// let user_data = entry.data("users").unwrap();
/// ```
///
/// # Builders
///
/// To create an `Entry`, use the associated `EntryBuilder`.
/// The preferred way to get an `EntryBuilder` is via the static methods
/// `Entry::builder()` for regular entries or `Entry::root_builder()` for new top-level tree roots.
///
/// ```
/// # use eidetica::entry::{Entry, RawData};
/// # let root_id: String = "some_root_id".to_string();
/// # let data: RawData = "{}".to_string();
/// // For a regular entry:
/// let builder = Entry::builder(root_id, data);
///
/// // For a new top-level tree root:
/// let root_builder = Entry::root_builder("initial_settings_data".to_string());
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    /// The main tree node data, including the root ID, parents in the main tree, and associated data.
    tree: TreeNode,
    /// A collection of named subtrees this entry contains data for.
    /// The vector is kept sorted alphabetically by subtree name during the build process.
    subtrees: Vec<SubTreeNode>,
    /// Authentication information for this entry
    pub auth: AuthInfo,
}

impl Entry {
    /// Creates a new `EntryBuilder` for an entry associated with a specific tree root.
    /// This is a convenience method and preferred over calling `EntryBuilder::new()` directly.
    ///
    /// # Arguments
    /// * `root` - The `ID` of the root `Entry` of the tree this entry will belong to.
    /// * `data` - `RawData` (serialized string) for the main tree node (`tree.data`).
    pub fn builder(root: impl Into<String>, data: RawData) -> EntryBuilder {
        EntryBuilder::new(root, data)
    }

    /// Creates a new `EntryBuilder` for a top-level (root) entry for a new tree.
    /// This is a convenience method and preferred over calling `EntryBuilder::new_top_level()` directly.
    ///
    /// Root entries have an empty string as their `root` ID and include a special ROOT subtree marker.
    /// This method is typically used when creating a new tree.
    ///
    /// # Arguments
    /// * `data` - `RawData` (serialized string) for the root entry's main data (`tree.data`), often tree settings.
    pub fn root_builder(data: impl Into<String>) -> EntryBuilder {
        EntryBuilder::new_top_level(data.into())
    }

    /// Get the content-addressable ID of the entry.
    ///
    /// The ID is calculated on demand by hashing the serialized JSON representation of the entry.
    /// Because entries are immutable once created and their contents are deterministically
    /// serialized, this ensures that identical entries will always have the same ID.
    pub fn id(&self) -> ID {
        // Entry itself derives Serialize and contains tree and subtrees.
        // These are kept sorted and finalized by the EntryBuilder before Entry creation.
        let json = serde_json::to_string(self).expect("Failed to serialize entry for hashing");

        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        // convert the hash to a string
        let hash = hasher.finalize();
        // convert the hash to a hex string
        format!("{hash:x}")
    }

    /// Get the ID of the root `Entry` of the tree this entry belongs to.
    pub fn root(&self) -> &str {
        &self.tree.root
    }

    /// Check if this entry is a root entry of a tree.
    ///
    /// Determined by the presence of a special ROOT subtree.
    pub fn is_root(&self) -> bool {
        self.subtrees.iter().any(|node| node.name == ROOT)
    }

    /// Check if this entry is the absolute top-level root entry (has no parent tree).
    pub fn is_toplevel_root(&self) -> bool {
        self.root().is_empty() && self.is_root()
    }

    /// Check if this entry contains data for a specific named subtree.
    pub fn in_subtree(&self, subtree_name: &str) -> bool {
        self.subtrees.iter().any(|node| node.name == subtree_name)
    }

    /// Check if this entry belongs to a specific tree, identified by its root ID.
    pub fn in_tree(&self, tree_id: &str) -> bool {
        // Entries that are roots exist in both trees
        self.root() == tree_id || (self.id() == tree_id)
    }

    /// Get the names of all subtrees this entry contains data for.
    /// The names are returned in alphabetical order.
    pub fn subtrees(&self) -> Vec<String> {
        self.subtrees
            .iter()
            .map(|subtree| subtree.name.clone())
            .collect()
    }

    /// Get the `RawData` associated with the main tree node (`tree.data`).
    /// This is not the same as the "settings" subtree data (which might be in a "settings" subtree).
    pub fn get_settings(&self) -> Result<RawData> {
        Ok(self.tree.data.clone())
    }

    /// Get the `RawData` for a specific named subtree within this entry.
    pub fn data(&self, subtree_name: &str) -> Result<&RawData> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree_name)
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
    pub fn subtree_parents(&self, subtree_name: &str) -> Result<Vec<ID>> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree_name)
            .map(|node| node.parents.clone())
            .ok_or(Error::NotFound)
    }

    /// Get the metadata for this entry's tree node, if present.
    pub fn get_metadata(&self) -> Option<&RawData> {
        self.tree.metadata.as_ref()
    }

    /// Create a canonical representation of this entry for signing purposes.
    ///
    /// This creates a copy of the entry with the signature field removed from auth,
    /// which is necessary for signature generation and verification.
    /// The returned entry has deterministic field ordering for consistent signatures.
    pub fn canonical_for_signing(&self) -> Self {
        let mut canonical = self.clone();
        canonical.auth.signature = None;
        canonical
    }

    /// Create canonical bytes for signing or ID generation.
    ///
    /// This method serializes the entry to JSON with deterministic field ordering.
    /// For signing purposes, call `canonical_for_signing()` first.
    pub fn canonical_bytes(&self) -> crate::Result<Vec<u8>> {
        let json = serde_json::to_string(self).map_err(crate::Error::Serialize)?;
        Ok(json.into_bytes())
    }

    /// Create canonical bytes for signing (convenience method).
    ///
    /// This combines `canonical_for_signing()` and `canonical_bytes()` for convenience.
    pub fn signing_bytes(&self) -> crate::Result<Vec<u8>> {
        self.canonical_for_signing().canonical_bytes()
    }
}

/// A builder for creating `Entry` instances.
///
/// `EntryBuilder` allows mutable construction of an entry's content.
/// Once finalized with the `build()` method, it produces an immutable `Entry`
/// with a deterministically calculated ID.
///
/// # Mutable Construction
///
/// The builder provides two patterns for construction:
/// 1. Ownership chaining: Each method returns `self` for chained calls.
///    ```
///    # use eidetica::entry::Entry;
///    # let root_id = "root_id".to_string();
///    # let data = "data".to_string();
///    let entry = Entry::builder(root_id, data)
///        .set_subtree_data("users".to_string(), "user_data".to_string())
///        .add_parent("parent_id".to_string())
///        .build();
///    ```
///
/// 2. Mutable reference: Methods ending in `_mut` modify the builder in place.
///    ```
///    # use eidetica::entry::Entry;
///    # let root_id = "root_id".to_string();
///    # let data = "data".to_string();
///    let mut builder = Entry::builder(root_id, data);
///    builder.set_subtree_data_mut("users".to_string(), "user_data".to_string());
///    builder.add_parent_mut("parent_id".to_string());
///    let entry = builder.build();
///    ```
///
/// # Example
///
/// ```
/// use eidetica::entry::Entry;
///
/// // Create a builder for a regular entry
/// let entry = Entry::builder("root_id".to_string(), "main_data".to_string())
///     .add_parent("parent1".to_string())
///     .set_subtree_data("users".to_string(), "user_data".to_string())
///     .build();
///
/// // Create a builder for a top-level root entry
/// let root_entry = Entry::root_builder("settings_data".to_string())
///     .set_subtree_data("users".to_string(), "initial_user_data".to_string())
///     .build();
/// ```
#[derive(Clone)]
pub struct EntryBuilder {
    tree: TreeNode,
    subtrees: Vec<SubTreeNode>,
    auth: AuthInfo,
}

impl EntryBuilder {
    /// Creates a new `EntryBuilder` for an entry associated with a specific tree root.
    ///
    /// # Arguments
    /// * `root` - The `ID` of the root `Entry` of the tree this entry will belong to.
    /// * `data` - `RawData` (serialized string) for the main tree node (`tree.data`).
    ///
    /// Note: It's generally preferred to use the static `Entry::builder()` method
    /// instead of calling this constructor directly.
    pub fn new(root: impl Into<String>, data: RawData) -> Self {
        Self {
            tree: TreeNode {
                root: root.into(),
                parents: Vec::new(),
                data,
                metadata: None,
            },
            subtrees: Vec::new(),
            auth: AuthInfo::default(),
        }
    }

    /// Creates a new `EntryBuilder` for a top-level (root) entry for a new tree.
    ///
    /// Root entries have an empty string as their `root` ID and include a special ROOT subtree marker.
    /// This method is typically used when creating a new tree.
    ///
    /// # Arguments
    /// * `data` - `RawData` (serialized string) for the root entry's main data (`tree.data`), often tree settings.
    ///
    /// Note: It's generally preferred to use the static `Entry::root_builder()` method
    /// instead of calling this constructor directly.
    pub fn new_top_level(data: impl Into<String>) -> Self {
        let mut builder = Self::new("".to_string(), data.into());
        // Add a special subtree that identifies this as a root entry
        builder.set_subtree_data_mut(ROOT.to_string(), "".to_string());
        builder
    }

    /// Set the authentication information for this entry.
    ///
    /// # Arguments
    /// * `auth` - The authentication information including key ID and optional signature
    pub fn set_auth(mut self, auth: AuthInfo) -> Self {
        self.auth = auth;
        self
    }

    /// Mutable reference version of set_auth.
    /// Set the authentication information for this entry.
    ///
    /// # Arguments
    /// * `auth` - The authentication information including key ID and optional signature
    pub fn set_auth_mut(&mut self, auth: AuthInfo) -> &mut Self {
        self.auth = auth;
        self
    }

    /// Get a reference to the current authentication information
    pub fn auth(&self) -> &AuthInfo {
        &self.auth
    }

    /// Get a mutable reference to the current authentication information
    pub fn auth_mut(&mut self) -> &mut AuthInfo {
        &mut self.auth
    }

    /// Get the names of all subtrees this entry builder contains data for.
    /// The names are returned in alphabetical order.
    pub fn subtrees(&self) -> Vec<String> {
        self.subtrees
            .iter()
            .map(|subtree| subtree.name.clone())
            .collect()
    }

    /// Get the `RawData` for a specific named subtree within this entry builder.
    pub fn data(&self, subtree_name: &str) -> Result<&RawData> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree_name)
            .map(|node| &node.data)
            .ok_or(Error::NotFound)
    }

    /// Get the IDs of the parent entries specific to a named subtree's history.
    /// The parent IDs are returned in alphabetical order.
    pub fn subtree_parents(&self, subtree_name: &str) -> Result<Vec<ID>> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree_name)
            .map(|node| node.parents.clone())
            .ok_or(Error::NotFound)
    }

    /// Sort a list of parent IDs in alphabetical order.
    fn sort_parents_list(parents: &mut [ID]) {
        parents.sort();
    }

    /// Sort the list of subtrees in alphabetical order by name.
    ///
    /// This is important for ensuring entries with the same content have the same ID.
    fn sort_subtrees_list(&mut self) {
        self.subtrees.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Sets data for a named subtree, creating it if it doesn't exist.
    /// The list of subtrees will be sorted by name when `build()` is called.
    ///
    /// # Arguments
    /// * `name` - The name of the subtree (e.g., "users", "products").
    /// * `data` - `RawData` (serialized string) specific to this entry for the named subtree.
    pub fn set_subtree_data(mut self, name: impl Into<String>, data: RawData) -> Self {
        let name = name.into();
        if let Some(node) = self.subtrees.iter_mut().find(|node| node.name == name) {
            node.data = data;
        } else {
            self.subtrees.push(SubTreeNode {
                name,
                data,
                parents: vec![],
            });
        }
        self
    }

    /// Mutable reference version of set_subtree_data.
    /// Sets data for a named subtree, creating it if it doesn't exist.
    /// The list of subtrees will be sorted by name when `build()` is called.
    ///
    /// # Arguments
    /// * `name` - The name of the subtree (e.g., "users", "products").
    /// * `data` - `RawData` (serialized string) specific to this entry for the named subtree.
    pub fn set_subtree_data_mut(&mut self, name: impl Into<String>, data: RawData) -> &mut Self {
        let name = name.into();
        if let Some(node) = self.subtrees.iter_mut().find(|node| node.name == name) {
            node.data = data;
        } else {
            self.subtrees.push(SubTreeNode {
                name,
                data,
                parents: vec![],
            });
        }
        self
    }

    /// Removes subtrees that do not have any data or have data "{}".
    /// This is useful for cleaning up entries before building.
    pub fn remove_empty_subtrees(mut self) -> Self {
        self.subtrees
            .retain(|subtree| !subtree.data.is_empty() && subtree.data != "{}");
        self
    }

    /// Mutable reference version of remove_empty_subtrees.
    /// Removes subtrees that do not have any data or have data "{}".
    /// This is useful for cleaning up entries before building.
    pub fn remove_empty_subtrees_mut(&mut self) -> &mut Self {
        self.subtrees
            .retain(|subtree| !subtree.data.is_empty() && subtree.data != "{}");
        self
    }

    /// Set the root ID for this entry.
    ///
    /// # Arguments
    /// * `root` - The ID of the root `Entry` of the tree this entry will belong to.
    ///
    /// # Returns
    /// A mutable reference to self for method chaining.
    pub fn set_root(mut self, root: impl Into<String>) -> Self {
        self.tree.root = root.into();
        self
    }

    /// Mutable reference version of set_root.
    /// Set the root ID for this entry.
    ///
    /// # Arguments
    /// * `root` - The ID of the root `Entry` of the tree this entry will belong to.
    ///
    /// # Returns
    /// A mutable reference to self for method chaining.
    pub fn set_root_mut(&mut self, root: impl Into<String>) -> &mut Self {
        self.tree.root = root.into();
        self
    }

    /// Set the main data for this entry's tree node.
    ///
    /// # Arguments
    /// * `data` - `RawData` (serialized string) for the main tree node.
    ///
    /// # Returns
    /// A mutable reference to self for method chaining.
    pub fn set_data(mut self, data: impl Into<String>) -> Self {
        self.tree.data = data.into();
        self
    }

    /// Mutable reference version of set_data.
    /// Set the main data for this entry's tree node.
    ///
    /// # Arguments
    /// * `data` - `RawData` (serialized string) for the main tree node.
    ///
    /// # Returns
    /// A mutable reference to self for method chaining.
    pub fn set_data_mut(&mut self, data: impl Into<String>) -> &mut Self {
        self.tree.data = data.into();
        self
    }

    /// Set the parent IDs for the main tree history.
    /// The provided vector will be sorted alphabetically during the `build()` process.
    pub fn set_parents(mut self, parents: Vec<ID>) -> Self {
        self.tree.parents = parents;
        self
    }

    /// Mutable reference version of set_parents.
    /// Set the parent IDs for the main tree history.
    /// The provided vector will be sorted alphabetically during the `build()` process.
    pub fn set_parents_mut(&mut self, parents: Vec<ID>) -> &mut Self {
        self.tree.parents = parents;
        self
    }

    /// Add a single parent ID to the main tree history.
    /// Parents will be sorted and duplicates handled during the `build()` process.
    pub fn add_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.tree.parents.push(parent_id.into());
        self
    }

    /// Mutable reference version of add_parent.
    /// Add a single parent ID to the main tree history.
    /// Parents will be sorted and duplicates handled during the `build()` process.
    pub fn add_parent_mut(&mut self, parent_id: impl Into<String>) -> &mut Self {
        self.tree.parents.push(parent_id.into());
        self
    }

    /// Set the parent IDs for a specific named subtree's history.
    /// The provided vector will be sorted alphabetically and de-duplicated during the `build()` process.
    /// If the subtree does not exist, it will be created with empty data ("{}").
    /// The list of subtrees will be sorted by name when `build()` is called.
    pub fn set_subtree_parents(
        mut self,
        subtree_name: impl Into<String>,
        parents: Vec<ID>,
    ) -> Self {
        let subtree_name = subtree_name.into();
        if let Some(node) = self
            .subtrees
            .iter_mut()
            .find(|node| node.name == subtree_name)
        {
            node.parents = parents;
        } else {
            // Create new SubTreeNode if it doesn't exist, then set parents
            self.subtrees.push(SubTreeNode {
                name: subtree_name,
                data: "{}".to_string(), // Default data if creating subtree just for parents
                parents,
            });
        }
        self
    }

    /// Mutable reference version of set_subtree_parents.
    /// Set the parent IDs for a specific named subtree's history.
    /// The provided vector will be sorted alphabetically and de-duplicated during the `build()` process.
    /// If the subtree does not exist, it will be created with empty data ("{}").
    /// The list of subtrees will be sorted by name when `build()` is called.
    pub fn set_subtree_parents_mut(
        &mut self,
        subtree_name: impl Into<String>,
        parents: Vec<ID>,
    ) -> &mut Self {
        let subtree_name = subtree_name.into();
        if let Some(node) = self
            .subtrees
            .iter_mut()
            .find(|node| node.name == subtree_name)
        {
            node.parents = parents;
        } else {
            // Create new SubTreeNode if it doesn't exist, then set parents
            self.subtrees.push(SubTreeNode {
                name: subtree_name,
                data: "{}".to_string(), // Default data if creating subtree just for parents
                parents,
            });
        }
        self
    }

    /// Add a single parent ID to a specific named subtree's history.
    /// If the subtree does not exist, it will be created with empty data ("{}").
    /// Parent IDs will be sorted and de-duplicated during the `build()` process.
    /// The list of subtrees will be sorted by name when `build()` is called.
    pub fn add_subtree_parent(
        mut self,
        subtree_name: impl Into<String>,
        parent_id: impl Into<String>,
    ) -> Self {
        let subtree_name = subtree_name.into();
        let parent_id = parent_id.into();
        if let Some(node) = self
            .subtrees
            .iter_mut()
            .find(|node| node.name == subtree_name)
        {
            node.parents.push(parent_id);
        } else {
            self.subtrees.push(SubTreeNode {
                name: subtree_name,
                data: "{}".to_string(),
                parents: vec![parent_id],
            });
        }
        self
    }

    /// Mutable reference version of add_subtree_parent.
    /// Add a single parent ID to a specific named subtree's history.
    /// If the subtree does not exist, it will be created with empty data ("{}").
    /// Parent IDs will be sorted and de-duplicated during the `build()` process.
    /// The list of subtrees will be sorted by name when `build()` is called.
    pub fn add_subtree_parent_mut(
        &mut self,
        subtree_name: impl Into<String>,
        parent_id: impl Into<String>,
    ) -> &mut Self {
        let subtree_name = subtree_name.into();
        let parent_id = parent_id.into();
        if let Some(node) = self
            .subtrees
            .iter_mut()
            .find(|node| node.name == subtree_name)
        {
            node.parents.push(parent_id);
        } else {
            self.subtrees.push(SubTreeNode {
                name: subtree_name,
                data: "{}".to_string(),
                parents: vec![parent_id],
            });
        }
        self
    }

    /// Set the metadata for this entry's tree node.
    ///
    /// Metadata is optional information attached to an entry that is not part of the
    /// main data model and is not merged between entries. It's used primarily for
    /// improving efficiency of operations and for experimentation.
    ///
    /// For example, metadata can contain references to the current tips of the settings
    /// subtree, allowing for efficient verification in sparse checkout scenarios.
    ///
    /// # Arguments
    /// * `metadata` - `RawData` (serialized string) for the main tree node metadata.
    ///
    /// # Returns
    /// Self for method chaining.
    pub fn set_metadata(mut self, metadata: impl Into<String>) -> Self {
        self.tree.metadata = Some(metadata.into());
        self
    }

    /// Mutable reference version of set_metadata.
    /// Set the metadata for this entry's tree node.
    ///
    /// Metadata is optional information attached to an entry that is not part of the
    /// main data model and is not merged between entries. It's used primarily for
    /// improving efficiency of operations and for experimentation.
    ///
    /// For example, metadata can contain references to the current tips of the settings
    /// subtree, allowing for efficient verification in sparse checkout scenarios.
    ///
    /// # Arguments
    /// * `metadata` - `RawData` (serialized string) for the main tree node metadata.
    ///
    /// # Returns
    /// A mutable reference to self for method chaining.
    pub fn set_metadata_mut(&mut self, metadata: impl Into<String>) -> &mut Self {
        self.tree.metadata = Some(metadata.into());
        self
    }

    /// Build and return the final immutable `Entry`.
    ///
    /// This method:
    /// 1. Sorts all parent lists in both the main tree and subtrees
    /// 2. Sorts the subtrees list by name
    /// 3. Removes any empty subtrees
    /// 4. Creates and returns the immutable `Entry`
    ///
    /// After calling this method, the builder is consumed and cannot be used again.
    /// The returned `Entry` is immutable and its parts cannot be modified.
    pub fn build(mut self) -> Entry {
        // Sort parent lists (if any)
        Self::sort_parents_list(&mut self.tree.parents);
        for subtree in &mut self.subtrees {
            Self::sort_parents_list(&mut subtree.parents);
        }

        // Deduplicate parents
        self.tree.parents.dedup();
        for subtree in &mut self.subtrees {
            subtree.parents.dedup();
        }

        // Sort subtrees
        self.sort_subtrees_list();

        Entry {
            tree: self.tree,
            subtrees: self.subtrees,
            auth: self.auth,
        }
    }
}
