"""Programmatic check that the documented Python API surface is
actually reachable in the `udoc` Python module.

This test runs against the installed wheel (via `maturin develop`).
It does NOT exhaustively cover every method; it covers the load-bearing
top-level types so the parity doc can be machine-checked.
"""

import importlib
import inspect
import pytest


udoc = pytest.importorskip("udoc")


# Top-level entry points (api-parity.md §1)
TOP_LEVEL_FUNCTIONS = [
    "extract",
    "extract_bytes",
    "stream",
    "detect_format",
]

# Classes (api-parity.md §2-§7)
EXPOSED_CLASSES = [
    "Document",
    "DocumentMetadata",
    "Page",
    "Block",
    "Inline",
    "Table",
    "TableRow",
    "TableCell",
    "Image",
    "Warning",
    "BoundingBox",
    "Format",
    "Config",
    "Limits",
    "Hooks",
    "AssetConfig",
    "LayerConfig",
    "RenderConfig",
    "Corpus",
    "Sourced",
    "Failed",
    "Chunk",
    "ChunkSource",
    "ExtractionContext",
]

# Exception classes (api-parity.md §8 / )
EXPOSED_EXCEPTIONS = [
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
]


@pytest.mark.parametrize("name", TOP_LEVEL_FUNCTIONS)
def test_top_level_function_reachable(name):
    """Each function listed in api-parity.md §1 must be importable from `udoc`."""
    assert hasattr(udoc, name), f"udoc.{name} must be reachable per api-parity.md §1"
    assert callable(getattr(udoc, name)), f"udoc.{name} must be callable"


@pytest.mark.parametrize("name", EXPOSED_CLASSES)
def test_exposed_class_reachable(name):
    """Each class listed as 'Exposed' in api-parity.md must be importable from `udoc`."""
    assert hasattr(udoc, name), f"udoc.{name} must be reachable per api-parity.md"
    obj = getattr(udoc, name)
    assert inspect.isclass(obj) or hasattr(obj, "__class__"), (
        f"udoc.{name} must be a class or class-like object"
    )


@pytest.mark.parametrize("name", EXPOSED_EXCEPTIONS)
def test_exposed_exception_reachable(name):
    """Each exception class listed in api-parity.md §8 /  must be importable."""
    assert hasattr(udoc, name), f"udoc.{name} must be reachable"
    obj = getattr(udoc, name)
    assert isinstance(obj, type) and issubclass(obj, BaseException), (
        f"udoc.{name} must be an exception class"
    )


def test_exception_hierarchy_under_udoc_error():
    """Per : every typed exception class inherits from UdocError."""
    base = udoc.UdocError
    for name in EXPOSED_EXCEPTIONS:
        if name == "UdocError":
            continue
        cls = getattr(udoc, name)
        assert issubclass(cls, base), f"{name} must inherit from UdocError"


def test_format_enum_has_all_12_variants():
    """Per : 12 formats supported. pyo3 0.28 enums aren't
    iterable on the type itself; use Format.all_variants()."""
    expected = {"Pdf", "Docx", "Xlsx", "Pptx", "Doc", "Xls", "Ppt",
                "Odt", "Ods", "Odp", "Rtf", "Md"}
    actual = {v.name for v in udoc.Format.all_variants()}
    missing = expected - actual
    assert not missing, f"Format enum missing variants: {missing}"


def test_format_capability_accessors_exist():
    """Per + api-parity.md §4: can_render, has_tables, has_pages."""
    fmt = udoc.Format.Pdf
    assert hasattr(fmt, "can_render")
    assert hasattr(fmt, "has_tables")
    assert hasattr(fmt, "has_pages")
    # PDF supports all three
    assert fmt.can_render is True
    assert fmt.has_tables is True
    assert fmt.has_pages is True


def test_config_has_all_4_presets():
    """2: default, agent, batch, ocr presets."""
    for preset in ["default", "agent", "batch", "ocr"]:
        cls_method = getattr(udoc.Config, preset, None)
        assert cls_method is not None, f"udoc.Config.{preset} preset must exist"
        cfg = cls_method()
        assert isinstance(cfg, udoc.Config), (
            f"udoc.Config.{preset}() must return a Config instance"
        )


def test_async_adapter_reachable():
    """udoc.asyncio submodule + key entry points."""
    aio = importlib.import_module("udoc.asyncio")
    for name in ["extract", "extract_bytes", "stream", "Corpus"]:
        assert hasattr(aio, name), f"udoc.asyncio.{name} must be reachable"


def test_integrations_pandas_module_importable():
    """udoc.integrations.pandas module is importable; pandas itself stays optional."""
    mod = importlib.import_module("udoc.integrations.pandas")
    assert hasattr(mod, "to_dataframe")
    assert hasattr(mod, "corpus_tables_to_dataframe")


def test_integrations_arrow_placeholder_importable():
    """Arrow integration is a placeholder should-have AC #22."""
    mod = importlib.import_module("udoc.integrations.arrow")
    assert mod is not None  # docstring-only placeholder


def test_py_typed_marker_present():
    """PEP 561 marker tells mypy this package ships typed."""
    import os
    py_typed = os.path.join(os.path.dirname(udoc.__file__), "py.typed")
    assert os.path.exists(py_typed), (
        "python/udoc/py.typed must exist for mypy to recognize the typed package"
    )


def test_version_string_format():
    """Version string follows PEP 440 alpha-dev pattern."""
    import re
    v = udoc.__version__
    # Accept either 0.1.0a1.dev0 or 0.1.0-alpha.1 (pyo3 reads from
    # Cargo.toml which uses the dash form).
    assert re.match(r"^0\.1\.0[-.]?a(?:lpha)?\.?[\d\.]+(?:\.dev\d*)?$", v), (
        f"unexpected __version__ format: {v!r}"
    )
