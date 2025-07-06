//! Value types for nested CRDT structures.
//!
//! This module provides `Value` (formerly `NestedValue`), which represents the possible
//! value types that can be stored in nested CRDT structures like `Nested`.

use crate::crdt::errors::CRDTError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Represents a value within a `Nested` structure, which can be either a String, another `Nested` map, an Array, or a tombstone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Value {
    String(String),
    Map(crate::crdt::nested::Nested),
    Array(crate::crdt::array::Array),
    Deleted, // Tombstone
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl Value {
    /// Type name for String variant
    pub const STRING_TYPE: &'static str = "String";
    /// Type name for Map variant  
    pub const MAP_TYPE: &'static str = "Map";
    /// Type name for Array variant
    pub const ARRAY_TYPE: &'static str = "Array";
    /// Type name for Deleted variant
    pub const DELETED_TYPE: &'static str = "Deleted";

    /// Returns a human-readable name for this value type
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => Self::STRING_TYPE,
            Value::Map(_) => Self::MAP_TYPE,
            Value::Array(_) => Self::ARRAY_TYPE,
            Value::Deleted => Self::DELETED_TYPE,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.type_name())
    }
}

impl From<crate::crdt::nested::Nested> for Value {
    fn from(map: crate::crdt::nested::Nested) -> Self {
        Value::Map(map)
    }
}

impl From<crate::crdt::array::Array> for Value {
    fn from(array: crate::crdt::array::Array) -> Self {
        Value::Array(array)
    }
}

// Type alias for backward compatibility
pub type NestedValue = Value;

// Generic conversions for HashMap<String, T>
use std::collections::HashMap;

impl<T> From<HashMap<String, T>> for Value
where
    T: serde::Serialize,
{
    fn from(map: HashMap<String, T>) -> Self {
        let mut nested = crate::crdt::nested::Nested::new();
        for (key, value) in map {
            nested.set_json(&key, value).unwrap();
        }
        Value::Map(nested)
    }
}

impl<T> TryFrom<Value> for HashMap<String, T>
where
    T: for<'de> serde::Deserialize<'de>,
{
    type Error = CRDTError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Map(nested) => {
                let mut map = HashMap::new();
                for key in nested.as_hashmap().keys() {
                    match nested.get_json::<T>(key) {
                        Ok(converted) => {
                            map.insert(key.clone(), converted);
                        }
                        Err(e) => {
                            return Err(CRDTError::DeserializationFailed {
                                reason: format!("Failed to convert value for key '{key}': {e}"),
                            });
                        }
                    }
                }
                Ok(map)
            }
            value => Err(CRDTError::TypeMismatch {
                expected: "Map containing HashMap data".to_string(),
                actual: value.type_name().to_string(),
            }),
        }
    }
}

// Generic JSON conversions for any serializable type
// Note: These are lower priority than specific conversions like String -> Value

// Generic conversion for Vec<T> where T is serializable
impl<T> From<Vec<T>> for Value
where
    T: Serialize,
{
    fn from(value: Vec<T>) -> Self {
        match serde_json::to_string(&value) {
            Ok(json) => Value::String(json),
            Err(_) => Value::String("[]".to_string()), // Fallback to empty array JSON
        }
    }
}

impl<T> TryFrom<Value> for Vec<T>
where
    T: for<'de> Deserialize<'de>,
{
    type Error = CRDTError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(s) => {
                serde_json::from_str(&s).map_err(|e| CRDTError::DeserializationFailed {
                    reason: format!("Failed to deserialize Vec: {e}"),
                })
            }
            Value::Array(_) => Err(CRDTError::TypeMismatch {
                expected: "String containing JSON array data for Vec<T> conversion".to_string(),
                actual: "CRDT Array (use Array-specific methods instead)".to_string(),
            }),
            Value::Map(_) => Err(CRDTError::TypeMismatch {
                expected: "String containing JSON array data for Vec<T> conversion".to_string(),
                actual: "Nested Map (use Map-specific methods instead)".to_string(),
            }),
            Value::Deleted => Err(CRDTError::InvalidValue {
                reason: "Cannot convert deleted value to Vec<T>".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_type_names() {
        let string_val = Value::from("hello");
        let map_val = Value::Map(crate::crdt::nested::Nested::new());
        let array_val = Value::Array(crate::crdt::array::Array::new());
        let deleted_val = Value::Deleted;

        // Test instance method
        assert_eq!(string_val.type_name(), "String");
        assert_eq!(map_val.type_name(), "Map");
        assert_eq!(array_val.type_name(), "Array");
        assert_eq!(deleted_val.type_name(), "Deleted");

        // Test constants
        assert_eq!(Value::STRING_TYPE, "String");
        assert_eq!(Value::MAP_TYPE, "Map");
        assert_eq!(Value::ARRAY_TYPE, "Array");
        assert_eq!(Value::DELETED_TYPE, "Deleted");

        // Test Display implementation
        assert_eq!(format!("{string_val}"), "String");
        assert_eq!(format!("{map_val}"), "Map");
        assert_eq!(format!("{array_val}"), "Array");
        assert_eq!(format!("{deleted_val}"), "Deleted");
    }

    use std::collections::HashMap;

    #[test]
    fn test_hashmap_to_value_conversion() {
        let mut map = HashMap::new();
        map.insert("key1".to_string(), "value1");
        map.insert("key2".to_string(), "value2");

        let value: Value = map.into();
        match value {
            Value::Map(nested) => {
                assert_eq!(
                    nested.get("key1"),
                    Some(&Value::String("\"value1\"".to_string()))
                );
                assert_eq!(
                    nested.get("key2"),
                    Some(&Value::String("\"value2\"".to_string()))
                );
            }
            _ => panic!("Expected Value::Map"),
        }
    }

    #[test]
    fn test_vec_string_conversions() {
        let strings = vec![
            "item1".to_string(),
            "item2".to_string(),
            "item3".to_string(),
        ];

        // Test Vec<String> -> Value
        let value: Value = strings.clone().into();

        // Test Value -> Vec<String>
        let converted: Vec<String> = value.try_into().unwrap();
        assert_eq!(converted, strings);
    }

    #[test]
    fn test_vec_serializable_conversions() {
        #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
        struct Item {
            name: String,
            count: i32,
        }

        let items = vec![
            Item {
                name: "first".to_string(),
                count: 1,
            },
            Item {
                name: "second".to_string(),
                count: 2,
            },
        ];

        // Test Vec<T> -> Value (generic)
        let value: Value = items.clone().into();

        // Test Value -> Vec<T> (generic)
        let converted: Vec<Item> = value.try_into().unwrap();
        assert_eq!(converted, items);
    }
}
