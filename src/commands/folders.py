from src.db import User, Folder, Database


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


def create_database(folder_id, name, session):
    import secrets
    import string
    import psycopg2
    from psycopg2 import sql
    from pathlib import Path
    from os import getenv

    if not name:
        print("Error: Database name cannot be empty")
        return False

    # Generate credentials
    username = "".join(secrets.choice(string.ascii_lowercase) for _ in range(12))
    password = "".join(
        secrets.choice(string.ascii_letters + string.digits) for _ in range(16)
    )

    # Create database in PostgreSQL
    try:
        # Get database URL from environment variable
        db_url = getenv("POSTGRES_URL")
        if not db_url:
            raise ValueError("POSTGRES_URL environment variable is not set")

        # Connect to PostgreSQL server using connection string
        conn = psycopg2.connect(db_url)
        conn.autocommit = True
        cursor = conn.cursor()

        # Create database
        cursor.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))

        # Create user and grant privileges
        cursor.execute(
            sql.SQL("CREATE USER {} WITH PASSWORD %s").format(sql.Identifier(username)),
            [password],
        )
        cursor.execute(
            sql.SQL("GRANT ALL PRIVILEGES ON DATABASE {} TO {}").format(
                sql.Identifier(name), sql.Identifier(username)
            )
        )

        cursor.close()
        conn.close()
    except Exception as e:
        print(f"Error creating database: {e}")
        return False

    # Store credentials in local database
    database = Database(
        name=name, username=username, password=password, folder_id=folder_id
    )
    session.add(database)
    session.commit()

    print(f"Database '{name}' created successfully")
    print(f"Username: {username}")
    print(f"Password: {password}")
    return True


def handle_folder_commands(args, session):
    user = session.query(User).filter_by(username=args.username).first()
    if not user:
        print(f"Error: User '{args.username}' not found")
        return

    if args.folder_command == "list":
        list_folders(user.id, session)
    elif args.folder_command == "create":
        create_folder(user.id, args.name, session)
    elif args.folder_command == "delete":
        delete_folder(user.id, args.folder_id, session)
    elif args.folder_command == "rename":
        rename_folder(user.id, args.folder_id, args.new_name, session)
    elif args.folder_command == "create-db":
        if not args.folder_id:
            print("Error: Folder ID is required for database creation")
            return
        create_database(args.folder_id, args.name, session)
