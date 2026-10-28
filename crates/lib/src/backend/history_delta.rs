//! Strict forward Store snapshot deltas (native history-pages.v2).
//!
//! Both ancestor walks and the target metadata are accounted, not Entry bodies.
//! This intentionally rescans known + target on every page. v1 stays permissive.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::{
    BackendError, HistoryPageLimits,
    history_page::{
        HistoryQuery, MAX_HISTORY_PAGE_ENCODING, MAX_HISTORY_TIPS, MAX_HISTORY_WORKSPACE_BYTES,
        TopK, Visits, Workspace, resource,
    },
};
use crate::{
    Result,
    encoding::json_encoded_len,
    entry::{Entry, ID},
    snapshot::Snapshot,
};

/// A bounded canonical prefix of A(target) minus A(known).
/// `covered` is the maximal frontier of (A(target) intersect A(known)) plus
/// `entries`. It is target-local, not an advertisement of the server's tips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryDelta {
    pub entries: Vec<Entry>,
    pub covered: Snapshot,
    /// Domain-separated binding to the exact canonical target input and scope.
    /// Neither this digest nor `covered` grants authority or Verified status.
    pub binding: [u8; 32],
    pub complete: bool,
}

pub(crate) fn invalid(reason: &str) -> crate::Error {
    BackendError::InvalidHistoryDelta {
        reason: reason.into(),
    }
    .into()
}

pub(crate) struct DeltaQuery<'a> {
    pub tree: &'a ID,
    pub store: &'a str,
    pub target: &'a Snapshot,
    pub known: &'a Snapshot,
    pub binding: [u8; 32],
}
impl<'a> DeltaQuery<'a> {
    pub fn new(
        tree: &'a ID,
        store: &'a str,
        target: &'a Snapshot,
        known: &'a Snapshot,
    ) -> Result<Self> {
        if target.len() > MAX_HISTORY_TIPS || known.len() > MAX_HISTORY_TIPS {
            return Err(resource("history Snapshot tips", MAX_HISTORY_TIPS));
        }
        let mut hash = blake3::Hasher::new_derive_key("eidetica history delta v2");
        let mut field = |bytes: &[u8]| {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        };
        field(tree.to_string().as_bytes());
        field(store.as_bytes());
        for tip in target {
            field(tip.to_string().as_bytes());
        }
        Ok(Self {
            tree,
            store,
            target,
            known,
            binding: *hash.finalize().as_bytes(),
        })
    }
}

struct Node {
    height: u64,
    parents: Vec<ID>,
}

