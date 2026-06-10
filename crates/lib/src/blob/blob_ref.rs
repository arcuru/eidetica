//! [`BlobRef`] — a typed, embeddable reference to a content-addressed blob.

use serde::{Deserialize, Serialize};

use crate::entry::ID;

/// A reference to a content-addressed blob: its CID plus the `size` of the bytes
/// it names.
///
/// A blob reference *can* be a bare CID string (and still is, on the wire — see
/// below), but carrying `size` alongside it lets a reader act *before* fetching
/// the bytes:
///
/// - **Size-before-fetch / DoS cap.** A content address bounds a blob's identity
///   but says nothing about how many bytes it names. With a declared `size` a
///   resolver can refuse an over-cap reference up front and budget the transfer,
///   rather than discovering the size only by downloading it (§5.4).
/// - **Length integrity.** The delivered bytes must hash to `cid` *and* be
///   exactly `size` bytes; a mismatch is rejected
///   ([`BlobSizeMismatch`](crate::backend::errors::BackendError::BlobSizeMismatch)).
/// - **Partial replicas.** A replica that does not hold everything can decide
///   fetch-now / fetch-lazily / never from `size` without first downloading the
///   blob to learn how big it is.
///
/// It stays trivially mergeable (last-writer-wins on the whole value): `cid`
/// serializes as the very same CID representation a bare reference would use
/// (a multibase string in human-readable formats), with `size` as an adjacent
/// field. So a `BlobRef` is *additive* over the "a reference is just a string"
/// model, not a replacement. This is the lean form; richer per-reference
/// metadata (`mime`, transport `hints`) layers on later without changing
/// `cid`/`size`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    /// The blob's content address (raw-codec `0x55` BLAKE3 CID).
    pub cid: ID,
    /// Byte length of the referenced blob.
    pub size: u64,
}

impl BlobRef {
    /// Construct a reference from a content address and the size of its bytes.
    pub fn new(cid: ID, size: u64) -> Self {
        Self { cid, size }
    }

    /// The referenced blob's content address.
    pub fn cid(&self) -> &ID {
        &self.cid
    }

    /// The declared byte length of the referenced blob.
    pub fn size(&self) -> u64 {
        self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_with_cid_as_string() {
        let cid = ID::from_bytes(b"some blob bytes");
        let blob_ref = BlobRef::new(cid.clone(), 15);

        let json = serde_json::to_string(&blob_ref).unwrap();
        // `cid` is the same multibase CID string a bare reference would use, and
        // `size` is an adjacent field — additive, not a new opaque encoding.
        assert!(json.contains(&cid.to_string()));
        assert!(json.contains("\"size\":15"));

        let restored: BlobRef = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, blob_ref);
        assert_eq!(restored.cid(), &cid);
        assert_eq!(restored.size(), 15);
    }
}
