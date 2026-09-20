"""A live private class holding one method nothing calls.

`_unused_internal` is the only statement that would be removed from the class
body, and `_never_used` the only statement in its enclosing function: deleting
either mechanically would leave a syntax error.
"""


class _Registry:
    def __init__(self) -> None:
        self._items = []

    def add(self, item: str) -> None:
        self._items.append(item)

    def _unused_internal(self) -> int:
        return len(self._items)


def build() -> str:
    registry = _Registry()
    registry.add("one")
    return "built"


def _unused_top_level() -> str:
    def _never_used() -> str:
        return "dead"

    return "held"
