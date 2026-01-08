//! Time provider abstraction
//!
//! This module provides a [`Clock`] trait that abstracts over time sources,
//! allowing production code to use real system time while tests can use
//! controllable mock time.
//!
//! # Example
//!
//! ```
//! use eidetica::{Clock, SystemClock};
//!
//! let clock = SystemClock;
//! let millis = clock.now_millis();
//! let rfc3339 = clock.now_rfc3339();
//! ```

use std::fmt::Debug;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(any(test, feature = "testing"))]
use std::sync::Mutex;

/// A time provider for getting current timestamps.
///
/// This trait abstracts over time sources to enable:
/// - Controllable time in tests (fixed starting point, manual advance)
/// - Monotonic timestamps within a single clock instance
pub trait Clock: Send + Sync + Debug {
    /// Returns the current time as milliseconds since Unix epoch.
    fn now_millis(&self) -> u64;

    /// Returns the current time as an RFC3339-formatted string.
    fn now_rfc3339(&self) -> String;

    /// Get current time as seconds since Unix epoch.
    ///
    /// Convenience method that converts milliseconds to seconds.
    fn now_secs(&self) -> i64 {
        (self.now_millis() / 1000) as i64
    }
}

/// Production clock using real system time.
///
/// This is the default clock implementation used in production code.
/// It calls through to [`std::time::SystemTime`] and [`chrono::Utc`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn now_rfc3339(&self) -> String {
        chrono::Utc::now().to_rfc3339()
    }
}

/// Test clock with auto-advancing time.
///
/// This clock auto-advances on each `now_millis()` call, providing monotonically
/// increasing timestamps. Use `hold()` to temporarily freeze the clock for tests
/// needing stable timestamps.
///
/// Note: While timestamps are monotonic, concurrent threads may receive values
/// in non-deterministic order depending on scheduling.
///
/// # Example
///
/// ```
/// use eidetica::{Clock, FixedClock};
///
/// let clock = FixedClock::new(1000);
/// let t1 = clock.now_millis();  // Returns 1000, then advances
/// let t2 = clock.now_millis();  // Returns next value
/// assert!(t2 > t1);
///
/// // Use hold() for stable timestamps
/// {
///     let _hold = clock.hold();
///     let a = clock.now_millis();
///     let b = clock.now_millis();
///     assert_eq!(a, b);  // Frozen
/// }
/// ```
#[cfg(any(test, feature = "testing"))]
pub struct FixedClock {
    state: Mutex<FixedClockState>,
}

#[cfg(any(test, feature = "testing"))]
struct FixedClockState {
    millis: u64,
    held: bool,
}

/// RAII guard that freezes a [`FixedClock`] while held.
///
/// The clock resumes auto-advancing when this guard is dropped.
#[cfg(any(test, feature = "testing"))]
pub struct ClockHold<'a>(&'a FixedClock);

#[cfg(any(test, feature = "testing"))]
impl Drop for ClockHold<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().held = false;
    }
}

#[cfg(any(test, feature = "testing"))]
impl FixedClock {
    /// Create a new fixed clock with the given initial time in milliseconds.
    pub fn new(millis: u64) -> Self {
        Self {
            state: Mutex::new(FixedClockState {
                millis,
                held: false,
            }),
        }
    }

    /// Hold the clock, preventing auto-advance until the guard is dropped.
    ///
    /// Returns an RAII guard that releases the hold when dropped.
    /// Use a scoped block for clean auto-release:
    ///
    /// ```
    /// # use eidetica::{Clock, FixedClock};
    /// let clock = FixedClock::new(1000);
    /// let expected = {
    ///     let _hold = clock.hold();
    ///     clock.now_millis()  // Frozen value
    /// };  // hold released
    /// ```
    pub fn hold(&self) -> ClockHold<'_> {
        self.state.lock().unwrap().held = true;
        ClockHold(self)
    }

    /// Advance the clock by the given number of milliseconds.
    pub fn advance(&self, ms: u64) {
        self.state.lock().unwrap().millis += ms;
    }

    /// Set the clock to a specific time in milliseconds.
    pub fn set(&self, ms: u64) {
        self.state.lock().unwrap().millis = ms;
    }

    /// Get the current time without advancing (even if not held).
    pub fn get(&self) -> u64 {
        self.state.lock().unwrap().millis
    }
}

