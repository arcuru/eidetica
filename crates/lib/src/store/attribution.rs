//! Project one Store's registration change from a signed `_index` Doc delta.
//!
//! The ordinary registry view is last-writer-wins, so it cannot say which Type a
//! contribution was written under. Only the literal row's `type` is interpreted
//! here. `config` is carried for the Type and never inspected.

use std::{collections::HashMap, fmt};

use serde::{
    Deserialize, Deserializer,
    de::{Error, MapAccess, SeqAccess, Visitor},
};
use serde_json::value::RawValue;

use crate::{
    Result,
    crdt::{Doc, doc::Value},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypeEvent {
    Declared(String),
    Removed,
    Malformed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigEvent {
    /// Ordinary Doc merge into the inherited config.
    Patch(Value),
    /// The containing row or index was removed or replaced as a whole.
    Reset(Option<Value>),
    Malformed,
}

/// The historical config at one cut. Contents belong to the Type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigState {
    Absent,
    Present(Doc),
    Malformed,
}

impl ConfigState {
    fn of(value: Value) -> Self {
        match value {
            Value::Deleted => Self::Absent,
            Value::Doc(doc) => Self::Present(doc),
            _ => Self::Malformed,
        }
    }

    pub(crate) fn apply(&self, event: &ConfigEvent) -> Self {
        match event {
            ConfigEvent::Reset(value) => value.clone().map_or(Self::Absent, Self::of),
            ConfigEvent::Malformed => Self::Malformed,
            ConfigEvent::Patch(value) => match (self, value) {
                (Self::Present(doc), value) => {
                    let mut merged = Value::Doc(doc.clone());
                    merged.merge(value);
                    Self::of(merged)
                }
                // A partial edit cannot repair an unreadable base.
                (Self::Malformed, Value::Doc(patch)) if !patch.is_atomic() => Self::Malformed,
                (_, value) => Self::of(value.clone()),
            },
        }
    }
}

/// `None` in either field means this delta leaves that half alone. An invalid
/// relevant structure is an event too: a descendant can replace it, but the older
/// declaration must not be treated as if it survived.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowDelta {
    pub(crate) type_event: Option<TypeEvent>,
    pub(crate) config: Option<ConfigEvent>,
}

impl RowDelta {
    fn removed() -> Self {
        Self {
            type_event: Some(TypeEvent::Removed),
            config: Some(ConfigEvent::Reset(None)),
        }
    }
}

pub(crate) fn project(index: &[u8], name: &str) -> RowDelta {
    project_row(index, name).unwrap_or(RowDelta {
        type_event: Some(TypeEvent::Malformed),
        config: Some(ConfigEvent::Malformed),
    })
}

fn project_row(index: &[u8], name: &str) -> Result<RowDelta> {
    let index: Index<'_> = serde_json::from_slice(index)?;
    if index.version != 0 || has_path_alias(&index, name)? {
        return Err(malformed());
    }
    let Some(raw) = index.children.0.get(name) else {
        return Ok(if index.atomic {
            RowDelta::removed()
        } else {
            RowDelta::default()
        });
    };
    let row = match value(raw)? {
        Value::Deleted => return Ok(RowDelta::removed()),
        Value::Doc(row) => row,
        _ => return Err(malformed()),
    };
    let replaced = index.atomic || row.is_atomic();
    let mut delta = if replaced {
        RowDelta::removed()
    } else {
        RowDelta::default()
    };
    for (key, value) in row.iter_all() {
        match key.as_str() {
            "type" => {
                delta.type_event = Some(match value {
                    Value::Text(id) if !id.is_empty() => TypeEvent::Declared(id.clone()),
                    Value::Deleted => TypeEvent::Removed,
                    _ => TypeEvent::Malformed,
                });
            }
            "config" => {
                delta.config = Some(if replaced {
                    ConfigEvent::Reset(Some(value.clone()))
                } else {
                    ConfigEvent::Patch(value.clone())
                });
            }
            _ => {}
        }
    }
    Ok(delta)
}

