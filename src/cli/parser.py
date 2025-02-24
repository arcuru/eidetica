import argparse
from typing import Optional
from enum import Enum


class OutputFormat(Enum):
    PLAIN = "plain"
    JSON = "json"
    TABLE = "table"


def setup_argparse():
    parser = argparse.ArgumentParser(
        description="Eidetica folder and database management",
        formatter_class=argparse.RawTextHelpFormatter,
    )
    parser.add_argument(
        "--format",
        type=OutputFormat,
        choices=list(OutputFormat),
        default=OutputFormat.PLAIN,
        help="Output format (default: plain)",
    )

    # Main command groups
    subparsers = parser.add_subparsers(dest="command", required=True)

    # Users command group
    users_parser = subparsers.add_parser("users", help="Manage users")
    users_subparsers = users_parser.add_subparsers(dest="user_command", required=True)

    # User create
    create_parser = users_subparsers.add_parser("create", help="Create a new user")
    create_parser.add_argument("username", help="Username to create")
    create_parser.add_argument(
        "--password", help="User password (prompt if not provided)"
    )
    create_parser.add_argument(
        "--role", choices=["admin", "user"], default="user", help="User role"
    )

    # User list
    list_parser = users_subparsers.add_parser("list", help="List users")
    list_parser.add_argument("--role", choices=["admin", "user"], help="Filter by role")

    # User update
    update_parser = users_subparsers.add_parser("update", help="Update user details")
    update_parser.add_argument("username", help="Username to update")
    update_parser.add_argument(
        "--role", choices=["admin", "user"], help="Update user role"
    )

    # User delete
    delete_parser = users_subparsers.add_parser("delete", help="Delete a user")
    delete_parser.add_argument("username", help="Username to delete")
    delete_parser.add_argument(
        "--force", action="store_true", help="Skip confirmation prompt"
    )

    # User check
    check_parser = users_subparsers.add_parser("check", help="Check if user exists")
    check_parser.add_argument("username", help="Username to check")

    # User login
    login_parser = users_subparsers.add_parser(
        "login", help="Login with username and password"
    )
    login_parser.add_argument("username", help="Username to login")
    login_parser.add_argument(
        "--password", help="User password (prompt if not provided)"
    )

    # Folders command group
    folders_parser = subparsers.add_parser("folders", help="Manage folders")
    folders_subparsers = folders_parser.add_subparsers(
        dest="folder_command", required=True
    )

    # Folders list
    list_parser = folders_subparsers.add_parser("list", help="List all folders")
    list_parser.add_argument("username", help="Username to list folders for")

    # Folders create
    create_parser = folders_subparsers.add_parser("create", help="Create a new folder")
    create_parser.add_argument("username", help="Username to create folder for")
    create_parser.add_argument(
        "--name", required=True, help="Name of the folder to create"
    )
    create_parser.add_argument(
        "--description", help="Optional description for the folder"
    )

    # Folders rename
    rename_parser = folders_subparsers.add_parser("rename", help="Rename a folder")
    rename_parser.add_argument("username", help="Username to rename folder for")
    rename_parser.add_argument("old_name", help="Current name of the folder")
    rename_parser.add_argument("new_name", help="New name for the folder")
    rename_parser.add_argument(
        "--force", action="store_true", help="Skip confirmation prompt"
    )

    # Folders delete
    delete_parser = folders_subparsers.add_parser("delete", help="Delete a folder")
    delete_parser.add_argument("username", help="Username to delete folder for")
    delete_parser.add_argument("name", help="Name of the folder to delete")
    delete_parser.add_argument(
        "--force", action="store_true", help="Skip confirmation prompt"
    )
    delete_parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be deleted without actually deleting",
    )

    # Folders info
    info_parser = folders_subparsers.add_parser(
        "info", help="Get information about a folder"
    )
    info_parser.add_argument("username", help="Username to get folder info for")
    info_parser.add_argument("name", help="Name of the folder to get info for")

    # Folders search
    search_parser = folders_subparsers.add_parser(
        "search", help="Search folders by name"
    )
    search_parser.add_argument("username", help="Username to search folders for")
    search_parser.add_argument("query", help="Search query")

    # Databases command group
    databases_parser = subparsers.add_parser("databases", help="Manage databases")
    databases_subparsers = databases_parser.add_subparsers(
        dest="database_command", required=True
    )

    # Databases list
    db_list_parser = databases_subparsers.add_parser(
        "list", help="List databases in a folder"
    )
    db_list_parser.add_argument(
        "--folder", required=True, help="Folder containing the databases"
    )

    # Databases create
    db_create_parser = databases_subparsers.add_parser(
        "create", help="Create a new database"
    )
    db_create_parser.add_argument(
        "--folder", required=True, help="Folder to create the database in"
    )
    db_create_parser.add_argument(
        "--dbname", required=True, help="Name of the database to create"
    )

    # Databases info
    db_info_parser = databases_subparsers.add_parser(
        "info", help="Get database connection info"
    )
    db_info_parser.add_argument("dbname", help="Name of the database to get info for")

    # Databases reset-password
    db_reset_parser = databases_subparsers.add_parser(
        "reset-password", help="Reset database password"
    )
    db_reset_parser.add_argument(
        "dbname", help="Name of the database to reset password for"
    )
    db_reset_parser.add_argument(
        "--force", action="store_true", help="Skip confirmation prompt"
    )

    # Databases delete
    db_delete_parser = databases_subparsers.add_parser(
        "delete", help="Delete a database"
    )
    db_delete_parser.add_argument("dbname", help="Name of the database to delete")
    db_delete_parser.add_argument(
        "--force", action="store_true", help="Skip confirmation prompt"
    )
    db_delete_parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be deleted without actually deleting",
    )

    return parser.parse_args()