/// One engine shared by the locked InMemory and transactional SQL readers.
/// Calls to `parent` admit storage before cloning/growing metadata containers.
pub(crate) struct DeltaWalk<'a> {
    query: &'a DeltaQuery<'a>,
    pub workspace: Workspace,
    known: Visits<ID>,
    target: Visits<ID>,
    nodes: HashMap<ID, Node>,
    in_known: bool,
    top: TopK,
}
impl<'a> DeltaWalk<'a> {
    pub fn new(
        query: &'a DeltaQuery<'a>,
        limits: &HistoryPageLimits,
        mut workspace: Workspace,
    ) -> Result<Self> {
        // Input argument copies, container envelopes and frontend/driver staging.
        workspace.charge(
            query
                .store
                .len()
                .checked_mul(4)
                .and_then(|n| {
                    n.checked_add((query.target.len() + query.known.len()) * Visits::<ID>::COST)
                })
                .ok_or_else(|| {
                    resource("history metadata workspace", MAX_HISTORY_WORKSPACE_BYTES)
                })?,
        )?;
        let q = HistoryQuery::new(query.tree, query.store, query.target, None)?;
        let top = TopK::new(&q, limits, &mut workspace)?;
        let mut known = Visits::new();
        for id in query.known {
            known.insert(id.clone(), &mut workspace)?;
        }
        Ok(Self {
            query,
            workspace,
            known,
            target: Visits::new(),
            nodes: HashMap::new(),
            in_known: true,
            top,
        })
    }
    pub fn next(&mut self) -> Result<Option<ID>> {
        if self.in_known {
            if let Some(id) = self.known.queue.pop_front() {
                return Ok(Some(id));
            }
            self.in_known = false;
            for id in self.query.target {
                self.target.insert(id.clone(), &mut self.workspace)?;
            }
        }
        Ok(self.target.queue.pop_front())
    }
    pub fn member(&mut self, id: &ID, height: u64) -> Result<()> {
        if !self.in_known {
            self.workspace
                .charge(16 * (std::mem::size_of::<ID>() + std::mem::size_of::<Node>() + 1))?;
            self.nodes
                .try_reserve(1)
                .map_err(|_| resource("history allocation", MAX_HISTORY_WORKSPACE_BYTES))?;
            self.nodes.insert(
                id.clone(),
                Node {
                    height,
                    parents: Vec::new(),
                },
            );
            if !self.known.seen.contains(id) {
                self.top.offer(height, id);
            }
        }
        Ok(())
    }
    pub fn parent(&mut self, id: &ID, parent: ID) -> Result<()> {
        if self.in_known {
            return self.known.insert(parent, &mut self.workspace);
        }
        // Vec growth/overlap, plus the parent ID retained separately by Visits.
        self.workspace.charge(4 * std::mem::size_of::<ID>())?;
        let node = self.nodes.get_mut(id).expect("member precedes parents");
        node.parents
            .try_reserve(1)
            .map_err(|_| resource("history allocation", MAX_HISTORY_WORKSPACE_BYTES))?;
        node.parents.push(parent.clone());
        self.target.insert(parent, &mut self.workspace)
    }
    pub fn finish(mut self, limits: HistoryPageLimits) -> Result<DeltaBuilder<'a>> {
        let mut covered = BTreeSet::new();
        // Charge frontier nodes, prospective copy and serialization Snapshot.
        self.workspace
            .charge(self.nodes.len() * 16 * (std::mem::size_of::<ID>() + 32))?;
        for (id, node) in &self.nodes {
            for parent in &node.parents {
                let p = self
                    .nodes
                    .get(parent)
                    .ok_or_else(|| invalid("missing target ancestry"))?;
                if (p.height, parent) >= (node.height, id) {
                    return Err(invalid("Store edge violates canonical forward order"));
                }
            }
            if self.known.seen.contains(id) {
                covered.insert(id.clone());
            }
        }
        // Independent of metadata visitation order. Intersection is ancestor-closed.
        for (id, node) in &self.nodes {
            if self.known.seen.contains(id) {
                for parent in &node.parents {
                    covered.remove(parent);
                }
            }
        }
        let (selected, more) = self.top.finish()?;
        let page = HistoryDelta {
            entries: Vec::new(),
            covered: Snapshot::from(covered.iter().cloned().collect::<Vec<_>>()),
            binding: self.query.binding,
            complete: selected.is_empty() && !more,
        };
        let builder = DeltaBuilder {
            query: self.query,
            selected,
            more,
            nodes: self.nodes,
            covered,
            page,
            limits,
            items_size: 0,
            singleton: false,
        };
        builder.metadata_size(
            &builder.covered,
            builder.page.complete,
            builder
                .limits
                .max_encoded_bytes
                .min(MAX_HISTORY_PAGE_ENCODING),
        )?;
        Ok(builder)
    }
}

pub(crate) struct DeltaBuilder<'a> {
    query: &'a DeltaQuery<'a>,
    pub selected: Vec<(u64, ID)>,
    more: bool,
    nodes: HashMap<ID, Node>,
    covered: BTreeSet<ID>,
    page: HistoryDelta,
    limits: HistoryPageLimits,
    items_size: usize,
    singleton: bool,
}
impl DeltaBuilder<'_> {
    fn metadata_size(&self, covered: &BTreeSet<ID>, complete: bool, cap: usize) -> Result<usize> {
        if covered.len() > MAX_HISTORY_TIPS {
            return Err(resource("history covered tips", MAX_HISTORY_TIPS));
        }
        let metadata = HistoryDelta {
            entries: Vec::new(),
            covered: Snapshot::from(covered.iter().cloned().collect::<Vec<_>>()),
            binding: self.query.binding,
            complete,
        };
        json_encoded_len(&metadata, cap)?.ok_or_else(|| super::record_page::too_large(cap).into())
    }
    pub fn is_full(&self) -> bool {
        self.singleton
    }

    pub fn push(&mut self, index: usize, entry: &Entry) -> Result<bool> {
        if self.singleton {
            return Ok(false);
        }
        let (height, id) = &self.selected[index];
        let node = &self.nodes[id];
        if entry.id_ref() != id
            || !entry.in_tree(self.query.tree)
            || !entry.in_subtree(self.query.store)
            || entry.subtree_height(self.query.store).unwrap_or(0) != *height
            || (entry.subtree_parents_ref(self.query.store).len() != node.parents.len()
                || !entry
                    .subtree_parents_ref(self.query.store)
                    .iter()
                    .all(|parent| node.parents.contains(parent)))
        {
            return Err(invalid("Entry body disagrees with scoped Store metadata"));
        }
        let mut prospective = self.covered.clone();
        for parent in &node.parents {
            prospective.remove(parent);
        }
        prospective.insert(id.clone());
        let complete = !self.more && index + 1 == self.selected.len();
        let preferred = self.limits.max_encoded_bytes.min(MAX_HISTORY_PAGE_ENCODING);
        let cap = if self.page.entries.is_empty() && self.limits.allow_large_singleton {
            MAX_HISTORY_PAGE_ENCODING
        } else {
            preferred
        };
        let item = json_encoded_len(entry, cap)?;
        let metadata = self.metadata_size(&prospective, complete, cap);
        let total = item
            .and_then(|size| {
                size.checked_add(self.items_size + usize::from(!self.page.entries.is_empty()))
            })
            .and_then(|size| size.checked_add(metadata.as_ref().ok().copied()?));
        if total.is_none_or(|size| size > cap) {
            if !self.page.entries.is_empty() {
                return Ok(false);
            }
            metadata?;
            return Err(BackendError::EntryTooLarge {
                id: id.clone(),
                encoded_bytes: cap + 1,
            }
            .into());
        }
        self.singleton = total.is_some_and(|size| size > preferred);
        self.items_size += item.unwrap() + usize::from(!self.page.entries.is_empty());
        self.covered = prospective;
        self.page.entries.push(entry.clone());
        self.page.covered = Snapshot::from(self.covered.iter().cloned().collect::<Vec<_>>());
        self.page.complete = complete;
        Ok(true)
    }
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub fn raw_too_large(&self, id: &ID) -> Result<()> {
        if self.page.entries.is_empty() {
            return Err(BackendError::EntryTooLarge {
                id: id.clone(),
                encoded_bytes: super::history_page::MAX_ENTRY_RAW_BYTES + 1,
            }
            .into());
        }
        Ok(())
    }
    pub fn finish(self) -> Result<HistoryDelta> {
        if !self.page.complete && self.page.entries.is_empty() {
            return Err(invalid("nonterminal delta did not progress"));
        }
        Ok(self.page)
    }
}

