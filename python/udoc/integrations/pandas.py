"""udoc.integrations.pandas -- Table.to_pandas() implementation.

Per  this is a pure-Python module. pandas is an optional extra:

    pip install udoc[pandas]

Public API:
  - to_dataframe(table)              -> pd.DataFrame
  - corpus_tables_to_dataframe(corpus, *, source_columns=True) -> pd.DataFrame

Both functions import pandas lazily so that merely importing this module
without pandas installed does not raise. The ImportError is only raised
when the user actually calls one of the conversion functions, and the
message points at the install extra.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import pandas as pd  # type: ignore[import-untyped]

    import udoc


def _require_pandas() -> "pd":
    """Import pandas, or raise a helpful ImportError pointing at the extra."""
    try:
        import pandas as pd
    except ImportError as e:
        raise ImportError(
            "pandas is not installed. Install with: pip install udoc[pandas]"
        ) from e
    return pd


def to_dataframe(table: "udoc.Table") -> "pd.DataFrame":
    """Convert a udoc Table to a pandas DataFrame.

    Behavior:
      - Empty tables (no rows) become an empty DataFrame.
      - When ``table.has_header_row`` is True and there are at least two
        rows, the first row supplies the column labels and the remainder
        becomes the body.
      - Otherwise columns are integer indices (the pandas default).
      - Merged cells are flattened: ``cell.text`` is the plain-text
        reduction the Rust side already produced; we don't try to fill
        spans across rows or columns.

    Raises:
        ImportError: if pandas is not installed.
    """
    pd = _require_pandas()
    rows_list = list(table.rows) if hasattr(table, "rows") else []
    cell_rows = [
        [(cell.text if cell is not None else "") for cell in row.cells]
        for row in rows_list
    ]
    if not cell_rows:
        return pd.DataFrame()
    if getattr(table, "has_header_row", False) and len(cell_rows) >= 2:
        header = cell_rows[0]
        body = cell_rows[1:]
        return pd.DataFrame(body, columns=header)
    return pd.DataFrame(cell_rows)


def corpus_tables_to_dataframe(
    corpus: "udoc.Corpus", *, source_columns: bool = True
) -> "pd.DataFrame":
    """Concatenate every table from every document in the corpus into one DataFrame.

    With ``source_columns=True`` (the default), prepends two columns so
    that provenance survives the concat:
      - ``__source``: the path of the source document (string).
      - ``__page``: the page index (int; ``-1`` when the format has no
        pages, e.g. DOCX or RTF).

    Raises:
        ImportError: if pandas is not installed.
    """
    pd = _require_pandas()
    frames = []
    for sourced in corpus.tables():
        df = to_dataframe(sourced.value)
        if source_columns:
            df.insert(0, "__page", sourced.page if sourced.page is not None else -1)
            df.insert(0, "__source", str(sourced.path))
        frames.append(df)
    if not frames:
        return pd.DataFrame()
    return pd.concat(frames, ignore_index=True)
