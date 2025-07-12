//! Tree module provides functionality for managing collections of related entries.
//!
//! A `Tree` represents a hierarchical structure of entries, similar to a table in a database
//! or a branch in a version control system. Each tree has a root entry and maintains
//! the history and relationships between entries, interfacing with a backend storage system.

use crate::Result;
use crate::atomicop::AtomicOp;
use crate::backend::Database;
use crate::basedb::errors::BaseError;
use crate::constants::{ROOT, SETTINGS};
use crate::crdt::Map;
use crate::crdt::map::Value;
use crate::entry::{Entry, ID};
use crate::subtree::{Dict, SubTree};

use crate::auth::crypto::format_public_key;
use crate::auth::settings::AuthSettings;
use crate::auth::types::{AuthKey, KeyStatus, Permission};
use rand::{Rng, distributions::Alphanumeric};
use serde_json;
use std::sync::Arc;

/// Represents a collection of related entries, analogous to a table or a branch in a version control system.
///
/// Each `Tree` is identified by the ID of its root `Entry` and manages the history of data
/// associated with that root. It interacts with the underlying `Backend` for storage.
#[derive(Clone)]
pub struct Tree {
    root: ID,
    backend: Arc<dyn Database>,
    /// Default authentication key ID for operations on this tree
    default_auth_key: Option<String>,
}

impl Tree {
    /// Creates a new `Tree` instance.
    ///
    /// Initializes the tree by creating a root `Entry` containing the provided settings
    /// and storing it in the backend. All trees must now be created with authentication.
    ///
    /// # Arguments
    /// * `settings` - A `Map` CRDT containing the initial settings for the tree.
    /// * `backend` - An `Arc<Mutex<>>` protected reference to the backend where the tree's entries will be stored.
    /// * `signing_key_id` - Authentication key ID to use for the initial commit. Required for all trees.
    ///
    /// # Returns
    /// A `Result` containing the new `Tree` instance or an error.
    pub fn new(
        initial_settings: Map,
        backend: Arc<dyn Database>,
        signing_key_id: impl AsRef<str>,
    ) -> Result<Self> {
        let signing_key_id = signing_key_id.as_ref();
        // Check if auth is configured in the initial settings
        let auth_configured = matches!(initial_settings.get("auth"), Some(Value::Map(auth_map)) if !auth_map.as_hashmap().is_empty());

        let (super_user_key_id, final_tree_settings) = if auth_configured {
            // Auth settings are already provided - use them as-is with the provided signing key
            (signing_key_id.to_string(), initial_settings)
        } else {
            // No auth config provided - bootstrap auth configuration with the provided key
            // Verify the key exists first
            let _private_key = backend.get_private_key(signing_key_id)?.ok_or_else(|| {
                BaseError::SigningKeyNotFound {
                    key_id: signing_key_id.to_string(),
                }
            })?;

            // Bootstrap auth configuration with the provided key
            let private_key = backend.get_private_key(signing_key_id)?.unwrap();
            let public_key = private_key.verifying_key();

            // Create auth settings with the provided key
            let mut auth_settings_handler = AuthSettings::new();
            let super_user_auth_key = AuthKey {
                pubkey: format_public_key(&public_key),
                permissions: Permission::Admin(0), // Highest priority
                status: KeyStatus::Active,
            };
            auth_settings_handler.add_key(signing_key_id, super_user_auth_key)?;

            // Prepare final tree settings for the initial commit
            let mut final_tree_settings = initial_settings.clone();
            final_tree_settings.set_map("auth", auth_settings_handler.as_kvnested().clone());

            (signing_key_id.to_string(), final_tree_settings)
        };

        // Create the initial root entry using a temporary Tree and AtomicOp
        // This placeholder ID should not exist in the backend, so get_tips will be empty.
        let bootstrap_placeholder_id = format!(
            "bootstrap_root_{}",
            rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(10)
                .map(char::from)
                .collect::<String>()
        );

        let temp_tree_for_bootstrap = Tree {
            root: bootstrap_placeholder_id.clone().into(),
            backend: backend.clone(),
            default_auth_key: Some(super_user_key_id.clone()),
        };

        // Create the operation. If we have an auth key, it will be used automatically
        let op = temp_tree_for_bootstrap.new_operation()?;

        // IMPORTANT: For the root entry, we need to set the tree root to empty string
        // so that is_toplevel_root() returns true and all_roots() can find it
        op.set_entry_root("")?;

        // Populate the SETTINGS and ROOT subtrees for the very first entry
        op.update_subtree(SETTINGS, &serde_json::to_string(&final_tree_settings)?)?;
        op.update_subtree(ROOT, &serde_json::to_string(&"".to_string())?)?; // Standard practice for root entry's _root

        // Commit the initial entry
        let new_root_id = op.commit()?;

        // Now create the real tree with the new_root_id
        Ok(Self {
            root: new_root_id,
            backend,
            default_auth_key: Some(super_user_key_id),
        })
    }

