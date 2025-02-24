"""Main screen for the Eidetica TUI."""

from textual.app import ComposeResult
from textual.screen import Screen
from textual.widgets import Header, Footer, Static, Label, Tree
from textual.containers import Horizontal, Vertical
from textual import on
from ..widgets.tree import DataTree
from src.db.models import User


class DetailsPanel(Static):
    """Panel for displaying details of selected items."""

    def __init__(self):
        """Initialize the details panel."""
        super().__init__("No item selected", expand=True)
        self.current_item = None
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write("[DEBUG] DetailsPanel initialized\n")

    def update_details(self, node) -> None:
        """Update the details panel with information about the selected node."""
        # Debug logging
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(f"[DEBUG] DetailsPanel.update_details called with node: {node}\n")
            if node is not None:
                f.write(f"[DEBUG] Node data: {node.data}\n")

        if node is None or node.data is None:
            self.update("No item selected")
            return

        item_type = node.data.get("type", "unknown")
        details = []

        if item_type == "user":
            details = [
                "User Details",
                "------------",
                f"Username: {node.data['name']}",
            ]
        elif item_type == "folder":
            details = [
                "Folder Details",
                "--------------",
                f"Name: {node.data['name']}",
                f"Description: {node.data.get('description', 'No description')}",
                f"Created: {node.data['created_at']}",
            ]
        elif item_type == "database":
            details = [
                "Database Details",
                "----------------",
                f"Name: {node.data['name']}",
                f"Username: {node.data['username']}",
                f"Password: {node.data['password']}",
                f"Created: {node.data['created_at']}",
            ]
        else:
            details = ["Unknown item type"]

        # Format details with proper line endings for the terminal
        content = "\n".join(details)
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(f"[DEBUG] Updating DetailsPanel with content:\n{content}\n")

        # Use markup to ensure proper text rendering
        self.update(f"[default on default]{content}[/]")


