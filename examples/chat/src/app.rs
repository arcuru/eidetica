use crate::models::ChatMessage;
use eidetica::{
    Database, Instance, Result,
    auth::{AuthKey, Permission, types::Permission as PermissionType},
    crdt::Doc,
    store::Table,
    sync::{
        DatabaseTicket, SyncError,
        transports::{http::HttpTransport, iroh::IrohTransport},
    },
    user::{User, types::SyncSettings},
};
use ratatui::widgets::ScrollbarState;
use tracing::{debug, info};

pub struct App {
    pub user: User,
    pub instance: Instance,

    // Current room state
    pub current_room: Option<Database>,
    pub current_room_address: Option<String>,
    pub current_room_name: Option<String>,

    // Chat state
    pub input: String,
    pub messages: Vec<ChatMessage>,
    pub scroll_state: ScrollbarState,
    pub scroll_position: usize,

    // User info
    pub username: String,

    // Sync state
    pub server_running: bool,
    pub transport: String, // "http" or "iroh"

    pub status_message: Option<String>,

    pub should_quit: bool,
}

impl App {
    pub fn new(instance: Instance, user: User, username: String, transport: &str) -> Result<Self> {
        Ok(Self {
            user,
            instance,
            current_room: None,
            current_room_address: None,
            current_room_name: None,
            input: String::new(),
            messages: Vec::new(),
            scroll_state: ScrollbarState::default(),
            scroll_position: 0,
            username,
            server_running: false,
            transport: transport.to_string(),
            status_message: None,
            should_quit: false,
        })
    }

    pub async fn create_room(&mut self, name: &str) -> Result<()> {
        // Create new database with the given name
        let mut settings = Doc::new();
        settings.set("name", name);

        // Get the user's default key
        let key_id = self.user.get_default_key()?;

        // User API automatically configures auth with the creating key as admin
        let database = self.user.create_database(settings, &key_id).await?;

        // Add global "*" permission so anyone with the room ID can write
        // Global permission uses "*" as the pubkey, with no name
        let tx = database.new_transaction().await?;
        let settings_store = tx.get_settings()?;
        let global_key = AuthKey::active(None, Permission::Write(0));
        settings_store.set_auth_key("*", global_key).await?;
        tx.commit().await?;

        // Enable sync for this database with periodic sync every 2 seconds
        let database_id = database.root_id().clone();
        self.user
            .track_database(
                database_id,
                key_id.clone(),
                SyncSettings::on_commit().with_interval(2),
            )
            .await?;

        // Open the new room
        self.enter_room(database).await?;

        // Set a status message with the room address for sharing
        if let Some(addr) = &self.current_room_address {
            self.status_message = Some(format!("Room created! Share this address: {addr}"));
        }

        Ok(())
    }

    pub async fn enter_room(&mut self, database: Database) -> Result<()> {
        // Start server if not running
        if !self.server_running {
            self.start_server().await?;
        }

        // Database from User API already has auth key configured

        // Generate a shareable ticket URL for this room
        let room_address = if let Some(sync) = self.instance.sync() {
            match sync.create_ticket(database.root_id()).await {
                Ok(ticket) => ticket.to_string(),
                Err(e) => {
                    tracing::warn!("Failed to create ticket with addresses: {e}");
                    DatabaseTicket::new(database.root_id().clone()).to_string()
                }
            }
        } else {
            DatabaseTicket::new(database.root_id().clone()).to_string()
        };

        // Cache the room name for the UI (since get_name is async)
        let room_name = database.get_name().await.ok();

        self.current_room = Some(database);
        self.current_room_address = Some(room_address);
        self.current_room_name = room_name;
        self.load_messages().await?;

        Ok(())
    }

    async fn start_server(&mut self) -> Result<()> {
        if let Some(sync) = self.instance.sync() {
            match self.transport.as_str() {
                "http" => {
                    // Enable HTTP transport with simple client-server communication
                    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
                        .await?;
                    // Start server
                    sync.accept_connections().await?;
                }
                "iroh" => {
                    // Enable Iroh transport for P2P communication with NAT traversal
                    sync.register_transport("iroh", IrohTransport::builder())
                        .await?;
                    // Start server
                    sync.accept_connections().await?;
                }
                _ => {
                    return Err(SyncError::Network(format!(
                        "Unknown transport: {}",
                        self.transport
                    ))
                    .into());
                }
            }

            self.server_running = true;
        }
        Ok(())
    }

