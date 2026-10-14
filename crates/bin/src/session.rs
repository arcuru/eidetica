//! Session management for web interface
//!
//! Provides in-memory session storage mapping session tokens to authenticated AccountSession objects.

use std::{collections::HashMap, sync::Arc};

use eidetica::account::AccountSession;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Session token (UUID stored in cookie)
pub type SessionToken = String;

/// In-memory session store
///
/// Maps session tokens (UUIDs) to authenticated AccountSession objects with decrypted keys.
/// Sessions are ephemeral and lost on server restart.
///
/// Note: Users are wrapped in `Arc<RwLock>` since AccountSession doesn't implement Clone
/// (for security reasons - we don't want to duplicate keys in memory).
#[derive(Clone)]
pub struct SessionStore {
    sessions: Arc<RwLock<HashMap<SessionToken, Arc<RwLock<AccountSession>>>>>,
}

impl SessionStore {
    /// Create a new empty session store
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new session for a user
    ///
    /// Generates a random UUID token and stores the AccountSession object.
    ///
    /// # Arguments
    /// * `user` - The authenticated AccountSession object
    ///
    /// # Returns
    /// The session token (UUID) to be stored in a cookie
    pub async fn create_session(&self, user: AccountSession) -> SessionToken {
        let token = Uuid::new_v4().to_string();
        let mut sessions = self.sessions.write().await;
        sessions.insert(token.clone(), Arc::new(RwLock::new(user)));
        token
    }

    /// Get a user from a session token
    ///
    /// # Arguments
    /// * `token` - The session token from the cookie
    ///
    /// # Returns
    /// An `Arc<RwLock<AccountSession>>` if the session exists, None otherwise
    pub async fn get_user(&self, token: &str) -> Option<Arc<RwLock<AccountSession>>> {
        let sessions = self.sessions.read().await;
        sessions.get(token).cloned()
    }

    /// Destroy a session
    ///
    /// Removes the session from the store. The AccountSession object will be dropped
    /// and its keys will be zeroized.
    ///
    /// # Arguments
    /// * `token` - The session token to destroy
    pub async fn destroy_session(&self, token: &str) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(token);
    }

    /// Get the number of active sessions (for debugging)
    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.read().await;
        sessions.len()
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}
