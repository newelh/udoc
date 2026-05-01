#!/usr/bin/env bash
# Pipe-friendly udoc CLI recipes.
#
# Run any of these directly. None require special config.

set -euo pipefail

# Plain text grep across a PDF.
udoc report.pdf | grep -i 'revenue'

# Extract metadata title from every PDF in a directory.
udoc -J docs/*.pdf | jq -r '.metadata.title // empty'

# Tables only, paginated by sheet for an XLSX, then count rows.
udoc -t big.xlsx | wc -l

# Streaming JSONL into duckdb for ad-hoc analysis.
udoc -J ingest.pdf \
  | duckdb -c "COPY (SELECT * FROM read_json_auto('/dev/stdin')) TO 'pages.parquet'"

# Read from stdin so udoc fits in long pipelines.
curl -sL "https://example.com/docs/spec.pdf" | udoc -

# Bare filenames work too — no `extract` subcommand needed.
udoc memo.docx
