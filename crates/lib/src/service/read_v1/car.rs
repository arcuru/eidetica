//! CARv1 framing for the existing canonical Entry format. gRPC chunks aren't
//! CAR sections; split length prefixes, CIDs and blocks are all legal.

use crate::{Entry, ID, Snapshot};
use cid::Cid;
use serde::{Deserialize, Serialize};

use super::{ReadError, wire};

pub(super) const MAX_HEADER_BYTES: usize = 8 * 1024;
// CIDv1 + DAG-CBOR codec + BLAKE3-256 multihash.
const ENTRY_CID_BYTES: usize = 36;

#[derive(Serialize, Deserialize)]
struct Header {
    roots: Vec<Cid>,
    version: u64,
}

pub(super) fn header(at: &Snapshot) -> Result<Vec<u8>, ReadError> {
    let roots = at
        .iter()
        .map(|id| id.as_cid().copied().ok_or(ReadError::InvalidBoundary))
        .collect::<Result<_, _>>()?;
    serde_ipld_dagcbor::to_vec(&Header { roots, version: 1 })
        .map_err(|_| ReadError::InvalidData("CAR header encoding"))
}

pub(super) fn check_header(bytes: &[u8], expected: &Snapshot) -> Result<(), ReadError> {
    let header: Header = serde_ipld_dagcbor::from_slice(bytes)
        .map_err(|_| ReadError::InvalidData("invalid CAR header"))?;
    let roots: Vec<_> = header.roots.into_iter().map(ID::from).collect();
    if header.version != 1 || roots != expected.tips() {
        return Err(ReadError::InvalidData(
            "CAR roots do not match the read cut",
        ));
    }
    Ok(())
}

pub(super) fn decode_entry(section: Vec<u8>, max_object_bytes: usize) -> Result<Entry, ReadError> {
    let (cid_bytes, block) = section
        .split_at_checked(ENTRY_CID_BYTES)
        .ok_or(ReadError::InvalidData("short CAR section"))?;
    if block.is_empty() {
        return Err(ReadError::InvalidData("empty Entry body"));
    }
    if block.len() > max_object_bytes {
        return Err(ReadError::Limit);
    }
    let id = ID::from_dagcbor_bytes(block);
    if id.as_cid().is_none_or(|c| c.to_bytes() != cid_bytes) {
        return Err(ReadError::InvalidData("CAR Entry CID mismatch"));
    }
    let entry = serde_ipld_dagcbor::from_slice::<Entry>(block)
        .map_err(|_| ReadError::InvalidData("invalid Entry encoding"))?;
    drop(section);
    entry
        .validate()
        .map_err(|_| ReadError::InvalidData("invalid Entry structure"))?;
    if entry.id_ref() != &id {
        return Err(ReadError::InvalidData("noncanonical Entry bytes"));
    }
    Ok(entry)
}

/// The body is transferred diffusely in chunks, without combining a CID and
/// object into a second object-sized buffer.
pub(super) fn entry_body(entry: &Entry) -> Result<(Vec<u8>, Vec<u8>), ReadError> {
    let cid = entry
        .id_ref()
        .as_cid()
        .ok_or(ReadError::InvalidData("missing Entry CID"))?
        .to_bytes();
    let bytes = entry
        .canonical_bytes()
        .map_err(|_| ReadError::InvalidData("Entry encoding"))?;
    if bytes.len() > wire::OBJECT_BYTES {
        return Err(ReadError::Limit);
    }
    Ok((cid, bytes))
}

pub(super) fn max_section(object: usize) -> usize {
    object + ENTRY_CID_BYTES
}