    /// Creates a new `Tree` instance from an existing ID.
    ///
    /// This constructor takes an existing `ID` and an `Arc<dyn Backend>`
    /// and constructs a `Tree` instance with the specified root ID.
    ///
    /// # Arguments
    /// * `id` - The `ID` of the root entry.
    /// * `backend` - An `Arc<dyn Backend>` reference to the backend where the tree's entries will be stored.
    ///
    /// # Returns
    /// A `Result` containing the new `Tree` instance or an error.
    pub(crate) fn new_from_id(id: ID, backend: Arc<dyn Database>) -> Result<Self> {
        Ok(Self {
            root: id,
            backend,
            default_auth_key: None,
        })
    }

    /// Set the default authentication key ID for operations on this tree.
    ///
    /// When set, all operations created via `new_operation()` will automatically
    /// use this key for signing unless explicitly overridden.
    ///
    /// # Arguments
    /// * `key_id` - The identifier of the private key to use by default
    pub fn set_default_auth_key(&mut self, key_id: impl Into<String>) {
        self.default_auth_key = Some(key_id.into());
    }

    /// Clear the default authentication key for this tree.
    pub fn clear_default_auth_key(&mut self) {
        self.default_auth_key = None;
    }

    /// Get the default authentication key ID for this tree.
    pub fn default_auth_key(&self) -> Option<&str> {
        self.default_auth_key.as_deref()
    }

    /// Create a new atomic operation on this tree with authentication.
    ///
    /// This is a convenience method that creates an operation and sets the authentication
    /// key in one call.
    ///
    /// # Arguments
    /// * `key_id` - The identifier of the private key to use for signing
    ///
    /// # Returns
    /// A `Result<AtomicOp>` containing the new authenticated operation
    pub fn new_authenticated_operation(&self, key_id: impl AsRef<str>) -> Result<AtomicOp> {
        let op = self.new_operation()?;
        Ok(op.with_auth(key_id.as_ref()))
    }

