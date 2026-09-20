"""Handlers registered by decoration, never called by name."""

from .models import User

ROUTES = {}


def route(path):
    def decorate(fn):
        ROUTES[path] = fn
        return fn

    return decorate


def register() -> None:
    """Decorators above have already populated ROUTES at import time."""
    return None


@route("/users")
def list_users() -> list:
    return [User("ada")]


@route("/health")
def health() -> str:
    return "ok"
