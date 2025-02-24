"""Database management commands."""

from typing import Optional, List
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


def get_database_by_name(
    user_id: int, dbname: str, session, folder_name: str = None
) -> Optional[Database]:
    """Get database by name for a specific user

    Args:
        user_id: User ID
        dbname: Database name
        session: Database session
        folder_name: Optional folder name to filter by

    Returns:
        Database object or None if not found
    """
    query = (
        session.query(Database)
        .join(Folder)
        .filter(Folder.user_id == user_id, Database.name == dbname)
    )

    if folder_name:
        query = query.filter(Folder.name == folder_name)

    return query.first()


def validate_dbname_uniqueness(folder_id: int, dbname: str, session) -> bool:
    """Validate that a database name is unique within a folder"""
    existing = (
        session.query(Database).filter_by(folder_id=folder_id, name=dbname).first()
    )
    return existing is None


def list_databases(
    folder_name: str, user_id: int, session, format: str = "plain"
) -> List[Database]:
    """List databases in a folder"""
    folder = session.query(Folder).filter_by(user_id=user_id, name=folder_name).first()
    if not folder:
        print(colored(f"Folder '{folder_name}' not found", "red"))
        return []

    databases = folder.databases
    if not databases:
        print(colored("No databases found", "yellow"))
        return []

    if format != "plain":
        data = {
            db.id: {"name": db.name, "created_at": db.created_at} for db in databases
        }
        print(format_output(data, format))

    return list(databases)  # Convert to list to ensure it's not a SQLAlchemy collection


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
    dbname: str, user_id: int, session, format: str = "plain", folder_name: str = None
) -> Optional[dict]:
    """Get database connection info"""
    database = get_database_by_name(user_id, dbname, session, folder_name)
    if not database:
        print(colored(f"Database '{dbname}' not found", "red"))
        return None

    data = {
        "name": database.name,
        "username": database.username,
        "password": database.password,
        "connection_url": f"postgresql://{database.username}:{database.password}@localhost/{database.name}",
        "created_at": database.created_at,
    }
    print(format_output(data, format))
    return data


def reset_database_password(
    dbname: str, user_id: int, session, force: bool = False, folder_name: str = None
) -> bool:
    """Reset database password

    Args:
        dbname: Database name
        user_id: User ID
        session: Database session
        force: Skip confirmation prompt if True
        folder_name: Optional folder name to ensure correct database is selected

    Returns:
        True if successful, False otherwise
    """
    if not force and not confirm_action(
        f"Are you sure you want to reset password for database '{dbname}'?"
    ):
        print(colored("Password reset cancelled", "yellow"))
        return False

    database = get_database_by_name(user_id, dbname, session, folder_name)
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
    dbname: str,
    user_id: int,
    session,
    force: bool = False,
    dry_run: bool = False,
    folder_name: str = None,
) -> bool:
    """Delete a database

    Args:
        dbname: Database name
        user_id: User ID
        session: Database session
        force: Skip confirmation prompt if True
        dry_run: Don't actually delete if True
        folder_name: Optional folder name to ensure correct database is deleted

    Returns:
        True if successful, False otherwise
    """
    with open("/tmp/eidetica_debug.log", "a") as f:
        f.write(
            f"[DEBUG] delete_database called with dbname={dbname}, user_id={user_id}, folder_name={folder_name}\n"
        )

    if not force and not confirm_action(
        f"Are you sure you want to delete database '{dbname}'?"
    ):
        print(colored("Deletion cancelled", "yellow"))
        return False

    database = get_database_by_name(user_id, dbname, session, folder_name)
    if not database:
        print(colored(f"Database '{dbname}' not found", "red"))
        return False

    if dry_run:
        print(colored(f"Would delete database '{dbname}'", "blue"))
        return True

    try:
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(f"[DEBUG] Starting PostgreSQL operations for database deletion\n")
            f.write(f"[DEBUG] Database name: {dbname}\n")
            f.write(f"[DEBUG] Database username: {database.username}\n")
            f.write(f"[DEBUG] Database ID: {database.id}\n")
            f.write(f"[DEBUG] Database folder_id: {database.folder_id}\n")

        # Check if POSTGRES_URL is set
        db_url = getenv("POSTGRES_URL")
        if not db_url:
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[ERROR] POSTGRES_URL environment variable not set\n")
            raise ValueError("POSTGRES_URL environment variable is not set")

        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(f"[DEBUG] POSTGRES_URL: {db_url}\n")

        try:
            # Connect to postgres database to avoid being connected to the db we're dropping
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(
                    f"[DEBUG] Connecting to PostgreSQL with URL: {db_url}/postgres\n"
                )

            conn = psycopg2.connect(db_url + "/postgres")
            conn.autocommit = True
            cursor = conn.cursor()

            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(
                    "[DEBUG] Connected to PostgreSQL (postgres db), attempting DROP DATABASE\n"
                )

            # Terminate any existing connections to the database
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(
                    f"[DEBUG] Terminating existing connections to database: {dbname}\n"
                )

            cursor.execute(
                sql.SQL(
                    """
                    SELECT pg_terminate_backend(pg_stat_activity.pid)
                    FROM pg_stat_activity
                    WHERE pg_stat_activity.datname = %s
                    AND pid <> pg_backend_pid()
                """
                ),
                [dbname],
            )

            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[DEBUG] Terminated existing connections\n")

            # Now drop the database
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(f"[DEBUG] Executing DROP DATABASE IF EXISTS {dbname}\n")

            cursor.execute(
                sql.SQL("DROP DATABASE IF EXISTS {}").format(sql.Identifier(dbname))
            )

            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[DEBUG] Database dropped successfully, attempting DROP USER\n")

            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(f"[DEBUG] Executing DROP USER {database.username}\n")

            cursor.execute(
                sql.SQL("DROP USER {}").format(sql.Identifier(database.username))
            )

            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[DEBUG] User dropped successfully\n")

            cursor.close()
            conn.close()

            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write("[DEBUG] PostgreSQL connection closed\n")
        except Exception as pg_error:
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(f"[ERROR] PostgreSQL error: {str(pg_error)}\n")
                f.write("[DEBUG] Continuing with local database deletion anyway\n")

        # Delete from local database regardless of PostgreSQL success
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write("[DEBUG] Deleting database record from local DB\n")
            f.write(f"[DEBUG] Database object before deletion: {database}\n")
            f.write(f"[DEBUG] Session object: {session}\n")

        try:
            session.delete(database)
            session.commit()
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(
                    "[DEBUG] Database deletion from local DB completed successfully\n"
                )
        except Exception as db_error:
            with open("/tmp/eidetica_debug.log", "a") as f:
                f.write(f"[ERROR] Error deleting from local DB: {str(db_error)}\n")
            session.rollback()
            raise

        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write("[DEBUG] Database deletion completed successfully\n")

        return True

    except Exception as e:
        with open("/tmp/eidetica_debug.log", "a") as f:
            f.write(f"[ERROR] Exception during database deletion: {str(e)}\n")
        print(colored(f"Error deleting database: {e}", "red"))
        return False


