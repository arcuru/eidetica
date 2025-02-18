import os
import argparse
import getpass
from .models import get_engine, get_session, init_db, User


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

    session.close()