    /// Get the ID of the root entry
    pub fn root_id(&self) -> &ID {
        &self.root
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Arc<dyn Database> {
        &self.backend
    }

    /// Retrieve the root entry from the backend
    pub fn get_root(&self) -> Result<Entry> {
        self.backend.get(&self.root)
    }

    /// Get a settings store for the tree.
    ///
    /// Returns a Dict subtree for managing the tree's settings.
    ///
    /// # Returns
    /// A `Result` containing the `Dict` for settings or an error.
    pub fn get_settings(&self) -> Result<Dict> {
        self.get_subtree_viewer::<Dict>(SETTINGS)
    }

    /// Get the name of the tree from its settings subtree
    pub fn get_name(&self) -> Result<String> {
        // Get the settings subtree
        let settings = self.get_settings()?;

        // Get the name from the settings
        settings.get_string("name")
    }

    /// Create a new atomic operation on this tree
    ///
    /// This creates a new atomic operation containing a new Entry.
    /// The atomic operation will be initialized with the current state of the tree.
    /// If a default authentication key is set, the operation will use it for signing.
    ///
    /// # Returns
    /// A `Result<AtomicOp>` containing the new atomic operation
    pub fn new_operation(&self) -> Result<AtomicOp> {
        let tips = self.get_tips()?;
        self.new_operation_with_tips(&tips)
    }

    /// Create a new atomic operation on this tree with specific parent tips
    ///
    /// This creates a new atomic operation that will have the specified entries as parents
    /// instead of using the current tree tips. This allows creating complex DAG structures
    /// like diamond patterns for testing and advanced use cases.
    ///
    /// # Arguments
    /// * `tips` - The specific parent tips to use for this operation
    ///
    /// # Returns
    /// A `Result<AtomicOp>` containing the new atomic operation
    pub fn new_operation_with_tips(&self, tips: impl AsRef<[ID]>) -> Result<AtomicOp> {
        let mut op = AtomicOp::new_with_tips(self, tips.as_ref())?;

        // Set default authentication if configured
        if let Some(ref key_id) = self.default_auth_key {
            op.set_auth_key(key_id);
        }

        Ok(op)
    }

    /// Insert an entry into the tree without modifying it.
    /// This is primarily for testing purposes or when you need full control over the entry.
    /// Note: Since all entries must now be authenticated, this method assumes the entry
    /// is already properly signed and verified.
    pub fn insert_raw(&self, entry: Entry) -> Result<ID> {
        let id = entry.id();

        self.backend.put_verified(entry)?;

        Ok(id)
    }

    /// Get a SubTree type that will handle accesses to the SubTree
    /// This will return a SubTree initialized to point at the current state of the tree.
    ///
    /// The returned subtree should NOT be used to modify the tree, as it intentionally does not
    /// expose the AtomicOp.
    pub fn get_subtree_viewer<T>(&self, name: impl Into<String>) -> Result<T>
    where
        T: SubTree,
    {
        let op = self.new_operation()?;
        T::new(&op, name)
    }

    /// Get the current tips (leaf entries) of the main tree branch.
    ///
    /// Tips represent the latest entries in the tree's main history, forming the heads of the DAG.
    ///
    /// # Returns
    /// A `Result` containing a vector of `ID`s for the tip entries or an error.
    pub fn get_tips(&self) -> Result<Vec<ID>> {
        self.backend.get_tips(&self.root)
    }

    /// Get the full `Entry` objects for the current tips of the main tree branch.
    ///
    /// # Returns
    /// A `Result` containing a vector of the tip `Entry` objects or an error.
    pub fn get_tip_entries(&self) -> Result<Vec<Entry>> {
        let tips = self.backend.get_tips(&self.root)?;
        let entries: Result<Vec<_>> = tips.iter().map(|id| self.backend.get(id)).collect();
        entries
    }

    /// Get a single entry by ID from this tree.
    ///
    /// This is the primary method for retrieving entries after commit operations.
    /// It provides safe, high-level access to entry data without exposing backend details.
    ///
    /// The method verifies that the entry belongs to this tree by checking its root ID.
    /// If the entry exists but belongs to a different tree, an error is returned.
    ///
    /// # Arguments
    /// * `entry_id` - The ID of the entry to retrieve (accepts anything that converts to ID/String)
    ///
    /// # Returns
    /// A `Result` containing the `Entry` or an error if not found or not part of this tree
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::basedb::BaseDB;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::crdt::Map;
    /// # fn main() -> Result<()> {
    /// # let backend = Box::new(InMemory::new());
    /// # let db = BaseDB::new(backend);
    /// # db.add_private_key("TEST_KEY")?;
    /// # let tree = db.new_tree(Map::new(), "TEST_KEY")?;
    /// # let op = tree.new_operation()?;
    /// let entry_id = op.commit()?;
    /// let entry = tree.get_entry(&entry_id)?;           // Using &String
    /// let entry = tree.get_entry("some_entry_id")?;     // Using &str
    /// let entry = tree.get_entry(entry_id.clone())?;    // Using String
    /// println!("Entry signature: {:?}", entry.sig);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_entry<I: Into<ID>>(&self, entry_id: I) -> Result<Entry> {
        let id = entry_id.into();
        let entry = self.backend.get(&id)?;

        // Check if the entry belongs to this tree
        if !entry.in_tree(&self.root) {
            return Err(BaseError::EntryNotInTree {
                entry_id: id,
                tree_id: self.root.clone(),
            }
            .into());
        }

        Ok(entry)
    }