def update_database(
    old_name: str,
    new_name: str,
    username: str,
    folder_name: str,
    session,
) -> bool:
    """Update a database's name and metadata"""
    try:
        # Get the database record
        database = (
            session.query(Database)
            .join(Folder)
            .filter(Database.name == old_name, Folder.name == folder_name)
            .first()
        )
        if not database:
            print(
                colored(
                    f"Database '{old_name}' not found in folder '{folder_name}'", "red"
                )
            )
            return False

        # Update PostgreSQL database name
        db_url = getenv("POSTGRES_URL")
        if not db_url:
            raise ValueError("POSTGRES_URL environment variable is not set")

        conn = psycopg2.connect(db_url)
        conn.autocommit = True
        cursor = conn.cursor()

        cursor.execute(
            sql.SQL("ALTER DATABASE {} RENAME TO {}").format(
                sql.Identifier(old_name), sql.Identifier(new_name)
            )
        )

        cursor.close()
        conn.close()

        # Update local database record
        database.name = new_name
        database.username = username
        session.commit()

        print(colored(f"Database '{old_name}' updated to '{new_name}'", "green"))
        return True

    except Exception as e:
        print(colored(f"Error updating database: {e}", "red"))
        return False


def search_databases(
    user_id: int, query: str, session, format: str = "plain"
) -> List[Database]:
    """Search databases by name across all folders"""
    databases = (
        session.query(Database)
        .join(Folder)
        .filter(Folder.user_id == user_id, Database.name.ilike(f"%{query}%"))
        .all()
    )

    if format != "plain":
        if not databases:
            print(colored("No matching databases found", "yellow"))
        else:
            data = {
                db.id: {"name": db.name, "created_at": db.created_at}
                for db in databases
            }
            print(format_output(data, format))

    return databases


def handle_database_commands(args, session):
    """Handle database commands"""
    user = session.query(User).filter_by(username=args.username).first()
    if not user:
        print(colored(f"Error: User '{args.username}' not found", "red"))
        return

    try:
        if args.database_command == "list":
            databases = list_databases(args.folder, user.id, session, args.format)
            if args.format == "plain" and databases:
                # Print in plain format if not already printed in another format
                for db in databases:
                    print(f"Name: {db.name}")
                    print(f"Created: {db.created_at}")
                    print("---")
        elif args.database_command == "create":
            create_database(args.folder, args.dbname, user.id, session)
        elif args.database_command == "info":
            # For info command, use the folder if provided
            folder_name = getattr(args, "folder", None)
            get_database_info(args.dbname, user.id, session, args.format, folder_name)
        elif args.database_command == "reset-password":
            # For reset-password command, use the folder if provided
            folder_name = getattr(args, "folder", None)
            reset_database_password(
                args.dbname, user.id, session, args.force, folder_name
            )
        elif args.database_command == "delete":
            # For delete command, use the folder if provided
            folder_name = getattr(args, "folder", None)
            delete_database(
                args.dbname, user.id, session, args.force, args.dry_run, folder_name
            )
    except Exception as e:
        print(colored(f"Error: {str(e)}", "red"))
        sys.exit(1)
