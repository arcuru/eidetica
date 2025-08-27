//! List positioning system and List type for CRDT documents.
//!
//! This module provides both the Position type for stable list ordering
//! and the List type itself for ordered collections in CRDT documents.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::crdt::CRDTError;
use crate::crdt::traits::Data;

// Import Value from the value module
use super::value::Value;

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
/// use eidetica::crdt::doc::list::Position;
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
    /// use eidetica::crdt::doc::list::Position;
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
    /// use eidetica::crdt::doc::list::Position;
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
    /// use eidetica::crdt::doc::list::Position;
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
    /// use eidetica::crdt::doc::list::Position;
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

/// Ordered list with stable positioning for CRDT operations.
///
/// `List` maintains a stable ordering of elements using [`Position`] keys
/// in a `BTreeMap`. This design enables concurrent insertions without requiring
/// coordination between distributed replicas.
///
/// # Key Features
///
/// - **Stable ordering**: Elements maintain their relative positions even with concurrent modifications
/// - **Insertion anywhere**: Can insert between any two existing elements
/// - **CRDT semantics**: Proper merge behavior for distributed systems
/// - **Efficient access**: O(log n) access by position, O(1) by index for small lists
///
/// # Usage Patterns
///
/// ```
/// # use eidetica::crdt::doc::List;
/// # use eidetica::crdt::doc::list::Position;
/// let mut list = List::new();
///
/// // Simple append operations
/// list.push("first");  // Returns index 0
/// list.push("second"); // Returns index 1
///
/// // Insert between existing elements using index
/// list.insert(1, "between").unwrap();
///
/// // Access by traditional index
/// assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
///
/// // Advanced users can use Position for precise control
/// let pos1 = Position::new(1, 1);  // 1.0
/// let pos2 = Position::new(2, 1);  // 2.0
/// let middle = Position::between(&pos1, &pos2);
/// list.insert_at_position(middle, "advanced");
/// ```
///
/// # Concurrent Operations
///
/// When two replicas insert at the same logical position, the rational number
/// system ensures a consistent final order:
///
/// ```text
/// Replica A: ["item1", "item3"] -> inserts "item2" between them
/// Replica B: ["item1", "item3"] -> inserts "item4" between them
///
/// After merge: ["item1", "item2", "item4", "item3"]
/// (order determined by Position comparison)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    /// Internal storage using BTreeMap for ordered access
    items: BTreeMap<Position, Value>,
}

impl List {
    /// Creates a new empty list
    pub fn new() -> Self {
        Self {
            items: BTreeMap::new(),
        }
    }

    /// Returns the number of items in the list (excluding tombstones)
    pub fn len(&self) -> usize {
        self.items
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
            .count()
    }

    /// Returns the total number of items including tombstones
    pub fn total_len(&self) -> usize {
        self.items.len()
    }

    /// Returns true if the list is empty
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Pushes a value to the end of the list
    /// Returns the index of the newly added element
    pub fn push(&mut self, value: impl Into<Value>) -> usize {
        let value = value.into();
        let position = if let Some((last_pos, _)) = self.items.last_key_value() {
            // Create a position after the last element
            Position::new(last_pos.numerator.saturating_add(1), 1)
        } else {
            // First element
            Position::beginning()
        };

        self.items.insert(position, value);
        // Return the index (count of non-tombstone elements - 1)
        self.len() - 1
    }

    /// Inserts a value at a specific index
    pub fn insert(&mut self, index: usize, value: impl Into<Value>) -> Result<(), CRDTError> {
        let len = self.len();
        if index > len {
            return Err(CRDTError::ListIndexOutOfBounds { index, len });
        }

        let position = if index == 0 {
            // Insert at beginning
            if let Some((first_pos, _)) = self.items.first_key_value() {
                Position::new(first_pos.numerator - 1, first_pos.denominator)
            } else {
                Position::beginning()
            }
        } else if index == len {
            // Insert at end (same as push)
            if let Some((last_pos, _)) = self.items.last_key_value() {
                Position::new(last_pos.numerator + 1, last_pos.denominator)
            } else {
                Position::beginning()
            }
        } else {
            // Insert between two existing positions
            let positions: Vec<_> = self.items.keys().collect();
            let left_pos = positions[index - 1];
            let right_pos = positions[index];
            Position::between(left_pos, right_pos)
        };

        self.items.insert(position, value.into());
        Ok(())
    }

