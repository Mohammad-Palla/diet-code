"""Loads modules by computed name: nothing here is statically resolvable."""

import importlib


def load_plugins(name: str):
    module = importlib.import_module(f"app.plugins.{name}")
    return module.activate()


def load_handler(name: str):
    # Concatenation rather than interpolation, and the `__import__` builtin
    # rather than importlib: the static prefix is still `app.handlers`.
    module = __import__("app.handlers." + name, globals(), locals(), ["handle"])
    return module.handle()
