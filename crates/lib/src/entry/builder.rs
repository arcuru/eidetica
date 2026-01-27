//! Builder for creating Entry instances.

use std::collections::HashSet;

use rand::Rng;

use super::{ENTRY_VERSION, Entry, EntryError, ID, RawData, SubTreeNode, TreeNode};
use crate::{Result, auth::types::SigInfo, constants::ROOT, crdt::Doc, store::StoreError};

/// A builder for creating `Entry` instances.
///
/// `EntryBuilder` allows mutable construction of an entry's content.
/// Once finalized with the `build()` method, it produces an immutable `Entry`
/// with a deterministically calculated ID.
///
/// # Parameter Type Efficiency
///
/// The builder uses `impl Into<ID>` and `impl Into<RawData>` for parameters, allowing you to pass
/// string literals, `&str`, `String`, or the appropriate types without unnecessary conversions:
///
/// ```ignore
/// // Efficient - no unnecessary .to_string() calls needed
/// let entry = Entry::builder("root_id")
///     .add_parent("parent1")
///     .set_subtree_data("users", "user_data")
///     .build();
/// ```
///
/// # Mutable Construction
///
/// The builder provides two patterns for construction:
/// 1. Ownership chaining: Each method returns `self` for chained calls.
///    ```
///    # use eidetica::Entry;
///    # let root_id = "root_id".to_string();
///    # let data = "data".to_string();
///    let entry = Entry::builder(root_id)
///        .set_subtree_data("users".to_string(), "user_data".to_string())
///        .add_parent("parent_id".to_string())
///        .build();
///    ```
///
/// 2. Mutable reference: Methods ending in `_mut` modify the builder in place.
///    ```
///    # use eidetica::Entry;
///    # let root_id = "root_id".to_string();
///    # let data = "data".to_string();
///    let mut builder = Entry::builder(root_id);
///    builder.set_subtree_data_mut("users".to_string(), "user_data".to_string());
///    builder.add_parent_mut("parent_id".to_string());
///    let entry = builder.build();
///    ```
///
/// # Example
///
/// ```
/// use eidetica::Entry;
///
/// // Create a builder for a regular entry
/// let entry = Entry::builder("root_id")
///     .add_parent("parent1")
///     .set_subtree_data("users", "user_data")
///     .build();
///
/// // Create a builder for a top-level root entry
/// let root_entry = Entry::root_builder()
///     .set_subtree_data("users", "initial_user_data")
///     .build();
/// ```
#[derive(Clone, Debug)]
pub struct EntryBuilder {
    pub(super) tree: TreeNode,
    pub(super) subtrees: Vec<SubTreeNode>,
    pub(super) sig: SigInfo,
}

impl EntryBuilder {
    /// Creates a new `EntryBuilder` for an entry associated with a specific tree root.
    ///
    /// # Arguments
    /// * `root` - The `ID` of the root `Entry` of the tree this entry will belong to.
    ///
    /// Note: It's generally preferred to use the static `Entry::builder()` method
    /// instead of calling this constructor directly.
    pub fn new(root: impl Into<ID>) -> Self {
        Self {
            tree: TreeNode {
                root: root.into(),
                parents: Vec::new(),
                metadata: None,
                height: 0,
            },
            subtrees: Vec::new(),
            sig: SigInfo::default(),
        }
    }

    /// Creates a new `EntryBuilder` for a top-level (root) entry for a new tree.
    ///
    /// Root entries have an empty string as their `root` ID and include a special ROOT subtree marker.
    /// This method is typically used when creating a new tree.
    ///
    /// Note: It's generally preferred to use the static `Entry::root_builder()` method
    /// instead of calling this constructor directly.
    pub fn new_top_level() -> Self {
        let mut builder = Self::new("");
        // Add a special subtree that identifies this as a root entry
        builder.set_subtree_data_mut(ROOT, "");

        // Add random entropy to metadata to ensure unique IDs for each root entry
        let entropy: u64 = rand::thread_rng().r#gen();
        let metadata_json = format!(r#"{{"entropy":{entropy}}}"#);
        builder.set_metadata_mut(&metadata_json);

        builder
    }

    /// Set the authentication information for this entry.
    ///
    /// # Arguments
    /// * `auth` - The authentication information including key ID and optional signature
    pub fn set_sig(mut self, sig: SigInfo) -> Self {
        self.sig = sig;
        self
    }

    /// Mutable reference version of set_auth.
    /// Set the authentication information for this entry.
    ///
    /// # Arguments
    /// * `auth` - The authentication information including key ID and optional signature
    pub fn set_sig_mut(&mut self, sig: SigInfo) -> &mut Self {
        self.sig = sig;
        self
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
    ///
    /// Returns an error if the subtree is not found or if the subtree exists but has no data (`None`).
    pub fn data(&self, subtree_name: impl AsRef<str>) -> Result<&RawData> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree_name.as_ref())
            .and_then(|node| node.data.as_ref())
            .ok_or_else(|| {
                StoreError::KeyNotFound {
                    store: "entry".to_string(),
                    key: subtree_name.as_ref().to_string(),
                }
                .into()
            })
    }

