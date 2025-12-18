//! Height calculation strategies for entries in Eidetica databases.
//!
//! The height of an entry determines its position in the causal ordering of the Merkle DAG.
//! Different strategies provide different trade-offs between simplicity and time-awareness.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Height calculation strategy for entries in a database.
///
/// The height of an entry determines its position in the causal ordering
/// of the Merkle DAG. Different strategies provide different trade-offs:
///
/// - [`Incremental`](HeightStrategy::Incremental): Simple monotonic counter, optimal for offline-first
/// - [`Timestamp`](HeightStrategy::Timestamp): Time-aware ordering
///
/// # Database Configuration
///
/// Height strategy is configured at the database level and stored in
/// `_settings.height_strategy`. All transactions from a database inherit
/// the strategy. The strategy should be set at database creation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HeightStrategy {
    /// Incremental height: `height = max(parent_heights) + 1`
    ///
    /// Root entries have height 0. Each subsequent entry has height
    /// equal to the maximum height of its parents plus one.
    ///
    /// This is the default strategy and provides optimal behavior for
    /// offline-first scenarios where entries may be created without
    /// network connectivity.
    #[default]
    Incremental,

    /// Timestamp-based height: `height = max(current_timestamp_ms, max(parent_heights) + 1)`
    ///
    /// Uses milliseconds since Unix epoch as the height value, ensuring
    /// entries are ordered by creation time while maintaining monotonic
    /// progression (never less than parent height + 1).
    ///
    /// This strategy is useful for:
    /// - Time-series data where temporal ordering matters
    /// - Debugging and auditing (heights indicate creation time)
    /// - Scenarios where clock synchronization is reasonable
    ///
    /// **Note**: Requires reasonably synchronized clocks across clients.
    /// Clock skew may cause entries to have heights slightly in the future
    /// relative to other clients. Use caution.
    Timestamp,
}

impl HeightStrategy {
    /// Calculate the height for an entry given its parent heights.
    ///
    /// # Arguments
    /// * `max_parent_height` - The maximum height among all parent entries,
    ///   or `None` if this is a root entry (no parents)
    ///
    /// # Returns
    /// The calculated height for the new entry
    pub fn calculate_height(&self, max_parent_height: Option<u64>) -> u64 {
        match self {
            HeightStrategy::Incremental => max_parent_height.map(|h| h + 1).unwrap_or(0),
            HeightStrategy::Timestamp => {
                let timestamp_ms = current_timestamp_ms();
                let min_height = max_parent_height.map(|h| h + 1).unwrap_or(0);

                // FIXME: Clock skew detection should be more sophisticated - track
                // cumulative skew, alert on persistent drift, and potentially reject
                // entries from peers with severely skewed clocks.
                if min_height > timestamp_ms {
                    let skew_ms = min_height - timestamp_ms;
                    tracing::warn!(
                        parent_height = max_parent_height.unwrap_or(0),
                        current_timestamp_ms = timestamp_ms,
                        skew_ms,
                        "Clock skew detected: parent timestamp is {}ms ahead of local clock",
                        skew_ms
                    );
                }

                timestamp_ms.max(min_height)
            }
        }
    }
}

/// Get current timestamp in milliseconds since Unix epoch.
fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_incremental_root() {
        let strategy = HeightStrategy::Incremental;
        assert_eq!(strategy.calculate_height(None), 0);
    }

    #[test]
    fn test_incremental_with_parent() {
        let strategy = HeightStrategy::Incremental;
        assert_eq!(strategy.calculate_height(Some(0)), 1);
        assert_eq!(strategy.calculate_height(Some(5)), 6);
        assert_eq!(strategy.calculate_height(Some(100)), 101);
    }

    #[test]
    fn test_timestamp_root() {
        let strategy = HeightStrategy::Timestamp;
        let height = strategy.calculate_height(None);
        // Should be current timestamp (roughly)
        let now = current_timestamp_ms();
        // Allow 1 second tolerance
        assert!(height >= now - 1000 && height <= now + 1000);
    }

    #[test]
    fn test_timestamp_with_low_parent() {
        let strategy = HeightStrategy::Timestamp;
        // Parent with low height - should use timestamp
        let height = strategy.calculate_height(Some(100));
        let now = current_timestamp_ms();
        assert!(height >= now - 1000 && height <= now + 1000);
    }

    #[test]
    fn test_timestamp_with_high_parent() {
        let strategy = HeightStrategy::Timestamp;
        // Parent with very high height (future timestamp) - should use parent + 1
        let future_height = current_timestamp_ms() + 1_000_000; // 1000 seconds in future
        let height = strategy.calculate_height(Some(future_height));
        assert_eq!(height, future_height + 1);
    }

    #[test]
    fn test_serialization() {
        // Test incremental serialization
        let json = serde_json::to_string(&HeightStrategy::Incremental).unwrap();
        assert_eq!(json, "\"incremental\"");

        // Test timestamp serialization
        let json = serde_json::to_string(&HeightStrategy::Timestamp).unwrap();
        assert_eq!(json, "\"timestamp\"");
    }

    #[test]
    fn test_deserialization() {
        let incremental: HeightStrategy = serde_json::from_str("\"incremental\"").unwrap();
        assert_eq!(incremental, HeightStrategy::Incremental);

        let timestamp: HeightStrategy = serde_json::from_str("\"timestamp\"").unwrap();
        assert_eq!(timestamp, HeightStrategy::Timestamp);
    }
}
