//! W1-CHUNKS: `Document.text_chunks(by, size)` -- agent-friendly retrieval-sized
//! windows over the document with citation anchors.
//!
//! Five strategies ( §6.2.5):
//! - `"page"` -- one chunk per page (no size cap).
//! - `"heading"` -- split at heading boundaries, capped at `size` chars.
//! - `"section"` -- split at top-level sections (heading level 1).
//! - `"size"` -- fixed size with sentence-boundary breaks.
//! - `"semantic"` -- paragraph-boundary breaks with size cap.
//!
//! Each chunk carries provenance (`PyChunkSource`): the page index if
//! resolvable, the list of contributing block NodeIds, and a tight
//! bounding box if every contributor has known geometry.
//!
//! Walks only the top-level `doc.content` spine for chunk boundaries; per
//!  nested Section/Shape children flatten into the parent's text
//! reduction via `Block::text()`. This matches the markdown emitter's
//! linearization and is what callers expect for retrieval.
//!
//! The strategy logic lives in pure-Rust functions that emit `RawChunk`
//! (no pyo3 dependency) so cargo test can exercise them without linking
//! libpython. `chunk_document` is the thin pyo3 wrapper that converts
//! `RawChunk -> Py<PyChunk>`.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use udoc_facade::{Block, BoundingBox, Document, NodeId};

use crate::convert::extract_inline_text;

// ---------------------------------------------------------------------------
// PyBoundingBox
// ---------------------------------------------------------------------------

/// An axis-aligned rectangle. Mirrors `udoc::BoundingBox`.
#[pyclass(name = "BoundingBox", frozen, get_all)]
pub struct PyBoundingBox {
    pub x_min: f64,
    pub y_min: f64,
    pub x_max: f64,
    pub y_max: f64,
}

#[pymethods]
impl PyBoundingBox {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str, &'static str) =
        ("x_min", "y_min", "x_max", "y_max");

    fn __repr__(&self) -> String {
        format!(
            "BoundingBox(x_min={}, y_min={}, x_max={}, y_max={})",
            self.x_min, self.y_min, self.x_max, self.y_max
        )
    }

    /// Width of the box (`x_max - x_min`).
    #[getter]
    fn width(&self) -> f64 {
        self.x_max - self.x_min
    }

    /// Height of the box (`y_max - y_min`).
    #[getter]
    fn height(&self) -> f64 {
        self.y_max - self.y_min
    }

    /// Area of the box (`width * height`).
    #[getter]
    fn area(&self) -> f64 {
        self.width() * self.height()
    }

    /// Point-in-box test. `point` is a 2-tuple `(x, y)`. Edge inclusive
    /// on min, exclusive on max so adjacent bboxes don't double-claim.
    fn __contains__(&self, point: &Bound<'_, PyAny>) -> PyResult<bool> {
        let tup = point
            .cast::<PyTuple>()
            .map_err(|_| PyTypeError::new_err("BoundingBox.__contains__ expects a (x, y) tuple"))?;
        if tup.len() != 2 {
            return Err(PyTypeError::new_err(
                "BoundingBox.__contains__ expects a 2-tuple",
            ));
        }
        let x: f64 = tup.get_item(0)?.extract()?;
        let y: f64 = tup.get_item(1)?.extract()?;
        Ok(x >= self.x_min && x < self.x_max && y >= self.y_min && y < self.y_max)
    }
}

impl PyBoundingBox {
    /// Build a `PyBoundingBox` from a Rust `udoc::BoundingBox`.
    pub fn from_rust(bb: udoc_facade::BoundingBox) -> Self {
        Self {
            x_min: bb.x_min,
            y_min: bb.y_min,
            x_max: bb.x_max,
            y_max: bb.y_max,
        }
    }
}

// ---------------------------------------------------------------------------
// PyChunkSource
// ---------------------------------------------------------------------------

