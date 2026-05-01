#!/usr/bin/env python3
"""Rewrite README.md links so it can serve as docs/index.md for mkdocs.

The README links to repo-relative paths that work on github.com but
404 on the docs site:

  [CLI](docs/cli.md)            -- becomes /docs/cli.html under docs site
  [Security](SECURITY.md)       -- has no docs analogue at all
  [Apache](LICENSE-APACHE)      -- repo-only file

This script translates each pattern to the form that resolves under
the rendered docs site. Run as `cat README.md | render... > docs/index.md`.
"""

from __future__ import annotations

import re
import sys

# Top-level repo files that exist on github but not in the docs tree.
# Keep them visible by linking to the github copy.
REPO_BASE = "https://github.com/newelh/udoc/blob/main/"
ABSOLUTE_LINKS = {
    "SECURITY.md",
    "CONTRIBUTING.md",
    "CHANGELOG.md",
    "LICENSE-APACHE",
    "LICENSE-MIT",
}


def rewrite(body: str) -> str:
    # `docs/X.md` (with optional fragment) -> `X.md` -- mkdocs reads
    # docs/index.md from inside docs/, so docs-relative paths drop
    # the docs/ prefix.
    body = re.sub(
        r"\]\(docs/([^)#]+?\.md)(#[^)]*)?\)",
        lambda m: f"]({m.group(1)}{m.group(2) or ''})",
        body,
    )
    # `docs/formats/` (trailing-slash dir link) -> `formats/index.md`
    body = re.sub(
        r"\]\(docs/([^)]*?)/\)",
        lambda m: f"]({m.group(1)}/index.md)",
        body,
    )

    # Top-level repo files that don't live under docs/. Convert to
    # absolute github URLs so the rendered link still resolves.
    for fname in ABSOLUTE_LINKS:
        body = re.sub(
            rf"\]\({re.escape(fname)}\)",
            f"]({REPO_BASE}{fname})",
            body,
        )

    return body


def main() -> int:
    sys.stdout.write(rewrite(sys.stdin.read()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
