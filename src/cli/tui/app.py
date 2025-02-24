"""Main TUI application for Eidetica."""

from textual.app import App
from textual.binding import Binding

from .screens.login import LoginScreen
from .screens.main import MainScreen
from .screens.dialogs import ConfirmDialog, HelpDialog
from .screens.forms import FolderForm, DatabaseForm
from .screens.search import SearchScreen


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
        "confirm_dialog": ConfirmDialog,
        "help_dialog": HelpDialog,
        "create_folder": FolderForm,
        "edit_folder": FolderForm,
        "create_database": DatabaseForm,
        "edit_database": DatabaseForm,
        "search": SearchScreen,
    }

    def __init__(self):
        """Initialize the application."""
        super().__init__()
        self.session = None
        self.username = None

    def on_mount(self) -> None:
        """Handle application mount."""
        self.push_screen("login")

    def push_screen_with_args(self, screen_name: str, **kwargs) -> None:
        """Push a screen with arguments.

        Args:
            screen_name: Name of the screen to push
            **kwargs: Arguments to pass to the screen
        """
        screen_class = self.SCREENS[screen_name]
        screen = screen_class(**kwargs)
        self.push_screen(screen)

    async def push_screen_wait(self, screen_name: str, **kwargs) -> bool:
        """Push a screen and wait for its result.

        Args:
            screen_name: Name of the screen to push
            **kwargs: Arguments to pass to the screen

        Returns:
            The result from the screen
        """
        screen_class = self.SCREENS[screen_name]
        screen = screen_class(**kwargs)
        return await self.push_screen(screen)

    def action_refresh(self) -> None:
        """Refresh the current screen."""
        if isinstance(self.screen, MainScreen):
            self.screen.action_refresh()