    /// Get multiple entries by ID efficiently.
    ///
    /// This method retrieves multiple entries in a single backend lock acquisition,
    /// making it more efficient than multiple `get_entry()` calls.
    ///
    /// The method verifies that all entries belong to this tree by checking their root IDs.
    /// If any entry exists but belongs to a different tree, an error is returned.
    ///
    /// # Arguments
    /// * `entry_ids` - An iterable of entry IDs to retrieve (accepts anything that converts to ID/String)
    ///
    /// # Returns
    /// A `Result` containing a vector of `Entry` objects or an error if any entry is not found or not part of this tree
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::basedb::BaseDB;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::crdt::Map;
    /// # fn main() -> Result<()> {
    /// # let backend = Box::new(InMemory::new());
    /// # let db = BaseDB::new(backend);
    /// # db.add_private_key("TEST_KEY")?;
    /// # let tree = db.new_tree(Map::new(), "TEST_KEY")?;
    /// let entry_ids = vec!["id1", "id2", "id3"];
    /// let entries = tree.get_entries(entry_ids)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_entries<I, T>(&self, entry_ids: I) -> Result<Vec<Entry>>
    where
        I: IntoIterator<Item = T>,
        T: Into<ID>,
    {
        entry_ids
            .into_iter()
            .map(|entry_id| {
                let id = entry_id.into();
                let entry = self.backend.get(&id)?;

                // Check if the entry belongs to this tree
                if !entry.in_tree(&self.root) {
                    return Err(BaseError::EntryNotInTree {
                        entry_id: id,
                        tree_id: self.root.clone(),
                    }
                    .into());
                }

                Ok(entry)
            })
            .collect()
    }

    // === AUTHENTICATION HELPERS ===

    /// Verify an entry's signature and authentication against the tree's configuration that was valid at the time of entry creation.
    ///
    /// This method validates that:
    /// 1. The entry belongs to this tree
    /// 2. The entry is properly signed with a key that was authorized in the tree's authentication settings at the time the entry was created
    /// 3. The signature is cryptographically valid
    ///
    /// The method uses the entry's metadata to determine which authentication settings were active when the entry was signed,
    /// ensuring that entries remain valid even if keys are later revoked or settings change.
    ///
    /// # Arguments
    /// * `entry_id` - The ID of the entry to verify (accepts anything that converts to ID/String)
    ///
    /// # Returns
    /// A `Result` containing `true` if the entry is valid and properly authenticated, `false` if authentication fails
    ///
    /// # Errors
    /// Returns an error if:
    /// - The entry is not found
    /// - The entry does not belong to this tree
    /// - The entry's metadata cannot be parsed
    /// - The historical authentication settings cannot be retrieved
    pub fn verify_entry_signature<I: Into<ID>>(&self, entry_id: I) -> Result<bool> {
        let entry = self.get_entry(entry_id)?;

        // If the entry has no authentication, it's considered valid for backward compatibility
        if entry.sig.key == crate::auth::types::SigKey::default() {
            return Ok(true);
        }

        // Get the authentication settings that were valid at the time this entry was created
        let historical_settings = self.get_historical_settings_for_entry(&entry)?;

        // Use the authentication validator with historical settings
        let mut validator = crate::auth::validation::AuthValidator::new();
        validator.validate_entry(&entry, &historical_settings, Some(&self.backend))
    }

    /// Get the authentication settings that were valid when a specific entry was created.
    ///
    /// This method examines the entry's metadata to find the settings tips that were active
    /// at the time of entry creation, then reconstructs the historical settings state.
    ///
    /// # Arguments
    /// * `entry` - The entry to get historical settings for
    ///
    /// # Returns
    /// A `Result` containing the historical settings data
    fn get_historical_settings_for_entry(&self, _entry: &Entry) -> Result<Map> {
        // TODO: Implement full historical settings reconstruction from entry metadata
        // For now, use current settings for simplicity and backward compatibility
        //
        // The complete implementation would:
        // 1. Parse entry metadata to get settings tips active at entry creation time
        // 2. Reconstruct the CRDT state from those historical tips
        // 3. Validate against that historical state
        //
        // This ensures entries remain valid even if keys are later revoked,
        // but requires more complex CRDT state reconstruction logic.

        let settings = self.get_settings()?;
        settings.get_all()
    }

    // === TREE QUERIES ===

    /// Get all entries in this tree.
    ///
    /// ⚠️ **Warning**: This method loads all entries into memory. Use with caution on large trees.
    /// Consider using `get_tips()` or `get_tip_entries()` for more efficient access patterns.
    ///
    /// # Returns
    /// A `Result` containing a vector of all `Entry` objects in the tree
    pub fn get_all_entries(&self) -> Result<Vec<Entry>> {
        self.backend.get_tree(&self.root)
    }
}
