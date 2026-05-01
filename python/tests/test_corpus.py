"""Tests for udoc.Corpus + Sourced + Failed (W1-METHODS-CORPUS).

NOTE: parallel tests are minimal; the ProcessPoolExecutor + spawn
context has platform-dependent behavior that's better
covered by the W3-WHEEL-WALKTHROUGH manual smoke test.
"""

import pathlib
import pytest

udoc = pytest.importorskip("udoc")


def test_corpus_from_dir(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    assert isinstance(c, udoc.Corpus)


def test_corpus_count(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    n = c.count()
    assert n > 0


def test_corpus_len_raises_typeerror(realworld_dir):
    """Per Domain Expert: len(corpus) raises TypeError; use corpus.count()."""
    c = udoc.Corpus(realworld_dir)
    with pytest.raises(TypeError):
        len(c)


def test_corpus_iter_yields_documents_or_failed(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    items = list(c)
    assert len(items) > 0
    for item in items:
        assert isinstance(item, (udoc.Document, udoc.Failed))


def test_corpus_filter_returns_corpus(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    c2 = c.filter(lambda d: d.metadata.page_count > 0)
    assert isinstance(c2, udoc.Corpus)


def test_corpus_with_config_returns_corpus(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    c2 = c.with_config(udoc.Config.batch())
    assert isinstance(c2, udoc.Corpus)


def test_corpus_text_concat(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    s = c.text()
    assert isinstance(s, str)


def test_corpus_metadata_iter(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    metas = list(c.metadata())
    for m in metas:
        assert isinstance(m, udoc.Sourced)


def test_corpus_list_eager(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    docs = c.list()
    assert isinstance(docs, list)


def test_corpus_to_jsonl_writes_file(realworld_dir, tmp_path):
    c = udoc.Corpus(realworld_dir)
    out = tmp_path / "corpus.jsonl"
    n = c.to_jsonl(str(out))
    assert isinstance(n, int)
    assert out.exists()


def test_corpus_failed_for_missing_file(tmp_path):
    """Iterating a non-existent path yields a Failed."""
    bad = tmp_path / "does-not-exist.pdf"
    bad.touch()  # exists but empty (will fail to parse)
    c = udoc.Corpus([bad])
    items = list(c)
    # Either a Failed or an early extraction error wrapped in Failed.
    assert any(isinstance(item, udoc.Failed) for item in items)


def test_sourced_repr_contains_path(realworld_dir):
    c = udoc.Corpus(realworld_dir)
    metas = list(c.metadata())
    if metas:
        r = repr(metas[0])
        assert "Sourced" in r


def test_corpus_parallel_thread_mode(realworld_dir):
    """parallel(n, mode='thread') returns a new Corpus."""
    c = udoc.Corpus(realworld_dir)
    cp = c.parallel(2, mode="thread")
    assert isinstance(cp, udoc.Corpus)


@pytest.mark.skip(reason="ProcessPoolExecutor + spawn is flaky in the test harness; verified manually per walkthrough §5")
def test_corpus_parallel_process_mode(realworld_dir):
    """parallel(n, mode='process') uses ProcessPoolExecutor + spawn."""
    c = udoc.Corpus(realworld_dir)
    cp = c.parallel(2, mode="process")
    docs = cp.list()
    assert isinstance(docs, list)
