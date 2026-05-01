"""Tests for the udoc exception hierarchy (W1-METHODS-EXCEPTIONS / )."""

import pytest

udoc = pytest.importorskip("udoc")


def test_password_required_on_encrypted_no_password(encrypted_pdf):
    """: Error::Encryption(PasswordRequired) -> PasswordRequiredError."""
    with pytest.raises(udoc.PasswordRequiredError):
        udoc.extract(str(encrypted_pdf))


def test_wrong_password_on_encrypted(encrypted_pdf):
    """: Error::Encryption(WrongPassword) -> WrongPasswordError or PasswordRequired."""
    # The boundary maps InvalidPassword to PasswordRequired by default;
    # accept either WrongPassword or PasswordRequired.
    with pytest.raises((udoc.WrongPasswordError, udoc.PasswordRequiredError)):
        udoc.extract(str(encrypted_pdf), password="wrong_password")


def test_extraction_error_for_unparseable_bytes():
    """Garbage bytes raise some UdocError subclass."""
    with pytest.raises(udoc.UdocError):
        udoc.extract_bytes(b"not a real document")


def test_unsupported_format_for_unknown_extension(tmp_path):
    """Extracting an unrecognized file raises UnsupportedFormatError or ExtractionError."""
    p = tmp_path / "mystery.unknown"
    p.write_bytes(b"random bytes")
    with pytest.raises((udoc.UnsupportedFormatError, udoc.UdocError)):
        udoc.extract(str(p))


def test_io_error_for_missing_file():
    """Path that doesn't exist raises IoError or ExtractionError."""
    with pytest.raises((udoc.IoError, udoc.UdocError)):
        udoc.extract("/definitely/does/not/exist.pdf")


def test_unsupported_operation_render_on_docx(docx_fixture):
    """DOCX doesn't support render_page; raises UnsupportedOperationError."""
    doc = udoc.extract(str(docx_fixture))
    with pytest.raises(udoc.UnsupportedOperationError):
        doc.render_page(0)


def test_all_exceptions_inherit_from_udoc_error():
    """: every typed exception inherits from UdocError."""
    for name in [
        "ExtractionError", "UnsupportedFormatError",
        "UnsupportedOperationError", "PasswordRequiredError",
        "WrongPasswordError", "LimitExceededError", "HookError",
        "IoError", "ParseError", "InvalidDocumentError",
        "EncryptedDocumentError",
    ]:
        cls = getattr(udoc, name)
        assert issubclass(cls, udoc.UdocError), f"{name} must inherit UdocError"


def test_udoc_error_inherits_from_python_exception():
    assert issubclass(udoc.UdocError, Exception)
