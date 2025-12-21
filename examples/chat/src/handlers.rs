use crate::app::App;
use crossterm::event::{KeyCode, KeyModifiers};

pub async fn handle_key_event(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    match key {
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
            app.should_quit = true;
        }
        KeyCode::Enter => {
            // Clear status message when sending a message
            app.clear_status_message();
            if let Err(e) = app.send_message().await {
                eprintln!("Error sending message: {e}");
            }
        }
        KeyCode::Char(c) => {
            // Clear status message when typing
            if app.status_message.is_some() {
                app.clear_status_message();
            }
            app.input.push(c);
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Up => app.scroll_up(),
        KeyCode::Down => app.scroll_down(),
        _ => {}
    }
}