    /// Gets a value by index (0-based), filtering out tombstones
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.items
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
            .nth(index)
    }

    /// Gets a mutable reference to a value by index (0-based), filtering out tombstones
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Value> {
        // Find the position of the nth non-tombstone element
        let mut current_index = 0;
        let mut target_position = None;

        for (pos, value) in &self.items {
            if !matches!(value, Value::Deleted) {
                if current_index == index {
                    target_position = Some(pos.clone());
                    break;
                }
                current_index += 1;
            }
        }

        if let Some(pos) = target_position {
            self.items.get_mut(&pos)
        } else {
            None
        }
    }

    /// Inserts a value at a specific position (advanced API)
    pub fn insert_at_position(&mut self, position: Position, value: impl Into<Value>) {
        self.items.insert(position, value.into());
    }

    /// Gets a value by position
    pub fn get_by_position(&self, position: &Position) -> Option<&Value> {
        self.items.get(position)
    }

    /// Gets a mutable reference to a value by position
    pub fn get_by_position_mut(&mut self, position: &Position) -> Option<&mut Value> {
        self.items.get_mut(position)
    }

    /// Sets a value at a specific index, returns the old value if present
    /// Only considers non-tombstone elements for indexing
    pub fn set(&mut self, index: usize, value: impl Into<Value>) -> Option<Value> {
        let value = value.into();
        // Find the position of the nth non-tombstone element
        let mut current_index = 0;
        let mut target_position = None;

        for (pos, val) in &self.items {
            if !matches!(val, Value::Deleted) {
                if current_index == index {
                    target_position = Some(pos.clone());
                    break;
                }
                current_index += 1;
            }
        }

        if let Some(pos) = target_position {
            self.items.insert(pos, value)
        } else {
            None
        }
    }

    /// Removes a value by index (tombstones it for CRDT semantics)
    /// Only considers non-tombstone elements for indexing
    pub fn remove(&mut self, index: usize) -> Option<Value> {
        // Find the position of the nth non-tombstone element
        let mut current_index = 0;
        let mut target_position = None;

        for (pos, val) in &self.items {
            if !matches!(val, Value::Deleted) {
                if current_index == index {
                    target_position = Some(pos.clone());
                    break;
                }
                current_index += 1;
            }
        }

        if let Some(pos) = target_position {
            let old_value = self.items.get(&pos).cloned();
            self.items.insert(pos, Value::Deleted);
            old_value
        } else {
            None
        }
    }

    /// Removes a value by position
    pub fn remove_by_position(&mut self, position: &Position) -> Option<Value> {
        self.items.remove(position)
    }

    /// Returns an iterator over the values in order (excluding tombstones)
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.items.values().filter(|v| !matches!(v, Value::Deleted))
    }

    /// Returns an iterator over all values including tombstones
    pub fn iter_all(&self) -> impl Iterator<Item = &Value> {
        self.items.values()
    }

    /// Returns an iterator over position-value pairs in order
    pub fn iter_with_positions(&self) -> impl Iterator<Item = (&Position, &Value)> {
        self.items.iter()
    }

    /// Returns a mutable iterator over the values in order
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.items.values_mut()
    }

    /// Merges another List into this one (CRDT merge operation)
    pub fn merge(&mut self, other: &List) {
        for (position, value) in &other.items {
            match self.items.get_mut(position) {
                Some(existing_value) => {
                    // If both lists have the same position, merge the values
                    existing_value.merge(value);
                }
                None => {
                    // Position doesn't exist, add it
                    self.items.insert(position.clone(), value.clone());
                }
            }
        }
    }

    /// Clears all items from the list
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Converts to a Vec of values (loses position information)
    pub fn to_vec(&self) -> Vec<Value> {
        self.items.values().cloned().collect()
    }
}

impl Default for List {
    fn default() -> Self {
        Self::new()
    }
}

// Custom serde implementation to handle Position keys in JSON
impl serde::Serialize for List {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;

        // Serialize as an array of [position, value] pairs
        let mut seq = serializer.serialize_seq(Some(self.items.len()))?;
        for (position, value) in &self.items {
            seq.serialize_element(&(position, value))?;
        }
        seq.end()
    }
}

impl<'de> serde::Deserialize<'de> for List {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{SeqAccess, Visitor};
        use std::fmt;

        struct ListVisitor;

        impl<'de> Visitor<'de> for ListVisitor {
            type Value = List;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a sequence of [position, value] pairs")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut items = BTreeMap::new();

                while let Some((position, value)) = seq.next_element::<(Position, Value)>()? {
                    items.insert(position, value);
                }

                Ok(List { items })
            }
        }

        deserializer.deserialize_seq(ListVisitor)
    }
}

impl FromIterator<Value> for List {
    fn from_iter<T: IntoIterator<Item = Value>>(iter: T) -> Self {
        let mut list = List::new();
        for value in iter {
            list.push(value);
        }
        list
    }
}

// Data trait implementations
impl Data for List {}
