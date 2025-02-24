"""Login screen for the Eidetica TUI."""

import logging
from textual.app import ComposeResult
from textual.screen import Screen
from textual.widgets import Input, Button, Label
from textual.containers import Container
from textual import on

from src.commands.users import login_user
from src.db.session import setup_database

# Configure logging
logging.basicConfig(level=logging.DEBUG)
logger = logging.getLogger(__name__)


class LoginScreen(Screen):
    """Login screen for user authentication."""

    BINDINGS = [
        ("q", "quit", "Quit"),
        ("enter", "submit", "Submit"),
    ]

    def compose(self) -> ComposeResult:
        """Create child widgets for the screen."""
        yield Container(
            Label("Eidetica", id="title"),
            Container(
                Label("Username:"),
                Input(id="username", placeholder="Enter username"),
                Label("Password:"),
                Input(id="password", password=True, placeholder="Enter password"),
                Button("Login", variant="primary", id="login"),
                Label("", id="error-message", classes="error"),
                id="login-form",
            ),
            id="login-container",
        )

    def on_mount(self) -> None:
        """Handle screen mount event."""
        self.query_one("#username").focus()

    @on(Button.Pressed, "#login")
    def handle_login(self) -> None:
        """Handle login button press."""
        self.try_login()

    def action_submit(self) -> None:
        """Handle enter key press."""
        self.try_login()

    def try_login(self) -> None:
        """Attempt to log in with current credentials."""
        username = self.query_one("#username").value
        password = self.query_one("#password").value
        error_label = self.query_one("#error-message")

        if not username or not password:
            error_msg = "Please enter both username and password"
            logger.debug(f"Login validation error: {error_msg}")
            error_label.update(error_msg)
            return

        try:
            logger.debug(f"Attempting login for user: {username}")
            session = setup_database()
            if login_user(session, username, password):
                logger.debug("Login successful")
                # Store session and username for main app
                self.app.session = session
                self.app.username = username
                # Switch to main screen
                self.app.push_screen("main")
            else:
                error_msg = "Invalid username or password"
                logger.debug(f"Login failed: {error_msg}")
                error_label.update(error_msg)
        except Exception as e:
            error_msg = f"Login error: {str(e)}"
            logger.error(f"Login exception: {error_msg}")
            error_label.update(error_msg)
            error_label.refresh()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Handle input submission."""
        if event.input.id == "username":
            # Move focus to password field
            self.query_one("#password").focus()
        elif event.input.id == "password":
            # Trigger login
            self.try_login()

    CSS = """
    Screen {
        background: $surface;
    }

    #login-container {
        align: center middle;
        width: 60;
        height: 100%;
        background: $panel;
    }

    #title {
        text-align: center;
        content-align: center middle;
        width: 100%;
        height: 3;
        text-style: bold;
        color: $accent;
        background: $panel;
        margin-bottom: 1;
    }

    #login-form {
        layout: vertical;
        padding: 1;
        width: 100%;
        height: auto;
        border: solid $primary;
        background: $panel;
    }

    Input {
        width: 100%;
        margin: 1 0;
        background: $boost;
    }

    Label {
        padding: 1 0;
        text-style: bold;
        color: $text;
    }

    #error-message {
        color: $error;
        text-style: italic;
        height: 1;
        margin: 1 0;
        width: 100%;
        text-align: center;
    }

    Button {
        margin: 1 0;
        width: 100%;
        background: $accent;
    }
    """
