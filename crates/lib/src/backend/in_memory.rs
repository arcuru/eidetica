use crate::backend::{Backend, VerificationStatus};
use crate::entry::{Entry, ID};
use crate::{Error, Result};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::RwLock;

/// Heights cache: entry_id -> (tree_height, subtree_name -> subtree_height)
type TreeHeightsCache = HashMap<ID, (usize, HashMap<String, usize>)>;

/// Grouped tree tips cache: (tree_tips, subtree_name -> subtree_tips)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TreeTipsCache {
    tree_tips: HashSet<ID>,
    subtree_tips: HashMap<String, HashSet<ID>>,
}

/// A simple in-memory backend implementation using a `HashMap` for storage.
///
/// This backend is suitable for testing, development, or scenarios where
/// data persistence is not strictly required or is handled externally
/// (e.g., by saving/loading the entire state to/from a file).
///
/// It provides basic persistence capabilities via `save_to_file` and
/// `load_from_file`, serializing the `HashMap` to JSON.
///
/// **Security Note**: Private keys are stored in memory in plaintext in this implementation.
/// This is acceptable for development and testing but should not be used in production
/// without proper encryption or hardware security module integration.
#[derive(Debug)]
pub struct InMemoryBackend {
    /// Entries storage with read-write lock for concurrent access
    entries: RwLock<HashMap<ID, Entry>>,
    /// Verification status for each entry
    verification_status: RwLock<HashMap<ID, VerificationStatus>>,
    /// Private key storage for authentication
    ///
    /// **Security Warning**: Keys are stored in memory without encryption.
    /// This is suitable for development/testing only. Production systems should use
    /// proper key management with encryption at rest.
    private_keys: RwLock<HashMap<String, SigningKey>>,
    /// Generic key-value cache for frequently computed results
    cache: RwLock<HashMap<String, String>>,
    /// Cached heights grouped by tree: tree_id -> (entry_id -> (tree_height, subtree_name -> subtree_height))
    heights: RwLock<HashMap<ID, TreeHeightsCache>>,
    /// Cached tips grouped by tree: tree_id -> (tree_tips, subtree_name -> subtree_tips)
    tips: RwLock<HashMap<ID, TreeTipsCache>>,
}

/// Serializable version of InMemoryBackend for persistence
#[derive(Serialize, Deserialize)]
struct SerializableBackend {
    entries: HashMap<ID, Entry>,
    #[serde(default)]
    verification_status: HashMap<ID, VerificationStatus>,
    /// Private keys stored as 32-byte arrays for serialization
    #[serde(default)]
    private_keys_bytes: HashMap<String, [u8; 32]>,
    /// Generic key-value cache (not serialized - cache is rebuilt on load)
    #[serde(default)]
    cache: HashMap<String, String>,
    /// Cached heights grouped by tree
    #[serde(default)]
    heights: HashMap<ID, TreeHeightsCache>,
    /// Cached tips grouped by tree
    #[serde(default)]
    tips: HashMap<ID, TreeTipsCache>,
}

impl Serialize for InMemoryBackend {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let entries = self.entries.read().unwrap().clone();
        let verification_status = self.verification_status.read().unwrap().clone();
        let private_keys = self.private_keys.read().unwrap();
        let private_keys_bytes = private_keys
            .iter()
            .map(|(k, v)| (k.clone(), v.to_bytes()))
            .collect();
        let cache = self.cache.read().unwrap().clone();
        let heights = self.heights.read().unwrap().clone();
        let tips = self.tips.read().unwrap().clone();

        let serializable = SerializableBackend {
            entries,
            verification_status,
            private_keys_bytes,
            cache,
            heights,
            tips,
        };

        serializable.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InMemoryBackend {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let serializable = SerializableBackend::deserialize(deserializer)?;

        let private_keys = serializable
            .private_keys_bytes
            .into_iter()
            .map(|(k, bytes)| {
                let signing_key = SigningKey::from_bytes(&bytes);
                (k, signing_key)
            })
            .collect();

        Ok(InMemoryBackend {
            entries: RwLock::new(serializable.entries),
            verification_status: RwLock::new(serializable.verification_status),
            private_keys: RwLock::new(private_keys),
            cache: RwLock::new(serializable.cache),
            heights: RwLock::new(serializable.heights),
            tips: RwLock::new(serializable.tips),
        })
    }
}

