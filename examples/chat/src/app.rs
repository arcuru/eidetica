use crate::models::ChatMessage;
use eidetica::crdt::Doc;
use eidetica::store::Table;
use eidetica::sync::transports::{http::HttpTransport, iroh::IrohTransport};
use eidetica::{Database, Instance, Result, user::User};
use ratatui::widgets::ScrollbarState;
use tracing::{debug, error, info, warn};

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
    pub server_address: Option<String>,
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
            server_address: None,
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
        let tx = database.new_transaction().await?;
        let settings_store = tx.get_settings()?;
        let global_key =
            eidetica::auth::AuthKey::active("*", eidetica::auth::Permission::Write(10))?;
        settings_store.set_auth_key("*", global_key).await?;
        tx.commit().await?;

        // Enable sync for this database with periodic sync every 2 seconds
        let database_id = database.root_id().clone();
        self.user
            .track_database(eidetica::user::types::TrackedDatabase {
                database_id,
                key_id: key_id.clone(),
                sync_settings: eidetica::user::types::SyncSettings {
                    sync_enabled: true,
                    sync_on_commit: true,
                    interval_seconds: Some(2), // Sync every 2 seconds
                    properties: std::collections::HashMap::new(),
                },
            })
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

        // Generate room address (room_id@http-addr)
        let room_id = database.root_id().to_string();
        let room_address = if let Some(addr) = &self.server_address {
            // For HTTP, addr is a simple HTTP URL
            format!("{room_id}@{addr}")
        } else {
            room_id.clone()
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
                    return Err(eidetica::Error::Sync(eidetica::sync::SyncError::Network(
                        format!("Unknown transport: {}", self.transport),
                    )));
                }
            }

            // Get the server address
            if let Ok(addr) = sync.get_server_address().await {
                self.server_running = true;
                self.server_address = Some(addr);
            }
        }
        Ok(())
    }

    pub async fn connect_to_room_debug(&mut self, room_address: &str) -> Result<()> {
        debug!(room_address = %room_address, "Starting connection to room");

        // Parse format: room_id@http://host:port
        let parts: Vec<&str> = room_address.split('@').collect();
        if parts.len() != 2 {
            error!("Invalid room address format. Expected format: room_id@http://host:port");
            return Ok(());
        }

        let room_id = parts[0];
        let server_addr = parts[1]; // This is the HTTP address
        debug!(room_id = %room_id, server_addr = %server_addr, "Parsed room address components");

        // Check if room already exists locally
        let room_id_obj = eidetica::entry::ID::from(room_id);
        if let Ok(_database) = self.user.open_database(&room_id_obj).await {
            debug!("Room already exists locally");
        } else {
            debug!("Room does not exist locally, will bootstrap from remote");
        }

        // Check if this is a bootstrap scenario (we don't have the room locally)
        let is_bootstrap = match self.user.backend().get(&room_id_obj).await {
            Ok(_) => false,                     // Room exists locally
            Err(e) if e.is_not_found() => true, // Room doesn't exist, need bootstrap
            Err(e) => return Err(e),            // Other error
        };

        debug!(
            " Using {}sync API to connect and sync room...",
            if is_bootstrap {
                "authenticated bootstrap "
            } else {
                "regular "
            }
        );

        let connection_success = if let Some(sync) = self.instance.sync() {
            let key_id = self.user.get_default_key()?;
            let sync_result = if is_bootstrap {
                // Bootstrap sync - authenticate and sync a room we don't have locally
                info!(" Starting authenticated bootstrap sync (we don't have this room yet)...");

                println!(
                    "DEBUG: Starting bootstrap sync with server_addr={server_addr}, room_id={room_id}"
                );

                // For bootstrap sync, use the User API which handles key management internally
                let result = self
                    .user
                    .request_database_access(
                        &sync,
                        server_addr,
                        &room_id_obj,
                        &key_id,
                        eidetica::auth::types::Permission::Write(5),
                    )
                    .await;

                println!("DEBUG: request_database_access result: {result:?}");

                match &result {
                    Ok(_) => {
                        println!("DEBUG: ✓ Bootstrap sync call returned Ok");
                        info!(" Bootstrap sync completed successfully");
                        // Check if the database root actually exists in backend
                        match self.instance.backend().get(&room_id_obj).await {
                            Ok(entry) => {
                                println!("DEBUG: ✓ Root entry FOUND in backend: {}", entry.id());
                                info!(" ✓ Root entry confirmed in backend");
                            }
                            Err(e) => {
                                println!("DEBUG: ✗ Root entry NOT in backend after sync: {e:?}");
                                error!(" ✗ Root entry NOT in backend after sync: {:?}", e);
                            }
                        }

                        // IMPORTANT: After bootstrap sync, we must register the database with the User
                        // so it knows which key to use when loading this database.
                        // request_database_access() only syncs the data - it doesn't update the User's
                        // key mappings. track_database() discovers available SigKeys and creates the mapping.
                        println!("DEBUG: Registering database with User's key manager...");
                        match self
                            .user
                            .track_database(eidetica::user::types::TrackedDatabase {
                                database_id: room_id_obj.clone(),
                                key_id: key_id.clone(),
                                sync_settings: eidetica::user::types::SyncSettings {
                                    sync_enabled: true,
                                    sync_on_commit: true,
                                    interval_seconds: Some(2), // Sync every 2 seconds
                                    properties: std::collections::HashMap::new(),
                                },
                            })
                            .await
                        {
                            Ok(_) => {
                                println!("DEBUG: ✓ Database registered with User");
                                info!(" ✓ Database registered with User's key manager");
                            }
                            Err(e) => {
                                println!("DEBUG: ✗ Failed to register database: {e:?}");
                                error!(" Failed to register database with User: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        println!("DEBUG: ✗ Bootstrap sync call failed: {e:?}");
                        error!(" Bootstrap sync failed: {:?}", e);
                    }
                }

                result
            } else {
                // Use regular sync for existing rooms
                sync.sync_with_peer(server_addr, Some(&room_id_obj)).await
            };

            match sync_result {
                Ok(()) => {
                    info!(" Successfully synced room using simplified API");
                    true
                }
                Err(e) => {
                    error!(" Failed to sync with peer: {:?}", e);
                    return Err(e);
                }
            }
        } else {
            error!(" No sync instance available");
            false
        };

        if connection_success {
            // Try to load the room (no retry loop - don't block UI)
            debug!(" Attempting to load synced room...");
            println!("DEBUG: Attempting to load database {room_id_obj}");

            match self.user.open_database(&room_id_obj).await {
                Ok(database) => {
                    println!("DEBUG: ✓ Successfully loaded database!");
                    info!(" Successfully loaded synced room!");

                    self.current_room = Some(database);
                    self.current_room_address = Some(room_address.to_string());
                    self.load_messages().await?;
                    return Ok(());
                }
                Err(e) => {
                    println!("DEBUG: ✗ Failed to load database: {e:?}");
                }
            }

            // If we get here, sync hasn't completed yet - create placeholder
            // The room will sync in the background and messages will appear when ready
            warn!(" Database not yet available, creating placeholder (syncing in background)");
            let mut settings = Doc::new();
            settings.set("name", format!("Room {room_id} (Syncing...)"));
            let key_id = self.user.get_default_key()?;
            let database = self.user.create_database(settings, &key_id).await?;
            info!(" Created placeholder room");

            self.current_room = Some(database);
            self.current_room_address = Some(room_address.to_string());
            self.load_messages().await?;
        }

        Ok(())
    }

    pub async fn connect_to_room(&mut self, room_address: &str) -> Result<()> {
        // Use the debug version for now
        self.connect_to_room_debug(room_address).await
    }

    pub async fn load_messages(&mut self) -> Result<()> {
        if let Some(database) = &self.current_room {
            let op = database.new_transaction().await?;
            let store = op.get_store::<Table<ChatMessage>>("messages").await?;

            let entries = store.search(|_| true).await?;
            let mut messages = Vec::new();

            for (_, msg) in entries {
                messages.push(msg);
            }

            // Sort by timestamp
            messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

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
            let op = database.new_transaction().await?;
            let store = op.get_store::<Table<ChatMessage>>("messages").await?;
            store.insert(message.clone()).await?;
            op.commit().await?;
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
