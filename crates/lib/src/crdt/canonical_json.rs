//! Schema-independent RFC 8785 JSON values for canonical row payloads.

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

/// Validated JSON encoded with the `canonical-json:v0` (RFC 8785) codec.
///
/// Readers may deserialize a row into different Rust types, but Entry and record
/// bytes remain the same. This is not a Rust schema or a typed row container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalJson(Vec<u8>);

impl CanonicalJson {
    /// Canonicalize a serializable row, rejecting duplicate final member names,
    /// non-finite numbers, and integers outside the exactly representable JSON
    /// number domain.
    pub fn from_value<T: Serialize>(value: &T) -> serde_json::Result<Self> {
        // serde_json serializes non-finite floats as null. JCS rejects them; do
        // this first without using its output, since its object set drops duplicates.
        serde_json_canonicalizer::to_vec(value)?;
        let json = serde_json::to_vec(value)?;
        Self::parse(&json)
    }

    /// Parse JSON while checking every object before canonicalizing it.
    pub fn parse(json: &[u8]) -> serde_json::Result<Self> {
        let mut deserializer = serde_json::Deserializer::from_slice(json);
        let value = StrictValue::deserialize(&mut deserializer)?.0;
        deserializer.end()?;
        Ok(Self(serde_json_canonicalizer::to_vec(&value)?))
    }

    /// Return the validated RFC 8785 encoding without schema conversion.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Deserialize at the typed API boundary, not during reduction or projection.
    pub fn to_value<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.0)
    }
}

// Value's ordinary JSON deserializer overwrites duplicate object members.
// Parse through a visitor instead, including nested maps and struct output.
struct StrictValue(serde_json::Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an I-JSON value without duplicate members")
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(v.into()))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(serde_json::Value::Null))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(v.into()))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
        Ok(StrictValue(v.into()))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
        if (v as f64) as i128 != v as i128 {
            return Err(E::custom(
                "integer is not exactly representable as a JSON double",
            ));
        }
        Ok(StrictValue(v.into()))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
        if (v as f64) as u128 != v as u128 {
            return Err(E::custom(
                "integer is not exactly representable as a JSON double",
            ));
        }
        Ok(StrictValue(v.into()))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        let number =
            serde_json::Number::from_f64(v).ok_or_else(|| E::custom("non-finite number"))?;
        Ok(StrictValue(number.into()))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(values.into()))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut values = BTreeMap::new();
        while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
            if values.insert(key, value.0).is_some() {
                return Err(serde::de::Error::custom("duplicate JSON object member"));
            }
        }
        Ok(StrictValue(
            serde_json::to_value(values).map_err(serde::de::Error::custom)?,
        ))
    }
}

impl Serialize for CanonicalJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !serializer.is_human_readable() {
            return serde_bytes::Bytes::new(&self.0).serialize(serializer);
        }
        // Retain UTF-16 key order in JSON Entry deltas.
        let raw = serde_json::value::RawValue::from_string(
            String::from_utf8(self.0.clone()).map_err(serde::ser::Error::custom)?,
        )
        .map_err(serde::ser::Error::custom)?;
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CanonicalJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if !deserializer.is_human_readable() {
            let bytes = serde_bytes::ByteBuf::deserialize(deserializer)?;
            return Self::parse(bytes.as_ref()).map_err(serde::de::Error::custom);
        }
        let value = StrictValue::deserialize(deserializer)?.0;
        let bytes = serde_json_canonicalizer::to_vec(&value).map_err(serde::de::Error::custom)?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_and_boundary_vectors() {
        let vectors = [
            (
                r#"{"b":2,"a":{"z":-0.0,"a":1e-7}}"#,
                r#"{"a":{"a":1e-7,"z":0},"b":2}"#,
            ),
            (
                r#"{"\uE000":1,"\uD83D\uDE00":2,"a":3}"#,
                "{\"a\":3,\"😀\":2,\"\":1}",
            ),
            (r#"{"v":"\u000f/\n\\\""}"#, r#"{"v":"\u000f/\n\\\""}"#),
            (
                r#"{"n":9007199254740992,"e":1e30}"#,
                r#"{"e":1e+30,"n":9007199254740992}"#,
            ),
        ];
        for (input, expected) in vectors {
            let value = CanonicalJson::parse(input.as_bytes()).unwrap();
            assert_eq!(value.as_bytes(), expected.as_bytes());
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
        for invalid in [
            r#"{"a":1,"a":2}"#,
            r#"{"x":{"a":1,"\u0061":2}}"#,
            r#"{"x":9007199254740993}"#,
            r#"{"x":"\uD800"}"#,
            r#"{"x":1e400}"#,
        ] {
            assert!(
                CanonicalJson::parse(invalid.as_bytes()).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn typed_readers_cannot_change_canonical_bytes() {
        #[derive(Serialize, Deserialize)]
        struct Narrow {
            a: i32,
        }
        let row = CanonicalJson::parse(br#"{"z":2,"a":1}"#).unwrap();
        let _: Narrow = row.to_value().unwrap();
        assert_eq!(row.as_bytes(), br#"{"a":1,"z":2}"#);
        assert_eq!(
            CanonicalJson::from_value(&serde_json::json!({"z":2,"a":1})).unwrap(),
            row
        );
    }

    #[test]
    fn canonical_row_survives_entry_cbor_roundtrip() {
        let row = CanonicalJson::parse(br#"{"z":2,"a":1}"#).unwrap();
        let encoded = serde_ipld_dagcbor::to_vec(&row).unwrap();
        let decoded: CanonicalJson = serde_ipld_dagcbor::from_slice(&encoded).unwrap();
        assert_eq!(decoded, row);
    }

    #[test]
    fn row_operation_is_inline_json_not_an_escaped_doc_string() {
        use crate::crdt::LwwMap;
        let row = CanonicalJson::parse(br#"{"z":2,"a":1}"#).unwrap();
        let mut delta = LwwMap::new();
        delta.set("a.b".to_owned(), row.clone());
        assert_eq!(
            serde_json::to_string(&delta).unwrap(),
            r#"[["a.b",{"set":{"a":1,"z":2}}]]"#
        );
        let decoded: LwwMap<String, CanonicalJson> =
            serde_json::from_str(&serde_json::to_string(&delta).unwrap()).unwrap();
        assert_eq!(decoded.get(&"a.b".to_owned()), Some(&row));
        assert_eq!(
            serde_ipld_dagcbor::from_slice::<LwwMap<String, CanonicalJson>>(
                &serde_ipld_dagcbor::to_vec(&delta).unwrap()
            )
            .unwrap(),
            delta
        );
    }

    #[test]
    fn rejects_duplicate_fields_emitted_by_serializer() {
        struct Duplicate;
        impl Serialize for Duplicate {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("x", &1)?;
                map.serialize_entry("x", &2)?;
                map.end()
            }
        }
        assert!(CanonicalJson::from_value(&Duplicate).is_err());
        assert!(CanonicalJson::from_value(&f64::NAN).is_err());
    }
}
