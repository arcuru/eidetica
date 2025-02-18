import os
from .models import get_engine, get_session, init_db


def setup_database():
    """Initialize database connection and return a session."""
    database_url = os.getenv("DATABASE_URL", "sqlite:///eidetica.db")
    engine = get_engine(database_url)
    init_db(engine)
    return get_session(engine)
