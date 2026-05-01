#!/usr/bin/env bash
# Build udoc Python wheels via cibuildwheel.
#
# Default: build wheels for the current platform. Pass --all-platforms
# to build everything cibuildwheel can handle on this host.
#
# Output lands in `dist/`. Verify locally with
#   pip install dist/udoc-*.whl
#   udoc --version
#   python -c "import udoc; print(udoc.__version__)"
#
# Requirements: cibuildwheel + Rust toolchain in the active environment.
#   pip install cibuildwheel
#   rustup default stable

set -e

ALL_PLATFORMS=0
if [ "${1:-}" = "--all-platforms" ]; then
    ALL_PLATFORMS=1
fi

if ! command -v cibuildwheel > /dev/null; then
    echo "FAIL: cibuildwheel not installed in the active Python environment."
    echo "    pip install cibuildwheel"
    exit 1
fi

if ! command -v cargo > /dev/null; then
    echo "FAIL: cargo not on PATH. Install Rust via https://rustup.rs/"
    exit 1
fi

mkdir -p dist
mkdir -p python/udoc/_bin

echo "==> Cleaning prior wheel artifacts in ./dist/"
rm -f dist/udoc-*.whl
ls dist/ 2>/dev/null | head -5

# Stage the host-built binary so a non-cibuildwheel run (or the macOS/
# Windows cibuildwheel run, which executes on the host) picks it up.
# Linux cibuildwheel rebuilds inside the manylinux container via the
# before-build hook in pyproject.toml.
echo "==> Building host udoc binary"
cargo build --release -p udoc --bin udoc
cp target/release/udoc python/udoc/_bin/udoc
chmod +x python/udoc/_bin/udoc

if [ "$ALL_PLATFORMS" = "1" ]; then
    echo "==> cibuildwheel (all platforms supported by this host)"
    cibuildwheel --output-dir dist
else
    echo "==> cibuildwheel (current platform only)"
    cibuildwheel --output-dir dist --platform "$(python -c 'import sys; print({"linux":"linux","darwin":"macos","win32":"windows"}[sys.platform])')"
fi

echo ""
echo "==> Wheels built:"
ls -la dist/udoc-*.whl 2>/dev/null || echo "(no wheels in dist/ -- check cibuildwheel output above for errors)"

echo ""
echo "==> Verify wheel metadata is dep-free:"
for whl in dist/udoc-*.whl; do
    [ -f "$whl" ] || continue
    deps=$(unzip -p "$whl" '*.dist-info/METADATA' 2>/dev/null | grep '^Requires-Dist:' || true)
    if [ -n "$deps" ]; then
        echo "WARN: $whl has runtime dependencies:"
        echo "$deps" | sed 's/^/    /'
    else
        echo "OK: $whl has no runtime deps (Requires-Dist empty)"
    fi
    # Sanity: the CLI binary should be inside.
    if unzip -l "$whl" 2>/dev/null | grep -q 'udoc/_bin/udoc'; then
        echo "OK: $whl ships the CLI binary"
    else
        echo "WARN: $whl is missing the CLI binary at udoc/_bin/udoc"
    fi
done

echo ""
echo "==> Next: install in a clean venv and run 'udoc --version'."
