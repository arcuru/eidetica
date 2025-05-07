use crate::backend::Backend;
use crate::entry::{Entry, ID};
use crate::Error;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

/// A simple in-memory backend implementation using a `HashMap` for storage.
///
/// This backend is suitable for testing, development, or scenarios where
/// data persistence is not strictly required or is handled externally
/// (e.g., by saving/loading the entire state to/from a file).
///
/// It provides basic persistence capabilities via `save_to_file` and
/// `load_from_file`, serializing the `HashMap` to JSON.
#[derive(Serialize, Deserialize, Debug)]
pub struct InMemoryBackend {
    entries: HashMap<ID, Entry>,
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
            entries: HashMap::new(),
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
        let json = serde_json::to_string(self).map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to serialize: {}", e),
            ))
        })?;

        fs::write(path, json).map_err(Error::Io)?;
        Ok(())
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
        let file_exists = path.as_ref().exists();
        if !file_exists {
            return Ok(Self::new());
        }

        let json = fs::read_to_string(path).map_err(Error::Io)?;
        serde_json::from_str(&json).map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to deserialize: {}", e),
            ))
        })
    }

    /// Returns a vector containing the IDs of all entries currently stored in the backend.
    pub fn all_ids(&self) -> Vec<ID> {
        self.entries.keys().cloned().collect()
    }

    /// Helper function to check if an entry is a tip within its tree.
    ///
    /// An entry is a tip if no other entry in the same tree lists it as a parent.
    pub fn is_tip(&self, tree: &ID, entry_id: &ID) -> bool {
        // Check if any other entry has this entry as its parent
        for other_entry in self.entries.values() {
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
        let entry = match self.entries.get(entry_id) {
            Some(e) => e,
            None => return false, // Entry doesn't exist
        };

        if !entry.in_subtree(subtree) {
            return false; // Entry is not in the subtree
        }

        // Check if any other entry has this entry as its subtree parent
        for other_entry in self.entries.values() {
            if other_entry.in_tree(tree) && other_entry.in_subtree(subtree) {
                if let Ok(parents) = other_entry.subtree_parents(subtree) {
                    if parents.contains(entry_id) {
                        return false; // Found a child in the subtree
                    }
                }
            }
        }
        true
    }

    /// Calculates the height of each entry within a specified tree or subtree.
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
        let mut heights: HashMap<ID, usize> = HashMap::new();
        let mut in_degree: HashMap<ID, usize> = HashMap::new();
        // Map: parent_id -> list of child_ids *within the context*
        let mut children_map: HashMap<ID, Vec<ID>> = HashMap::new();
        // Keep track of all nodes actually in the context
        let mut nodes_in_context: HashSet<ID> = HashSet::new();

        // 1. Build graph structure (children_map, in_degree) for the context
        for (id, entry) in &self.entries {
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
                    self.entries
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
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "BFS height calculation: Height missing for node {}",
                        current_id
                    )
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
                            return Err(Error::Io(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("BFS height calculation: Negative in-degree detected for child {}", child_id).as_str()
                            )));
                        }
                    } else {
                        // This indicates an inconsistency: child_id was in children_map but not in_degree map
                        return Err(Error::Io(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!(
                                "BFS height calculation: In-degree missing for child {}",
                                child_id
                            )
                            .as_str(),
                        )));
                    }
                }
            }
        }

        // 4. Check for cycles (if not all nodes were processed) - Assumes DAG
        if processed_nodes_count != nodes_in_context.len() {
            panic!("calculate_heights processed {} nodes, but found {} nodes in context. Potential cycle or disconnected graph portion detected.",
                 processed_nodes_count, nodes_in_context.len()
             );
        }

        // Ensure the final map only contains heights for nodes within the specified context
        heights.retain(|id, _| nodes_in_context.contains(id));

        Ok(heights)
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
    fn get(&self, id: &ID) -> Result<&Entry> {
        self.entries.get(id).ok_or(Error::NotFound)
    }

    /// Inserts or updates an entry in the internal `HashMap`.
    fn put(&mut self, entry: Entry) -> Result<()> {
        self.entries.insert(entry.id(), entry);
        Ok(())
    }

    /// Finds the tip entries for the specified tree.
    /// Iterates through all entries, checking if they belong to the tree and if `is_tip` returns true.
    fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        let mut tips = Vec::new();
        for (id, entry) in &self.entries {
            if entry.root() == *tree && self.is_tip(tree, id) {
                tips.push(id.clone());
            } else if entry.is_root() && entry.id() == *tree && self.is_tip(tree, id) {
                // Handle the special case of the root entry
                tips.push(id.clone());
            }
        }
        Ok(tips)
    }

    /// Finds the tip entries for the specified subtree.
    /// Iterates through all entries, checking if they belong to the subtree and if `is_subtree_tip` returns true.
    fn get_subtree_tips(&self, tree: &ID, subtree: &str) -> Result<Vec<ID>> {
        let mut tips = Vec::new();
        for (id, entry) in &self.entries {
            if entry.in_tree(tree)
                && entry.in_subtree(subtree)
                && self.is_subtree_tip(tree, subtree, id)
            {
                tips.push(id.clone());
            }
        }
        Ok(tips)
    }

    /// Finds all entries that are top-level roots (i.e., `entry.is_toplevel_root()` is true).
    fn all_roots(&self) -> Result<Vec<ID>> {
        let mut roots = Vec::new();
        for (id, entry) in &self.entries {
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
        for entry in self.entries.values() {
            if entry.in_tree(tree) {
                entries.push(entry.clone());
            }
        }

        // Sort entries by tree height
        self.sort_entries_by_height(tree, &mut entries)?;

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
        for entry in self.entries.values() {
            if entry.in_tree(tree) && entry.in_subtree(subtree) {
                entries.push(entry.clone());
            }
        }

        // Sort entries by subtree height
        self.sort_entries_by_subtree_height(tree, subtree, &mut entries)?;

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
        for tip in tips {
            if let Some(entry) = self.entries.get(tip) {
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

            if let Some(entry) = self.entries.get(&current_id) {
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

        // Sort the result by height within the tree context
        if !result.is_empty() {
            self.sort_entries_by_height(tree, &mut result)?;
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
        for tip in tips {
            if let Some(entry) = self.entries.get(tip) {
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

            if let Some(entry) = self.entries.get(&current_id) {
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

        self.sort_entries_by_subtree_height(tree, subtree, &mut result)?;

        Ok(result)
    }
}
