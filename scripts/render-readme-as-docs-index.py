#!/usr/bin/env python3
"""Rewrite README.md links so it can serve as docs/index.md for mkdocs.

The README links to repo-relative paths that work on github.com but
404 on the docs site:

  [CLI](docs/cli.md)            -- becomes /docs/cli.html under docs site
  [Security](SECURITY.md)       -- has no docs analogue at all
  [Apache](LICENSE-APACHE)      -- repo-only file

This script translates each pattern to the form that resolves under
the rendered docs site. Run as `cat README.md | render... > docs/index.md`.

It also strips github-readme boilerplate sections (Status, Security,
Contributing, Licence) when generating the docs site Overview. Those
are conventional README sections on github.com but pad the Overview
without adding much for docs readers; the same content lives in
SECURITY.md / CONTRIBUTING.md / LICENSE-* and the architecture/security
page.
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

# H2 headings whose section (heading + body, up to the next H2 / EOF)
# gets dropped from the docs index. github.com still shows them.
DROP_SECTIONS = {
    "Status",
    "Security",
    "Contributing",
    "Licence",
    "License",
}

# Standalone lines that point at the hosted docs from github.com but
# are pointless on the docs site itself.
DROP_LINE_PATTERNS = [
    re.compile(r"^The full hosted manual lives at .+\.\s*$"),
]


def strip_sections(body: str) -> str:
    out: list[str] = []
    skipping = False
    for line in body.splitlines(keepends=True):
        m = re.match(r"^## +(.+?)\s*$", line)
        if m:
            skipping = m.group(1).strip() in DROP_SECTIONS
            if skipping:
                continue
        if not skipping:
            if any(p.match(line) for p in DROP_LINE_PATTERNS):
                continue
            out.append(line)
    return "".join(out)


def rewrite(body: str) -> str:
    body = strip_sections(body)

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
