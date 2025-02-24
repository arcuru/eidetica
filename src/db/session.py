from sqlalchemy.orm import sessionmaker
import os

from .models import get_engine, get_session, init_db

# Create a session factory
engine = get_engine(os.getenv("DATABASE_URL", "sqlite:///eidetica.db"))
SessionLocal = sessionmaker(autocommit=False, autoflush=False, bind=engine)


def setup_database():
    """Initialize database connection and return a session."""
    init_db(engine)
    return get_session(engine)


def get_db():
    """Dependency to get database session"""
    db = SessionLocal()
    try:
        yield db
    finally:
        db.close()
