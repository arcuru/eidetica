"""Tree widget for displaying hierarchical data."""

from typing import Optional
from textual.widgets import Tree
from textual.widgets.tree import TreeNode
from textual import on
from sqlalchemy.orm import Session

from src.commands import databases, folders
from src.db.models import User


class DataTree(Tree[dict]):
    """Tree widget for displaying users, folders, and databases."""

    def __init__(self, session: Session, username: str) -> None:
        """Initialize the tree widget.

        Args:
            session: Database session
            username: Username to load data for
        """
        super().__init__("Root")
        self.session = session
        self.username = username
        self.root.expand()
        # Open debug log file
        self.debug_file = open("/tmp/eidetica_debug.log", "a")
        self.debug_file.write("\n=== New Session Started ===\n")

    def debug_log(self, msg: str) -> None:
        """Write a debug message to the log file."""
        self.debug_file.write(f"{msg}\n")
        self.debug_file.flush()  # Force write to disk

    def on_mount(self) -> None:
        """Load initial data when mounted."""
        self.load_user_data()

    def load_user_data(self) -> None:
        """Load user data into the tree."""
        try:
            # Clear existing nodes
            self.debug_log("[DEBUG] Starting tree refresh - removing existing nodes")
            self.root.remove_children()

            try:
                # Get user ID and create user node
                self.debug_log(f"[DEBUG] Querying for user: {self.username}")
                user = (
                    self.session.query(User).filter_by(username=self.username).first()
                )
                if not user:
                    self.debug_log(f"[ERROR] User '{self.username}' not found")
                    return

                self.debug_log(f"[DEBUG] Found user: {user.username} (ID: {user.id})")

                # Create user node
                user_node = self.root.add(
                    self.username, {"type": "user", "name": self.username}
                )
                user_node.expand()

                # Load folders
                try:
                    self.debug_log(f"[DEBUG] Listing folders for user ID: {user.id}")
                    # Force a fresh query from the database
                    self.session.expire_all()

                    user_folders = folders.list_folders(
                        user.id, self.session, format="plain"
                    )
                    self.debug_log(
                        f"[DEBUG] Found {len(user_folders) if user_folders else 0} folders"
                    )

                    if not user_folders:
                        self.debug_log(
                            f"[INFO] No folders found for user '{self.username}'"
                        )
                        return

                    # Add folders to user node
                    for folder in user_folders:
                        try:
                            self.debug_log(f"[DEBUG] Processing folder: {folder.name}")
                            folder_node = user_node.add(
                                folder.name,
                                {
                                    "type": "folder",
                                    "name": folder.name,
                                    "description": folder.description,
                                    "created_at": folder.created_at,
                                },
                            )
                            folder_node.expand()

                            # Load databases for this folder
                            try:
                                self.debug_log(
                                    f"[DEBUG] Loading databases for folder: {folder.name}"
                                )
                                # Force a fresh query from the database
                                self.session.expire_all()

                                folder_dbs = databases.list_databases(
                                    folder.name,  # Corrected argument order
                                    user.id,  # Using user ID instead of username
                                    self.session,
                                )

                                if not folder_dbs:
                                    self.debug_log(
                                        f"[INFO] No databases found in folder {folder.name}"
                                    )
                                    continue

                                self.debug_log(
                                    f"[DEBUG] Found {len(folder_dbs)} databases in folder {folder.name}"
                                )

                                for db in folder_dbs:
                                    self.debug_log(
                                        f"[DEBUG] Adding database to tree: {db.name}"
                                    )
                                    folder_node.add(
                                        db.name,
                                        {
                                            "type": "database",
                                            "name": db.name,
                                            "created_at": db.created_at,
                                            "username": db.username,
                                            "password": db.password,
                                        },
                                    )
                            except Exception as e:
                                self.debug_log(
                                    f"[ERROR] Error loading databases for folder '{folder.name}': {str(e)}"
                                )
                                # Add stack trace for database loading errors
                                import traceback

                                self.debug_log(
                                    f"[DEBUG] Database loading error trace:\n{traceback.format_exc()}"
                                )
                        except Exception as e:
                            self.debug_log(
                                f"[ERROR] Error adding folder '{folder.name}' to tree: {str(e)}"
                            )
                except Exception as e:
                    self.debug_log(f"[ERROR] Error loading folders: {str(e)}")
            except Exception as e:
                self.debug_log(f"[ERROR] Error loading user data: {str(e)}")
        except Exception as e:
            self.debug_log(f"[ERROR] Critical error in load_user_data: {str(e)}")

    def get_node_type(self, node: Optional[TreeNode[dict]]) -> str:
        """Get the type of a node.

        Args:
            node: Tree node to get type for

        Returns:
            String representing the node type
        """
        if node is None or node.data is None:
            return "none"
        return node.data.get("type", "none")

    class NodeSelected(Tree.NodeSelected):
        """Custom node selected message with data."""

        def __init__(self, node: TreeNode[dict]) -> None:
            """Initialize the node selected event.

            Args:
                node: The selected tree node
            """
            super().__init__(node)

    def on_tree_node_selected(self, event: Tree.NodeSelected) -> None:
        """Handle node selection and emit custom event."""
        if not isinstance(event.node, TreeNode):
            self.debug_log("[ERROR] Invalid node type received")
            return

        self.debug_log(f"[DEBUG] Tree selection event received for node: {event.node}")
        if hasattr(event.node, "data"):
            self.debug_log(f"[DEBUG] Node data: {event.node.data}")

        # Bubble up our custom event
        self.post_message(self.NodeSelected(event.node))
        self.debug_log("[DEBUG] Posted custom NodeSelected event")

    def action_expand_node(self) -> None:
        """Expand the currently selected node."""
        if self.cursor_node and not self.cursor_node.is_expanded:
            self.cursor_node.expand()

    def action_collapse_node(self) -> None:
        """Collapse the currently selected node."""
        if self.cursor_node and self.cursor_node.is_expanded:
            self.cursor_node.collapse()

    def find_node(self, label: str) -> Optional[TreeNode]:
        """Find a node by its label.

        Args:
            label: The label to search for

        Returns:
            The found node or None
        """

        def search_nodes(node: TreeNode) -> Optional[TreeNode]:
            if node.label == label:
                return node
            for child in node.children:
                result = search_nodes(child)
                if result:
                    return result
            return None

        return search_nodes(self.root)

    def select_node(self, node: TreeNode) -> None:
        """Select a specific node in the tree.

        Args:
            node: The node to select
        """
        if node:
            # Expand parent nodes
            current = node.parent
            while current and current != self.root:
                current.expand()
                current = current.parent

            # Select the node
            self.select_node(node)
            self.scroll_to_node(node)

    CSS = """
    DataTree {
        width: 30;
        height: 100%;
        border: solid $primary;
        padding: 1;
    }
    """
