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

    /// The actual data or reference to it
    pub data: Option<Vec<u8>>,
}
