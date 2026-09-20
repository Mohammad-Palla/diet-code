"""Loads modules by computed name; neither target is statically resolvable."""

import importlib


def load_plugin(name: str):
    return importlib.import_module(f"plugins.{name}").activate()


def load_handler(name: str):
    # Concatenation and the `__import__` builtin rather than interpolation.
    module = __import__("handlers." + name, globals(), locals(), ["handle"])
    return module.handle()
