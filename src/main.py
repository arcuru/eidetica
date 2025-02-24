from src.cli import setup_argparse
from src.commands import (
    handle_user_commands,
    handle_folder_commands,
    handle_database_commands,
)
from src.db import setup_database
from src.cli.tui import EideticaApp


def main():
    args = setup_argparse()
    session = setup_database()

    try:
        if args.command == "tui":
            # Launch TUI application
            app = EideticaApp()
            app.run()
        elif args.command == "users":
            handle_user_commands(args, session)
        elif args.command == "folders":
            handle_folder_commands(args, session)
        elif args.command == "databases":
            handle_database_commands(args, session)
    finally:
        session.close()


if __name__ == "__main__":
    main()
