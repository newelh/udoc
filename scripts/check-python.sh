#!/usr/bin/env bash
# check-python.sh -- Local Python gate for udoc-py.
#
# Builds the udoc-py extension via `maturin develop --release` into the
# active Python environment, then runs:
#   - pytest python/tests/      (the W3-PYTEST suite -- 50+ tests once landed)
#   - mypy --strict python/tests/   (the W2-STUBS reflection check + type
#                                    annotations on the test suite)
#
# This is the local equivalent of the GitHub Actions Python job that does
# not exist on this repo (DEC: no Actions billing). Wired into the verify-
# report at sprint close and into `scripts/pre-commit` is NOT done by
# design -- maturin develop --release adds 30-60s per commit which would
# blow the pre-commit budget. Run this manually before pushing or as part
# of `scripts/ci-local.sh` for the workspace-wide check.
#
# Requirements: a Python 3.10+ venv with maturin, pytest, mypy installed.
# If you don't have one, set it up with:
#
#   python -m venv .venv
#   source .venv/bin/activate
#   pip install maturin>=1.5 pytest mypy
#   # Plus optional dev deps for the integrations tests:
#   pip install pandas pyarrow

set -e

if ! command -v maturin > /dev/null; then
    echo "FAIL: maturin not installed in the active Python environment."
    echo "  Activate a venv with maturin installed, or:"
    echo "    pip install maturin>=1.5"
    exit 1
fi

if ! command -v pytest > /dev/null; then
    echo "FAIL: pytest not installed in the active Python environment."
    echo "    pip install pytest"
    exit 1
fi

if ! command -v mypy > /dev/null; then
    echo "FAIL: mypy not installed in the active Python environment."
    echo "    pip install mypy"
    exit 1
fi

echo "==> maturin develop --release"
maturin develop --release

echo "==> pytest python/tests/"
if [ -d python/tests ]; then
    pytest python/tests/ -v
else
    echo "(skipped: python/tests/ does not exist yet; W3-PYTEST will populate it)"
fi

echo "==> mypy --strict python/tests/"
if [ -d python/tests ]; then
    mypy --strict python/tests/
else
    echo "(skipped: python/tests/ does not exist yet)"
fi

echo "OK: Python gate clean."
