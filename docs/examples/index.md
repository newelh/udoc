# Examples

Worked examples for udoc, grouped by what you're trying to do.

- [Recipes](recipes.md) — small one-off tasks: extract tables,
  hit one page, dump JSONL, etc.
- [Pipelines](pipelines.md) — end-to-end shapes: ingest, RAG,
  agent loops.
- [Hook implementations](hooks.md) — Tesseract, layout models,
  entity extractors, cloud OCR.

This section is in progress. Until the worked examples land,
the closest things in the docs:

- The [Overview](../index.md) walks through `extract`, `stream`,
  and the CLI shapes.
- The [Library guide](../library.md) covers config, overlays,
  diagnostics, and batch processing with `Corpus`.
- The [Hooks chapter](../hooks.md) ships an end-to-end Tesseract
  OCR hook in 30 lines of Python.
- Longer hook recipes live under
  [`examples/hooks/`](https://github.com/newelh/udoc/tree/main/examples/hooks)
  in the repository.
