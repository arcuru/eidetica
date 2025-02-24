"""Form screens for creating, editing, and deleting items."""

from textual.screen import ModalScreen
from textual.app import ComposeResult
from textual.widgets import Button, Input, Static, Label
from textual.containers import Vertical, Horizontal
from textual.validation import Length

from src.commands import folders, databases
from src.db import User
from .main import MainScreen


class FolderForm(ModalScreen):
    """Base form for folder operations."""

    def __init__(self, folder_name: str = "") -> None:
        """Initialize the folder form.

        Args:
            folder_name: Optional existing folder name for editing
        """
        super().__init__()
        self.folder_name = folder_name
        self.is_edit = bool(folder_name)

    def compose(self) -> ComposeResult:
        """Create child widgets for the form."""
        yield Vertical(
            Label(f"{'Edit' if self.is_edit else 'Create'} Folder", id="title"),
            Vertical(
                Label("Name:"),
                Input(
                    value=self.folder_name,
                    id="name",
                    placeholder="Enter folder name",
                    validators=[Length(minimum=1)],
                ),
                Label("Description:"),
                Input(
                    id="description",
                    placeholder="Enter folder description",
                ),
                id="form",
            ),
            Horizontal(
                Button("Cancel", variant="default", id="cancel"),
                Button("Save", variant="primary", id="save"),
                id="buttons",
            ),
            id="dialog",
        )

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button presses."""
        if event.button.id == "cancel":
            self.dismiss()
            return

        if event.button.id == "save":
            name = self.query_one("#name", Input).value
            description = self.query_one("#description", Input).value

            try:
                if self.is_edit:
                    # Get user ID from session
                    user = (
                        self.app.session.query(User)
                        .filter_by(username=self.app.username)
                        .first()
                    )
                    if not user:
                        raise ValueError(f"User '{self.app.username}' not found")

                    folders.rename_folder(
                        user.id, self.folder_name, name, self.app.session
                    )
                    self.notify(f"Folder '{name}' updated")
                else:
                    # Get user ID from session
                    user = (
                        self.app.session.query(User)
                        .filter_by(username=self.app.username)
                        .first()
                    )
                    if not user:
                        raise ValueError(f"User '{self.app.username}' not found")

                    folders.create_folder(user.id, name, self.app.session, description)
                    self.notify(f"Folder '{name}' created")

                # Refresh tree view on main screen
                if isinstance(self.app.screen, MainScreen):
                    tree = self.app.screen.query_one("DataTree")
                    tree.load_user_data()
                self.dismiss()
            except Exception as e:
                self.notify(f"Error: {str(e)}", severity="error")

    CSS = """
    FolderForm {
        align: center middle;
    }

    #dialog {
        background: $surface;
        padding: 1 2;
        border: thick $primary;
        min-width: 50;
    }

    #title {
        text-align: center;
        text-style: bold;
        margin-bottom: 1;
    }

    #form {
        margin: 1 0;
    }

    Input {
        margin-bottom: 1;
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


class DatabaseForm(ModalScreen):
    """Base form for database operations."""

    def __init__(self, folder_name: str, database_name: str = "") -> None:
        """Initialize the database form.

        Args:
            folder_name: Parent folder name
            database_name: Optional existing database name for editing
        """
        super().__init__()
        self.folder_name = folder_name
        self.database_name = database_name
        self.is_edit = bool(database_name)

    def compose(self) -> ComposeResult:
        """Create child widgets for the form."""
        yield Vertical(
            Label(f"{'Edit' if self.is_edit else 'Create'} Database", id="title"),
            Vertical(
                Label("Name:"),
                Input(
                    value=self.database_name,
                    id="name",
                    placeholder="Enter database name",
                    validators=[Length(minimum=1)],
                ),
                id="form",
            ),
            Horizontal(
                Button("Cancel", variant="default", id="cancel"),
                Button("Save", variant="primary", id="save"),
                id="buttons",
            ),
            id="dialog",
        )

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button presses."""
        if event.button.id == "cancel":
            self.dismiss()
            return

        if event.button.id == "save":
            name = self.query_one("#name", Input).value

            try:
                if self.is_edit:
                    # Use info command to get current database info
                    # Get user ID from session
                    user = (
                        self.app.session.query(User)
                        .filter_by(username=self.app.username)
                        .first()
                    )
                    if not user:
                        raise ValueError(f"User '{self.app.username}' not found")

                    # Get database info
                    db_info = databases.get_database_info(
                        self.database_name,
                        user.id,
                        self.app.session,
                    )
                    if not db_info or not db_info["username"]:
                        raise ValueError(
                            f"Could not retrieve database info for '{db_info}'"
                        )

                    # Update database
                    databases.update_database(
                        self.database_name,
                        name,
                        db_info["username"],
                        self.folder_name,
                        self.app.session,
                    )
                    self.notify(f"Database '{name}' updated")
                else:
                    # Get user ID from session
                    user = (
                        self.app.session.query(User)
                        .filter_by(username=self.app.username)
                        .first()
                    )
                    if not user:
                        raise ValueError(f"User '{self.app.username}' not found")

                    # Create new database
                    databases.create_database(
                        self.folder_name,
                        name,
                        user.id,
                        self.app.session,
                    )
                    self.notify(f"Database '{name}' created")

                # Refresh tree view on main screen
                if isinstance(self.app.screen, MainScreen):
                    tree = self.app.screen.query_one("DataTree")
                    tree.load_user_data()
                self.dismiss()
            except Exception as e:
                self.notify(f"Error: {str(e)}", severity="error")

    CSS = """
    DatabaseForm {
        align: center middle;
    }

    #dialog {
        background: $surface;
        padding: 1 2;
        border: thick $primary;
        min-width: 50;
    }

    #title {
        text-align: center;
        text-style: bold;
        margin-bottom: 1;
    }

    #form {
        margin: 1 0;
    }

    Input {
        margin-bottom: 1;
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


class DeleteItemScreen(ModalScreen):
    """Screen for deleting folders or databases."""

    def __init__(self, item_type: str, item_name: str, folder_name: str = None) -> None:
        """Initialize the delete screen.

        Args:
            item_type: Type of item to delete ('folder' or 'database')
            item_name: Name of the item to delete
            folder_name: Parent folder name (for databases only)
        """
        super().__init__()
        self.item_type = item_type
        self.item_name = item_name
        self.folder_name = folder_name

    def compose(self) -> ComposeResult:
        """Create child widgets for the screen."""
        message = (
            f"Are you sure you want to delete {self.item_type} '{self.item_name}'?"
        )
        if self.item_type == "database" and self.folder_name:
            message = f"Are you sure you want to delete database '{self.item_name}' from folder '{self.folder_name}'?"

        yield Vertical(
            Label(f"Delete {self.item_type.capitalize()}", id="title"),
            Static(message, id="message"),
            Horizontal(
                Button("Cancel", variant="default", id="cancel"),
                Button("Delete", variant="error", id="delete"),
                id="buttons",
            ),
            id="dialog",
        )

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button presses."""
        if event.button.id == "cancel":
            self.dismiss(False)
            return

        if event.button.id == "delete":
            try:
                # Get user ID from session
                user = (
                    self.app.session.query(User)
                    .filter_by(username=self.app.username)
                    .first()
                )
                if not user:
                    raise ValueError(f"User '{self.app.username}' not found")

                if self.item_type == "folder":
                    # Delete folder
                    from src.commands.folders import delete_folder

                    result = delete_folder(
                        user.id, self.item_name, self.app.session, force=True
                    )
                    if result:
                        self.notify(f"Folder '{self.item_name}' deleted")
                    else:
                        self.notify("Failed to delete folder", severity="error")
                        self.dismiss(False)
                        return

                elif self.item_type == "database":
                    # Delete database
                    from src.commands.databases import delete_database

                    success = delete_database(
                        self.item_name,
                        user.id,
                        self.app.session,
                        force=True,
                        folder_name=self.folder_name,
                    )

                    if not success:
                        self.notify("Failed to delete database", severity="error")
                        self.dismiss(False)
                        return

                    self.notify(f"Database '{self.item_name}' deleted successfully")

                # Refresh tree view on main screen
                if isinstance(self.app.screen, MainScreen):
                    tree = self.app.screen.query_one("DataTree")
                    tree.load_user_data()

                self.dismiss(True)

            except Exception as e:
                self.notify(f"Error: {str(e)}", severity="error")
                self.dismiss(False)

    CSS = """
    DeleteItemScreen {
        align: center middle;
    }

    #dialog {
        background: $surface;
        padding: 1 2;
        border: thick $primary;
        min-width: 50;
    }

    #title {
        text-align: center;
        text-style: bold;
        margin-bottom: 1;
    }

    #message {
        margin: 1 0;
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
