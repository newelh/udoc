#!/bin/bash
# Debug hook: copies rendered page images to examples/hooks/tmp/<run-id>/
# for visual inspection of what hooks receive.
#
# Usage:
#   cargo run -p udoc -- document.pdf --hook examples/hooks/layout-debug.sh
#
# Then open examples/hooks/tmp/<run-id>/ to see the page PNGs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN_ID="run-$(date +%s)-$$"
OUT_DIR="$SCRIPT_DIR/../tmp/$RUN_ID"
mkdir -p "$OUT_DIR"

echo '{"protocol":"udoc-hook-v1","capabilities":["layout"],"needs":["image","spans"],"provides":["regions"]}'

while IFS= read -r line; do
  page=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('page_index','?'))" 2>/dev/null)
  img=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('image_path',''))" 2>/dev/null)
  spans=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d.get('spans',[])))" 2>/dev/null)

  if [ -n "$img" ] && [ -f "$img" ]; then
    cp "$img" "$OUT_DIR/page-${page}.png"
    echo "page $page: ${spans} spans -> $OUT_DIR/page-${page}.png" >&2
  else
    echo "page $page: no image" >&2
  fi

  echo '{"regions":[]}'
done

echo "renders saved to $OUT_DIR" >&2