    /// Get the IDs of the parent entries for the main tree.
    /// The parent IDs are returned in alphabetical order.
    pub fn parents(&self) -> Result<Vec<ID>> {
        Ok(self.tree.parents.clone())
    }

    /// Get the IDs of the parent entries specific to a named subtree's history.
    /// The parent IDs are returned in alphabetical order.
    pub fn subtree_parents(&self, subtree_name: impl AsRef<str>) -> Result<Vec<ID>> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree_name.as_ref())
            .map(|node| node.parents.clone())
            .ok_or_else(|| {
                StoreError::KeyNotFound {
                    store: "entry".to_string(),
                    key: subtree_name.as_ref().to_string(),
                }
                .into()
            })
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
    pub fn set_subtree_data(mut self, name: impl Into<String>, data: impl Into<RawData>) -> Self {
        let name = name.into();
        if let Some(node) = self.subtrees.iter_mut().find(|node| node.name == name) {
            node.data = Some(data.into());
        } else {
            self.subtrees.push(SubTreeNode {
                name,
                data: Some(data.into()),
                parents: vec![],
                height: None,
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
    pub fn set_subtree_data_mut(
        &mut self,
        name: impl Into<String>,
        data: impl Into<RawData>,
    ) -> &mut Self {
        let name = name.into();
        if let Some(node) = self.subtrees.iter_mut().find(|node| node.name == name) {
            node.data = Some(data.into());
        } else {
            self.subtrees.push(SubTreeNode {
                name,
                data: Some(data.into()),
                parents: vec![],
                height: None,
            });
        }
        self
    }

    /// Removes subtrees that have empty data.
    ///
    /// This removes subtrees with `Some("")` (actual empty data) and subtrees with `None`
    /// (no data changes) UNLESS the subtree is referenced in the `_index` subtree's data.
    ///
    /// When `_index` is updated for a subtree, that subtree must appear in the Entry.
    /// This is marked by having `None` data and being referenced in `_index`.
    ///
    /// This is useful for cleaning up entries before building.
    ///
    /// # Errors
    ///
    /// Returns an error if the `_index` subtree data exists but cannot be deserialized.
    pub fn remove_empty_subtrees(mut self) -> Result<Self> {
        // Get the set of subtrees referenced in _index
        let index_subtrees = self.get_index_referenced_subtrees()?;

        self.subtrees.retain(|subtree| match &subtree.data {
            None => {
                // Preserve None only if this subtree is referenced in _index
                index_subtrees.contains(&subtree.name)
            }
            Some(d) => !d.is_empty(), // Remove only if Some with empty string
        });
        Ok(self)
    }

    /// Mutable reference version of remove_empty_subtrees.
    ///
    /// Removes subtrees with `Some("")` and subtrees with `None` unless referenced in `_index`.
    ///
    /// # Errors
    ///
    /// Returns an error if the `_index` subtree data exists but cannot be deserialized.
    pub fn remove_empty_subtrees_mut(&mut self) -> Result<&mut Self> {
        // Get the set of subtrees referenced in _index
        let index_subtrees = self.get_index_referenced_subtrees()?;

        self.subtrees.retain(|subtree| match &subtree.data {
            None => {
                // Preserve None only if this subtree is referenced in _index
                index_subtrees.contains(&subtree.name)
            }
            Some(d) => !d.is_empty(), // Remove only if Some with empty string
        });
        Ok(self)
    }

    /// Get the set of subtree names that are referenced in the `_index` subtree's local data.
    ///
    /// Returns a set of subtree names that have entries in the local `_index` subtree.
    /// This is used to determine which subtrees with `None` data should be preserved.
    ///
    /// # Errors
    ///
    /// Returns an error if the `_index` subtree data exists but cannot be deserialized.
    fn get_index_referenced_subtrees(&self) -> Result<HashSet<String>> {
        let mut result = HashSet::new();

        // Find the _index subtree's local data
        if let Some(index_node) = self.subtrees.iter().find(|node| node.name == "_index") {
            // If _index has data, deserialize it and get all keys
            if let Some(data) = &index_node.data {
                // Try to deserialize as a Doc to get the keys
                let doc = serde_json::from_str::<Doc>(data).map_err(|e| {
                    EntryError::InvalidIndexData {
                        reason: format!(
                            "Failed to deserialize _index subtree data: {} (data preview: {})",
                            e,
                            data.chars().take(100).collect::<String>()
                        ),
                    }
                })?;

                for key in doc.keys() {
                    result.insert(key.to_string());
                }
            }
        }

        Ok(result)
    }

    /// Set the root ID for this entry.
    ///
    /// # Arguments
    /// * `root` - The ID of the root `Entry` of the tree this entry will belong to.
    ///
    /// # Returns
    /// A mutable reference to self for method chaining.
    pub fn set_root(mut self, root: impl Into<String>) -> Self {
        self.tree.root = root.into().into();
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
        self.tree.root = root.into().into();
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
        self.tree.parents.push(parent_id.into().into());
        self
    }

    /// Mutable reference version of add_parent.
    /// Add a single parent ID to the main tree history.
    /// Parents will be sorted and duplicates handled during the `build()` process.
    pub fn add_parent_mut(&mut self, parent_id: impl Into<String>) -> &mut Self {
        self.tree.parents.push(parent_id.into().into());
        self
    }

    /// Get a reference to the current parent IDs for the main tree history.
    pub fn get_parents(&self) -> Option<&Vec<ID>> {
        if self.tree.parents.is_empty() {
            None
        } else {
            Some(&self.tree.parents)
        }
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
                data: None,
                parents,
                height: None,
            });
        }
        self
    }

    /// Mutable reference version of set_subtree_parents.
    /// Set the parent IDs for a specific named subtree's history.
    /// The provided vector will be sorted alphabetically and de-duplicated during the `build()` process.
    /// If the subtree does not exist, it will be created with no data (`None`).
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
                data: None,
                parents,
                height: None,
            });
        }
        self
    }

    /// Add a single parent ID to a specific named subtree's history.
    /// If the subtree does not exist, it will be created with no data (`None`).
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
            node.parents.push(parent_id.into());
        } else {
            self.subtrees.push(SubTreeNode {
                name: subtree_name,
                data: None,
                parents: vec![parent_id.into()],
                height: None,
            });
        }
        self
    }

    /// Mutable reference version of add_subtree_parent.
    /// Add a single parent ID to a specific named subtree's history.
    /// If the subtree does not exist, it will be created with no data (`None`).
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
            node.parents.push(parent_id.into());
        } else {
            self.subtrees.push(SubTreeNode {
                name: subtree_name,
                data: None,
                parents: vec![parent_id.into()],
                height: None,
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

    /// Get the current metadata value for this entry builder.
    ///
    /// Metadata is optional information attached to an entry that is not part of the
    /// main data model and is not merged between entries. It's used primarily for
    /// improving efficiency of operations and for experimentation.
    ///
    /// # Returns
    ///
    /// Returns `Some(&RawData)` containing the serialized metadata if present,
    /// or `None` if no metadata has been set.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let builder = Entry::builder("root_id");
    /// assert!(builder.metadata().is_none());
    ///
    /// let builder = builder.set_metadata(r#"{"custom": "data"}"#);
    /// assert!(builder.metadata().is_some());
    /// ```
    pub fn metadata(&self) -> Option<&RawData> {
        self.tree.metadata.as_ref()
    }

    /// Set the height for this entry in the main tree DAG.
    ///
    /// # Arguments
    /// * `height` - The height value for this entry
    pub fn set_height(mut self, height: u64) -> Self {
        self.tree.height = height;
        self
    }

    /// Mutable reference version of set_height.
    pub fn set_height_mut(&mut self, height: u64) -> &mut Self {
        self.tree.height = height;
        self
    }

    /// Set the height for this entry in a specific subtree's DAG.
    ///
    /// If the subtree does not exist, it will be created with no data (`None`).
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree
    /// * `height` - The height value for this entry in the subtree
    pub fn set_subtree_height(
        mut self,
        subtree_name: impl Into<String>,
        height: Option<u64>,
    ) -> Self {
        let subtree_name = subtree_name.into();
        if let Some(node) = self
            .subtrees
            .iter_mut()
            .find(|node| node.name == subtree_name)
        {
            node.height = height;
        } else {
            self.subtrees.push(SubTreeNode {
                name: subtree_name,
                data: None,
                parents: vec![],
                height,
            });
        }
        self
    }

    /// Mutable reference version of set_subtree_height.
    pub fn set_subtree_height_mut(
        &mut self,
        subtree_name: impl Into<String>,
        height: Option<u64>,
    ) -> &mut Self {
        let subtree_name = subtree_name.into();
        if let Some(node) = self
            .subtrees
            .iter_mut()
            .find(|node| node.name == subtree_name)
        {
            node.height = height;
        } else {
            self.subtrees.push(SubTreeNode {
                name: subtree_name,
                data: None,
                parents: vec![],
                height,
            });
        }
        self
    }

    /// Build and return the final immutable `Entry`.
    ///
    /// This method:
    /// 1. Sorts all parent lists in both the main tree and subtrees
    /// 2. Sorts the subtrees list by name
    /// 3. Removes any empty subtrees
    /// 4. Creates the immutable `Entry`
    /// 5. Validates the entry structure
    /// 6. Returns the validated entry or an error
    ///
    /// After calling this method, the builder is consumed and cannot be used again.
    /// The returned `Entry` is immutable and its parts cannot be modified.
    ///
    /// # Returns
    ///
    /// - `Ok(Entry)` if the entry is structurally valid
    /// - `Err(crate::Error)` if the entry fails validation (e.g., root entry with parents)
    pub fn build(mut self) -> Result<Entry> {
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

        let entry = Entry {
            version: ENTRY_VERSION,
            tree: self.tree,
            subtrees: self.subtrees,
            sig: self.sig,
        };

        // Validate the built entry before returning
        entry.validate()?;

        Ok(entry)
    }
}
