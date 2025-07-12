//! List positioning system for CRDT lists.
//!
//! This module provides the position-based ordering system used by CRDT Lists
//! to maintain stable ordering across concurrent insertions.

use std::cmp::Ordering;
use uuid::Uuid;

/// Represents a position in a CRDT list using rational numbers.
///
/// This type provides a stable ordering mechanism for list elements that allows
/// insertion between any two existing elements without requiring renumbering.
/// Each position consists of:
/// - A rational number (numerator/denominator) for ordering
/// - A unique UUID for deterministic tie-breaking
///
/// # Examples
///
/// ```
/// use eidetica::crdt::map::list::Position;
///
/// let pos1 = Position::new(10, 1);
/// let pos2 = Position::new(20, 1);
/// let between = Position::between(&pos1, &pos2);
///
/// assert!(pos1 < between);
/// assert!(between < pos2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Position {
    /// Numerator of the rational number
    pub numerator: i64,
    /// Denominator of the rational number (always positive)
    pub denominator: u64,
    /// Unique identifier for deterministic ordering
    pub unique_id: Uuid,
}

impl Position {
    /// Creates a new position with the specified rational number.
    ///
    /// # Arguments
    /// * `numerator` - The numerator of the rational number
    /// * `denominator` - The denominator of the rational number (must be > 0)
    ///
    /// # Examples
    ///
    /// ```
    /// use eidetica::crdt::map::list::Position;
    ///
    /// let pos = Position::new(3, 2); // Represents 3/2 = 1.5
    /// ```
    pub fn new(numerator: i64, denominator: u64) -> Self {
        assert!(denominator > 0, "Denominator must be positive");
        let mut pos = Self {
            numerator,
            denominator,
            unique_id: Uuid::new_v4(),
        };
        pos.reduce();
        pos
    }

    /// Creates a position at the beginning of the sequence.
    ///
    /// # Examples
    ///
    /// ```
    /// use eidetica::crdt::map::list::Position;
    ///
    /// let beginning = Position::beginning();
    /// let after = Position::new(1, 1);
    /// assert!(beginning < after);
    /// ```
    pub fn beginning() -> Self {
        Self::new(0, 1)
    }

    /// Creates a position at the end of the sequence.
    ///
    /// # Examples
    ///
    /// ```
    /// use eidetica::crdt::map::list::Position;
    ///
    /// let end = Position::end();
    /// let before = Position::new(1000, 1);
    /// assert!(before < end);
    /// ```
    pub fn end() -> Self {
        Self::new(i64::MAX, 1)
    }

    /// Creates a position between two existing positions.
    ///
    /// This method finds the rational number that falls between the two given positions
    /// and creates a new position with that value.
    ///
    /// # Arguments
    /// * `left` - The left (smaller) position
    /// * `right` - The right (larger) position
    ///
    /// # Examples
    ///
    /// ```
    /// use eidetica::crdt::map::list::Position;
    ///
    /// let pos1 = Position::new(1, 1);
    /// let pos2 = Position::new(3, 1);
    /// let between = Position::between(&pos1, &pos2);
    ///
    /// assert!(pos1 < between);
    /// assert!(between < pos2);
    /// ```
    pub fn between(left: &Position, right: &Position) -> Self {
        // Convert to common denominator for easier calculation
        let left_num = left.numerator as i128 * right.denominator as i128;
        let right_num = right.numerator as i128 * left.denominator as i128;
        let common_denom = left.denominator as i128 * right.denominator as i128;

        // Find the midpoint
        let mid_num = (left_num + right_num) / 2;

        // If the midpoint is the same as one of the endpoints, we need to increase precision
        if mid_num == left_num || mid_num == right_num {
            // Double the denominator to increase precision
            let new_denom = common_denom * 2;
            let new_mid_num = (left_num * 2 + right_num * 2) / 2;

            Self::new(new_mid_num as i64, new_denom as u64)
        } else {
            Self::new(mid_num as i64, common_denom as u64)
        }
    }

    /// Reduces the fraction to its simplest form.
    fn reduce(&mut self) {
        let gcd = gcd(self.numerator.unsigned_abs(), self.denominator);
        self.numerator /= gcd as i64;
        self.denominator /= gcd;
    }

    /// Returns the rational value as a floating point number.
    ///
    /// Note: This is primarily for debugging and should not be used for ordering.
    pub fn as_f64(&self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
}

impl PartialOrd for Position {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Position {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare rational numbers: a/b vs c/d -> a*d vs c*b
        let left = self.numerator as i128 * other.denominator as i128;
        let right = other.numerator as i128 * self.denominator as i128;

        match left.cmp(&right) {
            Ordering::Equal => {
                // If rational numbers are equal, use UUID for deterministic ordering
                self.unique_id.cmp(&other.unique_id)
            }
            ordering => ordering,
        }
    }
}

/// Calculates the greatest common divisor of two numbers.
fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_creation() {
        let pos = Position::new(3, 2);
        assert_eq!(pos.numerator, 3);
        assert_eq!(pos.denominator, 2);
    }

    #[test]
    fn test_position_reduction() {
        let pos = Position::new(6, 4);
        // Should be reduced to 3/2
        assert_eq!(pos.numerator, 3);
        assert_eq!(pos.denominator, 2);
    }

    #[test]
    fn test_position_ordering() {
        let pos1 = Position::new(1, 2); // 0.5
        let pos2 = Position::new(3, 4); // 0.75
        let pos3 = Position::new(1, 1); // 1.0

        assert!(pos1 < pos2);
        assert!(pos2 < pos3);
        assert!(pos1 < pos3);
    }

    #[test]
    fn test_position_between() {
        let pos1 = Position::new(1, 1);
        let pos2 = Position::new(3, 1);
        let between = Position::between(&pos1, &pos2);

        assert!(pos1 < between);
        assert!(between < pos2);
    }

    #[test]
    fn test_position_beginning_end() {
        let beginning = Position::beginning();
        let end = Position::end();
        let middle = Position::new(100, 1);

        assert!(beginning < middle);
        assert!(middle < end);
    }

    #[test]
    fn test_position_uuid_ordering() {
        let pos1 = Position::new(1, 1);
        let pos2 = Position::new(1, 1);

        // Same rational number, but different UUIDs should provide deterministic ordering
        assert_ne!(pos1.cmp(&pos2), Ordering::Equal);
    }
}
