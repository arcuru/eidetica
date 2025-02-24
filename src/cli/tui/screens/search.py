"""Search screen for finding items."""

from textual.screen import ModalScreen
from textual.app import ComposeResult
from textual.widgets import Button, Input, Static, Label, DataTable
from textual.containers import Vertical, Horizontal
from textual.validation import Length

from src.commands import folders, databases
from src.db import User


class SearchScreen(ModalScreen):
    """Screen for searching folders and databases."""

    def compose(self) -> ComposeResult:
        """Create child widgets for the screen."""
        yield Vertical(
            Label("Search", id="title"),
            Horizontal(
                Input(placeholder="Enter search term", id="search"),
                Button("Search", variant="primary", id="search-btn"),
                id="search-bar",
            ),
            DataTable(id="results"),
            Button("Close", variant="default", id="close"),
            id="container",
        )

    def on_mount(self) -> None:
        """Set up the screen when mounted."""
        table = self.query_one("#results", DataTable)
        table.add_columns("Type", "Name", "Location", "Created")
        self.query_one("#search").focus()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button presses."""
        if event.button.id == "close":
            self.dismiss()
        elif event.button.id == "search-btn":
            self._perform_search()

    def on_input_submitted(self) -> None:
        """Handle search input submission."""
        self._perform_search()

    def _perform_search(self) -> None:
        """Perform the search operation."""
        search_term = self.query_one("#search", Input).value
        if not search_term:
            self.notify("Please enter a search term", severity="warning")
            return

        table = self.query_one("#results", DataTable)
        table.clear()

        try:
            # Get user ID from username
            user = (
                self.app.session.query(User)
                .filter_by(username=self.app.username)
                .first()
            )
            if not user:
                self.notify("User not found", severity="error")
                return

            # Search folders
            user_folders = folders.search_folders(
                user.id,
                search_term,
                self.app.session,
            )
            if user_folders:
                for folder in user_folders:
                    table.add_row(
                        "Folder",
                        folder.name,
                        self.app.username,
                        str(folder.created_at),
                    )

            # Search databases
            user_dbs = databases.search_databases(
                user.id,
                search_term,
                self.app.session,
            )
            if user_dbs:
                for db in user_dbs:
                    table.add_row(
                        "Database",
                        db.name,
                        db.folder.name,
                        str(db.created_at),
                    )

            if table.row_count == 0:
                self.notify("No results found")

        except Exception as e:
            self.notify(f"Search error: {str(e)}", severity="error")

    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        """Handle row selection in results table."""
        row = event.data_table.get_row_at(event.row_key)
        item_type, name, location = row[:3]

        # Find and select the item in the tree
        tree = self.app.query_one("DataTree")
        if item_type == "Folder":
            node = tree.find_node(name)
        else:  # Database
            folder_node = tree.find_node(location)
            if folder_node:
                folder_node.expand()
                node = tree.find_node(name)
            else:
                node = None

        if node:
            tree.select_node(node)
            self.dismiss()
        else:
            self.notify("Could not locate item in tree", severity="warning")

    CSS = """
    SearchScreen {
        align: center middle;
    }

    #container {
        width: 80%;
        height: 80%;
        background: $surface;
        border: thick $primary;
        padding: 1;
    }

    #title {
        text-align: center;
        text-style: bold;
        margin-bottom: 1;
    }

    #search-bar {
        margin-bottom: 1;
        height: 3;
    }

    #search {
        width: 1fr;
        margin-right: 1;
    }

    #results {
        height: 1fr;
        margin: 1 0;
    }

    #close {
        margin-top: 1;
        width: 100%;
    }
    """
