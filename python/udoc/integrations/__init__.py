"""udoc.integrations -- pure-Python sub-package for third-party-dep integrations.

Each integration lives as a sibling module (`pandas`, `arrow`, ...) and pulls
in its dep via the matching `pip install udoc[<name>]` extra declared in
`pyproject.toml [project.optional-dependencies]`. The core `udoc` import has
zero third-party runtime deps.
"""
