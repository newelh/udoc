"""Shared fixtures for the udoc Python test suite.

Resolves fixtures from the workspace root (`tests/corpus/`) so we don't
duplicate them under `python/tests/`. Skips tests if the fixture isn't
present.
"""

import os
import pathlib
import pytest


# Resolve workspace root from this file's location.
# python/tests/conftest.py -> python/tests -> python -> <root>.
_HERE = pathlib.Path(__file__).resolve().parent
WORKSPACE_ROOT = _HERE.parent.parent

CORPUS_ROOT = WORKSPACE_ROOT / "tests" / "corpus"


def _fixture(*parts) -> pathlib.Path:
    p = CORPUS_ROOT.joinpath(*parts)
    return p


def _require_fixture(*parts) -> pathlib.Path:
    p = _fixture(*parts)
    if not p.exists():
        pytest.skip(f"fixture missing: {p.relative_to(WORKSPACE_ROOT) if p.is_relative_to(WORKSPACE_ROOT) else p}")
    return p


@pytest.fixture(scope="session")
def hello_pdf() -> pathlib.Path:
    """The minimal hello-world PDF used as the smoke fixture."""
    return _require_fixture("minimal", "hello.pdf")


@pytest.fixture(scope="session")
def hundred_page_pdf() -> pathlib.Path:
    """100-page synthetic PDF for inspect perf + iteration tests ( carry-over)."""
    return _require_fixture("inspect-perf", "100page.pdf")


@pytest.fixture(scope="session")
def encrypted_pdf() -> pathlib.Path:
    """RC4-encrypted PDF requiring 'test123' user password."""
    return _require_fixture("..", "..", "crates", "udoc-pdf",
                           "tests", "corpus", "encrypted",
                           "rc4_128_user_password.pdf")


@pytest.fixture(scope="session")
def realworld_dir() -> pathlib.Path:
    """The 12-format realworld rosetta corpus directory."""
    return _require_fixture("realworld")


@pytest.fixture(scope="session")
def docx_fixture() -> pathlib.Path:
    return _require_fixture("..", "..", "crates", "udoc-docx",
                           "tests", "corpus", "real-world",
                           "pandoc_lists_compact.docx")


@pytest.fixture
def hello_pdf_bytes(hello_pdf: pathlib.Path) -> bytes:
    return hello_pdf.read_bytes()
