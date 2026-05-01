"""Tests for udoc.integrations.pandas.

These tests cover the pure-Python pandas integration that backs
``Table.to_pandas()``. The whole module is skipped when pandas is not
installed; the ImportError contract is verified by an inline subprocess
test that pretends pandas is missing.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

# Skip the whole module if pandas isn't installed (extras_require unmet).
pd = pytest.importorskip("pandas")

import udoc
from udoc.integrations.pandas import corpus_tables_to_dataframe, to_dataframe


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


REPO_ROOT = Path(__file__).resolve().parents[2]
PPTX_TABLE = REPO_ROOT / "tests" / "corpus" / "pptx" / "table.pptx"
PPTX_MERGED = REPO_ROOT / "tests" / "corpus" / "pptx" / "merged_cells.pptx"


def _first_table(doc: "udoc.Document") -> "udoc.Table":
    tables = list(doc.tables())
    if not tables:
        pytest.skip("no tables extracted from fixture")
    return tables[0]


@pytest.fixture
def simple_table():
    if not PPTX_TABLE.exists():
        pytest.skip(f"fixture not present: {PPTX_TABLE}")
    doc = udoc.extract_bytes(PPTX_TABLE.read_bytes())
    return _first_table(doc)


@pytest.fixture
def merged_table():
    if not PPTX_MERGED.exists():
        pytest.skip(f"fixture not present: {PPTX_MERGED}")
    doc = udoc.extract_bytes(PPTX_MERGED.read_bytes())
    return _first_table(doc)


# ---------------------------------------------------------------------------
# to_dataframe
# ---------------------------------------------------------------------------


def test_to_dataframe_returns_dataframe(simple_table):
    """to_dataframe returns a pd.DataFrame with the right shape."""
    df = to_dataframe(simple_table)
    assert isinstance(df, pd.DataFrame)
    # The PPTX fixture has 3 rows, 3 columns and no header flag.
    assert df.shape == (len(simple_table.rows), simple_table.num_columns)


def test_to_dataframe_with_header_row(simple_table):
    """When ``has_header_row`` is True the first row supplies columns."""
    # The fixture's has_header_row is False; build a synthetic table-like
    # adapter that exposes the same shape with the flag flipped.
    class _ShimRow:
        def __init__(self, cells):
            self.cells = cells
            self.is_header = False

    class _ShimCell:
        def __init__(self, text):
            self.text = text

    class _ShimTable:
        def __init__(self, rows, num_columns, has_header_row):
            self.rows = rows
            self.num_columns = num_columns
            self.has_header_row = has_header_row

    rows = [
        _ShimRow([_ShimCell("Name"), _ShimCell("Age")]),
        _ShimRow([_ShimCell("Alice"), _ShimCell("30")]),
        _ShimRow([_ShimCell("Bob"), _ShimCell("25")]),
    ]
    table = _ShimTable(rows=rows, num_columns=2, has_header_row=True)
    df = to_dataframe(table)
    assert list(df.columns) == ["Name", "Age"]
    assert df.shape == (2, 2)
    assert df.iloc[0]["Name"] == "Alice"
    assert df.iloc[1]["Age"] == "25"


def test_to_dataframe_merged_cells_handled(merged_table):
    """Merged cells flatten to plain text without crashing.

    The Rust extractor surfaces ``cell.text`` as the plain-text reduction
    and reports the original ``col_span``/``row_span``. We do not try to
    expand spans; we just make sure every produced row is iterable and
    the result is rectangular per pandas' rules.
    """
    df = to_dataframe(merged_table)
    assert isinstance(df, pd.DataFrame)
    # pandas pads short rows with NaN so the frame is rectangular.
    assert df.shape[0] == len(merged_table.rows)
    # At least one non-trivial cell value made it through.
    flat = [v for v in df.to_numpy().flatten().tolist() if isinstance(v, str)]
    assert any(v for v in flat)


def test_to_dataframe_empty_table():
    """A table with zero rows yields an empty DataFrame, not a crash."""

    class _Empty:
        rows = []
        num_columns = 0
        has_header_row = False

    df = to_dataframe(_Empty())
    assert isinstance(df, pd.DataFrame)
    assert df.empty


# ---------------------------------------------------------------------------
# corpus_tables_to_dataframe
# ---------------------------------------------------------------------------


def test_corpus_tables_to_dataframe_source_columns_true():
    """source_columns=True prepends __source and __page."""
    if not PPTX_TABLE.exists() or not PPTX_MERGED.exists():
        pytest.skip("fixtures missing")
    corpus = udoc.Corpus([str(PPTX_TABLE), str(PPTX_MERGED)])
    df = corpus_tables_to_dataframe(corpus, source_columns=True)
    assert isinstance(df, pd.DataFrame)
    assert "__source" in df.columns
    assert "__page" in df.columns
    # __source must be first, __page second.
    assert df.columns.tolist()[:2] == ["__source", "__page"]


def test_corpus_tables_to_dataframe_provenance_includes_path_and_page():
    """Every row carries the source path and the page (or -1)."""
    if not PPTX_TABLE.exists():
        pytest.skip("fixture missing")
    corpus = udoc.Corpus([str(PPTX_TABLE)])
    df = corpus_tables_to_dataframe(corpus, source_columns=True)
    if df.empty:
        pytest.skip("corpus produced no tables")
    sources = set(df["__source"].tolist())
    assert sources == {str(PPTX_TABLE)}
    # __page is either a real index or -1; it must always be an int.
    for v in df["__page"].tolist():
        assert isinstance(v, int)


def test_corpus_tables_to_dataframe_source_columns_false():
    """source_columns=False omits the provenance columns."""
    if not PPTX_TABLE.exists():
        pytest.skip("fixture missing")
    corpus = udoc.Corpus([str(PPTX_TABLE)])
    df = corpus_tables_to_dataframe(corpus, source_columns=False)
    assert "__source" not in df.columns
    assert "__page" not in df.columns


# ---------------------------------------------------------------------------
# Missing-pandas error message
# ---------------------------------------------------------------------------


def test_pandas_missing_raises_helpful_error():
    """Without pandas, to_dataframe must raise ImportError with the install hint.

    Run a short subprocess where ``pandas`` is forced unimportable; if
    the integration's import-error fallback is reachable it surfaces the
    `pip install udoc[pandas]` instruction.
    """
    import subprocess
    import sys
    import textwrap

    code = textwrap.dedent(
        """
        import sys
        # Pretend pandas is not installed: shadow it before importing.
        sys.modules['pandas'] = None
        from udoc.integrations.pandas import to_dataframe

        class T:
            rows = []
            num_columns = 0
            has_header_row = False

        try:
            to_dataframe(T())
        except ImportError as e:
            assert 'pip install udoc[pandas]' in str(e), str(e)
            print('OK')
        else:
            raise SystemExit('expected ImportError')
        """
    )
    env = dict(os.environ)
    result = subprocess.run(
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == 0, result.stderr
    assert "OK" in result.stdout
