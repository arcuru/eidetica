//! Canonical DAG-CBOR for new structured persistence boundaries.
//!
//! Serde describes one complete logical value; nested values are not separately
//! encoded byte payloads. This opt-in helper does not implement [`super::Codec`]
//! or change any existing Store/Entry format. See the internal CRDT guide for the
//! wire domain and schema requirements.

use ipld_core::ipld::Ipld;
use serde::{Serialize, de::DeserializeOwned};

use super::CRDTError;

/// Encode a complete structured value in canonical DAG-CBOR.
///
/// Maps require unique text keys. Integers span `[-2^64, 2^64-1]`; floats must
/// be finite and are encoded as f64, with negative zero normalized to positive
/// zero. Use `serde_bytes` for binary leaves; ordinary `Vec<u8>` is an array.
/// A schema must explicitly represent integers outside the native range.
///
/// Serialization must expose all continuation-relevant state and stable wire
/// names. Container iteration order is immaterial, but array order is not.
pub fn encode<T: Serialize + ?Sized>(value: &T) -> crate::Result<Vec<u8>> {
    let encode = || {
        let bytes = serde_ipld_dagcbor::to_vec(value).map_err(|e| e.to_string())?;
        canonical_bytes(&bytes)
    };
    encode().map_err(|reason| CRDTError::SerializationFailed { reason }.into())
}

/// Admit exactly one canonical value under the requested typed schema.
///
/// Rejects noncanonical bytes rather than silently normalizing persisted history.
/// In addition to generic DAG-CBOR validation, the decoded value must re-encode
/// identically. This rejects lossy schema decoding (such as repeated Doc List
/// positions, ignored fields, or narrowing a float). Thus the schema needs both
/// Serde directions and must preserve its complete logical representation.
pub fn decode<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> crate::Result<T> {
    let decode = || {
        let value: T = serde_ipld_dagcbor::from_slice(bytes).map_err(|e| e.to_string())?;
        if encode(&value).map_err(|e| e.to_string())? != bytes {
            return Err("decoded schema does not preserve the complete wire value".to_owned());
        }
        Ok(value)
    };
    decode().map_err(|reason| CRDTError::DeserializationFailed { reason }.into())
}

fn canonical_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    // Ipld's visitor enforces text keys and rejects duplicates, unlike a typed
    // BTreeMap visitor. Re-encoding checks minimal forms/order/float width using
    // the library instead of maintaining a second CBOR parser.
    let mut value: Ipld = serde_ipld_dagcbor::from_slice(bytes).map_err(|e| e.to_string())?;
    normalize_zero(&mut value);
    serde_ipld_dagcbor::to_vec(&value).map_err(|e| e.to_string())
}

fn normalize_zero(value: &mut Ipld) {
    match value {
        Ipld::Float(number) if *number == 0.0 => *number = 0.0,
        Ipld::List(values) => values.iter_mut().for_each(normalize_zero),
        Ipld::Map(values) => values.values_mut().for_each(normalize_zero),
        _ => {}
    }
}
