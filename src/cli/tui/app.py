"""Main TUI application for Eidetica."""

from textual.app import App
from textual.binding import Binding

from .screens.login import LoginScreen
from .screens.main import MainScreen


class EideticaApp(App[None]):
    """Eidetica TUI application."""

    CSS = """
    Screen {
        align: center middle;
        background: $surface;
    }
    """

    BINDINGS = [
        Binding("q", "quit", "Quit", show=True),
        Binding("r", "refresh", "Refresh", show=True),
    ]

    SCREENS = {
        "login": LoginScreen,
        "main": MainScreen,
    }

    def __init__(self):
        """Initialize the application."""
        super().__init__()
        self.session = None
        self.username = None

    def on_mount(self) -> None:
        """Handle application mount."""
        self.push_screen("login")

    def action_refresh(self) -> None:
        """Refresh the current screen."""
        if isinstance(self.screen, MainScreen):
            self.screen.refresh()
