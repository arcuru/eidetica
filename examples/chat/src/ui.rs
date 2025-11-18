use crate::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation},
};

pub fn ui(f: &mut ratatui::Frame, app: &App) {
    render_chat(f, app);
}

fn render_chat(f: &mut ratatui::Frame, app: &App) {
    let mut constraints = vec![
        Constraint::Length(3), // Room address bar
        Constraint::Min(0),    // Messages
        Constraint::Length(3), // Input
    ];

    // Add space for status message if present
    if app.status_message.is_some() {
        constraints.insert(1, Constraint::Length(2)); // Status message
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(f.area());

    // Room address bar (for sharing)
    let room_name = app
        .current_room
        .as_ref()
        .and_then(|r| r.get_name().ok())
        .unwrap_or_else(|| "Unknown Room".to_string());

    let address_text = if let Some(addr) = &app.current_room_address {
        format!("{room_name} | Share this room: {addr}")
    } else {
        room_name
    };

    let address_bar = Paragraph::new(address_text)
        .style(
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(ratatui::layout::Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Room Address")
                .style(Style::default().fg(Color::Yellow)),
        );
    f.render_widget(address_bar, chunks[0]);

    let mut message_chunk_index = 1;

    // Status message (if present)
    if let Some(status_msg) = &app.status_message {
        let status = Paragraph::new(status_msg.as_str())
            .style(Style::default().fg(Color::Cyan))
            .alignment(ratatui::layout::Alignment::Center)
            .block(Block::default().borders(Borders::NONE));
        f.render_widget(status, chunks[1]);
        message_chunk_index = 2;
    }

    // Messages area
    let messages: Vec<ListItem> = app
        .messages
        .iter()
        .map(|m| {
            let timestamp = m.timestamp.format("%H:%M:%S");
            let content = Line::from(vec![
                Span::styled(
                    format!("[{timestamp}] "),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{}: ", m.author),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&m.content),
            ]);
            ListItem::new(content)
        })
        .collect();

    let messages_list = List::new(messages).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(
                "Messages ({}) - ESC to leave room",
                app.messages.len()
            ))
            .style(Style::default().fg(Color::White)),
    );

    f.render_widget(messages_list, chunks[message_chunk_index]);

    // Render scrollbar
    let scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None);
    let scrollbar_area = chunks[message_chunk_index].inner(Margin {
        horizontal: 0,
        vertical: 1,
    });
    f.render_stateful_widget(scrollbar, scrollbar_area, &mut app.scroll_state.clone());

    // Input area
    let input_chunk_index = if app.status_message.is_some() { 3 } else { 2 };
    let input = Paragraph::new(app.input.as_str())
        .style(Style::default().fg(Color::Yellow))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Type your message (Enter to send)"),
        );
    f.render_widget(input, chunks[input_chunk_index]);

    // Set cursor position
    f.set_cursor_position((
        chunks[input_chunk_index].x + app.input.len() as u16 + 1,
        chunks[input_chunk_index].y + 1,
    ));
}
