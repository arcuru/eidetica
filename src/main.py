from src.cli import setup_argparse
from src.commands import handle_user_commands, handle_folder_commands
from src.db import setup_database


def main():
    args = setup_argparse()
    session = setup_database()

    try:
        if args.command in ["check", "create", "login"]:
            handle_user_commands(args, session)
        elif args.command == "folders":
            handle_folder_commands(args, session)
    finally:
        session.close()


if __name__ == "__main__":
    main()
