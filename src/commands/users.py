import getpass
from src.db import User


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


def handle_user_commands(args, session):
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