// `set("a.b", row)` on a Doc writes a nested row, not the literal key "a.b".
// Such a legacy registration makes the literal name ambiguous.
fn has_path_alias(index: &Index<'_>, name: &str) -> Result<bool> {
    let mut segments = name.split('.').filter(|segment| !segment.is_empty());
    let (Some(first), true) = (segments.next(), name.contains('.')) else {
        return Ok(false);
    };
    let Some(raw) = index.children.0.get(first) else {
        return Ok(false);
    };
    let mut row = value(raw)?;
    for segment in segments {
        let Value::Doc(doc) = &row else {
            return Ok(false);
        };
        let Some(next) = doc.get_literal(segment) else {
            return Ok(false);
        };
        row = next.clone();
    }
    Ok(matches!(&row, Value::Doc(row) if row.get_literal("type").is_some()))
}

fn value(raw: &RawValue) -> Result<Value> {
    // Serde maps silently keep one of two duplicate members.
    serde_json::from_str::<UniqueMembers>(raw.get())?;
    Ok(serde_json::from_str(raw.get())?)
}

fn malformed() -> crate::Error {
    crate::crdt::CRDTError::DeserializationFailed {
        reason: "malformed registration".into(),
    }
    .into()
}

// Unrelated rows stay opaque, so another Store's bad row is not our failure.
#[derive(Deserialize)]
struct Index<'a> {
    #[serde(rename = "_v", default)]
    version: u8,
    #[serde(rename = "_a", default)]
    atomic: bool,
    #[serde(borrow)]
    children: Fields<'a>,
}

struct Fields<'a>(HashMap<String, &'a RawValue>);

impl<'de: 'a, 'a> Deserialize<'de> for Fields<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Rows;
        impl<'de> Visitor<'de> for Rows {
            type Value = HashMap<String, &'de RawValue>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("unique Doc keys")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut fields = HashMap::new();
                while let Some((key, value)) = map.next_entry()? {
                    if fields.insert(key, value).is_some() {
                        return Err(M::Error::custom("duplicate Doc member"));
                    }
                }
                Ok(fields)
            }
        }
        d.deserialize_map(Rows).map(Fields)
    }
}

/// Accepts any JSON value whose objects, at every depth, have unique members.
struct UniqueMembers;

