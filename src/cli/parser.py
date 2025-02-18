import argparse


def setup_argparse():
    parser = argparse.ArgumentParser(description="Eidetica user management")
    subparsers = parser.add_subparsers(dest="command", required=True)

    # User management commands
    check_parser = subparsers.add_parser("check", help="Check if user exists")
    check_parser.add_argument("username", help="Username to check")

    create_parser = subparsers.add_parser("create", help="Create a new user")
    create_parser.add_argument("username", help="Username to create")
    create_parser.add_argument(
        "--password", help="User password (prompt if not provided)"
    )

    login_parser = subparsers.add_parser(
        "login", help="Login with username and password"
    )
    login_parser.add_argument("username", help="Username to login")
    login_parser.add_argument(
        "--password", help="User password (prompt if not provided)"
    )

    # Folder management commands
    folder_parser = subparsers.add_parser("folders", help="Manage folders")
    folder_subparsers = folder_parser.add_subparsers(
        dest="folder_command", required=True
    )

    list_parser = folder_subparsers.add_parser("list", help="List all folders")
    list_parser.add_argument("username", help="Username to list folders for")

    create_folder_parser = folder_subparsers.add_parser(
        "create", help="Create a folder"
    )
    create_folder_parser.add_argument("username", help="Username to create folder for")
    create_folder_parser.add_argument("name", help="Name of the folder to create")

    delete_folder_parser = folder_subparsers.add_parser(
        "delete", help="Delete a folder"
    )
    delete_folder_parser.add_argument("username", help="Username to delete folder for")
    delete_folder_parser.add_argument(
        "folder_id", type=int, help="ID of folder to delete"
    )

    rename_folder_parser = folder_subparsers.add_parser(
        "rename", help="Rename a folder"
    )
    rename_folder_parser.add_argument("username", help="Username to rename folder for")
    rename_folder_parser.add_argument(
        "folder_id", type=int, help="ID of folder to rename"
    )
    rename_folder_parser.add_argument("new_name", help="New name for the folder")

    return parser.parse_args()
