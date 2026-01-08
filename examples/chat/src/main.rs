mod app;
mod handlers;
mod models;
mod ui;

use app::App;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use eidetica::Instance;
use eidetica::Result;
use eidetica::backend::database::InMemory;
use handlers::handle_key_event;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use ui::ui;

#[derive(Parser)]
#[command(name = "eidetica-chat")]
#[command(about = "A TUI chat application using Eidetica for distributed messaging")]
#[command(version)]
struct Args {
    /// Room address to connect to (format: room_id@server). If not provided, creates a new room.
    #[arg(value_name = "ROOM_ADDRESS")]
    room_address: Option<String>,

    /// Username for the chat session
    #[arg(short, long)]
    username: Option<String>,

    /// Enable verbose debug output
    #[arg(short, long)]
    verbose: bool,

    /// Transport to use for sync (http or iroh)
    #[arg(long, default_value = "iroh")]
    transport: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    // Initialize tracing
    if args.verbose {
        tracing_subscriber::fmt().with_env_filter("debug").init();
    } else {
        tracing_subscriber::fmt().with_env_filter("warn").init();
    }

    // Initialize Eidetica with sync enabled
    let backend = InMemory::new();
    let instance = Instance::create(Box::new(backend)).await?;
    instance.enable_sync().await?;

    // Get username from args, environment, or use default
    let username = args
        .username
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "Anonymous".to_string());

    // Create a passwordless user (ignore error if user already exists)
    let _ = instance.create_user(&username, None).await;

    // Login the user to get a User session
    let user = instance.login_user(&username, None).await?;

    // Validate and parse transport
    let transport = match args.transport.to_lowercase().as_str() {
        "http" => "http",
        "iroh" => "iroh",
        _ => {
            eprintln!(
                "❌ Invalid transport '{}'. Use 'http' or 'iroh'",
                args.transport
            );
            std::process::exit(1);
        }
    };

    // Create app with user session and transport choice
    let mut app = App::new(instance, user, username.clone(), transport)?;

    // Either connect to existing room or create new one
    if let Some(room_address) = args.room_address {
        // Connect to existing room
        println!("🔗 Connecting to room...");
        println!("📍 Room Address: {room_address}");
        println!("👤 Username: {username}");
        println!();

        app.connect_to_room(&room_address).await?;

        println!("✅ Connected! Starting chat interface...");
    } else {
        // Create new room
        let room_name = format!(
            "Chat Room - {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
        );

        app.create_room(&room_name).await?;

        // Display the room address
        if let Some(addr) = &app.current_room_address {
            println!("🚀 Eidetica Chat Room Created!");
            println!();
            println!("📍 Room Address: {addr}");
            println!("👤 Username: {username}");
            println!();
            println!("Share this address with others to invite them to the chat.");
            println!();
            println!("Press Enter to start chatting...");

            // Wait for user to press Enter
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
        }
    }

    // Setup terminal for TUI
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the app
    let res = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    // Create a periodic timer for refreshing messages
    let mut refresh_interval = tokio::time::interval(tokio::time::Duration::from_secs(2));

    loop {
        terminal.draw(|f| ui(f, app))?;

        // Handle all available events first, then check for refresh
        let mut handled_event = false;

        // Process all available events without blocking
        while event::poll(std::time::Duration::from_millis(0))? {
            if let Ok(Event::Key(key)) = event::read() {
                handled_event = true;
                if key.kind == KeyEventKind::Press {
                    handle_key_event(app, key.code, key.modifiers).await;
                }
            }
        }

        // If no events were handled, check for periodic refresh or wait briefly
        if !handled_event {
            tokio::select! {
                _ = refresh_interval.tick() => {
                    // Periodically refresh messages from database
                    if let Err(e) = app.refresh_messages().await {
                        eprintln!("Error refreshing messages: {e}");
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(50)) => {
                    // Small delay to prevent busy waiting
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
