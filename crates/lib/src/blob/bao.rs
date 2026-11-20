//! bao verified-streaming adapter over our raw-codec BLAKE3 CIDs.
//!
//! BLAKE3's hash is the root of a Merkle tree over the content, so the bao root
//! is **identical** to our raw-codec (`0x55`) BLAKE3-256 CID digest — verified
//! range streaming needs no address change.
//!
//! This is the only module using the pre-1.0 `bao-tree` crate, so
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
        outboard::{PreOrderMemOutboard, PreOrderOutboard},
        round_up_to_chunks,
        sync::{DecodeResponseIter, ReadAt, encode_ranges_validated},
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
    crate::backend::validate_blob_cid(cid)?;
    let digest = cid.as_cid().unwrap().hash().digest();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest);
    Ok(bao_tree::blake3::Hash::from_bytes(bytes))
}

/// Compute the pre-order bao outboard (interior hash-tree nodes) for `data`.
///
/// This is the sidecar persisted alongside a blob: ~0.4% of `data` at the
/// 16 KiB block size, and the input to [`encode_range_with_outboard`] so a range
/// serve never re-hashes the whole blob. The returned bytes carry no length
/// prefix — the size is stored separately (the `blobs.size` column).
pub(crate) fn compute_outboard(data: &[u8]) -> Vec<u8> {
    PreOrderMemOutboard::create(data, BLOCK_SIZE).data
}

/// The chunk-group-aligned byte window of `data` that
/// [`encode_range_with_outboard`] must be given to serve `range`.
///
/// bao reads whole [`BLOCK_SIZE`]-aligned leaves, so serving an arbitrary byte
/// range needs the data snapped out to block boundaries (and clamped to the
/// blob `size`). The caller reads exactly this window from storage — not the
/// whole blob — and hands it back in. Empty and past-EOF reads of a nonempty
/// blob carry its last leaf, authenticating the size before returning no bytes.
pub(crate) fn serve_window(size: u64, range: Range<u64>) -> Range<u64> {
    let block = BLOCK_SIZE.bytes() as u64;
    let Range { start, end } = proof_range(size, range);
    if start == end {
        return start..start;
    }
    let win_start = (start / block) * block;
    let win_end = end.div_ceil(block).saturating_mul(block).min(size);
    win_start..win_end
}

/// An empty result still needs proof that the peer's claimed EOF belongs to
/// this CID. Authenticate the last byte rather than trusting a bare size prefix.
fn proof_range(size: u64, range: Range<u64>) -> Range<u64> {
    let start = range.start.min(size);
    let end = range.end.clamp(start, size);
    if start == end {
        size.saturating_sub(1)..size
    } else {
        start..end
    }
}

/// A [`ReadAt`] over a single in-memory window of a blob, mapping the blob's
/// absolute byte offsets onto a buffer that starts at `base`.
///
/// bao addresses leaves by absolute offset; we only hold the
/// [`serve_window`]-sized slice, so reads translate `pos - base` into `buf` and
/// report EOF for anything past the window. Every leaf bao reads for a request
/// lies inside the window by construction, so a read outside it signals a
/// malformed request rather than normal end-of-data.
struct WindowReader<'a> {
    base: u64,
    buf: &'a [u8],
}

impl ReadAt for WindowReader<'_> {
    fn read_at(&self, pos: u64, out: &mut [u8]) -> std::io::Result<usize> {
        if pos < self.base {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "bao read below the served window",
            ));
        }
        let rel = (pos - self.base) as usize;
        if rel >= self.buf.len() {
            return Ok(0);
        }
        let n = out.len().min(self.buf.len() - rel);
        out[..n].copy_from_slice(&self.buf[rel..rel + n]);
        Ok(n)
    }
}

