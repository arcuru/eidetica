//! Project a single declaration from the signed `_index` Doc delta, not from
//! the lossy (and deliberately last-writer-wins) ordinary registry view.

use std::{collections::HashMap, fmt};

use base64ct::{Base64, Encoding};
use serde::{
    Deserialize, Deserializer,
    de::{Error, MapAccess, Visitor},
};
use serde_json::value::RawValue;

use crate::{
    Result,
    crdt::{Doc, dag_cbor, doc::Value},
};

use super::{
    PasswordStoreConfig,
    schema::{ContractDescriptor, invalid},
};

pub(crate) const DECLARATION: &str = "structured_contract";
pub(crate) const PASSWORD_TYPE: &str = "encrypted:password:v0";

pub(crate) fn declaration_value(id: &ContractDescriptor) -> Result<Value> {
    id.validate()?;
    // Doc has no binary leaf. Use one atomic text leaf carrying the canonical
    // descriptor, so neither a partial Doc merge nor name escaping can change it.
    Ok(Value::Text(Base64::encode_string(&dag_cbor::encode(id)?)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Declaration {
    Plain(ContractDescriptor),
    Wrapped(PasswordStoreConfig),
    PlainConfigPatch,
    Legacy,
    Missing,
    Malformed,
    Unsupported,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDoc<'a> {
    #[serde(rename = "_v", default)]
    version: u8,
    #[serde(rename = "_a", default)]
    atomic: bool,
    #[serde(borrow)]
    children: Fields<'a>,
}

#[derive(Deserialize)]
enum DocValue<'a> {
    #[serde(borrow)]
    Doc(RawDoc<'a>),
}

#[derive(Deserialize)]
enum TextValue {
    Text(String),
}

// Validate duplicates before constructing a Doc. Only the relevant row and
// reserved containers are interpreted; unrelated Store configs remain opaque.
struct Fields<'a>(HashMap<String, &'a RawValue>);

impl<'de: 'a, 'a> Deserialize<'de> for Fields<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Unique;
        impl<'de> Visitor<'de> for Unique {
            type Value = Fields<'de>;
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
                Ok(Fields(fields))
            }
        }
        let Fields(fields) = d.deserialize_map(Unique)?;
        Ok(Fields(fields))
    }
}

impl<'a> RawDoc<'a> {
    fn get(&self, key: &str) -> Option<&'a RawValue> {
        self.children.0.get(key).copied()
    }

    fn nested(raw: &'a RawValue) -> Result<Self> {
        let DocValue::Doc(doc) = serde_json::from_str(raw.get())?;
        doc.check_version()?;
        Ok(doc)
    }

    fn check_version(&self) -> Result<()> {
        if self.version != 0 {
            return Err(invalid("unsupported index Doc version"));
        }
        Ok(())
    }

    fn text(&self, key: &str) -> Result<Option<String>> {
        self.get(key)
            .map(|raw| {
                let TextValue::Text(value) = serde_json::from_str(raw.get())?;
                Ok(value)
            })
            .transpose()
    }
}

/// `None` means this delta leaves the declaration alone. Invalid reserved
/// changes are events too: a descendant can replace them, but they may never
/// be treated as if the older declaration survived.
pub(crate) fn project(index: &[u8], name: &str) -> Option<Declaration> {
    project_row(index, name).unwrap_or(Some(Declaration::Malformed))
}

