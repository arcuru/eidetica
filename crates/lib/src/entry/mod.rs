//!
//! Defines the fundamental data unit (`Entry`) and related types.
//!
//! An `Entry` is the core, content-addressable building block of the database,
//! representing a snapshot of data in the main tree and potentially multiple named subtrees.
//! This module also defines the `ID` type and `RawData` type.

mod builder;
pub mod errors;
pub mod id;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

pub use builder::EntryBuilder;
pub use errors::EntryError;
pub use id::ID;

use crate::{Error, Result, auth::types::SigInfo, constants::ROOT, store::StoreError};

use id::IdError;

/// Represents serialized data, typically JSON, provided by the user.
///
/// This allows users to manage their own data structures and serialization formats.
pub type RawData = String;

/// Helper to check if tree height is zero for serde skip_serializing_if
fn is_zero(h: &u64) -> bool {
    *h == 0
}

/// Internal representation of the main tree node within an `Entry`.
#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct TreeNode {
    /// The ID of the root `Entry` of the tree this node belongs to.
    pub root: ID,
    /// IDs of the parent `Entry`s in the main tree history.
    /// The vector is kept sorted alphabetically.
    pub parents: Vec<ID>,
    /// Serialized metadata associated with this `Entry` in the main tree.
    /// This data is metadata about this specific entry only and is not merged with other entries.
    ///
    /// Metadata is used to improve the efficiency of certain operations and for experimentation.
    ///
    /// Metadata is optional and may not be present in all entries. Future versions
    /// may extend metadata to include additional information.
    pub metadata: Option<RawData>,
    /// Height of this entry in the tree DAG (longest path from root).
    /// Root entries have height 0, children have max(parent heights) + 1.
    #[serde(rename = "h", default, skip_serializing_if = "is_zero")]
    pub height: u64,
}

/// Internal representation of a named subtree node within an `Entry`.
#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct SubTreeNode {
    /// The name of the subtree, analogous to a table name.
    /// Subtrees are _named_, and not identified by an ID.
    pub name: String,
    /// IDs of the parent `Entry`s specific to this subtree's history.
    /// The vector is kept sorted alphabetically.
    pub parents: Vec<ID>,
    /// Serialized data specific to this `Entry` within this named subtree.
    ///
    /// `None` indicates that this Entry participates in the subtree but makes no data changes.
    /// This is used when there is information needed for this subtree found somewhere else (e.g. the `_index`)
    ///
    /// `Some(data)` contains the actual serialized data for this subtree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<RawData>,
    /// Height of this entry in the subtree DAG.
    ///
    /// `None` means the subtree inherits the tree's height (not serialized).
    /// `Some(h)` is an independent height for subtrees with their own strategy.
    #[serde(rename = "h", default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
}

/// The fundamental unit of data in Eidetica, representing a finalized, immutable Database Entry.
///
/// An `Entry` represents a snapshot of data within a `Database` and potentially one or more named `Store`s.
/// It is content-addressable, meaning its `ID` is a cryptographic hash of its contents.
/// Entries form a Merkle-DAG (Directed Acyclic Graph) structure through parent references.
///
/// # Authentication
///
/// Each entry contains authentication information with:
/// - `sig`: Base64-encoded cryptographic signature (optional, allows unsigned entry creation)
/// - `key`: Authentication key reference path, either:
///   - A direct key ID defined in this tree's `_settings.auth`
///   - A delegation path as an ordered list of `{"key": "delegated_tree_1", "tips": ["A", "B"]}`
///     where the last element must contain only a `"key"` field
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
/// # use eidetica::Entry;
///
/// // Create a new root entry (standalone entry that starts a new DAG)
/// let entry = Entry::root_builder()
///     .set_subtree_data("users", r#"{"user1":"data"}"#)
///     .build()
///     .expect("Entry should build successfully");
///
/// // Access entry data
/// let id = entry.id(); // Calculate content-addressable ID
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
/// let builder = Entry::builder(root_id);
///
/// // For a new top-level tree root:
/// let root_builder = Entry::root_builder();
/// ```
/// The current entry format version.
/// v0 indicates this is an unstable protocol subject to breaking changes.
pub const ENTRY_VERSION: u8 = 0;

/// Helper to check if version is default (0) for serde skip_serializing_if
fn is_v0(v: &u8) -> bool {
    *v == 0
}

