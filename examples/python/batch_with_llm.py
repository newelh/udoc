"""Walk a directory, extract every supported document, summarise each
with a generic chat LLM, and write the summaries to a CSV.

This is a worked example of the LLM-forward pattern: udoc handles all
the document parsing, your model handles the meaning. Replace the
`call_llm` stub with whichever provider you prefer.

Run with:

    pip install udoc
    export ANTHROPIC_API_KEY=...   # or OPENAI_API_KEY, etc.
    python examples/python/batch_with_llm.py /path/to/docs/ summaries.csv

Notes:
- The script is single-process, single-thread for clarity. For larger
  batches, use concurrent.futures.ThreadPoolExecutor and watch the
  rate limits of your model provider.
- Documents that fail to extract are recorded in the CSV with an empty
  summary and the error in the `note` column. The script does not stop
  on per-document errors.
"""
from __future__ import annotations

import csv
import os
import sys
from pathlib import Path

import udoc


SUPPORTED_EXTS = {
    ".pdf", ".docx", ".xlsx", ".pptx",
    ".doc", ".xls", ".ppt",
    ".odt", ".ods", ".odp",
    ".rtf", ".md",
}


def call_llm(text: str) -> str:
    """Replace this stub with your provider of choice.

    The default impl just returns the first 200 characters so the
    example runs without API keys. With a real model:

        from anthropic import Anthropic
        client = Anthropic()
        response = client.messages.create(
            model="claude-haiku-4-5",
            max_tokens=512,
            messages=[{"role": "user", "content": f"Summarise in one sentence:\n\n{text[:8000]}"}],
        )
        return response.content[0].text
    """
    return text[:200].replace("\n", " ")


def main(directory: str, output_csv: str) -> None:
    root = Path(directory)
    if not root.is_dir():
        print(f"not a directory: {directory}", file=sys.stderr)
        sys.exit(2)

    paths = [p for p in sorted(root.rglob("*")) if p.suffix.lower() in SUPPORTED_EXTS]
    print(f"found {len(paths)} documents under {root}", file=sys.stderr)

    with open(output_csv, "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["path", "title", "pages", "summary", "note"])
        writer.writeheader()

        for i, path in enumerate(paths):
            print(f"  [{i + 1:4d}/{len(paths)}] {path}", file=sys.stderr)
            try:
                doc = udoc.extract(str(path))
                text = "\n".join(b.text for b in doc.content if b.text).strip()
                summary = call_llm(text) if text else ""
                writer.writerow({
                    "path": str(path),
                    "title": doc.metadata.title or "",
                    "pages": doc.metadata.page_count,
                    "summary": summary,
                    "note": "",
                })
            except Exception as e:
                writer.writerow({
                    "path": str(path),
                    "title": "",
                    "pages": 0,
                    "summary": "",
                    "note": f"{type(e).__name__}: {e}",
                })


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <directory> <output.csv>", file=sys.stderr)
        sys.exit(2)
    main(sys.argv[1], sys.argv[2])
