"""Login screen for the Eidetica TUI."""

import logging
from textual.app import ComposeResult
from textual.screen import Screen
from textual.widgets import Input, Button, Label
from textual.containers import Container
from textual import on

from src.commands.users import login_user
from src.db.session import setup_database

# Configure logging - redirect to file instead of console to avoid TUI interference
logging.basicConfig(
    level=logging.DEBUG,
    filename="/tmp/eidetica_debug.log",  # Log to file instead of console
    filemode="a",
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
)
# Ensure debug messages are logged
logging.getLogger().setLevel(logging.DEBUG)
logger = logging.getLogger(__name__)
logger.setLevel(logging.DEBUG)


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
        # Hide error message initially
        self.query_one("#error-message").add_class("hidden")

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
            # Make sure error message is visible and properly updated
            error_label.update(error_msg)
            error_label.remove_class("hidden")
            error_label.styles.color = "red"  # Explicitly set color
            self.refresh()
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
                logger.debug(f"Updating error label with message: {error_msg}")
                # Make sure error message is visible and properly updated
                error_label.update(error_msg)
                logger.debug(f"Error label text after update: {error_label.renderable}")
                error_label.remove_class("hidden")
                logger.debug(
                    f"Error label classes after removal: {error_label.classes}"
                )
                error_label.styles.color = "red"  # Explicitly set color
                logger.debug(f"Error label styles after update: {error_label.styles}")
                self.refresh()
        except Exception as e:
            error_msg = f"Login error: {str(e)}"
            logger.error(f"Login exception: {error_msg}")
            # Make sure error message is visible and properly updated
            error_label.update(error_msg)
            error_label.remove_class("hidden")
            error_label.styles.color = "red"  # Explicitly set color
            self.refresh()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Handle input submission."""
        if event.input.id == "username":
            # Move focus to password field
            self.query_one("#password").focus()
        elif event.input.id == "password":
            # Trigger login
            self.try_login()

    @on(Input.Changed)
    def on_input_changed(self, event: Input.Changed) -> None:
        """Hide error message when user starts typing."""
        if event.input.id in ("username", "password"):
            # Hide error message when user starts typing
            error_label = self.query_one("#error-message")
            if not error_label.has_class("hidden"):
                error_label.add_class("hidden")

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
        text-style: bold italic;
        height: auto;
        min-height: 2;
        margin: 1 0;
        padding: 1;
        width: 100%;
        text-align: center;
        background: $error 10%;
        border: tall $error;
    }

    .hidden {
        display: none;
    }

    Button {
        margin: 1 0;
        width: 100%;
        background: $accent;
    }
    """