fn project_row(index: &[u8], name: &str) -> Result<Option<Declaration>> {
    let index: RawDoc<'_> = serde_json::from_slice(index)?;
    index.check_version()?;
    let Some(row) = index.get(name) else {
        return Ok(index.atomic.then_some(Declaration::Missing));
    };
    if row.get() == "\"Deleted\"" {
        return Ok(Some(Declaration::Missing));
    }
    let row = RawDoc::nested(row)?;
    let type_id = row.text("type")?;
    if let Some(raw) = row.get(DECLARATION) {
        if raw.get() == "\"Deleted\"" {
            return Ok(Some(Declaration::Missing));
        }
        let TextValue::Text(encoded) = serde_json::from_str(raw.get())?;
        let bytes =
            Base64::decode_vec(&encoded).map_err(|_| invalid("invalid declaration encoding"))?;
        let id: ContractDescriptor = dag_cbor::decode(&bytes)?;
        id.validate()?;
        if type_id.as_deref() != Some(id.store.as_str()) || !is_structured_family(&id.store) {
            return Err(invalid(
                "declaration does not match a structured registration",
            ));
        }
        return Ok(Some(Declaration::Plain(id)));
    }
    match type_id.as_deref() {
        Some(PASSWORD_TYPE) => {
            let raw = row
                .get("config")
                .ok_or_else(|| invalid("missing wrapped config"))?;
            let config = RawDoc::nested(raw)?;
            if config.children.0.is_empty() {
                return Ok(Some(Declaration::Missing));
            }
            if !config.atomic || config.children.0.len() != 2 {
                return Err(invalid("expected a complete atomic wrapped config"));
            }
            for member in ["encryption", "wrapped_config"] {
                let nested = RawDoc::nested(
                    config
                        .get(member)
                        .ok_or_else(|| invalid("missing wrapped config member"))?,
                )?;
                validate_wrapped_fields(member, &nested)?;
            }
            let DocValueOwned::Doc(doc) = serde_json::from_str(raw.get())?;
            Ok(Some(Declaration::Wrapped(PasswordStoreConfig::try_from(
                &doc,
            )?)))
        }
        Some(id) if is_legacy(id) => Ok(Some(Declaration::Legacy)),
        Some(_) => Ok(Some(Declaration::Unsupported)),
        None if !row.atomic
            && !index.atomic
            && row.children.0.keys().all(|key| key == "settings") =>
        {
            Ok(None)
        }
        None if row.atomic || index.atomic => Ok(Some(Declaration::Missing)),
        None if row
            .children
            .0
            .keys()
            .all(|key| key == "settings" || key == "config") =>
        {
            RawDoc::nested(
                row.get("config")
                    .ok_or_else(|| invalid("missing config patch"))?,
            )?;
            Ok(Some(Declaration::PlainConfigPatch))
        }
        None => Ok(Some(Declaration::Malformed)),
    }
}

#[derive(Deserialize)]
enum DocValueOwned {
    Doc(Doc),
}

fn validate_wrapped_fields(member: &str, doc: &RawDoc<'_>) -> Result<()> {
    for (field, raw) in &doc.children.0 {
        match (member, field.as_str()) {
            ("wrapped_config", "ciphertext" | "nonce")
            | ("encryption", "algorithm" | "kdf" | "salt" | "version") => {
                serde_json::from_str::<TextValue>(raw.get())?;
            }
            ("encryption", "argon2_m_cost" | "argon2_t_cost" | "argon2_p_cost") => {
                if !matches!(serde_json::from_str::<Value>(raw.get())?, Value::Int(cost) if u32::try_from(cost).is_ok())
                {
                    return Err(invalid("invalid wrapped config cost"));
                }
            }
            _ => return Err(invalid("unknown wrapped config member")),
        }
    }
    Ok(())
}

fn is_legacy(id: &str) -> bool {
    matches!(id, "docstore:v0" | "table:v0" | "settings:v0" | "ydoc:v0")
}

pub(crate) fn is_structured_family(id: &str) -> bool {
    !id.is_empty() && !is_legacy(id) && !matches!(id, PASSWORD_TYPE | "structured:v0.1")
}

pub(crate) fn validate_structured_store(name: &str, id: &ContractDescriptor) -> Result<()> {
    id.validate()?;
    if name.is_empty() || name.starts_with('_') || !is_structured_family(&id.store) {
        return Err(invalid(
            "invalid Store name or unsupported structured Store type",
        ));
    }
    Ok(())
}
