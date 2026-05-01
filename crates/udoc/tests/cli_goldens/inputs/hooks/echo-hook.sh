#!/bin/sh
# Deterministic no-op hook for CLI golden tests.
# Reads JSONL on stdin and emits an empty annotation list per record.
# Skips OCR processing entirely; pages with text remain unchanged.
while IFS= read -r line; do
    case "$line" in
        *'"phase":"start"'*) ;;
        *'"phase":"end"'*) ;;
        *) printf '{"annotations":[]}\n' ;;
    esac
done
