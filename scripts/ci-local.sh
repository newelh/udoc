#!/bin/sh
# Run the full CI pipeline locally.
#
# This mirrors .github/workflows/ci.yml exactly. Use this when GitHub CI
# is unavailable or to validate before pushing.
#
# Usage:
#   ./scripts/ci-local.sh          # Run all checks
#   ./scripts/ci-local.sh --quick  # Skip fuzz and deny (pre-commit only)

set -e

QUICK=0
if [ "$1" = "--quick" ]; then
    QUICK=1
fi

PASS=0
FAIL=0
SKIP=0

run_step() {
    STEP_NAME="$1"
    shift
    echo ""
    echo "=== $STEP_NAME ==="
    if "$@"; then
        PASS=$((PASS + 1))
        echo "--- $STEP_NAME: OK ---"
    else
        FAIL=$((FAIL + 1))
        echo "--- $STEP_NAME: FAILED ---"
    fi
}

skip_step() {
    STEP_NAME="$1"
    REASON="$2"
    SKIP=$((SKIP + 1))
    echo ""
    echo "=== $STEP_NAME: SKIPPED ($REASON) ==="
}

# 1. Format check
run_step "fmt" cargo fmt --check --all

# 2. Clippy (matches CI: RUSTFLAGS=-D warnings + clippy -D warnings)
run_step "clippy" cargo clippy --workspace --all-targets --all-features -- -D warnings

# 3. Tests
run_step "test" cargo test --workspace --all-features

# 4. Doc build
RUSTDOCFLAGS="-D warnings" run_step "doc" cargo doc --workspace --no-deps --all-features --quiet

# 5. Fuzz compile check (requires nightly)
if [ "$QUICK" = "1" ]; then
    skip_step "fuzz-check" "--quick mode"
elif [ -d "fuzz" ] && rustup run nightly rustc --version >/dev/null 2>&1; then
    run_step "fuzz-check" cargo +nightly check --manifest-path fuzz/Cargo.toml
else
    skip_step "fuzz-check" "nightly toolchain not installed or fuzz/ not found"
fi

# 6. cargo-deny (requires cargo-deny installed)
if [ "$QUICK" = "1" ]; then
    skip_step "deny" "--quick mode"
elif command -v cargo-deny >/dev/null 2>&1; then
    run_step "deny" cargo deny check
else
    skip_step "deny" "cargo-deny not installed (cargo install cargo-deny)"
fi

# Summary
echo ""
echo "================================"
echo "  $PASS passed, $FAIL failed, $SKIP skipped"
echo "================================"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
