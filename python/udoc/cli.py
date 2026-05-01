"""udoc CLI entry point.

The udoc wheel ships the Rust CLI binary at ``udoc/_bin/udoc``
(``udoc.exe`` on Windows). This shim is the target of the
``[project.scripts] udoc = "udoc.cli:main"`` entry in ``pyproject.toml``.
On Unix it ``execv``s the binary so signal handling and process
ownership are clean; on Windows it falls through ``subprocess``.

Source builds (``maturin develop``) without the binary copied in fall
back with a clear error pointing at the build instructions, so a
developer working only on the Python surface never gets a confusing
``ModuleNotFoundError``.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path


def _binary_path() -> Path:
    name = "udoc.exe" if sys.platform == "win32" else "udoc"
    return Path(__file__).resolve().parent / "_bin" / name


def main() -> int:
    binary = _binary_path()
    if not binary.exists():
        sys.stderr.write(
            "udoc: bundled CLI binary not found.\n"
            f"  expected: {binary}\n"
            "This wheel was built without the CLI binary copied in.\n"
            "From a source checkout, run:\n"
            "    cargo build --release -p udoc --bin udoc\n"
            "    mkdir -p python/udoc/_bin\n"
            "    cp target/release/udoc python/udoc/_bin/udoc\n"
            "    maturin develop --release\n"
        )
        return 1

    # Ensure the binary is executable. Wheel builds usually preserve
    # the +x bit, but some packaging paths strip it; restore it here
    # idempotently rather than failing.
    if sys.platform != "win32" and not os.access(binary, os.X_OK):
        try:
            binary.chmod(binary.stat().st_mode | 0o111)
        except OSError:
            pass

    args = [str(binary), *sys.argv[1:]]
    if sys.platform == "win32":
        # execv on Windows has weird semantics around process
        # parents and Ctrl+C. subprocess is the well-behaved path.
        return subprocess.call(args)
    # Unix: replace this Python process entirely.
    os.execv(args[0], args)
    return 0  # unreachable


if __name__ == "__main__":
    sys.exit(main() or 0)
