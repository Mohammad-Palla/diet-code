"""Models: one on the declared public surface, one unused helper class."""

__all__ = ["User"]


class User:
    def __init__(self, name: str) -> None:
        self.name = name

    @property
    def label(self) -> str:
        return self.name

    @staticmethod
    def _unused_static() -> str:
        return "dead"


class Draft:
    """Not in __all__ and never imported."""

    def summary(self) -> str:
        return "draft"
