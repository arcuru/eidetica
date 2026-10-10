//! Concrete operation/state encoding for the `table:v0.1` format.

use serde_bytes::ByteBuf;

use crate::Result;
use crate::crdt::{CRDT, CRDTError, Codec, LwwMap};

/// Exact-key LWW operations with opaque application row bytes.
///
/// The `table:v0.1` envelope is a DAG-CBOR sequence of key/operation pairs in
/// UTF-8 lexicographic key order, with `Set` payloads encoded as byte strings.
/// Tombstones remain part of both encoded operations and reduced state. Row
/// bytes are never parsed or normalized, and this representation is independent
/// of the application's row type and [`super::RowCodec`].
///
/// ```
/// use eidetica::{crdt::{CRDT, Codec, Lww}, store::TableData};
/// use serde_bytes::ByteBuf;
///
/// let mut state = TableData::default();
/// state.0.set("a.b".into(), ByteBuf::from(vec![0xff, 0]));
/// let mut later = TableData::default();
/// later.0.delete("a.b".into());
/// let merged = state.merge(&later)?;
/// let decoded = TableData::decode(&merged.encode()?)?;
/// assert_eq!(decoded.0.operation(&"a.b".into()), Some(&Lww::Delete));
/// # Ok::<(), eidetica::Error>(())
/// ```
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct TableData(pub LwwMap<String, ByteBuf>);

impl CRDT for TableData {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self(self.0.merge(&other.0)?))
    }
}

impl Codec for TableData {
    fn encode(&self) -> Result<Vec<u8>> {
        serde_ipld_dagcbor::to_vec(&self.0).map_err(|error| {
            CRDTError::SerializationFailed {
                reason: error.to_string(),
            }
            .into()
        })
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let data = Self(serde_ipld_dagcbor::from_slice(bytes).map_err(|error| {
            CRDTError::DeserializationFailed {
                reason: error.to_string(),
            }
        })?);
        // The dependency accepts alternate lengths and pair order. Only the
        // encoder's exact envelope is the table:v0.1 format.
        if data.encode()? != bytes {
            return Err(CRDTError::DeserializationFailed {
                reason: "noncanonical table:v0.1 DAG-CBOR envelope".into(),
            }
            .into());
        }
        Ok(data)
    }
}
