//! Delegation system types for authentication
//!
//! This module defines types related to tree delegation and reference management.

use serde::{Deserialize, Serialize};

use super::permissions::PermissionBounds;
use crate::crdt::{CRDTError, Doc, doc::Value};
use crate::entry::ID;

/// Reference to a Merkle-DAG tree (for delegated trees)
/// TODO: May standardize on this format across the codebase instead of just for delegation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TreeReference {
    /// Root entry ID of the referenced tree
    pub root: ID,
    /// Current tip entry IDs of the referenced tree
    pub tips: Vec<ID>,
}

/// Delegated tree reference stored in main tree's _settings.auth
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DelegatedTreeRef {
    /// Permission bounds for keys from this delegated tree
    #[serde(rename = "permission-bounds")]
    pub permission_bounds: PermissionBounds,
    /// Reference to the delegated tree
    pub tree: TreeReference,
}

// ==================== Doc Conversions ====================

impl From<TreeReference> for Value {
    fn from(tree_ref: TreeReference) -> Value {
        Value::Doc(Doc::from(tree_ref))
    }
}

impl From<TreeReference> for Doc {
    fn from(tree_ref: TreeReference) -> Doc {
        let mut doc = Doc::new();
        doc.set("root", tree_ref.root.as_str());
        let mut tips_doc = Doc::new();
        for (i, tip) in tree_ref.tips.iter().enumerate() {
            tips_doc.set(i.to_string(), tip.as_str());
        }
        doc.set("tips", tips_doc);
        doc
    }
}

impl TryFrom<&Doc> for TreeReference {
    type Error = crate::Error;

    fn try_from(doc: &Doc) -> crate::Result<Self> {
        let root_str = doc
            .get_as::<&str>("root")
            .ok_or_else(|| CRDTError::ElementNotFound {
                key: "root".to_string(),
            })?;
        let root = ID::new(root_str);

        let mut tips = Vec::new();
        if let Some(Value::Doc(tips_doc)) = doc.get("tips") {
            let mut entries: Vec<(usize, &str)> = tips_doc
                .iter()
                .filter_map(|(k, v)| {
                    let idx: usize = k.parse().ok()?;
                    let s = v.as_text()?;
                    Some((idx, s))
                })
                .collect();
            entries.sort_by_key(|(idx, _)| *idx);
            tips = entries.into_iter().map(|(_, s)| ID::new(s)).collect();
        }

        Ok(TreeReference { root, tips })
    }
}

impl From<DelegatedTreeRef> for Value {
    fn from(dtref: DelegatedTreeRef) -> Value {
        Value::Doc(Doc::from(dtref))
    }
}

impl From<DelegatedTreeRef> for Doc {
    fn from(dtref: DelegatedTreeRef) -> Doc {
        let mut doc = Doc::atomic();
        doc.set("permission_bounds", dtref.permission_bounds);
        doc.set("tree", dtref.tree);
        doc
    }
}

impl TryFrom<&Doc> for DelegatedTreeRef {
    type Error = crate::Error;

    fn try_from(doc: &Doc) -> crate::Result<Self> {
        let pb_doc = match doc.get("permission_bounds") {
            Some(Value::Doc(d)) => d,
            _ => {
                return Err(CRDTError::ElementNotFound {
                    key: "permission_bounds".to_string(),
                }
                .into());
            }
        };
        let permission_bounds = PermissionBounds::try_from(pb_doc)?;

        let tree_doc = match doc.get("tree") {
            Some(Value::Doc(d)) => d,
            _ => {
                return Err(CRDTError::ElementNotFound {
                    key: "tree".to_string(),
                }
                .into());
            }
        };
        let tree = TreeReference::try_from(tree_doc)?;

        Ok(DelegatedTreeRef {
            permission_bounds,
            tree,
        })
    }
}
