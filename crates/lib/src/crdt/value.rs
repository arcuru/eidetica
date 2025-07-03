//! Value types for nested CRDT structures.
//!
//! This module provides `Value` (formerly `NestedValue`), which represents the possible
//! value types that can be stored in nested CRDT structures like `Nested`.

use serde::{Deserialize, Serialize};

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
    type Error = String;

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
                            return Err(format!("Failed to convert value for key '{key}': {e}"));
                        }
                    }
                }
                Ok(map)
            }
            _ => Err("Cannot convert non-map value to HashMap".to_string()),
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
    type Error = String;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(s) => {
                serde_json::from_str(&s).map_err(|e| format!("Failed to deserialize Vec: {e}"))
            }
            Value::Array(_) => {
                Err("Cannot convert CRDT Array to Vec<T>, use Array methods instead".to_string())
            }
            Value::Map(_) => Err("Cannot convert map to Vec<T>".to_string()),
            Value::Deleted => Err("Cannot convert deleted value to Vec<T>".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