/// Validates the entry version during deserialization.
fn validate_entry_version<'de, D>(deserializer: D) -> std::result::Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let version = u8::deserialize(deserializer)?;
    if version != ENTRY_VERSION {
        return Err(serde::de::Error::custom(format!(
            "unsupported Entry version {version}; only version {ENTRY_VERSION} is supported"
        )));
    }
    Ok(version)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    /// Protocol version for this entry format.
    /// Used to verify that we support reading this entry.
    #[serde(
        rename = "_v",
        default,
        skip_serializing_if = "is_v0",
        deserialize_with = "validate_entry_version"
    )]
    version: u8,
    /// The main tree node data, including the root ID, parents in the main tree, and associated data.
    pub(super) tree: TreeNode,
    /// A collection of named subtrees this entry contains data for.
    /// The vector is kept sorted alphabetically by subtree name during the build process.
    pub(super) subtrees: Vec<SubTreeNode>,
    /// Authentication information for this entry
    pub sig: SigInfo,
}

impl Entry {
    /// Creates a new `EntryBuilder` for an entry associated with a specific tree root.
    /// This is a convenience method and preferred over calling `EntryBuilder::new()` directly.
    ///
    /// # Arguments
    /// * `root` - The `ID` of the root `Entry` of the tree this entry will belong to.
    pub fn builder(root: impl Into<ID>) -> EntryBuilder {
        EntryBuilder::new(root)
    }

    /// Creates a new `EntryBuilder` for a top-level (root) entry for a new tree.
    /// This is a convenience method and preferred over calling `EntryBuilder::new_top_level()` directly.
    ///
    /// Root entries have an empty string as their `root` ID and include a special ROOT subtree marker.
    /// This method is typically used when creating a new tree.
    pub fn root_builder() -> EntryBuilder {
        EntryBuilder::new_top_level()
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
        ID::from_bytes(json)
    }

    /// Get the ID of the root `Entry` of the tree this entry belongs to.
    pub fn root(&self) -> ID {
        self.tree.root.clone()
    }

    /// Check if this entry is a root entry (contains the ROOT marker and has no parents).
    ///
    /// Root entries are the top-level entries in the database and are distinguished by:
    /// 1. Containing a subtree with the ROOT marker
    /// 2. Having no parent entries (they are true tree roots)
    ///
    /// This ensures that root entries are actual starting points of trees in the DAG.
    pub fn is_root(&self) -> bool {
        self.subtrees.iter().any(|node| node.name == ROOT) && self.tree.parents.is_empty()
    }

    /// Check if this entry contains data for a specific named subtree.
    pub fn in_subtree(&self, subtree_name: impl AsRef<str>) -> bool {
        self.subtrees
            .iter()
            .any(|node| node.name == subtree_name.as_ref())
    }

    /// Check if this entry belongs to a specific tree, identified by its root ID.
    pub fn in_tree(&self, tree_id: impl AsRef<str>) -> bool {
        // Entries that are roots exist in both trees
        self.root() == tree_id.as_ref() || (self.id().as_str() == tree_id.as_ref())
    }

    /// Get the names of all subtrees this entry contains data for.
    /// The names are returned in alphabetical order.
    pub fn subtrees(&self) -> Vec<String> {
        self.subtrees
            .iter()
            .map(|subtree| subtree.name.clone())
            .collect()
    }

    /// Get the metadata associated with this entry's tree node.
    ///
    /// Metadata is optional information attached to an entry that is not part of the
    /// main data model and is not merged between entries.
    pub fn metadata(&self) -> Option<&RawData> {
        self.tree.metadata.as_ref()
    }

    /// Get the `RawData` for a specific named subtree within this entry.
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

    /// Get the IDs of the parent entries in the main tree history.
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

    /// Get the height of this entry in the main tree DAG.
    pub fn height(&self) -> u64 {
        self.tree.height
    }

    /// Get the height of this entry in a specific subtree's DAG.
    ///
    /// If the subtree has an explicit height (`Some(h)`), that value is returned.
    /// If the subtree height is `None`, it inherits from the main tree height.
    ///
    /// This allows subtrees to either track independent heights (for subtrees
    /// with their own height strategy) or share the tree's height (default).
    pub fn subtree_height(&self, subtree_name: impl AsRef<str>) -> Result<u64> {
        self.subtrees
            .iter()
            .find(|node| node.name == subtree_name.as_ref())
            .map(|node| node.height.unwrap_or_else(|| self.height()))
            .ok_or_else(|| {
                StoreError::KeyNotFound {
                    store: "entry".to_string(),
                    key: subtree_name.as_ref().to_string(),
                }
                .into()
            })
    }

