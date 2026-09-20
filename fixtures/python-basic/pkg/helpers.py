"""Helpers: one public entry, one live private, one dead private."""


def format_name(value: str) -> str:
    return _titlecase(value)


def _titlecase(value: str) -> str:
    return value.title()


def _unused_private(value: str) -> str:
    return value.lower()
