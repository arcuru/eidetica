from sqlalchemy import Column, Integer, String, ForeignKey, DateTime
from sqlalchemy.ext.declarative import declarative_base
from sqlalchemy.orm import relationship, sessionmaker
from sqlalchemy import create_engine
from datetime import datetime
import bcrypt

Base = declarative_base()


class User(Base):
    __tablename__ = "users"

    id = Column(Integer, primary_key=True)
    username = Column(String, unique=True, nullable=False)
    password_hash = Column(String, nullable=False)
    folders = relationship(
        "Folder", back_populates="user", cascade="all, delete-orphan"
    )

    def set_password(self, password):
        password_bytes = password.encode("utf-8")
        salt = bcrypt.gensalt()
        self.password_hash = bcrypt.hashpw(password_bytes, salt).decode("utf-8")

    def check_password(self, password):
        password_bytes = password.encode("utf-8")
        hash_bytes = self.password_hash.encode("utf-8")
        return bcrypt.checkpw(password_bytes, hash_bytes)


class Folder(Base):
    __tablename__ = "folders"

    id = Column(Integer, primary_key=True)
    name = Column(String, nullable=False)
    created_at = Column(DateTime, default=datetime.utcnow)
    user_id = Column(Integer, ForeignKey("users.id"), nullable=False)
    user = relationship("User", back_populates="folders")


def get_engine(database_url):
    return create_engine(database_url)


def get_session(engine):
    Session = sessionmaker(bind=engine)
    return Session()


def init_db(engine):
    Base.metadata.create_all(engine)
