//! bao verified-streaming adapter over our raw-codec BLAKE3 CIDs.
//!
//! BLAKE3's hash is the root of a Merkle tree over the content, so the bao root
//! is **identical** to our raw-codec (`0x55`) BLAKE3-256 CID digest — verified
//! range streaming needs no address change (see the Object Storage Design §7).
//!
//! This is the *only* place the pinned, pre-1.0 `bao-tree` crate is touched, so
//! a version bump is contained to one file. The encoded wire form is
//! self-describing: an 8-byte little-endian total blob size, followed by the
//! bao range encoding (interleaved parent hashes + leaf data) for the
//! chunk-aligned superset of the requested byte range. The receiver rebuilds the
//! tree from the size, then verifies every parent and leaf against the CID as it
//! decodes — a tampered or wrong-length stream is rejected, so bytes returned by
//! [`decode_range`] are guaranteed to belong to `cid`.

use std::ops::Range;

use bao_tree::{
    BaoTree, BlockSize, ByteRanges, ChunkRanges,
    io::{
        outboard::PreOrderMemOutboard,
        round_up_to_chunks,
        sync::{DecodeResponseIter, encode_ranges_validated},
    },
};

use crate::{Result, backend::errors::BackendError, entry::ID};

/// 16 KiB chunk groups (`2^4` BLAKE3 chunks), matching iroh's `IROH_BLOCK_SIZE`.
/// Governs outboard granularity and range alignment only — the root hash is
/// block-size-independent, so this never affects the CID.
const BLOCK_SIZE: BlockSize = BlockSize::from_chunk_log(4);

/// Number of leading bytes carrying the little-endian total blob size.
const SIZE_PREFIX: usize = 8;

/// Convert a byte range into the chunk ranges bao operates on, snapping up to
/// whole BLAKE3 chunks (bao addresses chunks, not arbitrary bytes).
fn byte_chunks(range: Range<u64>) -> ChunkRanges {
    round_up_to_chunks(ByteRanges::from(range).as_ref())
}

/// Map our CID's digest to a `bao_tree::blake3::Hash` (the bao root).
fn cid_to_hash(cid: &ID) -> Result<bao_tree::blake3::Hash> {
    let digest = cid
        .as_cid()
        .map(|c| c.hash().digest())
        .filter(|d| d.len() == 32)
        .ok_or_else(|| BackendError::BlobInvalidCodec { cid: cid.clone() })?;
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest);
    Ok(bao_tree::blake3::Hash::from_bytes(bytes))
}

/// Encode a verified bao stream covering `range` of `data`.
///
/// The outboard is recomputed from `data` (cheap; BLAKE3 is fast) — Phase 2
/// does not persist outboards. Returns the self-describing wire form documented
/// in the module header. In-memory encoding is infallible.
pub(crate) fn encode_range(data: &[u8], range: Range<u64>) -> Vec<u8> {
    let outboard = PreOrderMemOutboard::create(data, BLOCK_SIZE);
    // Clamp to the blob size so encoder and decoder snap to the same chunks.
    let len = data.len() as u64;
    let start = range.start.min(len);
    let end = range.end.clamp(start, len);
    let ranges = byte_chunks(start..end);

    let mut out = (data.len() as u64).to_le_bytes().to_vec();
    encode_ranges_validated(data, &outboard, &ranges, &mut out)
        .expect("in-memory bao encode cannot fail");
    out
}

