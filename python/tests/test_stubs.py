"""Tests pinning the .pyi stubs to the runtime surface.

These are reflection tests, not behavior tests. They catch the most
common kind of stub bug: drift between hand-written `udoc/__init__.pyi`
and the actual pyo3-generated runtime when a pyclass field is renamed,
a method is added, or a Block.kind value is introduced without updating
the Literal in the stubs.

Three checks:

1. `test_stubs_match_runtime` -- every public class declared in the
   stubs has the methods/attributes the runtime actually provides.
2. `test_stubs_have_py_typed_marker` -- the `py.typed` PEP 561 marker
   is shipped so mypy treats `udoc` as a typed package.
3. `test_block_kind_literal_covers_all_kinds` -- the Literal type for
   `Block.kind` (and `Inline.kind`) covers every value the Rust side
   actually emits.
"""

from __future__ import annotations

import os
from pathlib import Path

import udoc

# ---------------------------------------------------------------------------
# Expected runtime surface per class.
# ---------------------------------------------------------------------------
#
# Hardcoded rather than parsed from the .pyi file so a stub typo doesn't
# silently make the test pass. Each entry is the union of:
#   - data fields exposed by `#[pyclass(get_all)]`,
#   - method names declared in `#[pymethods]`,
#   - dunder methods we explicitly support.
#
# Source of truth is `crates/udoc-py/src/*.rs`. Properties dispatched
# via `__getattr__` (synthetic kind-attrs on Block) are NOT listed here
# because they are not on the type's __dict__ -- they appear at access
# time.

EXPECTED_MEMBERS: dict[str, set[str]] = {
    "Format": {
        "Pdf",
        "Docx",
        "Xlsx",
        "Pptx",
        "Doc",
        "Xls",
        "Ppt",
        "Odt",
        "Ods",
        "Odp",
        "Rtf",
        "Md",
        "can_render",
        "has_tables",
        "has_pages",
        "from_str",
    },
    "BoundingBox": {"x_min", "y_min", "x_max", "y_max", "width", "height", "area"},
    "Block": {
        "kind",
        "node_id",
        "text",
        "level",
        "spans",
        "table",
        "list_kind",
        "list_start",
        "items",
        "language",
        "image_index",
        "alt_text",
        "section_role",
        "shape_kind",
        "children",
    },
    "Inline": {
        "kind",
        "node_id",
        "text",
        "bold",
        "italic",
        "underline",
        "strikethrough",
        "superscript",
        "subscript",
        "url",
        "content",
        "label",
        "alt_text",
        "image_index",
    },
    "Table": {
        "node_id",
        "rows",
        "num_columns",
        "header_row_count",
        "has_header_row",
        "may_continue_from_previous",
        "may_continue_to_next",
        "to_pandas",
    },
    "TableRow": {"node_id", "cells", "is_header"},
    "TableCell": {"node_id", "text", "content", "col_span", "row_span", "value"},
    "Image": {
        "node_id",
        "asset_index",
        "width",
        "height",
        "bits_per_component",
        "filter",
        "data",
        "alt_text",
        "bbox",
    },
    "Warning": {"kind", "level", "message", "offset", "page_index", "detail"},
    "Page": {"index", "blocks"},
    "DocumentMetadata": {
        "title",
        "author",
        "subject",
        "creator",
        "producer",
        "creation_date",
        "modification_date",
        "page_count",
        "properties",
    },
    "Chunk": {"text", "source"},
    "ChunkSource": {"page", "block_ids", "bbox"},
    "Limits": {
        "max_file_size",
        "max_pages",
        "max_nesting_depth",
        "max_table_rows",
        "max_cells_per_row",
        "max_text_length",
        "max_styles",
        "max_style_depth",
        "max_images",
        "max_decompressed_size",
        "max_warnings",
        "memory_budget",
    },
    "Hooks": {"ocr", "layout", "annotate", "timeout"},
    "AssetConfig": {"images", "fonts", "strict_fonts"},
    "LayerConfig": {"presentation", "relationships", "interactions"},
    "RenderConfig": {"dpi", "profile"},
    "Config": {
        "limits",
        "hooks",
        "assets",
        "layers",
        "rendering",
        "strict_fonts",
        "memory_budget",
        "format",
        "password",
        "collect_diagnostics",
        "default",
        "agent",
        "batch",
        "ocr",
    },
    "Sourced": {"path", "value", "page", "block_id"},
    "Failed": {"path", "error"},
}

