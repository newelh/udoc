# Benchmarks

Performance numbers for udoc against the alternatives, per format
and per task. This page is a stub: the benchmark matrix is laid
out so contributors know what to measure; the numbers themselves
will be filled in once the harness lands.

## Methodology

Benchmarks run on two reference machines so that the numbers
reflect real-world hardware variation rather than a single CPU's
quirks:

| Host        | CPU                          | Memory  | OS                             |
|-------------|------------------------------|---------|--------------------------------|
| `linux-x64` | (TBD) AMD64 desktop          | TBD     | Linux 6.8 (Ubuntu 24.04)       |
| `mac-arm64` | Apple M1                     | TBD     | macOS (latest available)       |

Each benchmark run reports:

- Median wall time over N iterations (N to be set per benchmark).
- Throughput (pages/sec or MB/sec, depending on the workload).
- Peak resident set size during the run.
- Any output-quality metric that applies (character accuracy,
  SSIM, etc).

Benchmarks are not strictly apples-to-apples — tools differ in
what they extract, in default DPI, in rendering profile, and in
how strictly they reject bad input. Differences are noted per
tool. Where a tool has a knob that materially changes the answer
(e.g. `pdftotext -layout`), both modes are reported.

## Corpus

A standing benchmark corpus lives in (TBD path / link). It mixes:

- **Born-digital PDFs** — research papers (LaTeX), enterprise
  reports (Word print), marketing collateral (InDesign).
- **Scanned PDFs** — government records, legal exhibits, pre-
  digital archives. Used for OCR-related benchmarks, not for raw
  text-extraction comparisons.
- **Hybrid PDFs** — body pages digital, inserts scanned. The
  realistic input that exercises per-page detection.
- **Mixed Office documents** — DOCX, XLSX, PPTX, plus the legacy
  binary formats (DOC, XLS, PPT) and OpenDocument equivalents.
- **Stress cases** — pathological inputs (deeply nested tables,
  thousand-page reports, gigabyte-scale PDFs) used to exercise
  resource limits and memory budgeting rather than throughput.

Per-document provenance and a license review are documented
alongside the corpus. Documents that cannot be redistributed are
referenced by source and replayed locally.

## PDF: text extraction

Comparisons against the common open-source PDF text extractors:

| Tool         | Source          | Notes                                                                 |
|--------------|-----------------|-----------------------------------------------------------------------|
| `udoc`       | this project    | Default reading-order tier auto-selected.                             |
| `pdftotext`  | poppler         | Both default and `-layout` mode reported separately.                  |
| `pdfium`     | Chromium        | Reference for "what the browser sees."                                |
| `mupdf`      | Artifex         | `mutool extract` / `mutool convert -F txt`.                           |
| `pdfminer`   | python          | Pure-Python baseline for the Python ecosystem.                        |
| `pymupdf`    | wrapper on mupdf| Python convenience baseline; same engine as `mupdf`.                  |

### Character accuracy

Accuracy is measured against a hand-curated ground-truth subset
of the corpus (TBD count). Metric: edit distance per 1k characters
between extracted text and the reference, after Unicode NFC
normalisation and whitespace collapsing.

| Document class      | udoc | pdftotext (default) | pdftotext (-layout) | pdfium | mupdf | pdfminer | pymupdf |
|---------------------|------|---------------------|---------------------|--------|-------|----------|---------|
| Research papers     | TBD  | TBD                 | TBD                 | TBD    | TBD   | TBD      | TBD     |
| Reports (Word PDF)  | TBD  | TBD                 | TBD                 | TBD    | TBD   | TBD      | TBD     |
| Marketing / InDesign| TBD  | TBD                 | TBD                 | TBD    | TBD   | TBD      | TBD     |
| Multi-column        | TBD  | TBD                 | TBD                 | TBD    | TBD   | TBD      | TBD     |
| Tables-heavy        | TBD  | TBD                 | TBD                 | TBD    | TBD   | TBD      | TBD     |

### Throughput

Throughput is measured as MB/sec of input PDF processed, summed
over the corpus. Single-threaded; the parallel-throughput numbers
are reported separately under Batch processing.

| Host        | udoc | pdftotext | pdfium | mupdf | pdfminer | pymupdf |
|-------------|------|-----------|--------|-------|----------|---------|
| `linux-x64` | TBD  | TBD       | TBD    | TBD   | TBD      | TBD     |
| `mac-arm64` | TBD  | TBD       | TBD    | TBD   | TBD      | TBD     |

### Reading order

Reading-order quality is measured separately because edit distance
on text extracted in the wrong order can still score well. Metric:
sequence alignment error against the ground-truth reading order on
a multi-column subset of the corpus.

