use serde_json::Value;
use uuid::Uuid;

/// Represents a single entry in the metadata table
#[derive(Debug, Clone)]
pub struct MetadataEntry {
    /// UUIDv7 that serves as unique identifier across all devices
    pub id: Uuid,

    /// UUID identifying the device that created this entry
    pub device_id: Uuid,

    /// Whether this entry has been superseded by a newer version
    pub archived: bool,

    /// Optional reference to parent entry's UUID
    pub parent_id: Option<Uuid>,

    /// JSON metadata about the referenced data
    pub metadata: Value,

    /// Hash of the data with algorithm prefix
    /// Current format: "b2_" + hex(BLAKE2b-256)
    /// Allows for future hash algorithms with different prefixes
    pub data_hash: String,
}
