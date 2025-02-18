from src.db import User, Folder


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
