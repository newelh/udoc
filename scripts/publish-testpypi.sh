#!/usr/bin/env bash
# Manual TestPyPI / PyPI publish for udoc.
#
# Usually you do not run this — `release.yml` on a `v*` tag push handles
# the automated publish via PyPI trusted publishing. Use this script
# only for one-off manual rehearsals or recovery scenarios. Two guards
# fire before `maturin publish` actually runs:
#
#   1. Ownership check. Type the project name to confirm you have
#      verified ownership on the target index.
#
#   2. CONFIRM prompt. Belt-and-suspenders against accidental publishes.
#
# Requirements: maturin installed in the active venv, plus a PyPI /
# TestPyPI token configured in ~/.pypirc or via MATURIN_PYPI_TOKEN env
# var. See: https://www.maturin.rs/distribution.html

set -e

if ! command -v maturin > /dev/null; then
    echo "FAIL: maturin not installed in the active Python environment."
    echo "    pip install maturin>=1.5"
    exit 1
fi

# Default repository is testpypi. Pass --pypi to push to real PyPI
# (which you should ONLY do once testpypi rehearsal is clean).
REPO="testpypi"
if [ "${1:-}" = "--pypi" ]; then
    REPO="pypi"
fi

echo "================================================================"
echo "udoc PyPI publish (target: $REPO)"
echo "================================================================"
echo ""
echo "Pre-flight checks:"
echo ""
echo "1. Have you verified you OWN the 'udoc' project on $REPO?"
if [ "$REPO" = "testpypi" ]; then
    echo "   Visit: https://test.pypi.org/manage/project/udoc/"
else
    echo "   Visit: https://pypi.org/manage/project/udoc/"
fi
echo ""
echo "   To confirm ownership, type the dist name 'udoc':"
echo ""
read -r -p "   dist name: " DIST
if [ "$DIST" != "udoc" ]; then
    echo ""
    echo "ABORT: did not type 'udoc'. Verify ownership and re-run."
    exit 2
fi

echo ""
echo "2. Have you built a fresh wheel for the platform you're publishing?"
echo "   Wheels expected in ./dist/udoc-*.whl (run scripts/build-wheels.sh"
echo "   if needed)."
echo ""
ls -la dist/udoc-*.whl 2>/dev/null || {
    echo "ABORT: no wheels in dist/. Run scripts/build-wheels.sh first."
    exit 3
}
echo ""
echo "3. Final confirmation. Type CONFIRM to proceed with the upload"
echo "   to $REPO (or anything else to abort):"
echo ""
read -r -p "   confirm: " CONFIRM
if [ "$CONFIRM" != "CONFIRM" ]; then
    echo ""
    echo "ABORT: did not type CONFIRM. No upload performed."
    exit 4
fi

echo ""
echo "==> maturin publish --repository $REPO --skip-existing"
maturin publish --repository "$REPO" --skip-existing

echo ""
echo "OK: upload to $REPO complete. Verify the project page reflects"
echo "    the new wheel(s):"
if [ "$REPO" = "testpypi" ]; then
    echo "    https://test.pypi.org/project/udoc/"
else
    echo "    https://pypi.org/project/udoc/"
fi
echo ""
echo "Next: install in a clean venv and import udoc to smoke."
