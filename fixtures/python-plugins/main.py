"""Application entry point: no packaging metadata, so normal rules apply."""

from loader import load_handler, load_plugin

if __name__ == "__main__":
    print(load_plugin("alpha"), load_handler("beta"))
