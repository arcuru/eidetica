from typing import Optional
import getpass
from src.db import User
from src.db.session import SessionLocal
from termcolor import colored
import sys


def check_user_exists(username: str, session) -> bool:
    """Check if a user exists"""
    return session.query(User).filter_by(username=username).first() is not None


def create_user(
    username: str,
    password: str,
    session,
    role: str = "user",
) -> bool:
    """Create a new user"""
    if check_user_exists(username, session):
        print(colored(f"Error: User '{username}' already exists", "red"))
        return False

    user = User(username=username, role=role)
    user.set_password(password)
    session.add(user)
    session.commit()
    print(colored(f"User '{username}' created successfully", "green"))
    return True


def list_users(session, role: Optional[str] = None) -> None:
    """List users with optional filtering"""
    print("listing")
    query = session.query(User)

    if role:
        query = query.filter_by(role=role)

    users = query.all()

    if not users:
        print(colored("No users found", "yellow"))
        return
    print("Users:")
    print("=====")
    print(f"Username (Role)")
    print(f"---------")

    for user in users:
        print(f"{user.username} ({user.role})")


def update_user(username: str, session, role: Optional[str] = None) -> bool:
    """Update user details"""
    user = session.query(User).filter_by(username=username).first()
    if not user:
        print(colored(f"Error: User '{username}' not found", "red"))
        return False

    if role:
        user.role = role

    session.commit()
    print(colored(f"User '{username}' updated successfully", "green"))
    return True


def delete_user(username: str, session, force: bool = False) -> bool:
    """Delete a user"""
    user = session.query(User).filter_by(username=username).first()
    if not user:
        print(colored(f"Error: User '{username}' not found", "red"))
        return False

    if (
        not force
        and input(f"Are you sure you want to delete user '{username}'? [y/N] ").lower()
        != "y"
    ):
        print(colored("Deletion cancelled", "yellow"))
        return False

    session.delete(user)
    session.commit()
    print(colored(f"User '{username}' deleted successfully", "green"))
    return True


def login_user(session, username: str, password: str) -> bool:
    """Authenticate a user"""
    user = session.query(User).filter_by(username=username).first()
    if user and user.check_password(password):
        return True
    return False


def handle_user_commands(args, session):
    """Handle user commands"""
    try:
        if args.user_command == "create":
            password = args.password or getpass.getpass("Enter password: ")
            create_user(args.username, password, session, args.role)
        elif args.user_command == "list":
            list_users(session, args.role)
        elif args.user_command == "update":
            update_user(args.username, session, args.role)
        elif args.user_command == "delete":
            delete_user(args.username, session, args.force)
        elif args.user_command == "check":
            exists = check_user_exists(args.username, session)
            print(f"User '{args.username}' exists: {exists}")
        elif args.user_command == "login":
            password = args.password or getpass.getpass("Enter password: ")
            if login_user(session, args.username, password):
                print(colored(f"Login successful for user '{args.username}'", "green"))
            else:
                print(colored("Invalid username or password", "red"))
    except Exception as e:
        print(colored(f"Error: {str(e)}", "red"))
        sys.exit(1)