#[cfg(any(test, feature = "testing"))]
impl Clock for FixedClock {
    fn now_millis(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        if state.held {
            state.millis
        } else {
            let t = state.millis;
            state.millis += 1;
            t
        }
    }

    fn now_rfc3339(&self) -> String {
        use chrono::{TimeZone, Utc};
        // Use now_millis() to get consistent auto-advance behavior
        let millis = self.now_millis();
        let secs = (millis / 1000) as i64;
        let nanos = ((millis % 1000) * 1_000_000) as u32;
        Utc.timestamp_opt(secs, nanos)
            .single()
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_string())
    }
}

#[cfg(any(test, feature = "testing"))]
impl Default for FixedClock {
    fn default() -> Self {
        // Default to a reasonable timestamp (2024-01-01 00:00:00 UTC)
        Self::new(1704067200000)
    }
}

#[cfg(any(test, feature = "testing"))]
impl Clone for FixedClock {
    fn clone(&self) -> Self {
        // Clone creates independent clock at current value, not held
        Self::new(self.get())
    }
}

#[cfg(any(test, feature = "testing"))]
impl Debug for FixedClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap();
        f.debug_struct("FixedClock")
            .field("millis", &state.millis)
            .field("held", &state.held)
            .finish()
    }
}

#[cfg(test)]
mod fixed_clock_tests {
    use super::*;

    #[test]
    fn fixed_clock_auto_advances() {
        let clock = FixedClock::new(1000);
        let t1 = clock.now_millis();
        assert_eq!(t1, 1000); // Initial value correct
        let t2 = clock.now_millis();
        let t3 = clock.now_millis();
        assert!(t2 > t1); // Advances after each call
        assert!(t3 > t2);
    }

    #[test]
    fn fixed_clock_get_does_not_advance() {
        let clock = FixedClock::new(1000);
        let initial = clock.get();
        assert_eq!(clock.get(), initial); // get() doesn't change value
        assert_eq!(clock.get(), initial);
        let after_now = clock.now_millis(); // now_millis() advances
        assert!(clock.get() > initial); // Value changed after now_millis()
        assert_eq!(after_now, initial); // now_millis() returned the pre-advance value
    }

    #[test]
    fn fixed_clock_hold_freezes() {
        let clock = FixedClock::new(1000);
        let frozen_value = {
            let _hold = clock.hold();
            let v1 = clock.now_millis();
            let v2 = clock.now_millis();
            let v3 = clock.now_millis();
            assert_eq!(v1, v2); // Frozen - no advance
            assert_eq!(v2, v3);
            v1
        };
        // After hold drops, auto-advance resumes
        let t1 = clock.now_millis();
        let t2 = clock.now_millis();
        assert_eq!(t1, frozen_value); // First call returns frozen value
        assert!(t2 > t1); // Then advances again
    }

    #[test]
    fn fixed_clock_manual_advance() {
        let clock = FixedClock::new(1000);
        clock.advance(500);
        assert_eq!(clock.get(), 1500);
    }

    #[test]
    fn fixed_clock_set() {
        let clock = FixedClock::new(1000);
        clock.set(5000);
        assert_eq!(clock.get(), 5000);
    }

    #[test]
    fn fixed_clock_rfc3339() {
        // 2024-01-01 00:00:00 UTC = 1704067200000 ms
        let clock = FixedClock::new(1704067200000);
        let _hold = clock.hold();
        let rfc3339 = clock.now_rfc3339();
        assert!(rfc3339.starts_with("2024-01-01T00:00:00"));
    }
}
