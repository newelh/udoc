"""udoc -- document extraction toolkit.

This package re-exports the native cdylib symbols. Pure-Python
integrations (pandas, etc) live under `udoc.integrations.*` and pull
in their deps via extras: `pip install udoc-lib[pandas]`.
"""

from udoc.udoc import *  # noqa: F401,F403 -- cdylib re-export
from udoc.udoc import __version__  # noqa: F401

# : integrations sub-package is the third-party-dep extension surface.
# Imports are NOT done here -- users explicitly `from udoc.integrations.pandas
# import to_dataframe`. Keeps the core import dep-free.
