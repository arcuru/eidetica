//! Internal user sync coordination for the sync module.
//!
//! This module handles user preference tracking, user-database relationships,
//! and combined sync settings computation. It operates on the sync tree but
//! doesn't own it.

use tracing::debug;

use super::error::SyncError;
use crate::{
    Error, Result, Transaction, crdt::doc::path, entry::ID, store::DocStore,
    user::types::SyncSettings,
};

/// User-aware sync subtree constants
pub(super) const DATABASE_USERS_SUBTREE: &str = "database_users"; // Maps database_id -> {users, combined_settings}
pub(super) const USER_TRACKING_SUBTREE: &str = "user_tracking"; // Maps user_uuid -> {preferences_db_id, preferences_tips}

/// Internal user sync manager for the sync module.
///
/// This struct manages all user sync coordination operations for the sync module,
/// operating on a Transaction to stage changes.
pub(super) struct UserSyncManager<'a> {
    txn: &'a Transaction,
}

impl<'a> UserSyncManager<'a> {
    /// Create a new UserSyncManager that operates on the given Transaction.
    pub(super) fn new(txn: &'a Transaction) -> Self {
        Self { txn }
    }

    /// Track a user's preferences database for sync monitoring.
    ///
    /// This establishes the connection between the user and their preferences database
    /// for ongoing sync tracking. This operation is idempotent.
    ///
    /// # Arguments
    /// * `user_uuid` - The user's unique identifier
    /// * `preferences_db_id` - The ID of the user's private database
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) async fn track_user_preferences(
        &self,
        user_uuid: impl AsRef<str>,
        preferences_db_id: &ID,
    ) -> Result<()> {
        let user_tracking = self
            .txn
            .get_store::<DocStore>(USER_TRACKING_SUBTREE)
            .await?;

        // Check if the user is already registered
        if user_tracking
            .get_path(path!(user_uuid.as_ref(), "preferences_db_id"))
            .await
            .is_ok()
        {
            return Ok(());
        }

        // Store the preferences database ID
        user_tracking
            .set_path(
                path!(user_uuid.as_ref(), "preferences_db_id"),
                preferences_db_id.to_string(),
            )
            .await?;

        // Initialize with empty tips (will be populated on first update)
        user_tracking
            .set_path(
                path!(user_uuid.as_ref(), "preferences_tips"),
                serde_json::to_string(&Vec::<String>::new()).unwrap(),
            )
            .await?;

        debug!(user_uuid = %user_uuid.as_ref(), "Tracking user preferences for sync");
        Ok(())
    }

    /// Get the current state of a tracked user's preferences database.
    ///
    /// Returns the preferences database ID and the tips that were last read,
    /// enabling change detection via tip comparison.
    ///
    /// # Arguments
    /// * `user_uuid` - The user's unique identifier
    ///
    /// # Returns
    /// A tuple of (preferences_db_id, preferences_tips) or None if user not tracked
    pub(super) async fn get_tracked_user_state(
        &self,
        user_uuid: impl AsRef<str>,
    ) -> Result<Option<(ID, Vec<ID>)>> {
        let user_tracking = self
            .txn
            .get_store::<DocStore>(USER_TRACKING_SUBTREE)
            .await?;

        // Check if user exists
        if !user_tracking.contains_path_str(user_uuid.as_ref()).await {
            return Ok(None);
        }

        // Get preferences DB ID
        let prefs_db_id_str = user_tracking
            .get_path_as::<String>(path!(user_uuid.as_ref(), "preferences_db_id"))
            .await
            .map_err(|_| {
                Error::Sync(SyncError::SerializationError(
                    "Missing preferences_db_id field".to_string(),
                ))
            })?;
        let prefs_db_id = ID::from(prefs_db_id_str.as_str());

        // Get preferences tips
        let tips_json = user_tracking
            .get_path_as::<String>(path!(user_uuid.as_ref(), "preferences_tips"))
            .await
            .unwrap_or_else(|_| "[]".to_string());
        let tips_strings: Vec<String> = serde_json::from_str(&tips_json).unwrap_or_default();
        let tips: Vec<ID> = tips_strings
            .into_iter()
            .map(|s| ID::from(s.as_str()))
            .collect();

        Ok(Some((prefs_db_id, tips)))
    }

    /// Update the tracked tips for a user's preferences database.
    ///
    /// This should be called after successfully processing a user's preferences
    /// to record which version of the preferences database has been integrated
    /// into the sync tree.
    ///
    /// # Arguments
    /// * `user_uuid` - The user's unique identifier
    /// * `new_tips` - The current tips of the user's preferences database
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) async fn update_tracked_tips(
        &self,
        user_uuid: impl AsRef<str>,
        new_tips: &[ID],
    ) -> Result<()> {
        let user_tracking = self
            .txn
            .get_store::<DocStore>(USER_TRACKING_SUBTREE)
            .await?;

        // Convert tips to strings for JSON serialization
        let tips_strings: Vec<String> = new_tips.iter().map(|id| id.to_string()).collect();
        let tips_json = serde_json::to_string(&tips_strings).unwrap();

        user_tracking
            .set_path(path!(user_uuid.as_ref(), "preferences_tips"), tips_json)
            .await?;

        debug!(user_uuid = %user_uuid.as_ref(), tip_count = new_tips.len(), "Updated user preferences tips");
        Ok(())
    }

    /// Link a user to a database for sync tracking.
    ///
    /// This records that a specific user wants to sync a specific database.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    /// * `user_uuid` - The user's unique identifier
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) async fn link_user_to_database(
        &self,
        database_id: &ID,
        user_uuid: impl AsRef<str>,
    ) -> Result<()> {
        let database_users = self
            .txn
            .get_store::<DocStore>(DATABASE_USERS_SUBTREE)
            .await?;
        let db_id_str = database_id.to_string();

        // Get existing users list for this database
        let users_path = path!(&db_id_str, "users");
        let users_result = database_users.get_path_as::<String>(&users_path).await;
        let mut users: Vec<serde_json::Value> = users_result
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(Vec::new);

        // Check if user already exists
        let user_exists = users.iter().any(|u| {
            u.get("user_uuid")
                .and_then(|v| v.as_str())
                .map(|uuid| uuid == user_uuid.as_ref())
                .unwrap_or(false)
        });

        if !user_exists {
            // Add new user
            users.push(serde_json::json!({
                "user_uuid": user_uuid.as_ref()
            }));

            // Store updated users list
            let users_json = serde_json::to_string(&users).unwrap();
            database_users.set_path(&users_path, users_json).await?;

            debug!(database_id = %database_id, user_uuid = %user_uuid.as_ref(), "Linked user to database for sync tracking");
        }

        Ok(())
    }

    /// Unlink a user from a database's sync tracking.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    /// * `user_uuid` - The user's unique identifier
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) async fn unlink_user_from_database(
        &self,
        database_id: &ID,
        user_uuid: impl AsRef<str>,
    ) -> Result<()> {
        let database_users = self
            .txn
            .get_store::<DocStore>(DATABASE_USERS_SUBTREE)
            .await?;
        let db_id_str = database_id.to_string();

        // Get existing users list
        let users_path = path!(&db_id_str, "users");
        if let Ok(users_json) = database_users.get_path_as::<String>(&users_path).await
            && let Ok(mut users) = serde_json::from_str::<Vec<serde_json::Value>>(&users_json)
        {
            let initial_len = users.len();

            // Remove the user
            users.retain(|u| {
                u.get("user_uuid")
                    .and_then(|v| v.as_str())
                    .map(|uuid| uuid != user_uuid.as_ref())
                    .unwrap_or(true)
            });

            if users.len() != initial_len {
                if users.is_empty() {
                    // Remove entire database record if no users left
                    database_users.delete(&db_id_str).await?;
                } else {
                    // Update users list
                    let updated_json = serde_json::to_string(&users).unwrap();
                    database_users.set_path(&users_path, updated_json).await?;
                }

                debug!(database_id = %database_id, user_uuid = %user_uuid.as_ref(), "Unlinked user from database sync tracking");
            }
        }

        Ok(())
    }

    /// Get all users linked to a specific database.
    ///
    /// Returns a list of user UUIDs for each user who has this database
    /// in their sync preferences.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    ///
    /// # Returns
    /// A vector of user UUIDs
    pub(super) async fn get_linked_users(&self, database_id: &ID) -> Result<Vec<String>> {
        let database_users = self
            .txn
            .get_store::<DocStore>(DATABASE_USERS_SUBTREE)
            .await?;
        let db_id_str = database_id.to_string();

        let users_path = path!(&db_id_str, "users");
        let users_result = database_users.get_path_as::<String>(&users_path).await;
        let users: Vec<serde_json::Value> = users_result
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(Vec::new);

        let mut result = Vec::new();
        for user in users {
            if let Some(user_uuid) = user.get("user_uuid").and_then(|v| v.as_str()) {
                result.push(user_uuid.to_string());
            }
        }

        Ok(result)
    }

    /// Get all databases linked to a user.
    ///
    /// # Arguments
    /// * `user_uuid` - The user's unique identifier
    ///
    /// # Returns
    /// A vector of database IDs
    pub(super) async fn get_linked_databases(&self, user_uuid: impl AsRef<str>) -> Result<Vec<ID>> {
        let database_users = self
            .txn
            .get_store::<DocStore>(DATABASE_USERS_SUBTREE)
            .await?;
        let all_databases = database_users.get_all().await?;
        let mut result = Vec::new();

        for db_id_str in all_databases.keys() {
            let users_path = path!(db_id_str, "users");
            if let Ok(users_json) = database_users.get_path_as::<String>(&users_path).await
                && let Ok(users) = serde_json::from_str::<Vec<serde_json::Value>>(&users_json)
            {
                // Check if this user is in the list
                let has_user = users.iter().any(|u| {
                    u.get("user_uuid")
                        .and_then(|v| v.as_str())
                        .map(|uuid| uuid == user_uuid.as_ref())
                        .unwrap_or(false)
                });

                if has_user {
                    result.push(ID::from(db_id_str.as_str()));
                }
            }
        }

        Ok(result)
    }

    /// Set the combined sync settings for a database.
    ///
    /// This stores the merged settings computed from all users who are tracking
    /// this database. The background sync uses these combined settings to determine
    /// sync behavior.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    /// * `settings` - The combined sync settings
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) async fn set_combined_settings(
        &self,
        database_id: &ID,
        settings: &SyncSettings,
    ) -> Result<()> {
        let database_users = self
            .txn
            .get_store::<DocStore>(DATABASE_USERS_SUBTREE)
            .await?;
        let db_id_str = database_id.to_string();

        let settings_json = serde_json::to_string(settings)
            .map_err(|e| Error::Sync(SyncError::SerializationError(e.to_string())))?;

        database_users
            .set_path(path!(&db_id_str, "combined_settings"), settings_json)
            .await?;

        debug!(database_id = %database_id, "Updated combined sync settings");
        Ok(())
    }

    /// Get the combined sync settings for a database.
    ///
    /// Returns the merged settings that should be used for syncing this database,
    /// or None if no settings are configured (no users tracking this database).
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    ///
    /// # Returns
    /// The combined sync settings, or None if not found
    pub(super) async fn get_combined_settings(
        &self,
        database_id: &ID,
    ) -> Result<Option<SyncSettings>> {
        let database_users = self
            .txn
            .get_store::<DocStore>(DATABASE_USERS_SUBTREE)
            .await?;
        let db_id_str = database_id.to_string();

        let settings_path = path!(&db_id_str, "combined_settings");
        match database_users.get_path_as::<String>(&settings_path).await {
            Ok(settings_json) => {
                let settings = serde_json::from_str(&settings_json).map_err(|e| {
                    Error::Sync(SyncError::SerializationError(format!(
                        "Failed to parse combined settings: {e}"
                    )))
                })?;
                Ok(Some(settings))
            }
            Err(_) => Ok(None),
        }
    }
}
