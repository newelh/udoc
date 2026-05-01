#!/bin/bash
# Downloads extended PDF corpus for deep testing.
#
# This fetches PDFs from pdfium and pdf.js test suites into
# tests/corpus/extended/ (gitignored). Gate tests with:
#   UDOC_EXTENDED_CORPUS=1 cargo test extended
#
# Usage: ./tests/download-extended-corpus.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="$SCRIPT_DIR/corpus/extended"

echo "Downloading extended PDF corpus to $DEST"

# -- pdfium --
PDFIUM_DIR="$DEST/pdfium"
if [ -d "$PDFIUM_DIR" ]; then
    echo "pdfium corpus already exists, skipping"
else
    echo "Cloning pdfium test resources (sparse checkout)..."
    mkdir -p "$DEST"
    git clone --filter=blob:none --sparse --depth=1 \
        https://pdfium.googlesource.com/pdfium \
        "$DEST/pdfium-repo"
    (cd "$DEST/pdfium-repo" && git sparse-checkout set testing/resources)
    ln -snf pdfium-repo/testing/resources "$PDFIUM_DIR"
    echo "pdfium: done"
fi

# -- pdf.js --
PDFJS_DIR="$DEST/pdfjs"
if [ -d "$PDFJS_DIR" ]; then
    echo "pdf.js corpus already exists, skipping"
else
    echo "Cloning pdf.js test PDFs (sparse checkout)..."
    mkdir -p "$DEST"
    git clone --filter=blob:none --sparse --depth=1 \
        https://github.com/mozilla/pdf.js \
        "$DEST/pdfjs-repo"
    (cd "$DEST/pdfjs-repo" && git sparse-checkout set test/pdfs)
    ln -snf pdfjs-repo/test/pdfs "$PDFJS_DIR"
    echo "pdf.js: done"
fi

echo ""
echo "Extended corpus ready. Run tests with:"
echo "  UDOC_EXTENDED_CORPUS=1 cargo test extended"