    pub async fn connect_to_room(&mut self, room_address: &str) -> Result<()> {
        // Ensure transport is enabled before syncing
        if !self.server_running {
            self.start_server().await?;
        }

        // Parse the ticket URL
        let ticket: DatabaseTicket = room_address
            .parse()
            .map_err(|e| SyncError::Network(format!("Invalid ticket URL: {e}")))?;
        let room_id = ticket.database_id().clone();
        debug!(room_id = %room_id, addresses = ?ticket.addresses(), "Parsed ticket");

        // Check if this is a bootstrap scenario (we don't have the room locally)
        let is_bootstrap = match self.user.backend().get(&room_id).await {
            Ok(_) => false,
            Err(e) if e.is_not_found() => true,
            Err(e) => return Err(e),
        };

        let sync = self
            .instance
            .sync()
            .ok_or_else(|| SyncError::Network("No sync instance available".into()))?;

        if is_bootstrap {
            let key_id = self.user.get_default_key()?;
            info!("Starting bootstrap sync for room {room_id}");

            self.user
                .request_database_access(&sync, &ticket, &key_id, PermissionType::Write(5))
                .await?;

            // Register the database with the User so it knows which key to use
            self.user
                .track_database(
                    room_id.clone(),
                    key_id,
                    SyncSettings::on_commit().with_interval(2),
                )
                .await?;
        } else {
            sync.sync_with_ticket(&ticket).await?;
        }

        // Wait for the database to become available (sync may still be flushing)
        let mut attempts = 0;
        let database = loop {
            attempts += 1;
            match self.user.open_database(&room_id).await {
                Ok(db) => break db,
                Err(e) if attempts >= 30 => {
                    return Err(SyncError::Network(format!(
                        "Timed out waiting for room to sync: {e}"
                    ))
                    .into());
                }
                Err(_) => {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        };

        self.current_room = Some(database);
        self.current_room_address = Some(room_address.to_string());
        self.load_messages().await?;

        Ok(())
    }

    pub async fn load_messages(&mut self) -> Result<()> {
        if let Some(database) = &self.current_room {
            let txn = database.new_transaction().await?;
            let store = txn.get_store::<Table<ChatMessage>>("messages").await?;

            let entries = store.search(|_| true).await?;
            let mut messages = Vec::new();

            for (_, msg) in entries {
                messages.push(msg);
            }

            // Sort by timestamp
            messages.sort_by_key(|a| a.timestamp);

            self.messages = messages;
            self.update_scroll();
        }

        Ok(())
    }

    pub async fn send_message(&mut self) -> Result<()> {
        if self.input.trim().is_empty() || self.current_room.is_none() {
            return Ok(());
        }

        let message = ChatMessage::new(self.username.clone(), self.input.trim().to_string());

        if let Some(database) = &self.current_room {
            // Database from User API has auth configured automatically
            let txn = database.new_transaction().await?;
            let store = txn.get_store::<Table<ChatMessage>>("messages").await?;
            store.insert(message.clone()).await?;
            txn.commit().await?;
            // Note: commit() triggers sync callbacks which queue entries in background

            self.messages.push(message);
            self.input.clear();
            self.update_scroll();
        }

        Ok(())
    }

    pub fn update_scroll(&mut self) {
        if !self.messages.is_empty() {
            self.scroll_position = self.messages.len().saturating_sub(1);
            self.scroll_state = self.scroll_state.position(self.scroll_position);
        }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_position = self.scroll_position.saturating_sub(1);
        self.scroll_state = self.scroll_state.position(self.scroll_position);
    }

    pub fn scroll_down(&mut self) {
        if self.scroll_position < self.messages.len().saturating_sub(1) {
            self.scroll_position = self.scroll_position.saturating_add(1);
            self.scroll_state = self.scroll_state.position(self.scroll_position);
        }
    }

    pub async fn refresh_messages(&mut self) -> Result<()> {
        // Reload messages from database (picks up any new synced messages)
        // The library handles all syncing automatically based on interval_seconds
        let current_count = self.messages.len();
        self.load_messages().await?;

        // If we have new messages, update scroll to show them
        if self.messages.len() > current_count {
            self.update_scroll();
        }

        Ok(())
    }

    pub fn clear_status_message(&mut self) {
        self.status_message = None;
    }
}
