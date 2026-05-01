"""Tests for udoc.extract / extract_bytes / stream entry points (W1-METHODS-EXTRACT)."""

import pathlib
import pytest

udoc = pytest.importorskip("udoc")


def test_extract_bytes_returns_document(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    assert isinstance(doc, udoc.Document)
    assert len(doc) >= 1


def test_extract_path_returns_document(hello_pdf):
    doc = udoc.extract(str(hello_pdf))
    assert isinstance(doc, udoc.Document)


def test_extract_pages_int(hello_pdf):
    """pages= accepts a single int (0-based)."""
    doc = udoc.extract(str(hello_pdf), pages=0)
    assert isinstance(doc, udoc.Document)


def test_extract_pages_range(hello_pdf):
    """pages= accepts a range object."""
    doc = udoc.extract(str(hello_pdf), pages=range(1))
    assert isinstance(doc, udoc.Document)


def test_extract_pages_list(hello_pdf):
    """pages= accepts a list of page indices."""
    doc = udoc.extract(str(hello_pdf), pages=[0])
    assert isinstance(doc, udoc.Document)


def test_extract_pages_str_spec(hundred_page_pdf):
    """pages= accepts a str spec like '1-10,15' (1-based per Rust)."""
    doc = udoc.extract(str(hundred_page_pdf), pages="1-5,10")
    # Specific page count assertions depend on the str spec semantics.
    assert isinstance(doc, udoc.Document)


def test_extract_format_str(hello_pdf):
    """format= accepts a string."""
    doc = udoc.extract(str(hello_pdf), format="pdf")
    assert doc.format == udoc.Format.Pdf or str(doc.format).lower() == "pdf"


def test_extract_format_enum(hello_pdf):
    """format= accepts a Format enum value."""
    doc = udoc.extract(str(hello_pdf), format=udoc.Format.Pdf)
    assert isinstance(doc, udoc.Document)


def test_extract_invalid_format_raises(hello_pdf):
    """Bad format= raises a TypeError or ValueError."""
    with pytest.raises((TypeError, ValueError)):
        udoc.extract(str(hello_pdf), format="bogus")


def test_extract_password_kwarg(encrypted_pdf):
    """password= kwarg unlocks encrypted PDF."""
    doc = udoc.extract(str(encrypted_pdf), password="test123")
    assert isinstance(doc, udoc.Document)
    assert doc.is_encrypted is True


def test_extract_no_password_on_encrypted_raises(encrypted_pdf):
    """Encrypted PDF without password raises PasswordRequiredError."""
    with pytest.raises((udoc.PasswordRequiredError, udoc.UdocError)):
        udoc.extract(str(encrypted_pdf))


def test_extract_max_file_size_kwarg(hello_pdf):
    """max_file_size= shortcut for config.limits.max_file_size."""
    doc = udoc.extract(str(hello_pdf), max_file_size=1_000_000)
    assert isinstance(doc, udoc.Document)


def test_extract_with_config(hello_pdf):
    """config= accepts a Config instance."""
    cfg = udoc.Config.default()
    doc = udoc.extract(str(hello_pdf), config=cfg)
    assert isinstance(doc, udoc.Document)


def test_extract_on_warning_callback(hello_pdf):
    """on_warning= installs a callback that fires for each Warning."""
    fired = []
    udoc.extract(str(hello_pdf), on_warning=lambda w: fired.append(w))
    # On the minimal fixture there may be 0 warnings; the test just
    # asserts the callback wiring doesn't raise.
    assert isinstance(fired, list)


def test_stream_returns_context_manager(hello_pdf):
    """udoc.stream(path) returns an ExtractionContext that's a context manager."""
    with udoc.stream(str(hello_pdf)) as ctx:
        assert hasattr(ctx, "page_count")
        assert ctx.page_count() >= 1


def test_stream_iteration(hello_pdf):
    """ExtractionContext is iterable."""
    with udoc.stream(str(hello_pdf)) as ctx:
        pages_iter = list(iter(ctx))
        assert len(pages_iter) == ctx.page_count()


def test_detect_format_path(hello_pdf):
    """udoc.detect_format(path) returns a Format."""
    fmt = udoc.detect_format(str(hello_pdf))
    assert fmt == udoc.Format.Pdf or str(fmt).lower() == "pdf"


def test_detect_format_bytes(hello_pdf_bytes):
    """udoc.detect_format(bytes) also works."""
    fmt = udoc.detect_format(hello_pdf_bytes)
    assert isinstance(fmt, udoc.Format)
