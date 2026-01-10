//! Height calculation strategies for entries in Eidetica databases.
//!
//! The height of an entry determines its position in the causal ordering of the Merkle DAG.
//! Different strategies provide different trade-offs between simplicity and time-awareness.

use std::sync::Arc;

use crate::clock::Clock;
use serde::{Deserialize, Serialize};

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
    /// Convert this strategy into a [`HeightCalculator`] with the given clock.
    pub(crate) fn into_calculator(self, clock: Arc<dyn Clock>) -> HeightCalculator {
        HeightCalculator::new(self, clock)
    }
}

/// Runtime height calculator with a bound clock.
///
/// Created from a [`HeightStrategy`] via [`HeightStrategy::into_calculator`].
/// This separates the serializable configuration (strategy) from the
/// runtime behavior (strategy + clock).
#[derive(Clone)]
pub(crate) struct HeightCalculator {
    strategy: HeightStrategy,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for HeightCalculator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeightCalculator")
            .field("strategy", &self.strategy)
            .finish_non_exhaustive()
    }
}

impl HeightCalculator {
    /// Create a new calculator with the given strategy and clock.
    pub(crate) fn new(strategy: HeightStrategy, clock: Arc<dyn Clock>) -> Self {
        Self { strategy, clock }
    }

    /// Calculate the height for an entry given its parent heights.
    ///
    /// # Arguments
    /// * `max_parent_height` - The maximum height among all parent entries,
    ///   or `None` if this is a root entry (no parents)
    ///
    /// # Returns
    /// The calculated height for the new entry
    pub(crate) fn calculate_height(&self, max_parent_height: Option<u64>) -> u64 {
        match self.strategy {
            HeightStrategy::Incremental => max_parent_height.map(|h| h + 1).unwrap_or(0),
            HeightStrategy::Timestamp => {
                let timestamp_ms = self.clock.now_millis();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    #[test]
    fn test_incremental_root() {
        let clock = Arc::new(FixedClock::default());
        let calculator = HeightStrategy::Incremental.into_calculator(clock);
        assert_eq!(calculator.calculate_height(None), 0);
    }

    #[test]
    fn test_incremental_with_parent() {
        let clock = Arc::new(FixedClock::default());
        let calculator = HeightStrategy::Incremental.into_calculator(clock);
        assert_eq!(calculator.calculate_height(Some(0)), 1);
        assert_eq!(calculator.calculate_height(Some(5)), 6);
        assert_eq!(calculator.calculate_height(Some(100)), 101);
    }

    #[test]
    fn test_timestamp_root() {
        let clock = Arc::new(FixedClock::new(1704067200000)); // 2024-01-01 00:00:00 UTC
        let _hold = clock.hold();
        let calculator = HeightStrategy::Timestamp.into_calculator(clock.clone());
        let height = calculator.calculate_height(None);
        assert_eq!(height, 1704067200000);
    }

    #[test]
    fn test_timestamp_with_low_parent() {
        let clock = Arc::new(FixedClock::new(1704067200000)); // 2024-01-01 00:00:00 UTC
        let _hold = clock.hold();
        let calculator = HeightStrategy::Timestamp.into_calculator(clock.clone());
        // Parent with low height - should use timestamp
        let height = calculator.calculate_height(Some(100));
        assert_eq!(height, 1704067200000);
    }

    #[test]
    fn test_timestamp_with_high_parent() {
        let clock = Arc::new(FixedClock::new(1704067200000)); // 2024-01-01 00:00:00 UTC
        let _hold = clock.hold();
        let calculator = HeightStrategy::Timestamp.into_calculator(clock.clone());
        // Parent with very high height (future timestamp) - should use parent + 1
        let future_height = 1704067200000 + 1_000_000; // 1000 seconds in future
        let height = calculator.calculate_height(Some(future_height));
        assert!(height > future_height);
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
