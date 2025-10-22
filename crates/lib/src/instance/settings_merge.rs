//! Settings merge logic for combining user preferences.
//!
//! This module implements the "most aggressive" merge strategy for combining
//! sync settings from multiple users who are tracking the same database.

use crate::user::types::SyncSettings;

/// Merge multiple user sync settings into a single combined setting.
///
/// Uses the "most aggressive" strategy:
/// - `sync_enabled`: OR (true if any user wants sync)
/// - `sync_on_commit`: OR (true if any user wants it)
/// - `interval_seconds`: MIN (most frequent sync wins)
/// - `properties`: UNION (combine all properties, later values override)
///
/// # Arguments
/// * `settings` - Vector of sync settings from different users
///
/// # Returns
/// Combined sync settings, or a default if the input is empty
///
/// # Examples
/// ```
/// use eidetica::instance::settings_merge::merge_sync_settings;
/// use eidetica::user::types::SyncSettings;
///
/// let settings1 = SyncSettings {
///     sync_enabled: true,
///     sync_on_commit: false,
///     interval_seconds: Some(300),
///     properties: Default::default(),
/// };
///
/// let settings2 = SyncSettings {
///     sync_enabled: false,
///     sync_on_commit: true,
///     interval_seconds: Some(60),
///     properties: Default::default(),
/// };
///
/// let combined = merge_sync_settings(vec![settings1, settings2]);
///
/// // Most aggressive wins
/// assert_eq!(combined.sync_enabled, true);  // OR
/// assert_eq!(combined.sync_on_commit, true);  // OR
/// assert_eq!(combined.interval_seconds, Some(60));  // MIN
/// ```
pub fn merge_sync_settings(settings: Vec<SyncSettings>) -> SyncSettings {
    if settings.is_empty() {
        return SyncSettings::default();
    }

    // Start with the first setting as base
    let mut combined = settings[0].clone();

    // Merge with remaining settings
    for setting in settings.iter().skip(1) {
        // OR for boolean flags
        combined.sync_enabled = combined.sync_enabled || setting.sync_enabled;
        combined.sync_on_commit = combined.sync_on_commit || setting.sync_on_commit;

        // MIN for interval (most frequent sync)
        combined.interval_seconds = match (combined.interval_seconds, setting.interval_seconds) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        // UNION properties (later values override)
        for (key, value) in &setting.properties {
            combined.properties.insert(key.clone(), value.clone());
        }
    }

    combined
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_merge_empty_settings() {
        let result = merge_sync_settings(vec![]);
        assert!(!result.sync_enabled);
        assert!(!result.sync_on_commit);
        assert_eq!(result.interval_seconds, None);
        assert!(result.properties.is_empty());
    }

    #[test]
    fn test_merge_single_setting() {
        let settings = SyncSettings {
            sync_enabled: true,
            sync_on_commit: true,
            interval_seconds: Some(60),
            properties: {
                let mut map = HashMap::new();
                map.insert("key1".to_string(), "value1".to_string());
                map
            },
        };

        let result = merge_sync_settings(vec![settings.clone()]);
        assert_eq!(result.sync_enabled, settings.sync_enabled);
        assert_eq!(result.sync_on_commit, settings.sync_on_commit);
        assert_eq!(result.interval_seconds, settings.interval_seconds);
        assert_eq!(result.properties.len(), 1);
    }

    #[test]
    fn test_merge_sync_enabled_or() {
        let settings1 = SyncSettings {
            sync_enabled: true,
            ..Default::default()
        };

        let settings2 = SyncSettings {
            sync_enabled: false,
            ..Default::default()
        };

        let result = merge_sync_settings(vec![settings1, settings2.clone()]);
        assert!(result.sync_enabled); // true OR false = true

        let result2 = merge_sync_settings(vec![settings2.clone(), settings2]);
        assert!(!result2.sync_enabled); // false OR false = false
    }

    #[test]
    fn test_merge_sync_on_commit_or() {
        let settings1 = SyncSettings {
            sync_on_commit: true,
            ..Default::default()
        };

        let settings2 = SyncSettings {
            sync_on_commit: false,
            ..Default::default()
        };

        let result = merge_sync_settings(vec![settings1, settings2]);
        assert!(result.sync_on_commit); // true OR false = true
    }

    #[test]
    fn test_merge_interval_min() {
        let settings1 = SyncSettings {
            interval_seconds: Some(300),
            ..Default::default()
        };

        let settings2 = SyncSettings {
            interval_seconds: Some(60),
            ..Default::default()
        };

        let settings3 = SyncSettings {
            interval_seconds: Some(120),
            ..Default::default()
        };

        let result = merge_sync_settings(vec![settings1, settings2, settings3]);
        assert_eq!(result.interval_seconds, Some(60)); // MIN(300, 60, 120) = 60
    }

    #[test]
    fn test_merge_interval_with_none() {
        let settings1 = SyncSettings {
            interval_seconds: Some(300),
            ..Default::default()
        };

        let settings2 = SyncSettings {
            interval_seconds: None,
            ..Default::default()
        };

        let result = merge_sync_settings(vec![settings1.clone(), settings2.clone()]);
        assert_eq!(result.interval_seconds, Some(300)); // Some takes precedence

        let result2 = merge_sync_settings(vec![settings2.clone(), settings1]);
        assert_eq!(result2.interval_seconds, Some(300));

        let result3 = merge_sync_settings(vec![settings2.clone(), settings2]);
        assert_eq!(result3.interval_seconds, None); // None + None = None
    }

    #[test]
    fn test_merge_properties_union() {
        let settings1 = SyncSettings {
            properties: {
                let mut map = HashMap::new();
                map.insert("key1".to_string(), "value1".to_string());
                map.insert("key2".to_string(), "value2".to_string());
                map
            },
            ..Default::default()
        };

        let settings2 = SyncSettings {
            properties: {
                let mut map = HashMap::new();
                map.insert("key2".to_string(), "value2_override".to_string());
                map.insert("key3".to_string(), "value3".to_string());
                map
            },
            ..Default::default()
        };

        let result = merge_sync_settings(vec![settings1, settings2]);

        assert_eq!(result.properties.len(), 3);
        assert_eq!(result.properties.get("key1"), Some(&"value1".to_string()));
        assert_eq!(
            result.properties.get("key2"),
            Some(&"value2_override".to_string())
        ); // Override
        assert_eq!(result.properties.get("key3"), Some(&"value3".to_string()));
    }

    #[test]
    fn test_merge_all_features() {
        let settings1 = SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: Some(300),
            properties: {
                let mut map = HashMap::new();
                map.insert("priority".to_string(), "low".to_string());
                map
            },
        };

        let settings2 = SyncSettings {
            sync_enabled: false,
            sync_on_commit: true,
            interval_seconds: Some(60),
            properties: {
                let mut map = HashMap::new();
                map.insert("priority".to_string(), "high".to_string());
                map.insert("transport".to_string(), "http".to_string());
                map
            },
        };

        let settings3 = SyncSettings {
            sync_enabled: false,
            sync_on_commit: false,
            interval_seconds: Some(120),
            properties: {
                let mut map = HashMap::new();
                map.insert("region".to_string(), "us-west".to_string());
                map
            },
        };

        let result = merge_sync_settings(vec![settings1, settings2, settings3]);

        // Most aggressive settings win
        assert!(result.sync_enabled); // true OR false OR false = true
        assert!(result.sync_on_commit); // false OR true OR false = true
        assert_eq!(result.interval_seconds, Some(60)); // MIN(300, 60, 120) = 60

        // Properties are unioned
        assert_eq!(result.properties.len(), 3);
        assert_eq!(result.properties.get("priority"), Some(&"high".to_string())); // Last override
        assert_eq!(
            result.properties.get("transport"),
            Some(&"http".to_string())
        );
        assert_eq!(
            result.properties.get("region"),
            Some(&"us-west".to_string())
        );
    }
}
