from typing import Optional
from src.db import User, Folder, Database
from src.db.session import SessionLocal, get_db
from termcolor import colored
from src.commands.utils import confirm_action, format_output
import sys


def get_folder_by_name(user_id: int, name: str, session) -> Optional[Folder]:
    """Get folder by name for a specific user"""
    return session.query(Folder).filter_by(user_id=user_id, name=name).first()


def validate_name_uniqueness(user_id: int, name: str, session) -> bool:
    """Validate that a folder/database name is unique for the user"""
    existing = session.query(Folder).filter_by(user_id=user_id, name=name).first()
    return existing is None


def list_folders(user_id: int, session, format: str = "plain") -> None:
    """List all folders for a user"""
    folders = session.query(Folder).filter_by(user_id=user_id).all()
    if not folders:
        print(colored("No folders found", "yellow"))
        return

    data = {
        f.id: {"name": f.name, "created_at": f.created_at, "description": f.description}
        for f in folders
    }
    print(format_output(data, format))


def create_folder(
    user_id: int, name: str, session, description: Optional[str] = None
) -> bool:
    """Create a new folder"""
    if not validate_name_uniqueness(user_id, name, session):
        print(colored(f"Error: Folder '{name}' already exists", "red"))
        return False

    folder = Folder(name=name, user_id=user_id, description=description)
    session.add(folder)
    session.commit()
    print(colored(f"Folder '{name}' created successfully", "green"))
    return True


def rename_folder(
    user_id: int, old_name: str, new_name: str, session, force: bool = False
) -> bool:
    """Rename a folder"""
    if not force and not confirm_action(
        f"Are you sure you want to rename '{old_name}' to '{new_name}'?"
    ):
        print(colored("Rename cancelled", "yellow"))
        return False

    folder = get_folder_by_name(user_id, old_name, session)
    if not folder:
        print(colored(f"Error: Folder '{old_name}' not found", "red"))
        return False

    folder.name = new_name
    session.commit()
    print(colored(f"Folder renamed to '{new_name}' successfully", "green"))
    return True


def delete_folder(
    user_id: int, name: str, session, force: bool = False, dry_run: bool = False
) -> bool:
    """Delete a folder"""
    folder = get_folder_by_name(user_id, name, session)
    if not folder:
        print(colored(f"Error: Folder '{name}' not found", "red"))
        return False

    if not force and not confirm_action(
        f"Are you sure you want to delete folder '{name}' and all its contents?"
    ):
        print(colored("Deletion cancelled", "yellow"))
        return False

    if dry_run:
        print(colored(f"Would delete folder '{name}' and its contents", "blue"))
        return True

    session.delete(folder)
    session.commit()
    print(colored(f"Folder '{name}' deleted successfully", "green"))
    return True


def search_folders(user_id: int, query: str, session, format: str = "plain") -> None:
    """Search folders by name"""
    folders = (
        session.query(Folder)
        .filter(Folder.user_id == user_id, Folder.name.ilike(f"%{query}%"))
        .all()
    )

    if not folders:
        print(colored("No matching folders found", "yellow"))
        return

    data = {
        f.id: {"name": f.name, "created_at": f.created_at, "description": f.description}
        for f in folders
    }
    print(format_output(data, format))


def handle_folder_commands(args, session):
    """Handle folder commands"""
    user = session.query(User).filter_by(username=args.username).first()
    if not user:
        print(colored(f"Error: User '{args.username}' not found", "red"))
        return

    try:
        if args.folder_command == "list":
            list_folders(user.id, session, args.format)
        elif args.folder_command == "create":
            create_folder(user.id, args.name, session, args.description)
        elif args.folder_command == "rename":
            rename_folder(user.id, args.old_name, args.new_name, session, args.force)
        elif args.folder_command == "delete":
            delete_folder(user.id, args.name, session, args.force, args.dry_run)
        elif args.folder_command == "info":
            folder = get_folder_by_name(user.id, args.name, session)
            if folder:
                data = {
                    "name": folder.name,
                    "created_at": folder.created_at,
                    "description": folder.description,
                    "database_count": len(folder.databases),
                }
                print(format_output(data, args.format))
            else:
                print(colored(f"Folder '{args.name}' not found", "red"))
        elif args.folder_command == "search":
            search_folders(user.id, args.query, session, args.format)
    except Exception as e:
        print(colored(f"Error: {str(e)}", "red"))
        sys.exit(1)