impl Default for InMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryBackend {
    /// Creates a new, empty `InMemoryBackend`.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            verification_status: RwLock::new(HashMap::new()),
            private_keys: RwLock::new(HashMap::new()),
            cache: RwLock::new(HashMap::new()),
            heights: RwLock::new(HashMap::new()),
            tips: RwLock::new(HashMap::new()),
        }
    }

    /// Saves the entire backend state (all entries) to a specified file as JSON.
    ///
    /// # Arguments
    /// * `path` - The path to the file where the state should be saved.
    ///
    /// # Returns
    /// A `Result` indicating success or an I/O or serialization error.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Io(std::io::Error::other(format!("Failed to serialize: {e}"))))?;
        fs::write(path, json).map_err(Error::Io)
    }

    /// Loads the backend state from a specified JSON file.
    ///
    /// If the file does not exist, a new, empty `InMemoryBackend` is returned.
    ///
    /// # Arguments
    /// * `path` - The path to the file from which to load the state.
    ///
    /// # Returns
    /// A `Result` containing the loaded `InMemoryBackend` or an I/O or deserialization error.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        if !path.as_ref().exists() {
            return Ok(Self::new());
        }

        let json = fs::read_to_string(path).map_err(Error::Io)?;
        let backend: Self = serde_json::from_str(&json)
            .map_err(|e| Error::Io(std::io::Error::other(format!("Failed to deserialize: {e}"))))?;

        Ok(backend)
    }

    /// Returns a vector containing the IDs of all entries currently stored in the backend.
    pub fn all_ids(&self) -> Vec<ID> {
        let entries = self.entries.read().unwrap();
        entries.keys().cloned().collect()
    }

    /// Helper function to check if an entry is a tip within its tree.
    ///
    /// An entry is a tip if no other entry in the same tree lists it as a parent.
    pub fn is_tip(&self, tree: &ID, entry_id: &ID) -> bool {
        // Check if any other entry has this entry as its parent
        let entries = self.entries.read().unwrap();
        for other_entry in entries.values() {
            if other_entry.root() == tree
                && other_entry.parents().unwrap_or_default().contains(entry_id)
            {
                return false;
            }
        }
        true
    }

    /// Helper function to check if an entry is a tip within a specific subtree.
    ///
    /// An entry is a subtree tip if it belongs to the subtree and no other entry
    /// *within the same subtree* lists it as a parent for that subtree.
    pub fn is_subtree_tip(&self, tree: &ID, subtree: &str, entry_id: &ID) -> bool {
        // First, check if the entry is in the subtree
        let entries = self.entries.read().unwrap();
        let entry = match entries.get(entry_id) {
            Some(e) => e,
            None => return false, // Entry doesn't exist
        };

        if !entry.in_subtree(subtree) {
            return false; // Entry is not in the subtree
        }

        // Check if any other entry has this entry as its subtree parent
        for other_entry in entries.values() {
            if other_entry.in_tree(tree)
                && other_entry.in_subtree(subtree)
                && let Ok(parents) = other_entry.subtree_parents(subtree)
                && parents.contains(entry_id)
            {
                return false; // Found a child in the subtree
            }
        }
        true
    }

    /// Helper function to update cached heights for a newly added entry.
    /// This function updates both tree heights and subtree heights if the parents are cached.
    fn update_cached_heights(cache: &mut TreeHeightsCache, entry: &Entry, entry_id: &ID) {
        let mut tree_height = None;
        let mut subtree_heights = HashMap::new();

        // Calculate tree height if parents are cached
        if let Ok(parents) = entry.parents() {
            if parents.is_empty() {
                // Root entry has height 0
                tree_height = Some(0);
            } else {
                // Check if all parents have cached heights
                let parent_heights: Vec<usize> = parents
                    .iter()
                    .filter_map(|p| cache.get(p).map(|(h, _)| *h))
                    .collect();

                if parent_heights.len() == parents.len() {
                    // All parents are cached, compute new entry's height
                    let max_parent_height = parent_heights.into_iter().max().unwrap_or(0);
                    tree_height = Some(max_parent_height + 1);
                }
            }
        }

        // Calculate subtree heights for each subtree the entry belongs to
        for subtree_name in entry.subtrees() {
            if let Ok(subtree_parents) = entry.subtree_parents(&subtree_name) {
                if subtree_parents.is_empty() {
                    // Subtree root has height 0
                    subtree_heights.insert(subtree_name.clone(), 0);
                } else {
                    // Check if all subtree parents have cached heights
                    let parent_heights: Vec<usize> = subtree_parents
                        .iter()
                        .filter_map(|p| {
                            cache
                                .get(p)
                                .and_then(|(_, sh)| sh.get(&subtree_name).copied())
                        })
                        .collect();

                    if parent_heights.len() == subtree_parents.len() {
                        // All parents are cached, compute new entry's height
                        let max_parent_height = parent_heights.into_iter().max().unwrap_or(0);
                        subtree_heights.insert(subtree_name.clone(), max_parent_height + 1);
                    }
                }
            }
        }

        // Only insert if we calculated a tree height
        if let Some(height) = tree_height {
            cache.insert(entry_id.clone(), (height, subtree_heights));
        }
    }

    /// Calculates the height of each entry within a specified tree or subtree.
    ///
    /// Uses lazy caching - computes heights on first access and caches results.
    /// Subsequent calls return cached values for O(1) lookups.
    ///
    /// Height is defined as the length of the longest path from a root node
    /// (a node with no parents *within the specified context*) to the entry.
    /// Root nodes themselves have height 0.
    /// This calculation assumes the graph formed by the entries and their parent relationships
    /// within the specified context forms a Directed Acyclic Graph (DAG).
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree context.
    /// * `subtree` - An optional subtree name. If `Some`, calculates heights within
    ///   that specific subtree context. If `None`, calculates heights within the main tree context.
    ///
    /// # Returns
    /// A `Result` containing a `HashMap` mapping entry IDs (within the context) to their
    /// calculated height, or an error if data is inconsistent (e.g., parent references).
    pub fn calculate_heights(
        &self,
        tree: &ID,
        subtree: Option<&str>,
    ) -> Result<HashMap<ID, usize>> {
        // Get all entries in the tree context
        let entries = self.entries.read().unwrap();
        let entries_in_tree: Vec<_> = entries
            .iter()
            .filter(|(_, entry)| entry.in_tree(tree))
            .map(|(id, _)| id.clone())
            .collect();

        match subtree {
            None => {
                // Tree context - check if we have tree heights cached
                let heights_cache = self.heights.read().unwrap();
                if let Some(tree_cache) = heights_cache.get(tree) {
                    // Check if all entries are cached
                    let mut all_cached = true;
                    let mut result = HashMap::new();

                    for id in &entries_in_tree {
                        if let Some((height, _)) = tree_cache.get(id) {
                            result.insert(id.clone(), *height);
                        } else {
                            all_cached = false;
                            break;
                        }
                    }

                    if all_cached {
                        return Ok(result);
                    }
                }
                drop(heights_cache);

                // Compute heights and cache them
                let computed_heights = self.calculate_heights_original(tree, subtree)?;

                // Update cache
                let mut heights_cache = self.heights.write().unwrap();
                let tree_cache = heights_cache.entry(tree.clone()).or_default();

                // Update tree heights for each entry
                for (id, height) in &computed_heights {
                    tree_cache
                        .entry(id.clone())
                        .and_modify(|(h, _)| *h = *height)
                        .or_insert((*height, HashMap::new()));
                }
                drop(heights_cache);

                Ok(computed_heights)
            }
            Some(subtree_name) => {
                // Subtree context - check if we have subtree heights cached
                let entries_in_subtree: Vec<_> = entries_in_tree
                    .iter()
                    .filter(|id| {
                        if let Some(entry) = entries.get(*id) {
                            entry.in_subtree(subtree_name)
                        } else {
                            false
                        }
                    })
                    .cloned()
                    .collect();

                let heights_cache = self.heights.read().unwrap();
                if let Some(tree_cache) = heights_cache.get(tree) {
                    // Check if all entries are cached
                    let mut all_cached = true;
                    let mut result = HashMap::new();

                    for id in &entries_in_subtree {
                        if let Some((_, subtree_map)) = tree_cache.get(id) {
                            if let Some(&height) = subtree_map.get(subtree_name) {
                                result.insert(id.clone(), height);
                            } else {
                                all_cached = false;
                                break;
                            }
                        } else {
                            all_cached = false;
                            break;
                        }
                    }

                    if all_cached {
                        return Ok(result);
                    }
                }
                drop(heights_cache);

                // Compute heights and cache them
                let computed_heights = self.calculate_heights_original(tree, subtree)?;

                // Update cache
                let mut heights_cache = self.heights.write().unwrap();
                let tree_cache = heights_cache.entry(tree.clone()).or_default();

                // Update subtree heights for each entry
                for (id, height) in &computed_heights {
                    tree_cache
                        .entry(id.clone())
                        .and_modify(|(_, sh)| {
                            sh.insert(subtree_name.to_string(), *height);
                        })
                        .or_insert((0, [(subtree_name.to_string(), *height)].into()));
                }
                drop(heights_cache);

                Ok(computed_heights)
            }
        }
    }

    /// Original height calculation implementation (fallback)
    fn calculate_heights_original(
        &self,
        tree: &ID,
        subtree: Option<&str>,
    ) -> Result<HashMap<ID, usize>> {
        let mut heights: HashMap<ID, usize> = HashMap::new();
        let mut in_degree: HashMap<ID, usize> = HashMap::new();
        // Map: parent_id -> list of child_ids *within the context*
        let mut children_map: HashMap<ID, Vec<ID>> = HashMap::new();
        // Keep track of all nodes actually in the context
        let mut nodes_in_context: HashSet<ID> = HashSet::new();

        // 1. Build graph structure (children_map, in_degree) for the context
        let entries = self.entries.read().unwrap();
        for (id, entry) in entries.iter() {
            // Check if entry is in the context (tree or tree+subtree)
            let in_context = match subtree {
                Some(subtree_name) => entry.in_tree(tree) && entry.in_subtree(subtree_name),
                None => entry.in_tree(tree),
            };
            if !in_context {
                continue;
            }

            nodes_in_context.insert(id.clone()); // Track node

            // Get the relevant parents for this context
            let parents = match subtree {
                Some(subtree_name) => entry.subtree_parents(subtree_name)?,
                None => entry.parents()?,
            };

            // Initialize in_degree for this node. It might be adjusted if parents are outside the context.
            in_degree.insert(id.clone(), parents.len());

            // Populate children_map and adjust in_degree based on parent context
            for parent_id in parents {
                // Check if the parent is ALSO in the context
                let parent_in_context =
                    entries
                        .get(&parent_id)
                        .is_some_and(|p_entry| match subtree {
                            Some(subtree_name) => {
                                p_entry.in_tree(tree) && p_entry.in_subtree(subtree_name)
                            }
                            None => p_entry.in_tree(tree),
                        });

                if parent_in_context {
                    // Parent is in context, add edge to children_map
                    children_map
                        .entry(parent_id.clone())
                        .or_default()
                        .push(id.clone());
                } else {
                    // Parent is outside context, this edge doesn't count for in-degree *within* the context
                    if let Some(d) = in_degree.get_mut(id) {
                        *d = d.saturating_sub(1);
                    }
                }
            }
        }

        // 2. Initialize queue with root nodes (in-degree 0 within the context)
        let mut queue: VecDeque<ID> = VecDeque::new();
        for id in &nodes_in_context {
            // Initialize all heights to 0, roots will start the propagation
            heights.insert(id.clone(), 0);
            let degree = in_degree.get(id).cloned().unwrap_or(0); // Get degree for this node
            if degree == 0 {
                // Nodes with 0 in-degree *within the context* are the roots for this calculation
                queue.push_back(id.clone());
                // Height is already set to 0
            }
        }

        // 3. Process nodes using BFS (topological sort order)
        let mut processed_nodes_count = 0;
        while let Some(current_id) = queue.pop_front() {
            processed_nodes_count += 1;
            let current_height = *heights.get(&current_id).ok_or_else(|| {
                Error::Io(std::io::Error::other(
                    format!("BFS height calculation: Height missing for node {current_id}")
                        .as_str(),
                ))
            })?;

            // Process children within the context
            if let Some(children) = children_map.get(&current_id) {
                for child_id in children {
                    // Child must be in context (redundant check if children_map built correctly, but safe)
                    if !nodes_in_context.contains(child_id) {
                        continue;
                    }

                    // Update child height: longest path = max(current paths)
                    let new_height = current_height + 1;
                    let child_current_height = heights.entry(child_id.clone()).or_insert(0); // Should exist, default 0
                    *child_current_height = (*child_current_height).max(new_height);

                    // Decrement in-degree and enqueue if it becomes 0
                    if let Some(degree) = in_degree.get_mut(child_id) {
                        // Only decrement degree if it's > 0
                        if *degree > 0 {
                            *degree -= 1;
                            if *degree == 0 {
                                queue.push_back(child_id.clone());
                            }
                        } else {
                            // This indicates an issue: degree already 0 but node is being processed as child.
                            return Err(Error::Io(std::io::Error::other(
                                format!("BFS height calculation: Negative in-degree detected for child {child_id}").as_str()
                            )));
                        }
                    } else {
                        // This indicates an inconsistency: child_id was in children_map but not in_degree map
                        return Err(Error::Io(std::io::Error::other(
                            format!(
                                "BFS height calculation: In-degree missing for child {child_id}"
                            )
                            .as_str(),
                        )));
                    }
                }
            }
        }

        // 4. Check for cycles (if not all nodes were processed) - Assumes DAG
        if processed_nodes_count != nodes_in_context.len() {
            panic!(
                "calculate_heights processed {} nodes, but found {} nodes in context. Potential cycle or disconnected graph portion detected.",
                processed_nodes_count,
                nodes_in_context.len()
            );
        }

        // Ensure the final map only contains heights for nodes within the specified context
        heights.retain(|id, _| nodes_in_context.contains(id));

        Ok(heights)
    }

    /// Creates a cache key for CRDT state from entry ID and subtree.
    fn create_crdt_cache_key(&self, entry_id: &ID, subtree: &str) -> String {
        format!("crdt:{entry_id}:{subtree}")
    }

    /// Sorts entries by their height (longest path from a root) within a tree.
    ///
    /// Entries with lower height (closer to a root) appear before entries with higher height.
    /// Entries with the same height are then sorted by their ID for determinism.
    /// Entries without any parents (root nodes) have a height of 0 and appear first.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree context.
    /// * `entries` - The vector of entries to be sorted in place.
    ///
    /// # Returns
    /// A `Result` indicating success or an error if height calculation fails.
    pub fn sort_entries_by_height(&self, tree: &ID, entries: &mut [Entry]) -> Result<()> {
        let heights = self.calculate_heights(tree, None)?;

        entries.sort_by(|a, b| {
            let a_height = *heights.get(&a.id()).unwrap_or(&0);
            let b_height = *heights.get(&b.id()).unwrap_or(&0);
            a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
        });
        Ok(())
    }

    /// Sorts entries by their height within a specific subtree context.
    ///
    /// Entries with lower height (closer to a root) appear before entries with higher height.
    /// Entries with the same height are then sorted by their ID for determinism.
    /// Entries without any subtree parents have a height of 0 and appear first.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree context.
    /// * `subtree` - The name of the subtree context.
    /// * `entries` - The vector of entries to be sorted in place.
    ///
    /// # Returns
    /// A `Result` indicating success or an error if height calculation fails.
    pub fn sort_entries_by_subtree_height(
        &self,
        tree: &ID,
        subtree: &str,
        entries: &mut [Entry],
    ) -> Result<()> {
        let heights = self.calculate_heights(tree, Some(subtree))?;
        entries.sort_by(|a, b| {
            let a_height = *heights.get(&a.id()).unwrap_or(&0);
            let b_height = *heights.get(&b.id()).unwrap_or(&0);
            a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
        });
        Ok(())
    }
}

