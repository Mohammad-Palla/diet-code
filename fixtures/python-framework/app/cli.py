"""Console script declared as `app-cli = "app.cli:main"` in pyproject.toml."""

from .plugins.loader import load_handler, load_plugins
from .routes import register


def main() -> int:
    register()
    load_plugins("alpha")
    load_handler("beta")
    return 0