/// Encode a verified bao stream covering `range`, using a *persisted* outboard
/// and only the [`serve_window`] bytes of the blob — never the whole blob.
///
/// `size` is the blob's total length, `outboard` its persisted pre-order tree
/// (from [`compute_outboard`]), and `window_bytes` the slice of the blob at
/// `[window_start, window_start + window_bytes.len())` (which must be the
/// [`serve_window`] for `range`). The blob's `cid` supplies the bao root, so the
/// encoder *validates* the window against the persisted tree as it encodes —
/// local bit-rot is caught here, not shipped to the peer. Returns the
/// self-describing wire form documented in the module header.
pub(crate) fn encode_range_with_outboard(
    cid: &ID,
    size: u64,
    outboard: &[u8],
    window_start: u64,
    window_bytes: &[u8],
    range: Range<u64>,
) -> Result<Vec<u8>> {
    let root = cid_to_hash(cid)?;
    if size > crate::backend::DEFAULT_MAX_BLOB_BYTES as u64 {
        return Err(BackendError::BlobTooLarge {
            size: size.try_into().unwrap_or(usize::MAX),
            max: crate::backend::DEFAULT_MAX_BLOB_BYTES,
        }
        .into());
    }
    let tree = BaoTree::new(size, BLOCK_SIZE);
    // The I/O-backed reader returns an error for a truncated persisted
    // outboard; PreOrderMemOutboard assumes a complete buffer and can panic.
    let outboard = PreOrderOutboard {
        root,
        tree,
        data: outboard,
    };

    let ranges = byte_chunks(proof_range(size, range));

    let reader = WindowReader {
        base: window_start,
        buf: window_bytes,
    };
    let mut out = size.to_le_bytes().to_vec();
    encode_ranges_validated(reader, &outboard, &ranges, &mut out)
        .map_err(|_| BackendError::BlobStreamInvalid { cid: cid.clone() })?;
    Ok(out)
}

/// Test-only convenience: outboard + window-encode a range over an in-memory
/// blob in one call. Mirrors the production serve path (persisted outboard +
/// windowed read) so the unit tests exercise the same code, just sourcing the
/// outboard and window from `data` directly.
#[cfg(test)]
pub(crate) fn encode_range(data: &[u8], range: Range<u64>) -> Vec<u8> {
    let cid = ID::from_bytes(data);
    let outboard = compute_outboard(data);
    let window = serve_window(data.len() as u64, range.clone());
    let window_bytes = &data[window.start as usize..window.end as usize];
    encode_range_with_outboard(
        &cid,
        data.len() as u64,
        &outboard,
        window.start,
        window_bytes,
        range,
    )
    .expect("in-memory bao encode cannot fail")
}