impl Backend for InMemoryBackend {
    /// Retrieves an entry by ID from the internal `HashMap`.
    fn get(&self, id: &ID) -> Result<Entry> {
        let entries = self.entries.read().unwrap();
        entries.get(id).cloned().ok_or(Error::NotFound)
    }

    /// Gets the verification status of an entry.
    fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        let entries = self.entries.read().unwrap();
        let verification_status = self.verification_status.read().unwrap();

        // Check if entry exists first
        if !entries.contains_key(id) {
            return Err(Error::NotFound);
        }

        // Return the verification status, defaulting to Verified if not set
        Ok(verification_status.get(id).copied().unwrap_or_default())
    }

    /// Stores an entry in the backend with the specified verification status.
    fn put(&self, verification_status: VerificationStatus, entry: Entry) -> Result<()> {
        let entry_id = entry.id();
        let tree_id = ID::from(entry.root());

        // Store the entry
        {
            let mut entries = self.entries.write().unwrap();
            entries.insert(entry_id.clone(), entry.clone());
        }

        // Store the verification status
        {
            let mut verification_status_map = self.verification_status.write().unwrap();
            verification_status_map.insert(entry_id.clone(), verification_status);
        }

        // Smart cache update for heights
        {
            let mut heights_cache = self.heights.write().unwrap();
            if let Some(cache) = heights_cache.get_mut(&tree_id) {
                Self::update_cached_heights(cache, &entry, &entry_id);
            }
        }

        // Smart cache update for tips
        {
            let mut tips_cache = self.tips.write().unwrap();
            if let Some(cache) = tips_cache.get_mut(&tree_id) {
                // Update tree tips
                if let Ok(parents) = entry.parents() {
                    if parents.is_empty() {
                        // Root entry is also a tip initially
                        cache.tree_tips.insert(entry_id.clone());
                    } else {
                        // Remove parents from tips if they exist (they're no longer tips)
                        for parent in &parents {
                            cache.tree_tips.remove(parent);
                        }
                        // Add the new entry as a tip (it has no children yet)
                        cache.tree_tips.insert(entry_id.clone());
                    }
                }

                // Update subtree tips for each subtree
                for subtree_name in entry.subtrees() {
                    if let Some(subtree_tips) = cache.subtree_tips.get_mut(&subtree_name) {
                        if let Ok(subtree_parents) = entry.subtree_parents(&subtree_name) {
                            if subtree_parents.is_empty() {
                                // Subtree root is also a tip initially
                                subtree_tips.insert(entry_id.clone());
                            } else {
                                // Remove parents from subtree tips if they exist
                                for parent in &subtree_parents {
                                    subtree_tips.remove(parent);
                                }
                                // Add the new entry as a subtree tip
                                subtree_tips.insert(entry_id.clone());
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Updates the verification status of an existing entry.
    fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> Result<()> {
        // Check if entry exists
        {
            let entries = self.entries.read().unwrap();
            if !entries.contains_key(id) {
                return Err(Error::NotFound);
            }
        }

        // Update the verification status
        {
            let mut status_map = self.verification_status.write().unwrap();
            status_map.insert(id.clone(), verification_status);
        }

        Ok(())
    }

    /// Gets all entries with a specific verification status.
    fn get_entries_by_verification_status(&self, status: VerificationStatus) -> Result<Vec<ID>> {
        let mut matching_entries = Vec::new();
        let entries = self.entries.read().unwrap();
        let verification_status = self.verification_status.read().unwrap();

        for entry_id in entries.keys() {
            let entry_status = verification_status
                .get(entry_id)
                .copied()
                .unwrap_or_default();
            if entry_status == status {
                matching_entries.push(entry_id.clone());
            }
        }

        Ok(matching_entries)
    }

    /// Finds the tip entries for the specified tree.
    /// Uses lazy cached tips for O(1) performance after first computation.
    fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        // Check if we have cached tree tips
        let tips_cache = self.tips.read().unwrap();
        if let Some(cache) = tips_cache.get(tree) {
            return Ok(cache.tree_tips.iter().cloned().collect());
        }
        drop(tips_cache);

        // Compute tips lazily
        let mut tips = Vec::new();
        let entries = self.entries.read().unwrap();
        for (id, entry) in entries.iter() {
            if entry.root() == *tree && self.is_tip(tree, id) {
                tips.push(id.clone());
            } else if entry.is_root() && entry.id() == *tree && self.is_tip(tree, id) {
                // Handle the special case of the root entry
                tips.push(id.clone());
            }
        }

        // Cache the result
        let tips_set: HashSet<ID> = tips.iter().cloned().collect();
        let mut tips_cache = self.tips.write().unwrap();
        let cache = tips_cache.entry(tree.clone()).or_default();
        cache.tree_tips = tips_set;
        drop(tips_cache);

        Ok(tips)
    }

    /// Finds the tip entries for the specified subtree.
    /// Uses lazy cached subtree tips for O(1) performance after first computation.
    fn get_subtree_tips(&self, tree: &ID, subtree: &str) -> Result<Vec<ID>> {
        // Check if we have cached subtree tips
        let tips_cache = self.tips.read().unwrap();
        if let Some(cache) = tips_cache.get(tree) {
            if let Some(subtree_tips) = cache.subtree_tips.get(subtree) {
                return Ok(subtree_tips.iter().cloned().collect());
            }
        }
        drop(tips_cache);

        // Compute subtree tips lazily
        let tree_tips = self.get_tips(tree)?;
        let subtree_tips = self.get_subtree_tips_up_to_entries(tree, subtree, &tree_tips)?;

        // Cache the result
        let tips_set: HashSet<ID> = subtree_tips.iter().cloned().collect();
        let mut tips_cache = self.tips.write().unwrap();
        let cache = tips_cache.entry(tree.clone()).or_default();
        cache.subtree_tips.insert(subtree.to_string(), tips_set);
        drop(tips_cache);

        Ok(subtree_tips)
    }

    /// Finds all entries that are top-level roots (i.e., `entry.is_toplevel_root()` is true).
    fn all_roots(&self) -> Result<Vec<ID>> {
        let mut roots = Vec::new();
        let entries = self.entries.read().unwrap();
        for (id, entry) in entries.iter() {
            if entry.is_toplevel_root() {
                roots.push(id.clone());
            }
        }
        Ok(roots)
    }

    /// Returns `self` as a `&dyn Any` reference.
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Get all entries within a specific tree.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree to fetch.
    ///
    /// # Returns
    /// A `Result` containing a `Vec<Entry>` of all entries belonging to the tree.
    fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        // Fill this tree vec with all entries in the tree
        let mut entries = Vec::new();
        let all_entries = self.entries.read().unwrap();
        for entry in all_entries.values() {
            if entry.in_tree(tree) {
                entries.push(entry.clone());
            }
        }

        // Sort entries by tree height
        let heights = self.calculate_heights(tree, None)?;
        entries.sort_by(|a, b| {
            let a_height = *heights.get(&a.id()).unwrap_or(&0);
            let b_height = *heights.get(&b.id()).unwrap_or(&0);
            a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
        });

        Ok(entries)
    }

    /// Get all entries in a specific subtree within a tree.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree containing the subtree.
    /// * `subtree` - The name of the subtree to fetch.
    ///
    /// # Returns
    /// A `Result` containing a `Vec<Entry>` of all entries belonging to both the tree and the subtree.
    /// Entries that belong to the tree but not the subtree are excluded.
    fn get_subtree(&self, tree: &ID, subtree: &str) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let all_entries = self.entries.read().unwrap();
        for entry in all_entries.values() {
            if entry.in_tree(tree) && entry.in_subtree(subtree) {
                entries.push(entry.clone());
            }
        }

        // Sort entries by subtree height
        let heights = self.calculate_heights(tree, Some(subtree))?;
        entries.sort_by(|a, b| {
            let a_height = *heights.get(&a.id()).unwrap_or(&0);
            let b_height = *heights.get(&b.id()).unwrap_or(&0);
            a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
        });

        Ok(entries)
    }

    /// Get entries in a specific tree starting from the given tip IDs.
    ///
    /// This method traverses the Directed Acyclic Graph (DAG) structure of the tree,
    /// starting from the specified tip entries and walking backwards through parent
    /// references to collect all relevant entries.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree containing the entries.
    /// * `tips` - The IDs of the tip entries to start the traversal from.
    ///
    /// # Returns
    /// A `Result` containing a `Vec<Entry>` of all entries reachable from the tips
    /// within the specified tree, sorted in topological order (parents before children).
    fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        let mut result = Vec::new();
        let mut to_process = VecDeque::new();
        let mut processed = HashSet::new();

        // Initialize with tips
        let entries = self.entries.read().unwrap();
        for tip in tips {
            if let Some(entry) = entries.get(tip) {
                // Only include entries that are part of the specified tree
                if entry.in_tree(tree) {
                    to_process.push_back(tip.clone());
                }
            }
        }

        // Process entries in breadth-first order
        while let Some(current_id) = to_process.pop_front() {
            // Skip if already processed
            if processed.contains(&current_id) {
                continue;
            }

            if let Some(entry) = entries.get(&current_id) {
                // Entry must be in the specified tree to be included
                if entry.in_tree(tree) {
                    // Add parents to be processed
                    if let Ok(parents) = entry.parents() {
                        for parent in parents {
                            if !processed.contains(&parent) {
                                to_process.push_back(parent);
                            }
                        }
                    }

                    // Include this entry in the result
                    result.push(entry.clone());
                    processed.insert(current_id);
                }
            }
        }
        drop(entries);

        // Sort the result by height within the tree context
        if !result.is_empty() {
            let heights = self.calculate_heights(tree, None)?;
            result.sort_by(|a, b| {
                let a_height = *heights.get(&a.id()).unwrap_or(&0);
                let b_height = *heights.get(&b.id()).unwrap_or(&0);
                a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
            });
        }

        Ok(result)
    }

    /// Get entries in a specific subtree within a tree, starting from the given tip IDs.
    ///
    /// This method traverses the Directed Acyclic Graph (DAG) structure of the subtree,
    /// starting from the specified tip entries and walking backwards through parent
    /// references to collect all relevant entries.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree containing the subtree.
    /// * `subtree` - The name of the subtree to fetch.
    /// * `tips` - The IDs of the tip entries to start the traversal from.
    ///
    /// # Returns
    /// A `Result` containing a `Vec<Entry>` of all entries reachable from the tips
    /// that belong to both the specified tree and subtree, sorted in topological order.
    /// Entries that don't contain data for the specified subtree are excluded even if
    /// they're part of the tree.
    fn get_subtree_from_tips(&self, tree: &ID, subtree: &str, tips: &[ID]) -> Result<Vec<Entry>> {
        let mut result = Vec::new();
        let mut to_process = VecDeque::new();
        let mut processed = HashSet::new();

        // Initialize with tips
        let entries = self.entries.read().unwrap();
        for tip in tips {
            if let Some(entry) = entries.get(tip) {
                // Only include entries that are part of both the tree and the subtree
                if entry.in_tree(tree) && entry.in_subtree(subtree) {
                    to_process.push_back(tip.clone());
                }
            }
        }

        // Process entries in breadth-first order
        while let Some(current_id) = to_process.pop_front() {
            // Skip if already processed
            if processed.contains(&current_id) {
                continue;
            }

            if let Some(entry) = entries.get(&current_id) {
                // Strict inclusion criteria: entry must be in BOTH the specific tree AND subtree
                if entry.in_subtree(subtree) && entry.in_tree(tree) {
                    // Get subtree parents to process, if available
                    if let Ok(subtree_parents) = entry.subtree_parents(subtree) {
                        for parent in subtree_parents {
                            if !processed.contains(&parent) {
                                to_process.push_back(parent);
                            }
                        }
                    }

                    // Include this entry in the result
                    result.push(entry.clone());
                    processed.insert(current_id);
                }
            }
        }
        drop(entries);

        // Sort the result by height within the subtree context
        let heights = self.calculate_heights(tree, Some(subtree))?;
        result.sort_by(|a, b| {
            let a_height = *heights.get(&a.id()).unwrap_or(&0);
            let b_height = *heights.get(&b.id()).unwrap_or(&0);
            a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
        });

        Ok(result)
    }

    // === Private Key Storage Implementation ===

    /// Store a private key in local memory storage.
    ///
    /// **Security Warning**: Keys are stored in plaintext memory without encryption.
    /// This implementation is suitable for development and testing only.
    fn store_private_key(&self, key_id: &str, private_key: SigningKey) -> Result<()> {
        let mut keys = self.private_keys.write().unwrap();
        keys.insert(key_id.to_string(), private_key);
        Ok(())
    }

    /// Retrieve a private key from local memory storage.
    fn get_private_key(&self, key_id: &str) -> Result<Option<SigningKey>> {
        let keys = self.private_keys.read().unwrap();
        Ok(keys.get(key_id).cloned())
    }

    /// List all stored private key identifiers.
    fn list_private_keys(&self) -> Result<Vec<String>> {
        let keys = self.private_keys.read().unwrap();
        Ok(keys.keys().cloned().collect())
    }

    /// Remove a private key from local memory storage.
    ///
    /// Returns Ok even if the key doesn't exist.
    fn remove_private_key(&self, key_id: &str) -> Result<()> {
        let mut keys = self.private_keys.write().unwrap();
        keys.remove(key_id);
        Ok(())
    }

    /// Gets the subtree tips that exist up to a specific set of main tree entries.
    ///
    /// This method finds all subtree entries that are reachable from the specified
    /// main tree entries, then filters to find which of those are tips within the subtree.
    fn get_subtree_tips_up_to_entries(
        &self,
        tree: &ID,
        subtree: &str,
        main_entries: &[ID],
    ) -> Result<Vec<ID>> {
        if main_entries.is_empty() {
            return Ok(Vec::new());
        }

        // Special case: if main_entries represents current tree tips (i.e., we want all subtree tips),
        // use the original algorithm that checks all entries
        let current_tree_tips = self.get_tips(tree)?;
        if main_entries == current_tree_tips {
            // Use original algorithm for current tips case
            let mut tips = Vec::new();
            let entries = self.entries.read().unwrap();
            for (id, entry) in entries.iter() {
                if entry.in_tree(tree)
                    && entry.in_subtree(subtree)
                    && self.is_subtree_tip(tree, subtree, id)
                {
                    tips.push(id.clone());
                }
            }
            return Ok(tips);
        }

        // For custom tips: Get all tree entries reachable from the main entries,
        // then filter to those that are in the specified subtree
        let all_tree_entries = self.get_tree_from_tips(tree, main_entries)?;
        let subtree_entries: Vec<_> = all_tree_entries
            .into_iter()
            .filter(|entry| entry.in_subtree(subtree))
            .collect();

        // If no subtree entries found, return empty
        if subtree_entries.is_empty() {
            return Ok(Vec::new());
        }

        // Find which of these are tips within the subtree scope
        let mut tips = Vec::new();
        for entry in &subtree_entries {
            let entry_id = entry.id();

            // Check if this entry is a tip by seeing if any other entry in our scope
            // has it as a subtree parent
            let is_tip = !subtree_entries.iter().any(|other_entry| {
                if let Ok(parents) = other_entry.subtree_parents(subtree) {
                    parents.contains(&entry_id)
                } else {
                    false
                }
            });

            if is_tip {
                tips.push(entry_id);
            }
        }

        Ok(tips)
    }

    /// Get cached CRDT state for a subtree at a specific entry.
    fn get_cached_crdt_state(&self, entry_id: &ID, subtree: &str) -> Result<Option<String>> {
        let key = self.create_crdt_cache_key(entry_id, subtree);
        let cache = self.cache.read().unwrap();
        Ok(cache.get(&key).cloned())
    }

    /// Cache CRDT state for a subtree at a specific entry.
    fn cache_crdt_state(&self, entry_id: &ID, subtree: &str, state: String) -> Result<()> {
        let key = self.create_crdt_cache_key(entry_id, subtree);
        let mut cache = self.cache.write().unwrap();
        cache.insert(key, state);
        Ok(())
    }

    /// Clear all cached CRDT states.
    fn clear_crdt_cache(&self) -> Result<()> {
        let mut cache = self.cache.write().unwrap();
        cache.clear();
        Ok(())
    }

    /// Get the subtree parent IDs for a specific entry and subtree, sorted by height then ID.
    fn get_sorted_subtree_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        subtree: &str,
    ) -> Result<Vec<ID>> {
        let entries = self.entries.read().unwrap();
        let entry = entries.get(entry_id).ok_or(Error::NotFound)?;

        if !entry.in_tree(tree_id) || !entry.in_subtree(subtree) {
            return Ok(Vec::new());
        }

        let mut parents = match entry.subtree_parents(subtree) {
            Ok(parents) => parents,
            Err(_) => return Ok(Vec::new()),
        };

        // Sort parents by height (ascending), then by ID for determinism
        if !parents.is_empty() {
            let heights = self.calculate_heights(tree_id, Some(subtree))?;
            parents.sort_by(|a, b| {
                let a_height = *heights.get(a).unwrap_or(&0);
                let b_height = *heights.get(b).unwrap_or(&0);
                a_height.cmp(&b_height).then_with(|| a.cmp(b))
            });
        }

        Ok(parents)
    }

    fn find_lca(&self, tree: &ID, subtree: &str, entry_ids: &[ID]) -> Result<ID> {
        use std::collections::{HashMap, HashSet, VecDeque};

        if entry_ids.is_empty() {
            return Err(Error::Io(std::io::Error::other(
                "No entry IDs provided for LCA",
            )));
        }

        if entry_ids.len() == 1 {
            return Ok(entry_ids[0].clone());
        }

        // Verify that all entries exist and belong to the specified tree
        for entry_id in entry_ids {
            match self.get(entry_id) {
                Ok(entry) => {
                    if !entry.in_tree(tree) {
                        return Err(Error::Io(std::io::Error::other(format!(
                            "Entry {entry_id} is not in tree {tree}"
                        ))));
                    }
                }
                Err(_) => {
                    return Err(Error::Io(std::io::Error::other(format!(
                        "Entry {entry_id} not found"
                    ))));
                }
            }
        }

        // Track which entries can reach each ancestor
        let mut ancestors: HashMap<ID, HashSet<usize>> = HashMap::new();
        let mut queues: Vec<VecDeque<ID>> = Vec::new();

        // Initialize BFS from each entry
        for (idx, entry_id) in entry_ids.iter().enumerate() {
            let mut queue = VecDeque::new();
            queue.push_back(entry_id.clone());
            ancestors.entry(entry_id.clone()).or_default().insert(idx);
            queues.push(queue);
        }

        // BFS upward until we find common ancestor
        loop {
            let mut any_progress = false;

            for (idx, queue) in queues.iter_mut().enumerate() {
                if let Some(current) = queue.pop_front() {
                    any_progress = true;

                    // Check if this ancestor is reachable by all entries
                    let reachable_by = ancestors.entry(current.clone()).or_default();
                    reachable_by.insert(idx);

                    if reachable_by.len() == entry_ids.len() {
                        // Found LCA!
                        return Ok(current);
                    }

                    // Add parents to queue
                    if let Ok(entry) = self.get(&current) {
                        let parents = if let Ok(parents) = entry.subtree_parents(subtree) {
                            parents
                        } else {
                            entry.parents()?
                        };

                        for parent in parents {
                            queue.push_back(parent);
                        }
                    }
                }
            }

            if !any_progress {
                break;
            }
        }

        Err(Error::Io(std::io::Error::other("No common ancestor found")))
    }

    fn collect_root_to_target(
        &self,
        tree: &ID,
        subtree: &str,
        target_entry: &ID,
    ) -> Result<Vec<ID>> {
        self.build_path_from_root(tree, subtree, target_entry)
    }

    fn get_path_from_to(
        &self,
        tree_id: &ID,
        subtree: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        if to_ids.is_empty() {
            return Ok(vec![]);
        }

        // If any target is the same as from_id, we still process others
        // Use breadth-first search to find ALL entries between from_id and any of the to_ids
        let mut result = Vec::new();
        let mut to_process = std::collections::VecDeque::new();
        let mut processed = std::collections::HashSet::new();

        // Start from all to_ids
        for to_id in to_ids {
            if to_id != from_id {
                to_process.push_back(to_id.clone());
            }
        }

        while let Some(current) = to_process.pop_front() {
            // Skip if already processed
            if processed.contains(&current) {
                continue;
            }

            // If we've reached the from_id, stop processing this path
            if current == *from_id {
                processed.insert(current);
                continue;
            }

            // Add current to result (unless it's the from_id)
            result.push(current.clone());
            processed.insert(current.clone());

            // Get parents in the subtree
            let parents = self.get_sorted_subtree_parents(tree_id, &current, subtree)?;

            // Add all parents to be processed
            for parent in parents {
                if !processed.contains(&parent) {
                    to_process.push_back(parent);
                }
            }
        }

        // Deduplicate and sort result by height then ID for deterministic ordering
        result.sort();
        result.dedup();

        if !result.is_empty() {
            let heights = self.calculate_heights(tree_id, Some(subtree))?;
            result.sort_by(|a, b| {
                let a_height = *heights.get(a).unwrap_or(&0);
                let b_height = *heights.get(b).unwrap_or(&0);
                a_height.cmp(&b_height).then_with(|| a.cmp(b))
            });
        }

        Ok(result)
    }
}

impl InMemoryBackend {
    /// Helper method to build the complete path from tree root to a target entry
    fn build_path_from_root(&self, tree: &ID, subtree: &str, target_entry: &ID) -> Result<Vec<ID>> {
        let mut path = Vec::new();
        let mut current = target_entry.clone();
        let mut visited = std::collections::HashSet::new();

        // Build path by following parents back to root
        loop {
            if visited.contains(&current) {
                return Err(Error::Io(std::io::Error::other("Cycle detected in DAG")));
            }
            visited.insert(current.clone());
            path.push(current.clone());

            // Get the entry
            let entry = self.get(&current)?;

            // Check if we've reached the tree root
            if current == *tree || entry.is_root() {
                break;
            }

            // Get subtree parents for this entry
            let parents = if let Ok(parents) = entry.subtree_parents(subtree) {
                parents
            } else {
                // If no subtree parents, follow main parents
                entry.parents()?
            };

            if parents.is_empty() {
                // No parents - this must be a root entry
                break;
            } else {
                // Follow the first parent (in height/ID sorted order)
                current = parents[0].clone();
            }
        }

        // Reverse to get root-to-target order
        path.reverse();

        Ok(path)
    }
}