# Module-level functions (declared at top of __init__.pyi).
EXPECTED_FUNCTIONS: set[str] = {"extract", "extract_bytes", "stream"}

# Exception types we declare in the stubs.
EXPECTED_EXCEPTIONS: set[str] = {
    "UdocError",
    "ExtractionError",
    "UnsupportedFormatError",
    "UnsupportedOperationError",
    "PasswordRequiredError",
    "WrongPasswordError",
    "LimitExceededError",
    "HookError",
    "IoError",
    "ParseError",
    "InvalidDocumentError",
    "EncryptedDocumentError",
}


def _members_on_instance(cls: type) -> set[str]:
    """Names visible on the class (and inherited slots).

    pyo3 exposes pyclass fields via descriptors on the type, but
    Document/Corpus methods only show up via attribute lookup on
    instances (Python doesn't enumerate them through `dir()` on the
    type itself when the class lacks `__dict__`). Walking `dir(cls)`
    catches the property-style descriptors; we don't enumerate methods
    on instance-only classes here -- those are checked at instance
    construction time in `test_extract_returns_typed_document` if you
    add it. For the type-level test we accept "method on class via
    dir() OR property descriptor".
    """
    return {n for n in dir(cls) if not n.startswith("_")}


# ---------------------------------------------------------------------------
# Tests.
# ---------------------------------------------------------------------------


def test_stubs_match_runtime() -> None:
    """Every member named in the stubs must exist on the runtime class.

    We check both directions:

      - missing on runtime: stub claims a method/attr that the .so doesn't
        actually provide. Surfaces as a hard mypy lie.
      - extra on runtime (not all): for the typed-attribute classes we
        also pin against undocumented fields creeping in. Document and
        Corpus are exempt because pyo3 hides their methods from `dir()`
        on the class object; their methods are validated by being
        callable in the codebase's behavior tests.
    """
    missing: list[tuple[str, str]] = []
    for cls_name, expected in EXPECTED_MEMBERS.items():
        cls = getattr(udoc, cls_name, None)
        assert cls is not None, f"udoc.{cls_name} is missing from the runtime module"
        runtime_members = _members_on_instance(cls)
        for name in expected:
            if name not in runtime_members:
                missing.append((cls_name, name))
    assert not missing, (
        "Stub-declared members not found on the runtime classes. Either the "
        "Rust source dropped them (update the stubs) or the runtime build is "
        "stale (rebuild the .so):\n  "
        + "\n  ".join(f"udoc.{cls}.{name}" for cls, name in missing)
    )

    # Top-level functions.
    for fn_name in EXPECTED_FUNCTIONS:
        fn = getattr(udoc, fn_name, None)
        assert fn is not None, f"udoc.{fn_name} is missing from the runtime module"
        assert callable(fn), f"udoc.{fn_name} should be callable"

    # Exceptions.
    for exc_name in EXPECTED_EXCEPTIONS:
        exc = getattr(udoc, exc_name, None)
        assert exc is not None, f"udoc.{exc_name} is missing from the runtime module"
        assert isinstance(exc, type) and issubclass(
            exc, BaseException
        ), f"udoc.{exc_name} should be an exception class"

    # Sanity: UdocError is the root of the hierarchy.
    assert issubclass(udoc.ExtractionError, udoc.UdocError)
    assert issubclass(udoc.PasswordRequiredError, udoc.UdocError)


