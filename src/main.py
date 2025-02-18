import os
import argparse
import getpass
from src.models import get_engine, get_session, init_db, User, Folder


def check_user_exists(username, session):
    return session.query(User).filter_by(username=username).first() is not None


def create_user(username, password, session):
    if check_user_exists(username, session):
        print(f"Error: User '{username}' already exists")
        return False

    user = User(username=username)
    user.set_password(password)
    session.add(user)
    session.commit()
    print(f"User '{username}' created successfully")
    return True


def list_folders(user_id, session):
    folders = session.query(Folder).filter_by(user_id=user_id).all()
    if not folders:
        print("No folders found")
        return

    print("Your folders:")
    for folder in folders:
        print(f"{folder.id}: {folder.name} (created: {folder.created_at})")


def create_folder(user_id, name, session):
    if not name:
        print("Error: Folder name cannot be empty")
        return False

    folder = Folder(name=name, user_id=user_id)
    session.add(folder)
    session.commit()
    print(f"Folder '{name}' created successfully")
    return True


def delete_folder(user_id, folder_id, session):
    folder = session.query(Folder).filter_by(id=folder_id, user_id=user_id).first()
    if not folder:
        print(f"Error: Folder with ID {folder_id} not found")
        return False

    session.delete(folder)
    session.commit()
    print(f"Folder '{folder.name}' deleted successfully")
    return True


def rename_folder(user_id, folder_id, new_name, session):
    if not new_name:
        print("Error: New folder name cannot be empty")
        return False

    folder = session.query(Folder).filter_by(id=folder_id, user_id=user_id).first()
    if not folder:
        print(f"Error: Folder with ID {folder_id} not found")
        return False

    folder.name = new_name
    session.commit()
    print(f"Folder renamed to '{new_name}' successfully")
    return True


def main():
    parser = argparse.ArgumentParser(description="Eidetica user management")
    subparsers = parser.add_subparsers(dest="command", required=True)

    # Check user command
    check_parser = subparsers.add_parser("check", help="Check if user exists")
    check_parser.add_argument("username", help="Username to check")

    # Create user command
    create_parser = subparsers.add_parser("create", help="Create a new user")
    create_parser.add_argument("username", help="Username to create")
    create_parser.add_argument(
        "--password", help="User password (prompt if not provided)"
    )

    # Login command
    login_parser = subparsers.add_parser(
        "login", help="Login with username and password"
    )
    login_parser.add_argument("username", help="Username to login")
    login_parser.add_argument(
        "--password", help="User password (prompt if not provided)"
    )

    # Folder commands
    folder_parser = subparsers.add_parser("folders", help="Manage folders")
    folder_subparsers = folder_parser.add_subparsers(
        dest="folder_command", required=True
    )

    # List folders
    list_parser = folder_subparsers.add_parser("list", help="List all folders")
    list_parser.add_argument("username", help="Username to list folders for")

    # Create folder
    create_folder_parser = folder_subparsers.add_parser(
        "create", help="Create a folder"
    )
    create_folder_parser.add_argument("username", help="Username to create folder for")
    create_folder_parser.add_argument("name", help="Name of the folder to create")

    # Delete folder
    delete_folder_parser = folder_subparsers.add_parser(
        "delete", help="Delete a folder"
    )
    delete_folder_parser.add_argument("username", help="Username to delete folder for")
    delete_folder_parser.add_argument(
        "folder_id", type=int, help="ID of folder to delete"
    )

    # Rename folder
    rename_folder_parser = folder_subparsers.add_parser(
        "rename", help="Rename a folder"
    )
    rename_folder_parser.add_argument("username", help="Username to rename folder for")
    rename_folder_parser.add_argument(
        "folder_id", type=int, help="ID of folder to rename"
    )
    rename_folder_parser.add_argument("new_name", help="New name for the folder")

    args = parser.parse_args()

    # Get database URL from environment variable
    database_url = os.getenv("DATABASE_URL", "sqlite:///eidetica.db")

    # Initialize database connection
    engine = get_engine(database_url)
    init_db(engine)
    session = get_session(engine)

    if args.command == "check":
        exists = check_user_exists(args.username, session)
        print(f"User '{args.username}' exists: {exists}")
    elif args.command == "create":
        password = args.password or getpass.getpass("Enter password: ")
        create_user(args.username, password, session)
    elif args.command == "login":
        password = args.password or getpass.getpass("Enter password: ")
        user = session.query(User).filter_by(username=args.username).first()
        if user and user.check_password(password):
            print(f"Login successful for user '{args.username}'")
        else:
            print("Invalid username or password")
    elif args.command == "folders":
        user = session.query(User).filter_by(username=args.username).first()
        if not user:
            print(f"Error: User '{args.username}' not found")
            session.close()
            return

        if args.folder_command == "list":
            list_folders(user.id, session)
        elif args.folder_command == "create":
            create_folder(user.id, args.name, session)
        elif args.folder_command == "delete":
            delete_folder(user.id, args.folder_id, session)
        elif args.folder_command == "rename":
            rename_folder(user.id, args.folder_id, args.new_name, session)

    session.close()
