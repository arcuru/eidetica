//! Versioned DAG-CBOR array envelopes shared by the built-in operation codecs.

use ipld_core::ipld::Ipld;

use super::CRDTError;
use crate::Result;

/// The only envelope revision currently written or accepted.
const REVISION: i128 = 0;

/// Encode `[REVISION, fields...]` as DAG-CBOR.
pub(super) fn encode(fields: impl IntoIterator<Item = Ipld>) -> Result<Vec<u8>> {
    let envelope = Ipld::List(
        std::iter::once(Ipld::Integer(REVISION))
            .chain(fields)
            .collect(),
    );
    serde_ipld_dagcbor::to_vec(&envelope).map_err(|error| {
        CRDTError::SerializationFailed {
            reason: error.to_string(),
        }
        .into()
    })
}

/// Decode one complete `[REVISION, fields...]` envelope and return its fields.
///
/// Trailing bytes, non-array envelopes and unknown revisions are rejected.
pub(super) fn decode(bytes: &[u8], type_name: &str) -> Result<Vec<Ipld>> {
    let envelope: Ipld = serde_ipld_dagcbor::from_slice(bytes)
        .map_err(|error| malformed(type_name, error.to_string()))?;
    let Ipld::List(mut items) = envelope else {
        return Err(malformed(type_name, "envelope is not an array"));
    };
    match items.first() {
        Some(Ipld::Integer(REVISION)) => Ok(items.split_off(1)),
        Some(Ipld::Integer(revision)) => Err(malformed(
            type_name,
            format!("unknown envelope revision {revision}"),
        )),
        _ => Err(malformed(type_name, "missing integer envelope revision")),
    }
}

/// A decode error for a structurally invalid envelope.
pub(super) fn malformed(type_name: &str, reason: impl std::fmt::Display) -> crate::Error {
    CRDTError::DeserializationFailed {
        reason: format!("invalid {type_name} encoding: {reason}"),
    }
    .into()
}