Tier breakdown is reported alongside, since udoc's reading-order
pipeline is a four-tier cascade (see [PDF format
guide](formats/pdf.md#reading-order)) and the picked tier matters
for interpreting the result.

## PDF: rendering

Comparisons against the common open-source PDF rasterisers:

| Tool      | Source        | Notes                                                              |
|-----------|---------------|--------------------------------------------------------------------|
| `udoc`    | this project  | Both `viewer` and `ocr` profiles reported.                         |
| `poppler` | `pdftoppm`    | Default cairo backend.                                             |
| `pdfium`  | Chromium      | The "what the browser sees" reference.                             |
| `mupdf`   | Artifex       | `mutool draw -r <dpi> -o page-%d.png`.                             |

### SSIM

Structural similarity index against the chosen reference at a
given DPI. The reference rasteriser is selected per document
class (typically `mupdf` for born-digital, since it tracks the PDF
spec most strictly).

| Document class      | DPI | udoc (viewer) | udoc (ocr) | poppler | pdfium | mupdf (ref) |
|---------------------|-----|---------------|------------|---------|--------|-------------|
| Research papers     | 150 | TBD           | TBD        | TBD     | TBD    | 1.000       |
| Research papers     | 300 | TBD           | TBD        | TBD     | TBD    | 1.000       |
| Reports             | 150 | TBD           | TBD        | TBD     | TBD    | 1.000       |
| Marketing           | 150 | TBD           | TBD        | TBD     | TBD    | 1.000       |
| Forms (AcroForm)    | 150 | TBD           | TBD        | TBD     | TBD    | 1.000       |

### Render speed

Pages per second at a fixed DPI, single-threaded.

| Host        | DPI | udoc (viewer) | udoc (ocr) | poppler | pdfium | mupdf |
|-------------|-----|---------------|------------|---------|--------|-------|
| `linux-x64` | 150 | TBD           | TBD        | TBD     | TBD    | TBD   |
| `linux-x64` | 300 | TBD           | TBD        | TBD     | TBD    | TBD   |
| `mac-arm64` | 150 | TBD           | TBD        | TBD     | TBD    | TBD   |
| `mac-arm64` | 300 | TBD           | TBD        | TBD     | TBD    | TBD   |

## PDF: tables

Table detection is heuristic across all of these tools; metrics
have to be careful about what counts as a "correct" table. The
metric here is per-cell content match against a ground-truth
table set.

| Tool            | Source               | Notes                                                                  |
|-----------------|----------------------|------------------------------------------------------------------------|
| `udoc`          | this project         | Built-in lattice + text-edge strategies merged.                        |
| `pdfplumber`    | python wrapper       | The pdfminer.six-based table extractor used in the Python ecosystem.   |
| `tabula`        | java                 | The table-extraction reference for many ETL pipelines.                 |
| `camelot`       | python               | Both `lattice` and `stream` flavours.                                  |

| Table style        | udoc | pdfplumber | tabula | camelot (lattice) | camelot (stream) |
|--------------------|------|------------|--------|-------------------|------------------|
| Ruled, simple      | TBD  | TBD        | TBD    | TBD               | TBD              |
| Ruled, merged      | TBD  | TBD        | TBD    | TBD               | TBD              |
| Unruled, columns   | TBD  | TBD        | TBD    | TBD               | TBD              |
| Rotated headers    | TBD  | TBD        | TBD    | TBD               | TBD              |
| Multi-page         | TBD  | TBD        | TBD    | TBD               | TBD              |

## DOCX / XLSX / PPTX

For the modern OOXML stack, the comparison set is the
language-native libraries:

| Format | Reference tools                                                      |
|--------|----------------------------------------------------------------------|
| DOCX   | `python-docx`, `mammoth` (DOCX-to-HTML), `docx2txt`, `pandoc`        |
| XLSX   | `openpyxl`, `xlsx2csv`, `libreoffice --headless --convert-to csv`    |
| PPTX   | `python-pptx`, `pptx2text`, `libreoffice --headless --convert-to txt`|

Per-format throughput, character accuracy, and table-quality
numbers will follow the same per-host, per-document-class shape
as the PDF tables above.

## Legacy binary Office (DOC / XLS / PPT)

The legacy formats are where most modern tooling either falls
back on a LibreOffice subprocess or fails outright. Comparisons:

| Tool                                    | Notes                                                              |
|-----------------------------------------|--------------------------------------------------------------------|
| `udoc`                                  | In-tree Rust parser per format.                                    |
| `antiword` (DOC)                        | The classic stdout-only DOC reader. No table support.              |
| `catdoc` (DOC / XLS / PPT)              | Single-binary suite. Older Unicode handling.                       |
| `libreoffice --headless --convert-to`   | The general-purpose subprocess fallback. Heavy startup cost.       |
| `python-oletools`                       | Forensic-grade access; not optimised for throughput.               |

The interesting axes here are throughput (LibreOffice startup
cost is a dominant factor for short documents), character
accuracy (codepage decoding correctness), and structural
recovery on fast-saved DOC files.

## Hooks and OCR

OCR throughput is a function of the OCR engine, not udoc — but
the per-page overhead udoc adds (page render, base64, JSONL,
sequence-number bookkeeping) is worth measuring on its own. The
microbenchmark:

- A no-op OCR hook that returns empty text immediately.
- Measures udoc-side overhead per page.
- Reported for `viewer` and `ocr` render profiles at 150 / 300
  DPI.

End-to-end OCR throughput numbers are reported for the example
hooks shipped under `examples/hooks/`:

| Hook                  | OCR engine        | Notes                                          |
|-----------------------|-------------------|------------------------------------------------|
| `tesseract-hook`      | Tesseract 5       | CPU baseline.                                  |
| `glm-ocr-hook`        | GLM-OCR           | GPU; numbers reported on a CUDA-equipped host. |
| `deepseek-ocr-hook`   | DeepSeek-OCR      | GPU; same caveat.                              |

## Font engine (`udoc-font`)

Per-task microbenchmarks against FreeType, the reference font
engine for the open-source ecosystem:

| Task                                | udoc-font | FreeType | Notes                                                       |
|-------------------------------------|-----------|----------|-------------------------------------------------------------|
| TrueType glyph outline (cold cache) | TBD       | TBD      | Synthetic load: 10k random glyphs from a representative TTF.|
| TrueType glyph outline (warm cache) | TBD       | TBD      | Same load, second pass.                                     |
| CFF glyph outline                   | TBD       | TBD      | OTF font fixture.                                           |
| Type 1 outline                      | TBD       | TBD      | Legacy PostScript fixture (PDF-embedded).                   |
| ToUnicode CMap parse                | TBD       | n/a      | Specific to PDF; no FreeType comparison.                    |
| `cmap` table lookup                 | TBD       | TBD      | 1k character codes against a representative font.           |
| Auto-hinter (single glyph)          | TBD       | TBD      | Software auto-hinter for unhinted CFF / Type 1.             |

## Image decoders (`udoc-image`)

Per-format throughput against the commonly-used decoder per
codec. The PDF use case is what motivates this crate, so the
benchmark inputs are representative PDF-embedded images rather
than standalone files.

| Codec   | udoc-image | Reference          | Notes                                                       |
|---------|-----------|--------------------|-------------------------------------------------------------|
| CCITT   | TBD       | `libtiff` / Group4 | Single-bit fax-style scans common in older PDFs.            |
| JBIG2   | TBD       | `jbig2dec`         | Compressed scans common in archive PDFs.                    |
| JPEG    | TBD       | `libjpeg-turbo`    | The default colour-image codec.                             |
| JPEG 2000 | TBD     | `OpenJPEG`         | Used in high-quality archive PDFs and some scan workflows.  |

Per-codec metrics: decode throughput (MB/sec), peak RSS during
decode, and a small correctness suite (bit-exact against the
reference for JPEG / CCITT; SSIM against the reference for
lossy decoders).

## Batch processing

End-to-end throughput for a representative ingest workload:
N documents, mixed formats, processed by `udoc.Corpus.parallel(...)`
on each host. Reported as documents/sec and MB/sec, with peak RSS
across the worker pool.

| Host        | Workers | Mode      | docs/sec | MB/sec | peak RSS |
|-------------|---------|-----------|----------|--------|----------|
| `linux-x64` | 1       | inline    | TBD      | TBD    | TBD      |
| `linux-x64` | 4       | thread    | TBD      | TBD    | TBD      |
| `linux-x64` | 4       | process   | TBD      | TBD    | TBD      |
| `linux-x64` | 16      | process   | TBD      | TBD    | TBD      |
| `mac-arm64` | 1       | inline    | TBD      | TBD    | TBD      |
| `mac-arm64` | 4       | thread    | TBD      | TBD    | TBD      |
| `mac-arm64` | 8       | process   | TBD      | TBD    | TBD      |

The interesting cross-cuts:

- Where the GIL still bites (`thread` mode trends).
- The crossover point between thread and process mode.
- The `Config(memory_budget=...)` setting's effect on peak RSS at
  high worker counts.

## Reproducing

The benchmark harness lives at (TBD `scripts/bench/`). It wraps
each tool in a uniform driver and emits CSV output suitable for
Pandas / DuckDB analysis. To reproduce on a new host:

```bash
# (TBD harness invocation)
scripts/bench/run.sh --corpus path/to/corpus --out results/
```

Pull requests adding tools to the comparison set are welcome —
keep the comparison apples-to-apples, document any tool-specific
flags in the harness, and call out where a tool's defaults are
materially different from `udoc`'s.

## Caveats

- Numbers are wall-clock medians on otherwise idle hosts. CI-host
  numbers are not reported because they vary too much.
- udoc's defaults are tuned for correctness over speed; flipping
  off overlays via `Config.layers` materially improves throughput
  for callers that only need text. The "udoc default" column is
  the conservative one; an "udoc text-only" column is added where
  it changes the picture.
- Memory measurements use peak RSS as observed by the OS. RSS
  understates actual cost when the kernel pages cleanly; the
  benchmark harness pins workloads to a single NUMA node on
  `linux-x64` to keep that consistent.
- Quality metrics are noisy on small samples. Confidence intervals
  are reported alongside the medians where the sample size makes
  them meaningful.