impl<'de> Deserialize<'de> for UniqueMembers {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Walk;
        impl<'de> Visitor<'de> for Walk {
            type Value = UniqueMembers;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON with unique members")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut keys = std::collections::HashSet::new();
                while let Some((key, UniqueMembers)) = map.next_entry::<String, _>()? {
                    if !keys.insert(key) {
                        return Err(M::Error::custom("duplicate member"));
                    }
                }
                Ok(UniqueMembers)
            }
            fn visit_seq<S: SeqAccess<'de>>(
                self,
                mut seq: S,
            ) -> std::result::Result<Self::Value, S::Error> {
                while let Some(UniqueMembers) = seq.next_element()? {}
                Ok(UniqueMembers)
            }
            fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
        }
        d.deserialize_any(Walk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TypeEvent::{Declared, Malformed, Removed};

    fn types(index: &str, name: &str) -> Option<TypeEvent> {
        project(index.as_bytes(), name).type_event
    }

    #[test]
    fn projects_exact_literal_type_events() {
        let b = || Some(Declared("b:v1".into()));
        for (index, expected) in [
            (r#"{"children":{}}"#, None),
            (r#"{"children":{"other":{"Doc":7}}}"#, None),
            (r#"{"_a":true,"children":{}}"#, Some(Removed)),
            (r#"{"children":{"s":"Deleted"}}"#, Some(Removed)),
            (
                r#"{"children":{"s":{"Doc":{"children":{"type":{"Text":"b:v1"}}}}}}"#,
                b(),
            ),
            (
                r#"{"_a":true,"children":{"s":{"Doc":{"children":{"type":{"Text":"b:v1"}}}}}}"#,
                b(),
            ),
            // Partial edits of siblings do not declare anything.
            (
                r#"{"children":{"s":{"Doc":{"children":{"config":{"Doc":{"children":{"type":{"Text":"x"}}}}}}}}}"#,
                None,
            ),
            (
                r#"{"children":{"s":{"Doc":{"children":{"lens":{"Text":"x"},"settings":{"Doc":{"children":{}}}}}}}}"#,
                None,
            ),
            // Replacement without a type must not resurrect the older one.
            (
                r#"{"children":{"s":{"Doc":{"_a":true,"children":{"settings":{"Doc":{"children":{}}}}}}}}"#,
                Some(Removed),
            ),
            (
                r#"{"children":{"s":{"Doc":{"children":{"type":"Deleted"}}}}}"#,
                Some(Removed),
            ),
            (
                r#"{"children":{"s":{"Doc":{"children":{"type":{"Text":""}}}}}}"#,
                Some(Malformed),
            ),
            (
                r#"{"children":{"s":{"Doc":{"children":{"type":{"Int":1}}}}}}"#,
                Some(Malformed),
            ),
            (r#"{"children":{"s":{"Text":"b:v1"}}}"#, Some(Malformed)),
            (r#"{"_v":1,"children":{}}"#, Some(Malformed)),
            (r#"not json"#, Some(Malformed)),
            // Duplicate members at any relevant depth.
            (
                r#"{"children":{"s":"Deleted","s":{"Doc":{"children":{"type":{"Text":"b:v1"}}}}}}"#,
                Some(Malformed),
            ),
            (
                r#"{"children":{"s":{"Doc":{"children":{"type":{"Text":"a"},"type":{"Text":"b:v1"}}}}}}"#,
                Some(Malformed),
            ),
        ] {
            assert_eq!(types(index, "s"), expected, "{index}");
        }
    }

    #[test]
    fn literal_names_do_not_alias_paths() {
        let literal = r#"{"children":{"a.b":{"Doc":{"children":{"type":{"Text":"t"}}}}}}"#;
        let nested = r#"{"children":{"a":{"Doc":{"children":{"b":{"Doc":{"children":{"type":{"Text":"t"}}}}}}}}}"#;
        let both = r#"{"children":{"a.b":{"Doc":{"children":{"type":{"Text":"t"}}}},"a":{"Doc":{"children":{"b":{"Doc":{"children":{"type":{"Text":"u"}}}}}}}}}"#;
        assert_eq!(types(literal, "a.b"), Some(Declared("t".into())));
        assert_eq!(types(literal, "a"), None);
        assert_eq!(types(nested, "a.b"), Some(Malformed));
        assert_eq!(types(nested, "a"), None);
        assert_eq!(types(both, "a.b"), Some(Malformed));
    }

    #[test]
    fn config_follows_doc_replacement_rules() {
        let doc = |key: &str, atomic: bool| {
            let mut doc = if atomic { Doc::atomic() } else { Doc::new() };
            doc.set(key, true);
            doc
        };
        let base = ConfigState::Present(doc("x", false));
        let patch = |doc| ConfigEvent::Patch(Value::Doc(doc));
        let ConfigState::Present(merged) = base.apply(&patch(doc("y", false))) else {
            panic!()
        };
        assert_eq!(merged.len(), 2);
        assert_eq!(
            base.apply(&patch(doc("y", true))),
            ConfigState::Present(doc("y", true))
        );
        assert_eq!(base.apply(&ConfigEvent::Reset(None)), ConfigState::Absent);
        assert_eq!(
            base.apply(&ConfigEvent::Patch(Value::Deleted)),
            ConfigState::Absent
        );
        let bad = base.apply(&ConfigEvent::Malformed);
        assert_eq!(bad.apply(&patch(doc("y", false))), ConfigState::Malformed);
        assert_eq!(
            bad.apply(&patch(doc("y", true))),
            ConfigState::Present(doc("y", true))
        );
    }
}