class MainScreen(Screen):
    """Main screen of the application."""

    BINDINGS = [
        ("q", "quit", "Quit"),
        ("r", "refresh", "Refresh"),
        ("left", "collapse_node", "Collapse Node"),
        ("right", "expand_node", "Expand Node"),
        ("n", "new_item", "New Item"),
        ("d", "delete_item", "Delete Item"),
        ("e", "edit_item", "Edit Item"),
        ("f", "search", "Search"),
        ("?", "show_help", "Help"),
    ]

    def compose(self) -> ComposeResult:
        """Create child widgets for the screen."""
        yield Header()
        yield Horizontal(
            DataTree(self.app.session, self.app.username),
            Vertical(
                Label("Details", classes="panel-title"),
                DetailsPanel(),
                id="details-container",
            ),
            id="main-container",
        )
        yield Footer()

    def on_mount(self) -> None:
        """Handle screen mount."""
        self.title = f"Eidetica - {self.app.username}"

    @on(Tree.NodeSelected)
    def on_tree_node_selected(self, event: Tree.NodeSelected) -> None:
        """Handle node selection in the tree."""
        # Debug logging
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(
                f"[DEBUG] MainScreen.on_tree_node_selected called with event: {event}\n"
            )
            f.write(f"[DEBUG] Event node: {event.node}\n")
            if hasattr(event.node, "data"):
                f.write(f"[DEBUG] Node data: {event.node.data}\n")

        details_panel = self.query_one(DetailsPanel)
        if details_panel:
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[DEBUG] Found DetailsPanel, updating details\n")
            details_panel.update_details(event.node)
        else:
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[ERROR] Could not find DetailsPanel\n")

    def action_collapse_node(self) -> None:
        """Collapse the currently selected node."""
        tree = self.query_one(DataTree)
        tree.action_collapse_node()

    def action_expand_node(self) -> None:
        """Expand the currently selected node."""
        tree = self.query_one(DataTree)
        tree.action_expand_node()

    def action_refresh(self) -> None:
        """Refresh the tree view."""
        tree = self.query_one(DataTree)
        tree.load_user_data()

    def action_new_item(self) -> None:
        """Create a new item based on current selection."""
        tree = self.query_one(DataTree)
        node = tree.cursor_node
        if not node:
            self.notify("Select a location first", severity="warning")
            return

        node_type = tree.get_node_type(node)
        if node_type == "user":
            # Create new folder
            self.app.push_screen_with_args("create_folder")
        elif node_type == "folder":
            # Create new database
            self.app.push_screen_with_args(
                "create_database", folder_name=node.data["name"]
            )
        else:
            self.notify("Cannot create item here", severity="error")

    async def action_delete_item(self) -> None:
        """Delete the selected item."""
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write("[DEBUG] action_delete_item called\n")

        tree = self.query_one(DataTree)
        if not tree:
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[ERROR] Could not find DataTree\n")
            return

        node = tree.cursor_node
        if not node or not node.data:
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[ERROR] No node selected or node has no data\n")
            self.notify("No item selected", severity="warning")
            return

        node_type = tree.get_node_type(node)
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(f"[DEBUG] Selected node type: {node_type}\n")
            f.write(f"[DEBUG] Node data: {node.data}\n")

        if node_type == "folder":
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[DEBUG] Starting folder deletion flow\n")

            try:
                confirmed = await self.app.push_screen_wait(
                    "confirm_dialog", message=f"Delete folder '{node.data['name']}'?"
                )
                with open("/tmp/eidetica_debug.log", "a") as f:
                    f.write(f"[DEBUG] Folder deletion confirmed: {confirmed}\n")

                if confirmed:
                    from src.commands.folders import delete_folder

                    try:
                        user = (
                            self.app.session.query(User)
                            .filter_by(username=self.app.username)
                            .first()
                        )
                        if not user:
                            raise Exception(f"User '{self.app.username}' not found")

                        result = delete_folder(
                            user.id, node.data["name"], self.app.session, force=True
                        )
                        if result:
                            self.notify(f"Folder '{node.data['name']}' deleted")
                            tree.load_user_data()
                        else:
                            self.notify("Failed to delete folder", severity="error")
                    except Exception as e:
                        self.notify(
                            f"Error deleting folder: {str(e)}", severity="error"
                        )

            except Exception as e:
                with open("/tmp/eidetica_debug.log", "a") as f:
                    f.write(f"[ERROR] Exception in folder deletion flow: {str(e)}\n")
                self.notify(f"Error in deletion flow: {str(e)}", severity="error")

        elif node_type == "database":
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[DEBUG] Starting database deletion flow\n")

            try:
                # Get parent folder node
                folder_node = node.parent
                if (
                    not folder_node
                    or not folder_node.data
                    or folder_node.data.get("type") != "folder"
                ):
                    with open("/tmp/eidetica_debug.log", "a") as f:
                        f.write("[ERROR] Cannot determine parent folder\n")
                    self.notify("Cannot determine parent folder", severity="error")
                    return

                with open("/tmp/eidetica_debug.log", "a") as f:
                    f.write(f"[DEBUG] Parent folder: {folder_node.data['name']}\n")

                # Handle confirmation the same way as folder deletion
                result = await self.app.push_screen_wait(
                    "confirm_dialog",
                    message=f"Delete database '{node.data['name']}' from folder '{folder_node.data['name']}'?",
                )

                with open("/tmp/eidetica_debug.log", "a") as f:
                    f.write(f"[DEBUG] Raw dialog result: {result}\n")

                # Convert to bool explicitly like folder deletion
                if bool(result):
                    with open("/tmp/eidetica_debug.log", "a") as f:
                        f.write(
                            "[DEBUG] Confirmation successful, proceeding with deletion\n"
                        )
                    from src.commands.databases import delete_database

                    try:
                        user = (
                            self.app.session.query(User)
                            .filter_by(username=self.app.username)
                            .first()
                        )
                        if not user:
                            with open("/tmp/eidetica_debug.log", "a") as f:
                                f.write(
                                    f"[ERROR] User not found: {self.app.username}\n"
                                )
                            raise Exception(f"User '{self.app.username}' not found")

                        with open("/tmp/eidetica_debug.log", "a") as f:
                            f.write(f"[DEBUG] Found user ID: {user.id}\n")
                            f.write("[DEBUG] Calling delete_database\n")

                        success = delete_database(
                            node.data["name"], user.id, self.app.session, force=True
                        )

                        with open("/tmp/eidetica_debug.log", "a") as f:
                            f.write(f"[DEBUG] Delete database result: {success}\n")

                        if not success:
                            with open("/tmp/eidetica_debug.log", "a") as f:
                                f.write("[ERROR] Database deletion failed\n")
                            self.notify("Failed to delete database", severity="error")
                            return

                        with open("/tmp/eidetica_debug.log", "a") as f:
                            f.write(f"[INFO] Database deleted successfully\n")
                        self.notify(
                            f"Database '{node.data['name']}' deleted successfully"
                        )

                        tree.load_user_data()
                        with open("/tmp/eidetica_debug.log", "a") as f:
                            f.write("[DEBUG] Tree view refreshed\n")

                    except Exception as e:
                        with open("/tmp/eidetica_debug.log", "a") as f:
                            f.write(f"[ERROR] Exception during deletion: {str(e)}\n")
                        self.notify(
                            f"Error deleting database: {str(e)}", severity="error"
                        )

            except Exception as e:
                with open("/tmp/eidetica_debug.log", "a") as f:
                    f.write(f"[ERROR] Exception in database deletion flow: {str(e)}\n")
                self.notify(f"Error in deletion flow: {str(e)}", severity="error")

        else:
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(f"[ERROR] Cannot delete item of type: {node_type}\n")
            self.notify("Cannot delete this item", severity="error")

    def action_edit_item(self) -> None:
        """Edit the selected item."""
        tree = self.query_one(DataTree)
        node = tree.cursor_node
        if not node or not node.data:
            self.notify("No item selected", severity="warning")
            return

        node_type = tree.get_node_type(node)
        if node_type == "folder":
            self.app.push_screen_with_args("edit_folder", folder_name=node.data["name"])
        elif node_type == "database":
            # Get parent folder node
            folder_node = node.parent
            if (
                not folder_node
                or not folder_node.data
                or folder_node.data.get("type") != "folder"
            ):
                self.notify("Cannot determine parent folder", severity="error")
                return

            self.app.push_screen_with_args(
                "edit_database",
                database_name=node.data["name"],
                folder_name=folder_node.data["name"],
            )
        else:
            self.notify("Cannot edit this item", severity="error")

    def action_search(self) -> None:
        """Search for items."""
        self.app.push_screen_with_args("search")

    def action_show_help(self) -> None:
        """Show help dialog with available commands."""
        help_text = """
Available Commands:

Navigation:
  ↑/↓ - Move selection
  ←   - Collapse node
  →   - Expand node
  
Operations:
  n - New item
  d - Delete item
  e - Edit item
  f - Search
  r - Refresh
  
General:
  q - Quit
  ? - Show this help
"""
        self.app.push_screen_with_args("help_dialog", content=help_text)

    CSS = """
    Screen {
        background: $surface;
    }

    #main-container {
        width: 100%;
        height: 100%;
        background: $surface;
    }

    #details-container {
        width: 70;
        height: 100%;
        margin-left: 1;
        background: $panel;
    }

    .panel-title {
        background: $accent;
        color: $text;
        padding: 1;
        text-style: bold;
        width: 100%;
        text-align: center;
    }

    DetailsPanel {
        width: 100%;
        height: 100%;
        border: solid $primary;
        padding: 1;
        background: $panel;
        color: $text;
        overflow: auto;
        scrollbar-gutter: stable;
    }
    """
