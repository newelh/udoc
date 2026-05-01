#!/usr/bin/env bash
# tesseract-ocr.sh -- OCR hook for udoc wrapping Tesseract (TSV mode).
#
# Prerequisites: bash, jq, tesseract (>= 4.0 for TSV output)
# Usage: udoc scanned.pdf --ocr ./tesseract-ocr.sh
#
# Protocol: reads JSONL page requests from stdin, writes JSONL
# responses to stdout. Each request has an "image_path" field with
# the path to a page image and a "dpi" field with the render DPI.
# Responds with per-word text spans with bounding boxes in points.
#
# Coordinate system: bbox is [x_min, y_min, x_max, y_max] in points
# (72 points = 1 inch), top-left origin (y-down), matching the
# udoc hook wire format.

set -euo pipefail

# Dependency check
for dep in jq tesseract; do
    if ! command -v "$dep" &>/dev/null; then
        echo "tesseract-ocr.sh: missing dependency: $dep" >&2
        exit 1
    fi
done

# Handshake
echo '{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["image"],"provides":["spans"]}'

while IFS= read -r line; do
    image=$(echo "$line" | jq -r '.image_path // empty')
    dpi=$(echo "$line" | jq -r '.dpi // 150')

    if [ -z "$image" ] || [ ! -f "$image" ]; then
        echo '{"spans":[]}'
        continue
    fi

    # Run tesseract in TSV mode with auto page segmentation (--psm 1).
    # TSV columns: level page_num block_num par_num line_num word_num
    #              left top width height conf text
    tsv=$(tesseract "$image" stdout --psm 1 tsv 2>/dev/null || echo "")

    if [ -z "$tsv" ]; then
        echo '{"spans":[]}'
        continue
    fi

    # Parse TSV and build spans JSON.
    # Skip the header row, skip conf<0 (empty block markers), skip conf<30 (junk).
    # Convert pixel coords to points: pts = pixels * 72 / dpi
    spans=$(echo "$tsv" | awk -v dpi="$dpi" '
        BEGIN { ORS=""; first=1 }
        NR == 1 { next }  # skip header
        {
            level=$1; conf=$11; text=$12
            left=$7; top=$8; w=$9; h=$10

            # skip empty markers (conf == -1) and low-confidence words
            if (conf+0 < 0) next
            if (conf+0 < 30) next
            # skip rows with no text
            if (text == "") next

            # convert pixels -> points
            x_min = left  * 72.0 / dpi
            y_min = top   * 72.0 / dpi
            x_max = (left + w) * 72.0 / dpi
            y_max = (top  + h) * 72.0 / dpi

            if (!first) printf ","
            first = 0
            printf "{\"text\":%s,\"bbox\":[%.3f,%.3f,%.3f,%.3f]}",
                   "\"" text "\"", x_min, y_min, x_max, y_max
        }
    ')

    echo "{\"spans\":[${spans}]}"
done
