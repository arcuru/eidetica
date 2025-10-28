use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// AppMode removed - app only has one mode: Chat

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

impl ChatMessage {
    pub fn new(author: String, content: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            author,
            content,
            timestamp: Utc::now(),
        }
    }
}
