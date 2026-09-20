#!/usr/bin/env python3
"""Application entry point."""

from pkg.helpers import format_name
from pkg.service import Service


def run() -> None:
    service = Service()
    print(service.describe(), format_name("diet"))


if __name__ == "__main__":
    run()
