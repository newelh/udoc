# Pipelines

End-to-end pipeline shapes. **Coming soon.** Planned worked
examples:

- **RAG ingest.** Walk a corpus, chunk by heading, embed with
  source provenance, write to a vector store.
- **Agent tool use.** Wire `udoc` into an LLM agent loop with
  the [agent instructions](../agents.md) page as context.
- **Document classification.** Run a layout-detection hook,
  classify by region label, route by type.
- **Forensic audit.** Read tracked changes, comments, and form
  fields from a DOCX corpus.
- **Spreadsheet ingest.** Multi-sheet typed cells into Pandas /
  DuckDB / Parquet.

Until they land, the [Library guide](../library.md) covers
`Corpus`, `text_chunks`, and the streaming `Extractor`.
