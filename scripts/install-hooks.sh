#!/bin/sh
# Install git hooks for udoc-pdf development
#
# Run this script once after cloning the repository:
#   ./scripts/install-hooks.sh

set -e

HOOKS_DIR=".git/hooks"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Check if we're in a git repository
if [ ! -d ".git" ]; then
    echo "Error: Not in a git repository root"
    exit 1
fi

# Install pre-commit hook
echo "Installing pre-commit hook..."
cp "$SCRIPT_DIR/pre-commit" "$HOOKS_DIR/pre-commit"
chmod +x "$HOOKS_DIR/pre-commit"

echo "✅ Git hooks installed successfully!"
echo ""
echo "The pre-commit hook will now run automatically before each commit."
echo "It checks:"
echo "  - Code formatting (cargo fmt)"
echo "  - Lint warnings (cargo clippy)"
echo "  - Test suite (cargo test)"
echo "  - Doc build (cargo doc)"
echo ""
echo "To bypass the hook (not recommended), use: git commit --no-verify"
