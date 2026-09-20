"""Service class reached from the entry point."""


class Service:
    def __init__(self) -> None:
        self._label = "service"

    def describe(self) -> str:
        return self._format()

    def _format(self) -> str:
        return self._label

    def _never_called(self) -> str:
        return "dead"


class UnusedService:
    def describe(self) -> str:
        return "unused"