/// Decode and verify a bao stream (as produced by [`encode_range_with_outboard`]) against
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
    if size > crate::backend::DEFAULT_MAX_BLOB_BYTES as u64 {
        return Err(BackendError::BlobTooLarge {
            size: size.try_into().unwrap_or(usize::MAX),
            max: crate::backend::DEFAULT_MAX_BLOB_BYTES,
        }
        .into());
    }
    let tree = BaoTree::new(size, BLOCK_SIZE);

    // Clamp the request to the real blob size before snapping to chunks.
    let start = range.start.min(size);
    let end = range.end.clamp(start, size);
    // The empty blob has no leaf to prove, but its CID is known. A forged zero
    // size must not turn a nonempty CID into a successful empty whole receive.
    if size == 0 {
        return if root == bao_tree::blake3::hash(&[]) && body.is_empty() {
            Ok(Vec::new())
        } else {
            Err(BackendError::BlobStreamInvalid { cid: cid.clone() }.into())
        };
    }
    let proof = proof_range(size, range);
    let ranges = byte_chunks(proof.clone());

    let iter = DecodeResponseIter::new(root, tree, std::io::Cursor::new(body), &ranges);

    // Collect the verified proof window, then slice out the requested bytes.
    // Empty reads authenticate one final leaf; other reads buffer only the
    // chunk-aligned requested window, never the whole blob unnecessarily.
    let window_start = chunk_floor(proof.start);
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

    if window.len() < (proof.end - window_start) as usize {
        // The stream did not actually cover the requested proof range.
        return Err(BackendError::BlobStreamInvalid { cid: cid.clone() }.into());
    }
    if start == end {
        return Ok(Vec::new());
    }
    let lo = (start - window_start) as usize;
    let hi = (end - window_start) as usize;
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
    fn empty_and_past_eof_ranges_decode_to_empty() {
        let data = blob(100_000);
        let cid = ID::from_bytes(&data);
        let n = data.len() as u64;

        // Empty, reversed and past-EOF ranges return no bytes, but authenticate
        // the claimed EOF using the last leaf rather than an unproved size.
        for range in [10..10, Range { start: 8, end: 4 }, n..n, n + 100..n + 200] {
            let encoded = encode_range(&data, range.clone());
            assert_eq!(
                decode_range(&cid, range.clone(), &encoded).unwrap(),
                Vec::<u8>::new()
            );
            assert!(decode_range(&cid, range.clone(), &encoded[..8]).is_err());
            let mut tampered = encoded;
            *tampered.last_mut().unwrap() ^= 1;
            assert!(decode_range(&cid, range, &tampered).is_err());
        }
        let cid = ID::from_bytes([]);
        let encoded = encode_range(&[], 0..u64::MAX);
        assert_eq!(
            decode_range(&cid, 0..u64::MAX, &encoded).unwrap(),
            Vec::<u8>::new()
        );
        assert!(decode_range(&ID::from_bytes(&data), 0..u64::MAX, &encoded).is_err());
    }

    #[test]
    fn wrong_cid_is_rejected() {
        let data = blob(50_000);
        let other = ID::from_bytes(b"a different blob entirely");
        let encoded = encode_range(&data, 0..data.len() as u64);
        assert!(decode_range(&other, 0..data.len() as u64, &encoded).is_err());
    }

    #[test]
    fn blob_truncated_outboard_is_rejected() {
        let data = blob(100_000);
        let cid = ID::from_bytes(&data);
        let mut outboard = compute_outboard(&data);
        outboard.truncate(1);
        let range = 40_000..40_123;
        let window = serve_window(data.len() as u64, range.clone());
        let result = encode_range_with_outboard(
            &cid,
            data.len() as u64,
            &outboard,
            window.start,
            &data[window.start as usize..window.end as usize],
            range,
        );
        assert!(matches!(result, Err(crate::Error::Backend(e))
            if matches!(*e, BackendError::BlobStreamInvalid { .. })));
    }

    #[test]
    fn blob_stream_rejects_a_retagged_hash_algorithm() {
        let data = b"valid BLAKE3 bytes";
        let original = ID::from_bytes(data);
        let digest = original.as_cid().unwrap().hash().digest();
        let hash = cid::multihash::Multihash::<64>::wrap(0x12, digest).unwrap();
        let retagged = ID::from(cid::Cid::new_v1(0x55, hash));
        let encoded = encode_range(data, 0..data.len() as u64);
        assert!(decode_range(&retagged, 0..data.len() as u64, &encoded).is_err());
        assert_eq!(
            decode_range(&original, 0..data.len() as u64, &encoded).unwrap(),
            data
        );
    }

    #[test]
    fn blob_stream_rejects_an_over_cap_size_even_for_empty_ranges() {
        let cid = ID::from_bytes(b"small");
        let encoded = (crate::backend::DEFAULT_MAX_BLOB_BYTES as u64 + 1).to_le_bytes();
        assert!(decode_range(&cid, u64::MAX..u64::MAX, &encoded).is_err());
    }

    #[test]
    fn blob_range_rejects_wrong_codec_and_digest_length() {
        let data = b"verified local range";
        let original = ID::from_bytes(data);
        let digest = original.as_cid().unwrap().hash().digest();
        let encoded = encode_range(data, 0..data.len() as u64);
        let wrong_codec = ID::from(cid::Cid::new_v1(0x71, *original.as_cid().unwrap().hash()));
        let short = cid::multihash::Multihash::<64>::wrap(0x1e, &digest[..31]).unwrap();
        for invalid in [wrong_codec, ID::from(cid::Cid::new_v1(0x55, short))] {
            assert!(
                matches!(decode_range(&invalid, 0..1, &encoded), Err(crate::Error::Backend(e)) if matches!(*e, BackendError::BlobInvalidCodec { .. }))
            );
        }
        assert_eq!(
            decode_range(&original, 0..data.len() as u64, &encoded).unwrap(),
            data
        );
    }
}
