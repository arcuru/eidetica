from typing import Optional
from src.db import User, Folder, Database
from src.db.session import SessionLocal, get_db
from termcolor import colored
from src.commands.utils import confirm_action, format_output
import sys
import secrets
import string
import psycopg2
from psycopg2 import sql
from os import getenv


def get_database_by_name(user_id: int, dbname: str, session) -> Optional[Database]:
    """Get database by name for a specific user"""
    return (
        session.query(Database)
        .join(Folder)
        .filter(Folder.user_id == user_id, Database.name == dbname)
        .first()
    )


def validate_dbname_uniqueness(folder_id: int, dbname: str, session) -> bool:
    """Validate that a database name is unique within a folder"""
    existing = (
        session.query(Database).filter_by(folder_id=folder_id, name=dbname).first()
    )
    return existing is None


def list_databases(
    folder_name: str, user_id: int, session, format: str = "plain"
) -> None:
    """List databases in a folder"""
    folder = session.query(Folder).filter_by(user_id=user_id, name=folder_name).first()
    if not folder:
        print(colored(f"Folder '{folder_name}' not found", "red"))
        return

    databases = folder.databases
    if not databases:
        print(colored("No databases found", "yellow"))
        return

    data = {db.id: {"name": db.name, "created_at": db.created_at} for db in databases}
    print(format_output(data, format))


def create_database(folder_name: str, dbname: str, user_id: int, session) -> bool:
    """Create a new database"""
    folder = session.query(Folder).filter_by(user_id=user_id, name=folder_name).first()
    if not folder:
        print(colored(f"Folder '{folder_name}' not found", "red"))
        return False

    if not validate_dbname_uniqueness(folder.id, dbname, session):
        print(
            colored(
                f"Error: Database '{dbname}' already exists in folder '{folder_name}'",
                "red",
            )
        )
        return False

    # Generate credentials
    username = "".join(secrets.choice(string.ascii_lowercase) for _ in range(12))
    password = "".join(
        secrets.choice(string.ascii_letters + string.digits) for _ in range(16)
    )

    # Create database in PostgreSQL
    try:
        db_url = getenv("POSTGRES_URL")
        if not db_url:
            raise ValueError("POSTGRES_URL environment variable is not set")

        conn = psycopg2.connect(db_url)
        conn.autocommit = True
        cursor = conn.cursor()

        cursor.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(dbname)))
        cursor.execute(
            sql.SQL("CREATE USER {} WITH PASSWORD %s").format(sql.Identifier(username)),
            [password],
        )
        cursor.execute(
            sql.SQL("GRANT ALL PRIVILEGES ON DATABASE {} TO {}").format(
                sql.Identifier(dbname), sql.Identifier(username)
            )
        )

        cursor.close()
        conn.close()
    except Exception as e:
        print(colored(f"Error creating database: {e}", "red"))
        return False

    # Store credentials in local database
    database = Database(
        name=dbname, username=username, password=password, folder_id=folder.id
    )
    session.add(database)
    session.commit()

    print(colored(f"Database '{dbname}' created successfully", "green"))
    print(colored(f"Username: {username}", "blue"))
    print(colored(f"Password: {password}", "blue"))
    return True


def get_database_info(
    dbname: str, user_id: int, session, format: str = "plain"
) -> None:
    """Get database connection info"""
    database = get_database_by_name(user_id, dbname, session)
    if not database:
        print(colored(f"Database '{dbname}' not found", "red"))
        return

    data = {
        "name": database.name,
        "username": database.username,
        "password": database.password,
        "connection_url": f"postgresql://{database.username}:{database.password}@localhost/{database.name}",
        "created_at": database.created_at,
        "size": database.size,
    }
    print(format_output(data, format))


def reset_database_password(
    dbname: str, user_id: int, session, force: bool = False
) -> bool:
    """Reset database password"""
    if not force and not confirm_action(
        f"Are you sure you want to reset password for database '{dbname}'?"
    ):
        print(colored("Password reset cancelled", "yellow"))
        return False

    database = get_database_by_name(user_id, dbname, session)
    if not database:
        print(colored(f"Database '{dbname}' not found", "red"))
        return False

    new_password = "".join(
        secrets.choice(string.ascii_letters + string.digits) for _ in range(16)
    )

    try:
        db_url = getenv("POSTGRES_URL")
        if not db_url:
            raise ValueError("POSTGRES_URL environment variable is not set")

        conn = psycopg2.connect(db_url)
        conn.autocommit = True
        cursor = conn.cursor()

        cursor.execute(
            sql.SQL("ALTER USER {} WITH PASSWORD %s").format(
                sql.Identifier(database.username)
            ),
            [new_password],
        )

        cursor.close()
        conn.close()
    except Exception as e:
        print(colored(f"Error resetting password: {e}", "red"))
        return False

    database.password = new_password
    session.commit()

    print(colored(f"Password for database '{dbname}' reset successfully", "green"))
    print(colored(f"New password: {new_password}", "blue"))
    return True


def delete_database(
    dbname: str, user_id: int, session, force: bool = False, dry_run: bool = False
) -> bool:
    """Delete a database"""
    if not force and not confirm_action(
        f"Are you sure you want to delete database '{dbname}'?"
    ):
        print(colored("Deletion cancelled", "yellow"))
        return False

    database = get_database_by_name(user_id, dbname, session)
    if not database:
        print(colored(f"Database '{dbname}' not found", "red"))
        return False

    if dry_run:
        print(colored(f"Would delete database '{dbname}'", "blue"))
        return True

    try:
        db_url = getenv("POSTGRES_URL")
        if not db_url:
            raise ValueError("POSTGRES_URL environment variable is not set")

        conn = psycopg2.connect(db_url)
        conn.autocommit = True
        cursor = conn.cursor()

        cursor.execute(sql.SQL("DROP DATABASE {}").format(sql.Identifier(dbname)))
        cursor.execute(
            sql.SQL("DROP USER {}").format(sql.Identifier(database.username))
        )

        cursor.close()
        conn.close()
    except Exception as e:
        print(colored(f"Error deleting database: {e}", "red"))
        return False

    session.delete(database)
    session.commit()

    print(colored(f"Database '{dbname}' deleted successfully", "green"))
    return True


def handle_database_commands(args, session):
    """Handle database commands"""
    user = session.query(User).filter_by(username=args.username).first()
    if not user:
        print(colored(f"Error: User '{args.username}' not found", "red"))
        return

    try:
        if args.database_command == "list":
            list_databases(args.folder, user.id, session, args.format)
        elif args.database_command == "create":
            create_database(args.folder, args.dbname, user.id, session)
        elif args.database_command == "info":
            get_database_info(args.dbname, user.id, session, args.format)
        elif args.database_command == "reset-password":
            reset_database_password(args.dbname, user.id, session, args.force)
        elif args.database_command == "delete":
            delete_database(args.dbname, user.id, session, args.force, args.dry_run)
    except Exception as e:
        print(colored(f"Error: {str(e)}", "red"))
        sys.exit(1)