/// Validate an empty-known collector's progress from delivered Store parent
/// links. This is structural validation, not a signature or completeness proof
/// against a malicious backend. The server remains the data authority.
#[cfg(all(unix, feature = "service"))]
pub(crate) fn validate_delta(
    query: &DeltaQuery<'_>,
    page: &HistoryDelta,
    previous: &mut Option<(u64, ID)>,
    seen: &mut std::collections::HashSet<ID>,
) -> Result<()> {
    if page.binding != query.binding
        || (!page.complete && page.entries.is_empty())
        || page.covered.len() > MAX_HISTORY_TIPS
    {
        return Err(invalid("delta binding or progress"));
    }
    let mut covered: BTreeSet<ID> = query.known.iter().cloned().collect();
    for entry in &page.entries {
        let key = (entry.subtree_height(query.store).unwrap_or(0), entry.id());
        if !entry.in_tree(query.tree)
            || !entry.in_subtree(query.store)
            || previous.as_ref().is_some_and(|last| key <= *last)
            || !seen.insert(entry.id())
        {
            return Err(invalid("delta scope, order or duplicate"));
        }
        for parent in entry.subtree_parents_ref(query.store) {
            if !seen.contains(parent) {
                return Err(invalid("delta omitted Store parent"));
            }
            covered.remove(parent);
        }
        covered.insert(entry.id());
        *previous = Some(key);
    }
    if page.covered != Snapshot::from(covered.into_iter().collect::<Vec<_>>())
        || page.complete != query.target.iter().all(|id| seen.contains(id))
    {
        return Err(invalid("delta covered frontier or completion"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_delta_snapshot_and_covered_width_are_explicit() {
        let tree = ID::from_bytes("tree");
        let tips = Snapshot::from(
            (0..=MAX_HISTORY_TIPS as u64)
                .map(|n| ID::from_bytes(n.to_le_bytes()))
                .collect::<Vec<_>>(),
        );
        assert!(DeltaQuery::new(&tree, "s", &tips, &Snapshot::EMPTY).is_err());
        assert!(DeltaQuery::new(&tree, "s", &Snapshot::EMPTY, &tips).is_err());
        let empty = Snapshot::EMPTY;
        let q = DeltaQuery::new(&tree, "s", &empty, &empty).unwrap();
        let limits = HistoryPageLimits {
            max_entries: 1,
            max_encoded_bytes: 1 << 20,
            allow_large_singleton: false,
        };
        let builder = DeltaWalk::new(&q, &limits, Workspace::new())
            .unwrap()
            .finish(limits)
            .unwrap();
        assert!(
            builder
                .metadata_size(
                    &tips.iter().cloned().collect(),
                    true,
                    MAX_HISTORY_PAGE_ENCODING
                )
                .is_err()
        );
        assert!(builder.finish().unwrap().complete);
    }
}
