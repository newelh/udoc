"""Tests for the Document pyclass (W1-METHODS-DOCUMENT)."""

import pytest

udoc = pytest.importorskip("udoc")


def test_document_len_returns_page_count(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    assert len(doc) == doc.metadata.page_count


def test_document_getitem_returns_page(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    page = doc[0]
    assert isinstance(page, udoc.Page)


def test_document_iteration(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    pages = list(doc)
    assert len(pages) == len(doc)
    assert all(isinstance(p, udoc.Page) for p in pages)


def test_document_pages_method(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    pages = list(doc.pages())
    assert len(pages) == len(doc)


def test_document_blocks_iteration(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    blocks = list(doc.blocks())
    # On the minimal fixture, there should be at least one block.
    assert isinstance(blocks, list)


def test_document_text_returns_str(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    text = doc.text()
    assert isinstance(text, str)


def test_document_to_markdown(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    md = doc.to_markdown(with_anchors=False)
    assert isinstance(md, str)


def test_document_to_markdown_with_anchors(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    md = doc.to_markdown(with_anchors=True)
    assert isinstance(md, str)


def test_document_to_dict(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    d = doc.to_dict()
    assert isinstance(d, dict)


def test_document_to_json(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    j = doc.to_json()
    assert isinstance(j, str)
    assert j.startswith("{") and j.endswith("}")


def test_document_to_json_pretty(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    j = doc.to_json(pretty=True)
    assert "\n" in j  # pretty-printed has newlines


def test_document_metadata_page_count_matches_len(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    assert doc.metadata.page_count == len(doc)


def test_document_format_matches_pdf(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    assert doc.format == udoc.Format.Pdf or str(doc.format).lower() == "pdf"


def test_document_is_encrypted_false_on_minimal(hello_pdf_bytes):
    """ W0-IS-ENCRYPTED: unencrypted doc returns False."""
    doc = udoc.extract_bytes(hello_pdf_bytes)
    assert doc.is_encrypted is False


def test_document_is_encrypted_true_on_encrypted(encrypted_pdf):
    """ W0-IS-ENCRYPTED: encrypted doc returns True even with correct password."""
    doc = udoc.extract(str(encrypted_pdf), password="test123")
    assert doc.is_encrypted is True


def test_document_warnings_is_list(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    assert isinstance(doc.warnings, list)
