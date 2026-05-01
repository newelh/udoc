"""Tests for Document.text_chunks() and the 5 strategies (W1-METHODS-CHUNKS)."""

import pytest

udoc = pytest.importorskip("udoc")


def test_text_chunks_default_heading_strategy(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    chunks = list(doc.text_chunks())
    assert isinstance(chunks, list)


def test_text_chunks_by_page(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    chunks = list(doc.text_chunks(by="page"))
    # Should be at most 1 chunk per page.
    assert len(chunks) <= len(doc)


def test_text_chunks_by_heading(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    chunks = list(doc.text_chunks(by="heading", size=2000))
    assert isinstance(chunks, list)


def test_text_chunks_by_section(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    chunks = list(doc.text_chunks(by="section"))
    assert isinstance(chunks, list)


def test_text_chunks_by_size(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    chunks = list(doc.text_chunks(by="size", size=500))
    assert isinstance(chunks, list)
    # Size-bounded chunks: each should be roughly <= size+overshoot.
    for c in chunks:
        if hasattr(c, "text"):
            assert len(c.text) <= 1000  # generous upper bound; 500 + sentence boundary slop


def test_text_chunks_by_semantic(hello_pdf_bytes):
    """The 5th strategy per  §6.2.5 (paragraph boundary + size cap)."""
    doc = udoc.extract_bytes(hello_pdf_bytes)
    chunks = list(doc.text_chunks(by="semantic", size=1000))
    assert isinstance(chunks, list)


def test_text_chunks_invalid_strategy_raises(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    with pytest.raises((ValueError, TypeError)):
        list(doc.text_chunks(by="bogus_strategy"))


def test_chunk_has_text_and_source(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    chunks = list(doc.text_chunks(by="heading"))
    for c in chunks:
        assert hasattr(c, "text")
        assert hasattr(c, "source")


def test_chunk_source_carries_provenance(hello_pdf_bytes):
    doc = udoc.extract_bytes(hello_pdf_bytes)
    chunks = list(doc.text_chunks(by="page"))
    for c in chunks:
        # ChunkSource has page, block_ids, bbox.
        assert hasattr(c.source, "page")
        assert hasattr(c.source, "block_ids")
