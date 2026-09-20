# The contents of this file are pickled, so don't put values in the namespace
# that aren't pickleable (module imports are okay, they're removed automatically).
import os
import sys

sys.path.insert(0, os.path.join(os.path.abspath(os.path.dirname(__file__)), "_ext"))

# Add any Sphinx extension module names here, as strings. They can be extensions
# coming with Sphinx (named 'sphinx.ext.*') or your custom ones.
extensions = [
    "sphinx.ext.autodoc",
    "llms_txt",
]
