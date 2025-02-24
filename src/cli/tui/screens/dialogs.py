"""Dialog screens for the TUI."""

from textual.screen import ModalScreen
from textual.app import ComposeResult
from textual.widgets import Button, Static
from textual.containers import Vertical, Horizontal


class ConfirmDialog(ModalScreen[bool]):
    """A modal dialog that asks for confirmation."""

    def __init__(self, message: str) -> None:
        """Initialize the confirm dialog.

        Args:
            message: The confirmation message to display
        """
        super().__init__()
        self.message = message

    def compose(self) -> ComposeResult:
        """Create child widgets for the dialog."""
        yield Vertical(
            Static(self.message, id="message"),
            Horizontal(
                Button("Cancel", variant="default", id="cancel"),
                Button("OK", variant="primary", id="ok"),
                id="buttons",
            ),
            id="dialog",
        )

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button presses."""
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(f"[DEBUG] Dialog button pressed: {event.button.id}\n")

        result = event.button.id == "ok"
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(f"[DEBUG] Dialog result: {result}\n")

        self.dismiss(result)

    CSS = """
    ConfirmDialog {
        align: center middle;
    }

    #dialog {
        background: $surface;
        padding: 1 2;
        border: thick $primary;
        min-width: 40;
    }

    #message {
        text-align: center;
        min-height: 2;
    }

    #buttons {
        margin-top: 1;
        width: 100%;
        align-horizontal: center;
    }

    Button {
        margin: 0 1;
        min-width: 10;
    }
    """


class HelpDialog(ModalScreen):
    """A modal dialog that displays help text."""

    def __init__(self, content: str) -> None:
        """Initialize the help dialog.

        Args:
            content: The help text to display
        """
        super().__init__()
        self.content = content

    def compose(self) -> ComposeResult:
        """Create child widgets for the dialog."""
        yield Vertical(
            Static(self.content, id="content"),
            Button("Close", variant="primary", id="close"),
            id="dialog",
        )

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button press."""
        self.dismiss()

    CSS = """
    HelpDialog {
        align: center middle;
    }

    #dialog {
        background: $surface;
        padding: 1 2;
        border: thick $primary;
        min-width: 50;
        max-width: 80;
        max-height: 80%;
    }

    #content {
        padding: 1;
        min-height: 10;
        overflow-y: auto;
    }

    Button {
        margin-top: 1;
        width: 100%;
        align-horizontal: center;
    }
    """