/// Provenance for a chunk: which page + which block IDs + bbox if known.
#[pyclass(name = "ChunkSource", frozen, get_all)]
pub struct PyChunkSource {
    /// Page index this chunk was sourced from. None if the chunk spans
    /// multiple pages or the format has no page concept.
    pub page: Option<u32>,
    /// NodeIds of every block contributing to this chunk, in document order.
    pub block_ids: Vec<u64>,
    /// Tight bbox if every block has known geometry, else None.
    pub bbox: Option<Py<PyBoundingBox>>,
}

#[pymethods]
impl PyChunkSource {
    #[classattr]
    const __match_args__: (&'static str, &'static str) = ("page", "block_ids");

    fn __repr__(&self) -> String {
        let page = match self.page {
            Some(p) => p.to_string(),
            None => "None".into(),
        };
        let bbox = if self.bbox.is_some() { "..." } else { "None" };
        format!(
            "ChunkSource(page={}, block_ids={:?}, bbox={})",
            page, self.block_ids, bbox
        )
    }

    /// rich library protocol. Each yielded value becomes a column in
    /// `rich.repr` output.
    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Py<PyAny>>> {
        let mut out: Vec<Py<PyAny>> = Vec::new();
        out.push(self.page.into_pyobject(py)?.unbind().into_any());
        out.push(
            self.block_ids
                .clone()
                .into_pyobject(py)?
                .unbind()
                .into_any(),
        );
        if let Some(bbox) = &self.bbox {
            out.push(bbox.clone_ref(py).into_any());
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// PyChunk
// ---------------------------------------------------------------------------

/// A chunk of text plus its provenance.
#[pyclass(name = "Chunk", frozen, get_all)]
pub struct PyChunk {
    pub text: String,
    pub source: Py<PyChunkSource>,
}

#[pymethods]
impl PyChunk {
    #[classattr]
    const __match_args__: (&'static str, &'static str) = ("text", "source");

    fn __repr__(&self, py: Python<'_>) -> String {
        let preview = preview_text(&self.text, 40);
        let source_repr = self.source.borrow(py).__repr__();
        format!("Chunk(text={preview:?}, source={source_repr})")
    }

    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Py<PyAny>>> {
        let text_obj = self.text.clone().into_pyobject(py)?.unbind().into_any();
        let source_obj = self.source.clone_ref(py).into_any();
        Ok(vec![text_obj, source_obj])
    }
}

fn preview_text(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(max + 1);
    for (i, ch) in text.chars().enumerate() {
        if i >= max {
            out.push('\u{2026}');
            break;
        }
        out.push(ch);
    }
    out
}

// ---------------------------------------------------------------------------
// Module registration
// ---------------------------------------------------------------------------

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBoundingBox>()?;
    m.add_class::<PyChunkSource>()?;
    m.add_class::<PyChunk>()?;
    install_dataclass_shims(m)?;
    Ok(())
}

///  shim: attach `__dataclass_fields__` so `dataclasses.fields(obj)`
/// returns plausible field metadata. We use a minimal dict-of-name->type-
/// string; full `dataclasses.Field` synthesis lives in the Python stub
/// layer. The shim is enough for typing-style introspection that the
/// agent-ingest scripts in the spec lean on.
fn install_dataclass_shims(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let attach = |class_name: &str, fields: &[(&str, &str)]| -> PyResult<()> {
        let cls = m.getattr(class_name)?;
        let dict = PyDict::new(py);
        for (name, ty) in fields {
            dict.set_item(*name, *ty)?;
        }
        cls.setattr("__dataclass_fields__", dict)?;
        Ok(())
    };
    attach(
        "BoundingBox",
        &[
            ("x_min", "float"),
            ("y_min", "float"),
            ("x_max", "float"),
            ("y_max", "float"),
        ],
    )?;
    attach(
        "ChunkSource",
        &[
            ("page", "Optional[int]"),
            ("block_ids", "List[int]"),
            ("bbox", "Optional[BoundingBox]"),
        ],
    )?;
    attach("Chunk", &[("text", "str"), ("source", "ChunkSource")])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Pure-Rust strategy core (no pyo3)
// ---------------------------------------------------------------------------

/// Plain-data chunk produced by the strategy walkers. Converted to
/// `Py<PyChunk>` by `chunk_document`. Living below the pyo3 boundary lets
/// cargo test exercise the strategies without linking libpython.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawChunk {
    pub text: String,
    pub block_ids: Vec<u64>,
    pub page: Option<u32>,
    pub bbox: Option<BoundingBox>,
}

/// Pure-Rust dispatch over the five strategies.
pub(crate) fn build_chunks(doc: &Document, by: &str, size: usize) -> Result<Vec<RawChunk>, String> {
    match by {
        "page" => Ok(chunk_by_page_raw(doc)),
        "heading" => Ok(chunk_by_heading_raw(doc, size)),
        "section" => Ok(chunk_by_section_raw(doc, size)),
        "size" => Ok(chunk_by_size_raw(doc, size)),
        "semantic" => Ok(chunk_by_semantic_raw(doc, size)),
        _ => Err(format!(
            "unknown chunk strategy '{by}'; expected one of: page, heading, section, size, semantic"
        )),
    }
}

// ---------------------------------------------------------------------------
// Module-level entry point: chunk_document (pyo3 wrapper)
// ---------------------------------------------------------------------------

/// Walk the Document and emit chunks per the requested strategy. Used by
/// `PyDocument.text_chunks()` and `PyCorpus.chunks()`.
pub fn chunk_document(
    py: Python<'_>,
    doc: &Document,
    by: &str,
    size: usize,
) -> PyResult<Vec<Py<PyChunk>>> {
    let raw = build_chunks(doc, by, size).map_err(PyValueError::new_err)?;
    let mut out = Vec::with_capacity(raw.len());
    for chunk in raw {
        out.push(raw_to_py(py, chunk)?);
    }
    Ok(out)
}

fn raw_to_py(py: Python<'_>, chunk: RawChunk) -> PyResult<Py<PyChunk>> {
    let bbox_py = match chunk.bbox {
        Some(bb) => Some(Py::new(py, PyBoundingBox::from_rust(bb))?),
        None => None,
    };
    let source = Py::new(
        py,
        PyChunkSource {
            page: chunk.page,
            block_ids: chunk.block_ids,
            bbox: bbox_py,
        },
    )?;
    Py::new(
        py,
        PyChunk {
            text: chunk.text,
            source,
        },
    )
}

// ---------------------------------------------------------------------------
// Provenance helpers
// ---------------------------------------------------------------------------

/// Per-block provenance pulled from the presentation overlay: the page
/// index assignment + the bbox if known.
fn block_provenance(doc: &Document, id: NodeId) -> (Option<u32>, Option<BoundingBox>) {
    let Some(pres) = doc.presentation.as_ref() else {
        return (None, None);
    };
    let page = pres.page_assignments.get(id).copied().map(|p| p as u32);
    let bbox = pres.geometry.get(id).copied();
    (page, bbox)
}

fn merge_bbox(running: &mut Option<BoundingBox>, next: BoundingBox) {
    match running {
        Some(bb) => *bb = bb.merge(&next),
        None => *running = Some(next),
    }
}

/// Page-index reduction across a chunk. If every block reports the same
/// page, return that page; if any block has no page assignment OR the
/// pages differ, return None ("multi-page or unknown").
fn reduce_page(pages: &[Option<u32>]) -> Option<u32> {
    if pages.is_empty() {
        return None;
    }
    let first = pages[0]?;
    for p in &pages[1..] {
        match p {
            Some(v) if *v == first => {}
            _ => return None,
        }
    }
    Some(first)
}

#[derive(Default)]
struct ChunkBuilder {
    text: String,
    block_ids: Vec<u64>,
    pages: Vec<Option<u32>>,
    bbox: Option<BoundingBox>,
    /// `false` if any contributing block had no geometry; in that case
    /// the bbox is incomplete and we drop it on emit.
    bbox_complete: bool,
}

impl ChunkBuilder {
    fn new() -> Self {
        Self {
            bbox_complete: true,
            ..Default::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty() && self.block_ids.is_empty()
    }

    fn add(&mut self, doc: &Document, id: NodeId, text: &str) {
        if !self.text.is_empty() && !text.is_empty() {
            self.text.push_str("\n\n");
        }
        self.text.push_str(text);
        self.block_ids.push(id.value());
        let (page, bbox) = block_provenance(doc, id);
        self.pages.push(page);
        match bbox {
            Some(bb) => merge_bbox(&mut self.bbox, bb),
            None => self.bbox_complete = false,
        }
    }

    fn emit(&mut self) -> Option<RawChunk> {
        if self.is_empty() {
            return None;
        }
        let page = reduce_page(&self.pages);
        let bbox = if self.bbox_complete { self.bbox } else { None };
        let chunk = RawChunk {
            text: std::mem::take(&mut self.text),
            block_ids: std::mem::take(&mut self.block_ids),
            page,
            bbox,
        };
        self.pages.clear();
        self.bbox = None;
        self.bbox_complete = true;
        Some(chunk)
    }
}

// ---------------------------------------------------------------------------
// Strategy: page
// ---------------------------------------------------------------------------

/// One chunk per page. Page index comes from the presentation overlay
/// (`page_assignments`). Blocks without an assignment fall into a
/// trailing "unknown-page" chunk (page=None) so no content is dropped.
fn chunk_by_page_raw(doc: &Document) -> Vec<RawChunk> {
    let mut buckets: Vec<(Option<u32>, ChunkBuilder)> = Vec::new();
    for block in &doc.content {
        let id = block.id();
        let (page, _) = block_provenance(doc, id);
        let text = block.text();
        if text.is_empty() {
            continue;
        }
        let bucket_idx = match buckets.iter().position(|(p, _)| *p == page) {
            Some(i) => i,
            None => {
                buckets.push((page, ChunkBuilder::new()));
                buckets.len() - 1
            }
        };
        buckets[bucket_idx].1.add(doc, id, &text);
    }
    let mut out = Vec::with_capacity(buckets.len());
    for (_page, mut b) in buckets {
        if let Some(c) = b.emit() {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Strategy: heading
// ---------------------------------------------------------------------------

/// Split at heading boundaries, capped at `size` chars.
///
/// A new chunk starts whenever a `Block::Heading` is encountered (the
/// heading itself is the first block of the new chunk). If the running
/// chunk's text would exceed `size` after appending the next block, the
/// running chunk is closed first and the next block starts a fresh one.
fn chunk_by_heading_raw(doc: &Document, size: usize) -> Vec<RawChunk> {
    let mut out = Vec::new();
    let mut builder = ChunkBuilder::new();
    for block in &doc.content {
        let id = block.id();
        let text = block.text();
        if text.is_empty() {
            continue;
        }
        let is_heading = matches!(block, Block::Heading { .. });
        if is_heading && !builder.is_empty() {
            if let Some(c) = builder.emit() {
                out.push(c);
            }
        }
        if !builder.is_empty() && size > 0 && builder.text.len() + 2 + text.len() > size {
            if let Some(c) = builder.emit() {
                out.push(c);
            }
        }
        builder.add(doc, id, &text);
    }
    if let Some(c) = builder.emit() {
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// Strategy: section
// ---------------------------------------------------------------------------

/// Split at top-level sections (heading level 1 only). Same size-cap
/// behaviour as `heading`.
fn chunk_by_section_raw(doc: &Document, size: usize) -> Vec<RawChunk> {
    let mut out = Vec::new();
    let mut builder = ChunkBuilder::new();
    for block in &doc.content {
        let id = block.id();
        let text = block.text();
        if text.is_empty() {
            continue;
        }
        let is_h1 = matches!(block, Block::Heading { level, .. } if *level == 1);
        if is_h1 && !builder.is_empty() {
            if let Some(c) = builder.emit() {
                out.push(c);
            }
        }
        if !builder.is_empty() && size > 0 && builder.text.len() + 2 + text.len() > size {
            if let Some(c) = builder.emit() {
                out.push(c);
            }
        }
        builder.add(doc, id, &text);
    }
    if let Some(c) = builder.emit() {
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// Strategy: size
// ---------------------------------------------------------------------------

/// Fixed size chunks at sentence boundaries (split on `[.!?]\s+`). The
/// chunk may exceed `size` if a single sentence is longer than the cap.
/// Block boundaries are still respected for provenance: each emitted
/// chunk records exactly the set of block IDs whose sentences contributed.
fn chunk_by_size_raw(doc: &Document, size: usize) -> Vec<RawChunk> {
    let cap = if size == 0 { usize::MAX } else { size };
    let mut out = Vec::new();
    let mut builder = ChunkBuilder::new();
    for block in &doc.content {
        let id = block.id();
        let text = block.text();
        if text.is_empty() {
            continue;
        }
        let (page, bbox) = block_provenance(doc, id);
        let mut block_pushed = false;
        for sentence in split_sentences(&text) {
            let sentence = sentence.trim();
            if sentence.is_empty() {
                continue;
            }
            let added_len = if builder.text.is_empty() {
                sentence.len()
            } else {
                builder.text.len() + 1 + sentence.len()
            };
            if added_len > cap && !builder.is_empty() {
                if let Some(c) = builder.emit() {
                    out.push(c);
                }
                block_pushed = false;
            }
            if !block_pushed {
                builder.block_ids.push(id.value());
                builder.pages.push(page);
                match bbox {
                    Some(bb) => merge_bbox(&mut builder.bbox, bb),
                    None => builder.bbox_complete = false,
                }
                block_pushed = true;
            }
            if !builder.text.is_empty() {
                builder.text.push(' ');
            }
            builder.text.push_str(sentence);
        }
    }
    if let Some(c) = builder.emit() {
        out.push(c);
    }
    out
}

/// Split text on sentence terminators followed by whitespace. Returns
/// borrowed substrings including the terminator.
fn split_sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'.' | b'!' | b'?') {
            let mut j = i + 1;
            while j < bytes.len() && matches!(bytes[j], b'.' | b'!' | b'?') {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_whitespace() {
                out.push(&text[start..j]);
                let mut k = j;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                start = k;
                i = k;
                continue;
            }
        }
        i += 1;
    }
    if start < bytes.len() {
        out.push(&text[start..]);
    }
    out
}

// ---------------------------------------------------------------------------
// Strategy: semantic
// ---------------------------------------------------------------------------

/// Split on paragraph boundaries (every `Block::Paragraph` is a unit),
/// accumulate paragraphs until size is reached, then emit. Headings end
/// the running chunk (so a heading and its body stay together) but do
/// not themselves trigger size-cap: headings are short.
fn chunk_by_semantic_raw(doc: &Document, size: usize) -> Vec<RawChunk> {
    let cap = if size == 0 { usize::MAX } else { size };
    let mut out = Vec::new();
    let mut builder = ChunkBuilder::new();
    for block in &doc.content {
        let id = block.id();
        let text = block.text();
        if text.is_empty() {
            continue;
        }
        let is_paragraph = matches!(block, Block::Paragraph { .. });
        if is_paragraph && !builder.is_empty() && builder.text.len() + 2 + text.len() > cap {
            if let Some(c) = builder.emit() {
                out.push(c);
            }
        }
        builder.add(doc, id, &text);
    }
    if let Some(c) = builder.emit() {
        out.push(c);
    }
    out
}

// `extract_inline_text` is referenced for symmetry with the markdown emitter
// boundaries; the chunker uses `Block::text()` directly because it walks
// whole blocks. Keep the import live so refactors that move helpers around
// stay honest.
#[allow(dead_code)]
fn _unused_inline_helper(spans: &[udoc_facade::Inline]) -> String {
    extract_inline_text(spans)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "extension-module")))]
mod tests {
    use super::*;
    use udoc_facade::{Block, Inline, NodeId, PageDef, Presentation, SpanStyle};

    fn text_inline(id: u64, text: &str) -> Inline {
        Inline::Text {
            id: NodeId::new(id),
            text: text.into(),
            style: SpanStyle::default(),
        }
    }

    fn paragraph(id: u64, text: &str) -> Block {
        Block::Paragraph {
            id: NodeId::new(id),
            content: vec![text_inline(id + 10_000, text)],
        }
    }

    fn heading(id: u64, level: u8, text: &str) -> Block {
        Block::Heading {
            id: NodeId::new(id),
            level,
            content: vec![text_inline(id + 10_000, text)],
        }
    }

    fn doc_of(blocks: Vec<Block>) -> Document {
        let mut doc = Document::new();
        doc.content = blocks;
        doc.metadata.page_count = 1;
        doc
    }

    fn doc_with_pages(blocks: Vec<Block>, assignments: &[(u64, usize)]) -> Document {
        let mut pres = Presentation::default();
        let max_page = assignments.iter().map(|(_, p)| *p).max().unwrap_or(0);
        for i in 0..=max_page {
            pres.pages.push(PageDef::new(i, 612.0, 792.0, 0));
        }
        for (nid, page) in assignments {
            pres.page_assignments.set(NodeId::new(*nid), *page);
        }
        let mut doc = Document::new();
        doc.content = blocks;
        doc.presentation = Some(pres);
        doc.metadata.page_count = max_page + 1;
        doc
    }

    fn chunk_texts(chunks: &[RawChunk]) -> Vec<String> {
        chunks.iter().map(|c| c.text.clone()).collect()
    }

    #[test]
    fn test_chunk_by_page_one_per_page() {
        let doc = doc_with_pages(
            vec![
                paragraph(1, "alpha one"),
                paragraph(2, "alpha two"),
                paragraph(3, "beta one"),
            ],
            &[(1, 0), (2, 0), (3, 1)],
        );
        let chunks = build_chunks(&doc, "page", 0).unwrap();
        assert_eq!(chunks.len(), 2);
        let texts = chunk_texts(&chunks);
        assert!(texts[0].contains("alpha one"));
        assert!(texts[0].contains("alpha two"));
        assert_eq!(chunks[0].page, Some(0));
        assert_eq!(chunks[1].page, Some(1));
        assert_eq!(chunks[0].block_ids, vec![1u64, 2u64]);
    }

    #[test]
    fn test_chunk_by_heading_breaks_at_heading() {
        let doc = doc_of(vec![
            heading(1, 1, "Intro"),
            paragraph(2, "body of intro"),
            heading(3, 2, "Methods"),
            paragraph(4, "body of methods"),
        ]);
        let chunks = build_chunks(&doc, "heading", 10_000).unwrap();
        assert_eq!(chunks.len(), 2);
        let texts = chunk_texts(&chunks);
        assert!(texts[0].contains("Intro"));
        assert!(texts[0].contains("body of intro"));
        assert!(texts[1].contains("Methods"));
        assert!(texts[1].contains("body of methods"));
    }

    #[test]
    fn test_chunk_by_heading_size_cap() {
        let mut blocks = vec![heading(1, 1, "Big")];
        for i in 0..10u64 {
            blocks.push(paragraph(2 + i, "lorem ipsum dolor sit amet"));
        }
        let doc = doc_of(blocks);
        let chunks = build_chunks(&doc, "heading", 80).unwrap();
        assert!(
            chunks.len() > 2,
            "expected size cap to force multiple chunks, got {}",
            chunks.len()
        );
        for chunk in &chunks {
            assert!(
                chunk.text.len() < 200,
                "chunk too big: {}",
                chunk.text.len()
            );
        }
    }

    #[test]
    fn test_chunk_by_section_only_h1() {
        let doc = doc_of(vec![
            heading(1, 1, "Part One"),
            paragraph(2, "body one"),
            heading(3, 2, "Subsection"),
            paragraph(4, "still part one"),
            heading(5, 1, "Part Two"),
            paragraph(6, "body two"),
        ]);
        let chunks = build_chunks(&doc, "section", 10_000).unwrap();
        assert_eq!(chunks.len(), 2, "section breaks only on H1");
        let texts = chunk_texts(&chunks);
        assert!(texts[0].contains("Part One"));
        assert!(texts[0].contains("Subsection"));
        assert!(texts[0].contains("still part one"));
        assert!(texts[1].contains("Part Two"));
    }

    #[test]
    fn test_chunk_by_size_sentence_boundary() {
        let doc = doc_of(vec![paragraph(
            1,
            "First sentence here. Second one is longer but ok. Third closes.",
        )]);
        let chunks = build_chunks(&doc, "size", 50).unwrap();
        assert!(chunks.len() >= 2, "expected sentence-driven splits");
        let texts = chunk_texts(&chunks);
        for t in &texts {
            assert!(!t.is_empty());
            assert!(!t.starts_with(' '));
        }
        let joined: String = texts.join(" ");
        assert!(joined.contains("First sentence here."));
        assert!(joined.contains("Second one is longer but ok."));
        assert!(joined.contains("Third closes."));
    }

    #[test]
    fn test_chunk_by_semantic_paragraph_boundary() {
        let doc = doc_of(vec![
            paragraph(1, "para one"),
            paragraph(2, "para two"),
            paragraph(3, "para three"),
            paragraph(4, "para four"),
        ]);
        let chunks = build_chunks(&doc, "semantic", 20).unwrap();
        assert!(
            chunks.len() >= 2,
            "expected paragraph-driven splits, got {}",
            chunks.len()
        );
        for chunk in &chunks {
            assert!(chunk.text.starts_with("para "));
        }
    }

    #[test]
    fn test_chunk_unknown_strategy_raises() {
        let doc = doc_of(vec![paragraph(1, "hello")]);
        let err = build_chunks(&doc, "nonsense", 100).unwrap_err();
        assert!(err.contains("nonsense"), "msg: {err}");
        assert!(err.contains("page"));
        assert!(err.contains("semantic"));
    }

    #[test]
    fn test_chunk_source_provenance() {
        let doc = doc_with_pages(
            vec![paragraph(1, "alpha"), paragraph(2, "beta")],
            &[(1, 3), (2, 3)],
        );
        let chunks = build_chunks(&doc, "page", 0).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].page, Some(3));
        assert_eq!(chunks[0].block_ids, vec![1u64, 2u64]);
    }

    #[test]
    fn test_chunk_source_multipage_collapses_to_none() {
        // Same chunk spanning two pages -> page = None on the merged chunk.
        // Use the size strategy with a high cap so all blocks land in
        // one chunk, even though they are on different pages.
        let doc = doc_with_pages(
            vec![
                paragraph(1, "first sentence."),
                paragraph(2, "second sentence."),
            ],
            &[(1, 0), (2, 1)],
        );
        let chunks = build_chunks(&doc, "size", 1_000).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].page, None, "multi-page should reduce to None");
    }

    #[test]
    fn test_bbox_area() {
        let bb = PyBoundingBox {
            x_min: 0.0,
            y_min: 0.0,
            x_max: 10.0,
            y_max: 5.0,
        };
        assert!((bb.width() - 10.0).abs() < 1e-9);
        assert!((bb.height() - 5.0).abs() < 1e-9);
        assert!((bb.area() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_bbox_repr() {
        let bb = PyBoundingBox {
            x_min: 1.0,
            y_min: 2.0,
            x_max: 3.0,
            y_max: 4.0,
        };
        assert_eq!(
            bb.__repr__(),
            "BoundingBox(x_min=1, y_min=2, x_max=3, y_max=4)"
        );
    }

    #[test]
    fn test_split_sentences_helper() {
        let s = "One. Two! Three? four";
        let out = split_sentences(s);
        assert_eq!(out, vec!["One.", "Two!", "Three?", "four"]);
        let out2 = split_sentences("no boundary here");
        assert_eq!(out2, vec!["no boundary here"]);
        let out3 = split_sentences("end.");
        assert_eq!(out3, vec!["end."]);
    }

    #[test]
    fn test_preview_text_truncates() {
        let s = "abcdefghijklmnopqrstuvwxyz";
        // max=5 -> "abcde" + ellipsis
        let p = preview_text(s, 5);
        assert!(p.contains("abcde"));
        assert!(p.contains('\u{2026}'));
        // shorter than max -> full text, no ellipsis
        let p2 = preview_text("abc", 10);
        assert_eq!(p2, "abc");
    }
}