    /// Create a canonical representation of this entry for signing purposes.
    ///
    /// This creates a copy of the entry with the signature field removed from auth,
    /// which is necessary for signature generation and verification.
    /// The returned entry has deterministic field ordering for consistent signatures.
    pub fn canonical_for_signing(&self) -> Self {
        let mut canonical = self.clone();
        canonical.sig.sig = None;
        canonical
    }

    /// Create canonical bytes for signing or ID generation.
    ///
    /// This method serializes the entry to JSON with deterministic field ordering.
    /// For signing purposes, call `canonical_for_signing()` first.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let json = serde_json::to_string(self).map_err(Error::Serialize)?;
        Ok(json.into_bytes())
    }

    /// Create canonical bytes for signing (convenience method).
    ///
    /// This combines `canonical_for_signing()` and `canonical_bytes()` for convenience.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        self.canonical_for_signing().canonical_bytes()
    }

    /// Validate the structural integrity of this entry.
    ///
    /// This method performs lightweight structural validation that can be done
    /// without access to the backend database. It checks for obvious structural
    /// issues while deferring complex DAG relationship validation to the transaction
    /// and backend layers where full database access is available.
    ///
    /// # Validation Rules
    ///
    /// ## Critical Main Tree Parent Validation (Prevents "No Common Ancestor" Errors)
    /// - **Root entries** (containing "_root" subtree): May have empty parents
    /// - **Non-root entries**: MUST have at least one parent - **HARD REQUIREMENT**
    /// - **Empty parent IDs**: Always rejected as invalid
    ///
    /// This strict enforcement prevents orphaned entries that cause sync failures.
    ///
    /// ## Subtree Parent Relationships
    /// - For root entries: Subtrees may have empty parents (they establish the subtree roots)
    /// - For non-root entries: Empty subtree parents require deeper validation:
    ///   - Could be legitimate (first entry in a new subtree)
    ///   - Could indicate broken relationships (needs DAG traversal to verify)
    ///
    /// ## Multi-Layer Validation System
    /// Complex validation happens at multiple layers:
    /// 1. **Entry Layer** (this method): Structural validation, main tree parent enforcement
    /// 2. **Transaction Layer**: Parent discovery, subtree parent validation with DAG access
    /// 3. **Backend Storage**: Final validation gate before persistence
    /// 4. **Sync Operations**: Validation of entries received from peers
    ///
    /// # Special Cases
    /// - The "_root" marker subtree has special handling and skips validation
    /// - The "_settings" subtree follows standard validation rules
    /// - Empty subtree parents are logged but deferred to transaction layer
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the entry is structurally valid
    /// - `Err(InstanceError::EntryValidationFailed)` if validation fails with specific reason
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Entry;
    /// # let entry: Entry = unimplemented!();
    /// // Validate an entry before storage or sync
    /// match entry.validate() {
    ///     Ok(()) => {
    ///         // Entry is valid, safe to store/sync
    ///         println!("Entry is valid");
    ///     }
    ///     Err(e) => {
    ///         // Entry is invalid, reject it
    ///         eprintln!("Invalid entry: {}", e);
    ///     }
    /// }
    /// ```
    /// Validates that an ID is in the correct format for the hash algorithm used.
    ///
    /// This function now supports multiple hash algorithms and uses the structured
    /// ID validation from the ID type itself.
    fn validate_id_format(id: &ID, context: &str) -> Result<()> {
        // Use the ID's built-in validation by attempting to parse its string representation
        // This ensures we validate according to the actual algorithm and format rules
        if let Err(id_err) = ID::parse(id.as_str()) {
            // Add context to the error and convert through the error system
            let contextual_err = match &id_err {
                IdError::InvalidFormat(_) => IdError::InvalidFormat(format!(
                    "Invalid ID format in {}: {}",
                    context,
                    id.as_str()
                )),
                IdError::InvalidHex(_) => IdError::InvalidHex(format!(
                    "Invalid hex characters in {} ID: {}",
                    context,
                    id.as_str()
                )),
                // For length and algorithm errors, the original error is sufficient
                _ => id_err,
            };
            return Err(contextual_err.into());
        }

        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        use crate::constants::{ROOT, SETTINGS};
        use crate::instance::errors::InstanceError;

        // CRITICAL VALIDATION: Root entries (with _root marker) cannot have parents
        // This enforces that root entries are true starting points of trees
        let has_root_marker = self.subtrees.iter().any(|node| node.name == ROOT);
        if has_root_marker && !self.tree.parents.is_empty() {
            return Err(InstanceError::EntryValidationFailed {
                reason: format!(
                    "Entry {} has _root marker but also has parents. Root entries cannot have parent relationships as they are the starting points of trees.",
                    self.id()
                ),
            }.into());
        }

        // Check if this is a root entry (will be true only if has ROOT marker AND no parents)
        let is_root_entry = has_root_marker && self.tree.parents.is_empty();

        // Validate root ID format (when not empty)
        if !self.tree.root.is_empty() {
            Self::validate_id_format(&self.tree.root, "tree root ID")?;
        }

        // Validate each subtree
        for subtree_node in &self.subtrees {
            let subtree_name = &subtree_node.name;
            let subtree_parents = &subtree_node.parents;

            // Empty string is not allowed as a subtree name
            if subtree_name.is_empty() {
                return Err(InstanceError::EntryValidationFailed {
                    reason: format!(
                        "Entry {} has a subtree with empty name. Store names must be non-empty.",
                        self.id()
                    ),
                }
                .into());
            }

            // Skip validation for the special "_root" marker subtree
            if subtree_name == ROOT {
                continue;
            }

            // For non-root entries with empty subtree parents, this is only valid if:
            // 1. The entry has no main parents (making it a legitimate subtree root), OR
            // 2. The subtree is genuinely being established for the first time within the tree
            //
            // Note: We can't perform deep validation here without access to the backend,
            // so we defer complex validation to transaction/backend layers where full
            // DAG traversal is possible. This basic validation catches obvious structural errors.
            if !is_root_entry && subtree_parents.is_empty() {
                // This is a lightweight structural check - more comprehensive validation
                // happens in transaction/backend layers with full DAG access
                tracing::debug!(
                    entry_id = %self.id(),
                    subtree = subtree_name,
                    "Entry has empty subtree parents - will be validated in transaction layer"
                );
            }

            // Special validation for the critical "_settings" subtree
            // Note: Settings subtree follows the same rules as other subtrees - empty parents
            // are valid for the first entry in the subtree. Comprehensive validation happens
            // in transaction/backend layers with full DAG access.
            if subtree_name == SETTINGS && !is_root_entry && subtree_parents.is_empty() {
                tracing::debug!(
                    entry_id = %self.id(),
                    "Settings subtree has empty parents - will be validated in transaction layer"
                );
            }

            // Validate that subtree parents are not empty strings and have valid format
            for parent_id in subtree_parents {
                if parent_id.is_empty() {
                    return Err(InstanceError::EntryValidationFailed {
                        reason: format!(
                            "Entry {} has subtree '{}' with empty parent ID. Parent IDs must be non-empty valid entry IDs.",
                            self.id(),
                            subtree_name
                        ),
                    }.into());
                }
                // Validate parent ID format
                Self::validate_id_format(
                    parent_id,
                    &format!("subtree '{subtree_name}' parent ID"),
                )?;
            }
        }

        // Enforce main tree parent requirements
        if !is_root_entry {
            let main_parents = self.tree.parents.clone();
            if main_parents.is_empty() {
                // This is a HARD FAILURE - reject the entry completely
                // Empty main tree parents create orphaned nodes that break LCA calculations
                return Err(InstanceError::EntryValidationFailed {
                    reason: format!(
                        "Non-root entry {} has empty main tree parents. All non-root entries must have valid parent relationships in the main tree.",
                        self.id()
                    ),
                }.into());
            }

            // Validate that main parents are not empty strings and have valid format
            for parent_id in &main_parents {
                if parent_id.is_empty() {
                    return Err(InstanceError::EntryValidationFailed {
                        reason: format!(
                            "Entry {} has empty parent ID in main tree. Parent IDs must be non-empty valid entry IDs.",
                            self.id()
                        ),
                    }.into());
                }
                // Validate parent ID format
                Self::validate_id_format(parent_id, "main tree parent ID")?;
            }
        }

        Ok(())
    }
}
