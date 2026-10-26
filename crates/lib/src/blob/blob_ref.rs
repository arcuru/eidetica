//! [`BlobRef`] — a typed, embeddable reference to a content-addressed blob.

use serde::{Deserialize, Serialize};

use crate::entry::ID;

/// A reference to a content-addressed blob: its CID plus the `size` of the bytes
/// it names.
///
/// The CID serializes as a multibase string in JSON; size is an adjacent field.
/// [`crate::Instance::get_blob_ref`] checks the declared size before reading and
/// rejects a length mismatch. A reference is ordinary Store data, not a request
/// to retain or fetch bytes.
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