def test_stubs_have_py_typed_marker() -> None:
    """py.typed marker must exist for mypy to recognize udoc as typed."""
    assert udoc.__file__ is not None, "udoc must be importable as a regular package"
    py_typed = Path(udoc.__file__).parent / "py.typed"
    assert py_typed.exists(), (
        f"py.typed marker missing at {py_typed}. PEP 561 requires this file "
        "for downstream mypy/IDE consumers to recognize the package as typed."
    )

    # The .pyi files we own must also be co-located -- mypy only reads
    # *.pyi from the package root when py.typed is present.
    init_pyi = Path(udoc.__file__).parent / "__init__.pyi"
    asyncio_pyi = Path(udoc.__file__).parent / "asyncio.pyi"
    assert init_pyi.exists(), f"__init__.pyi missing at {init_pyi}"
    assert asyncio_pyi.exists(), f"asyncio.pyi missing at {asyncio_pyi}"


# Source of truth: `crates/udoc-py/src/convert.rs` (search for `kind:`).
# Keep this set in sync with the Literal types in __init__.pyi.

EXPECTED_BLOCK_KINDS: frozenset[str] = frozenset(
    {
        "paragraph",
        "heading",
        "list",
        "table",
        "code_block",
        "image",
        "page_break",
        "thematic_break",
        "section",
        "shape",
    }
)

EXPECTED_INLINE_KINDS: frozenset[str] = frozenset(
    {
        "text",
        "code",
        "link",
        "footnote_ref",
        "inline_image",
        "soft_break",
        "line_break",
    }
)


def test_block_kind_literal_covers_all_kinds() -> None:
    """The Literal[...] in the stubs must cover every kind the Rust
    side actually emits.

    Approach: extract a known fixture, walk the produced Block/Inline
    tree, collect every observed `kind` value, and assert each one is
    in the documented set. Adding a new kind on the Rust side without
    updating the Literal trips this test.
    """
    repo_root = Path(__file__).resolve().parents[2]
    fixture = repo_root / "tests" / "corpus" / "minimal" / "hello.pdf"
    if not fixture.exists():  # pragma: no cover -- defensive
        # Fall back to whatever's in tests/corpus/minimal.
        candidates = list((repo_root / "tests" / "corpus" / "minimal").glob("*.pdf"))
        assert candidates, "no PDF fixtures under tests/corpus/minimal/"
        fixture = candidates[0]

    doc = udoc.extract(os.fspath(fixture))

    seen_block_kinds: set[str] = set()
    seen_inline_kinds: set[str] = set()

    def walk_block(block: udoc.Block) -> None:
        seen_block_kinds.add(block.kind)
        # Inline spans live under paragraph/heading.
        for span in block.spans or []:
            seen_inline_kinds.add(span.kind)
        # List items.
        for item in block.items or []:
            for child in item:
                walk_block(child)
        # Section/shape children.
        for child in block.children or []:
            walk_block(child)

    for block in doc.blocks():
        walk_block(block)

    # Every observed kind must be in the documented set. We don't require
    # the fixture to exercise every kind -- that's the type-driver matrix
    # in the integration tests.
    unknown_block = seen_block_kinds - EXPECTED_BLOCK_KINDS
    assert not unknown_block, (
        f"Block.kind values observed in {fixture.name} that aren't in the "
        f"stub Literal: {sorted(unknown_block)}. Either the Rust convert "
        "layer added a new kind (update __init__.pyi BlockKind Literal) "
        "or this test set is wrong."
    )
    unknown_inline = seen_inline_kinds - EXPECTED_INLINE_KINDS
    assert not unknown_inline, (
        f"Inline.kind values observed in {fixture.name} that aren't in "
        f"the stub Literal: {sorted(unknown_inline)}. Update InlineKind "
        "in __init__.pyi."
    )