/// Decode and verify a bao stream (as produced by [`encode_range`]) against
/// `cid`, returning exactly the bytes for `range`.
///
/// Every parent and leaf is checked against `cid` while decoding, so a tampered,
/// malformed, or wrong-length stream is rejected with
/// [`BackendError::BlobStreamInvalid`]. The requested `range` is clamped to the
/// blob's real size (carried in the stream).
pub(crate) fn decode_range(cid: &ID, range: Range<u64>, encoded: &[u8]) -> Result<Vec<u8>> {
    if encoded.len() < SIZE_PREFIX {
        return Err(BackendError::BlobStreamInvalid { cid: cid.clone() }.into());
    }
    let (size_bytes, body) = encoded.split_at(SIZE_PREFIX);
    let size = u64::from_le_bytes(size_bytes.try_into().expect("8 bytes"));

    let root = cid_to_hash(cid)?;
    let tree = BaoTree::new(size, BLOCK_SIZE);

    // Clamp the request to the real blob size before snapping to chunks.
    let start = range.start.min(size);
    let end = range.end.clamp(start, size);
    let ranges = byte_chunks(start..end);

    let iter = DecodeResponseIter::new(root, tree, std::io::Cursor::new(body), &ranges);

    // Collect the verified leaves covering the requested window. Leaves arrive
    // chunk-aligned, so we assemble from the first chunk boundary at/below
    // `start` and slice the exact range out at the end. Only the requested
    // window's leaves are buffered — memory is bounded by the range, not the
    // blob.
    let window_start = chunk_floor(start);
    let mut window: Vec<u8> = Vec::new();
    for item in iter {
        let item = item.map_err(|_| BackendError::BlobStreamInvalid { cid: cid.clone() })?;
        if let bao_tree::io::BaoContentItem::Leaf(leaf) = item {
            let offset = leaf.offset;
            let data = leaf.data;
            if offset < window_start {
                // A leaf below our window shouldn't happen for a chunk-aligned
                // request; treat as a malformed stream rather than underflow.
                return Err(BackendError::BlobStreamInvalid { cid: cid.clone() }.into());
            }
            let rel = (offset - window_start) as usize;
            if window.len() < rel + data.len() {
                window.resize(rel + data.len(), 0);
            }
            window[rel..rel + data.len()].copy_from_slice(&data);
        }
    }

    let lo = (start - window_start) as usize;
    let hi = (end - window_start) as usize;
    if window.len() < hi {
        // The stream did not actually cover the requested range.
        return Err(BackendError::BlobStreamInvalid { cid: cid.clone() }.into());
    }
    Ok(window[lo..hi].to_vec())
}

/// Largest chunk-group boundary at or below `pos` (bytes).
fn chunk_floor(pos: u64) -> u64 {
    let block = BLOCK_SIZE.bytes() as u64;
    (pos / block) * block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn round_trip_full_range() {
        let data = blob(100_000);
        let cid = ID::from_bytes(&data);
        let encoded = encode_range(&data, 0..data.len() as u64);
        let got = decode_range(&cid, 0..data.len() as u64, &encoded).unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn round_trip_sub_range() {
        let data = blob(100_000);
        let cid = ID::from_bytes(&data);
        let range = 40_000u64..40_123u64;
        let encoded = encode_range(&data, range.clone());
        let got = decode_range(&cid, range.clone(), &encoded).unwrap();
        assert_eq!(got, &data[range.start as usize..range.end as usize]);
    }

    #[test]
    fn round_trip_small_blob() {
        let data = b"small".to_vec();
        let cid = ID::from_bytes(&data);
        let encoded = encode_range(&data, 0..data.len() as u64);
        let got = decode_range(&cid, 0..data.len() as u64, &encoded).unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn tampered_stream_is_rejected() {
        let data = blob(100_000);
        let cid = ID::from_bytes(&data);
        let mut encoded = encode_range(&data, 0..data.len() as u64);
        // Flip a byte deep in the leaf data.
        let last = encoded.len() - 1;
        encoded[last] ^= 0xff;
        assert!(decode_range(&cid, 0..data.len() as u64, &encoded).is_err());
    }

    #[test]
    fn wrong_cid_is_rejected() {
        let data = blob(50_000);
        let other = ID::from_bytes(b"a different blob entirely");
        let encoded = encode_range(&data, 0..data.len() as u64);
        assert!(decode_range(&other, 0..data.len() as u64, &encoded).is_err());
    }
}
