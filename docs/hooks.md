# Hooks and LLM integration

Hooks let you wire external programs into udoc's extraction pipeline.
Anything that can read and write JSON line by line can participate. The
common cases are OCR engines for scanned pages, layout-detection models
for PDF reading order, and entity extractors that enrich page content
with structured metadata.

## The pipeline

```
            ┌──────────┐    ┌──────────┐    ┌────────────┐
udoc parse ─┤   OCR    ├───▶│  Layout  ├───▶│  Annotate  ├──▶ Document
            └──────────┘    └──────────┘    └────────────┘
              hook[0]         hook[0]         hook[0]
              hook[1]         hook[1]         hook[1]
              ...             ...             ...
```

Each phase is optional. Phases with no hook attached are no-ops. Within
a phase, hooks chain — output of hook *N* becomes input to hook *N+1*.
The result of the last hook in a phase feeds the first hook of the next
phase.

| Phase      | Input                                  | Typical use                          |
|------------|----------------------------------------|--------------------------------------|
| `ocr`      | Page image (PNG/JPEG)                  | Tesseract, GLM-OCR, DeepSeek-OCR     |
| `layout`   | Page text + positions + image          | DocLayout-YOLO, region detectors     |
| `annotate` | Page text + structured layout          | Entity extractors, classifiers, NER  |

OCR runs first because layout often wants reliable text. Layout runs
next because annotation often wants regions. Annotation runs last and
stamps the final structure with metadata.

## CLI

```bash
udoc --ocr "tesseract-hook" scanned.pdf
udoc --layout "doclayout-yolo" paper.pdf
udoc --annotate "ner-hook" paper.pdf

# Chain multiple hooks within a phase
udoc --ocr "tesseract-hook" --ocr "fix-i18n-hook" scanned.pdf

# Combine phases
udoc --ocr "tesseract-hook" --layout "doclayout-yolo" --annotate "ner-hook" paper.pdf
```

The flag accepts any executable on `$PATH` or an absolute path. udoc
spawns the process, sends a handshake, and pipes one JSON request per
line on stdin. The hook writes one JSON response per line on stdout.

## Library

Using Python

```python
import udoc

cfg = udoc.Config(
    hooks=[
        udoc.Hook(phase="ocr", command="tesseract-hook"),
        udoc.Hook(phase="layout", command="doclayout-yolo"),
    ]
)
doc = udoc.extract("scanned.pdf", config=cfg)
```


<details>
<summary>Rust equivalent</summary>

```rust
use udoc::hooks::{HookSpec, Phase};

let cfg = udoc::Config::new()
    .hook(HookSpec::new(Phase::Ocr, "tesseract-hook"))
    .hook(HookSpec::new(Phase::Layout, "doclayout-yolo"));

let doc = udoc::extract_with("scanned.pdf", cfg)?;
# Ok::<(), udoc::Error>(())
```
</details>

## A working hook, end to end

This is a complete OCR hook in 30 lines of Python that wraps Tesseract.
Save it as `tesseract-hook`, mark it executable, run with `udoc --ocr
./tesseract-hook scanned.pdf`.

```python
#!/usr/bin/env python3
"""Tesseract OCR hook for udoc.

Reads JSONL page requests on stdin, writes JSONL responses on stdout.
The first line written is the handshake declaring our protocol id and
capabilities; udoc reads that, then sends one request per page.
"""
import base64
import json
import subprocess
import sys
import tempfile

# 1. Handshake. udoc reads exactly one line; if it does not parse or
#    the protocol id is wrong, the hook is killed with a clear error.
sys.stdout.write(json.dumps({
    "protocol":     "udoc-hook-v1",
    "capabilities": ["ocr"],
    "needs":        ["image"],
    "provides":     ["spans"],
}) + "\n")
sys.stdout.flush()

# 2. Request loop. One request per line on stdin; one response per line
#    on stdout. udoc closes stdin when the document is done; we drain
#    any pending work and exit cleanly.
for line in sys.stdin:
    req = json.loads(line)
    seq = req["seq"]

    # The image is base64 PNG. Tesseract wants a real file.
    with tempfile.NamedTemporaryFile(suffix=".png") as png:
        png.write(base64.b64decode(req["image"]))
        png.flush()
        result = subprocess.run(
            ["tesseract", png.name, "stdout", "--psm", "1"],
            capture_output=True, text=True, check=True,
        )

    sys.stdout.write(json.dumps({
        "seq":  seq,
        "text": result.stdout,
    }) + "\n")
    sys.stdout.flush()
```

The same shape works for any model: HTTP API, in-process inference, an
external CLI. As long as you can write JSON to stdout for each request
on stdin, you can plug in.

Six longer examples live under
[`examples/hooks/`](https://github.com/newelh/udoc/tree/main/examples/hooks)
covering Tesseract, GLM-OCR, DeepSeek-OCR, DocLayout-YOLO, NER models,
and a cloud OCR mock you can adapt to AWS Textract or Google Cloud
Vision.

## Long-running and async work

Hooks are long-lived processes — udoc spawns each hook **once per
extraction** and reuses it across every page of the input document.
Model setup cost amortises across the whole document, not per page.

For genuinely async work like cloud OCR (Textract, Document AI, Azure
Form Recognizer) where the model returns immediately and you poll for
the actual result, the hook process owns the polling. Pattern:

```python
for line in sys.stdin:
    req = json.loads(line)
    seq = req["seq"]

    # Submit. The provider returns a job id immediately.
    job = textract.start_document_text_detection(
        Document={"Bytes": base64.b64decode(req["image"])}
    )

    # Poll until done. udoc has no opinion on how long this takes;
    # the per-request timeout is configurable on Config.hooks_config
    # (default 60 s; raise for slow providers).
    while True:
        result = textract.get_document_text_detection(JobId=job["JobId"])
        if result["JobStatus"] in ("SUCCEEDED", "FAILED"):
            break
        time.sleep(1)

    if result["JobStatus"] == "SUCCEEDED":
        sys.stdout.write(json.dumps({"seq": seq, "text": flatten(result)}) + "\n")
    else:
        sys.stdout.write(json.dumps({"seq": seq, "error": {
            "kind": "provider_failure", "message": result.get("StatusMessage", ""),
        }}) + "\n")
    sys.stdout.flush()
```

If a hook outruns its per-request timeout, udoc kills the process and
emits a `HookTimeout` diagnostic on the affected page; the extraction
continues with remaining pages.

## Security

The hook protocol is intentionally simple, which means trust is on you.

- **Hook processes are not sandboxed.** udoc spawns the binary you
  named, with the credentials and filesystem access of the calling
  process. If you run hooks from untrusted sources, sandbox them
  yourself (seccomp, container, separate user, etc.).
- **The I/O channel is bounded.** Per-request timeout (default 60 s,
  configurable), per-line size cap, and a per-page response budget
  prevent one rogue hook from hanging an entire batch — but they do
  not constrain what the hook does to the system while it is running.
- **No environment leakage by default.** Hooks inherit the calling
  process's environment (PATH, locale). Pass an explicit env via
  `Hook(env={...})` if you want to restrict what the hook sees.
- **Document images leave your process.** Hooks receive the rendered
  page image as base64 PNG. If your document content is sensitive and
  your hook ships data off-host (cloud OCR), that's a data-egress
  decision you are making — udoc does not warn about it because there
  is no way for udoc to know which hooks are network-bound.
- **Stdin/stdout/stderr are the contract.** Hooks should not assume any
  other channel is available. udoc captures stderr (truncated to a
  configurable cap) and surfaces it on diagnostic warnings; do not put
  load-bearing output there.

If you ship hooks for others to use, document the resources they touch
(network endpoints, GPU, local files) and any environment variables
they read. Treat your hook the way you'd treat any other long-running
subprocess in your pipeline.

## Protocol reference

Each hook is a long-lived subprocess that follows this lifecycle:

1. **Startup.** udoc spawns the process. The hook writes one handshake
   line to stdout (see [handshake fields](#handshake-fields) below):
   ```json
   {"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["image"],"provides":["spans"]}
   ```
   udoc reads the handshake; if it does not arrive within the
   configurable startup timeout (default 5 s) the hook is killed and
   the extraction fails fast. A wrong `protocol` value is rejected
   with a clear error rather than silently demoted to the default OCR
   shape.
2. **Request loop.** udoc writes one JSON request per line to the
   hook's stdin. Each request has a sequence number, the page index,
   and a payload (varies by phase). The hook writes one JSON response
   per line to stdout, with the matching sequence number.
3. **Shutdown.** When udoc has no more pages to send, it closes the
   hook's stdin. The hook should drain any pending work, write its
   final responses, and exit cleanly.

### Handshake fields

The four fields in the handshake line are all required. Each is a
fixed enum — pass an unrecognised value and the hook is rejected.

#### `protocol` (string)

Exactly `"udoc-hook-v1"`. This is the wire-format version. A future
incompatible protocol revision would bump the suffix; udoc will only
accept hooks that name the version it was built against. There is no
auto-negotiation.

#### `capabilities` (array of string)

Which phase(s) the hook participates in. Order matters: when a hook
declares more than one capability, udoc pins it to the first one in
this priority order.

| Value         | Phase     | Meaning                                                       |
|---------------|-----------|---------------------------------------------------------------|
| `"ocr"`       | OCR       | Hook produces text (and optionally spans) from a page image.  |
| `"layout"`    | Layout    | Hook labels regions on the page (titles, figures, tables).    |
| `"annotate"`  | Annotate  | Hook stamps entities, classifications, or metadata.           |

A hook that wraps Tesseract for OCR declares `["ocr"]`. A hook that
detects layout regions and also wants to refine OCR output declares
`["ocr", "layout"]` and runs in the OCR phase (the earlier capability
wins).

#### `needs` (array of string)

What inputs the hook wants in each request. udoc only sends the
fields the hook asks for; everything else is omitted to keep request
payloads small.

| Value          | Meaning                                                                  |
|----------------|--------------------------------------------------------------------------|
| `"image"`      | The rendered page as a base64 PNG, plus its DPI.                         |
| `"spans"`      | Positioned text spans (whatever was extracted or produced by earlier OCR).|
| `"blocks"`     | The current `Block` tree for the page (paragraphs, headings, tables).    |
| `"text"`       | Plain text reconstruction of the page.                                   |
| `"document"`   | The whole document in one request rather than per-page (see below).      |

`"document"` is special. When a hook needs whole-document context
(e.g. cross-page entity resolution), declaring `"document"` switches
udoc from per-page request loop to one-shot delivery: udoc sends a
single request shaped
`{"document_path": "...", "page_count": N, "format": "pdf"}` and
expects a response shaped
`{"pages": [{"page_index": 0, "spans": [...]}, ...]}`. Use sparingly;
it disables streaming and forces the hook to handle the whole
document at once.

#### `provides` (array of string)

What the hook produces. udoc routes the response fields back into
the document model based on these.

| Value         | Where it lands in the `Document` model                              |
|---------------|---------------------------------------------------------------------|
| `"spans"`     | New positioned text spans on the page (typical OCR output).         |
| `"regions"`   | Labelled regions on the presentation overlay (typical layout output).|
| `"tables"`    | Detected table structures.                                          |
| `"blocks"`    | New blocks inserted into the content spine.                         |
| `"overlays"`  | Arbitrary key-value annotations on the presentation overlay.        |
| `"entities"`  | Entity stamps on the relationships overlay (NER output).            |
| `"labels"`    | Free-form labels on slide / page metadata.                          |

A hook that wraps Tesseract declares `"provides": ["spans"]`. A
DocLayout-YOLO hook declares `["regions"]`. An NER hook declares
`["entities"]`. A hook that does multiple things declares all of
them; udoc routes each response field to the right destination.

#### Worked examples

```json
// Tesseract OCR wrapper
{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["image"],"provides":["spans"]}

// Layout-detection model
{"protocol":"udoc-hook-v1","capabilities":["layout"],"needs":["image","spans"],"provides":["regions"]}

// Entity-extraction NER on already-extracted text
{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text","blocks"],"provides":["entities"]}

// Whole-document table reconciler
{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["document"],"provides":["tables"]}
```

If you read your hook's handshake line and any field is not in
the table above, either udoc will reject the hook at startup
(unknown `capability` / `need` / `provide`) or your handshake will
be diagnosed as `WrongProtocol` (mismatched `protocol`). The
canonical list is `crates/udoc/src/hooks/protocol.rs`.

### Request and response shapes

```jsonc
// OCR phase. Hook gets the rendered page image and returns text + spans.
//
// Request:
{
  "seq":   1,                   // sequence number, for matching responses
  "phase": "ocr",
  "page":  0,                   // 0-indexed page number
  "image": "<base64 PNG>",
  "dpi":   150                  // resolution the image was rendered at
}
// Response:
{
  "seq":   1,
  "text":  "...full page text...",
  "spans": [                    // optional: positioned spans
    {"text": "hello", "x": 10, "y": 20, "w": 40, "h": 15}
  ]
}
```

```jsonc
// Layout phase. Hook gets existing spans and returns labelled regions.
//
// Request:
{
  "seq":   1,
  "phase": "layout",
  "page":  0,
  "spans": [/* positioned spans from extraction or earlier OCR phase */],
  "image": "<optional base64 PNG>"
}
// Response:
{
  "seq":     1,
  "regions": [
    {"label": "title",  "x": 10, "y": 20, "w": 400, "h": 50},
    {"label": "figure", "x": 10, "y": 80, "w": 400, "h": 200}
  ]
}
```

```jsonc
// Annotate phase. Hook gets content + layout and returns structured stamps.
//
// Request:
{
  "seq":     1,
  "phase":   "annotate",
  "page":    0,
  "content": [/* block tree */],
  "regions": [/* labelled regions from layout phase */]
}
// Response:
{
  "seq":         1,
  "annotations": [
    {"kind": "entity", "type": "PERSON",
     "text": "Ada Lovelace", "span": [12, 24]}
  ]
}
```

The full schema lives in
[`crates/udoc/src/hooks/protocol.rs`](https://github.com/newelh/udoc/blob/main/crates/udoc/src/hooks/protocol.rs).

### Errors

Hooks signal failures by writing an error response with the matching
sequence number:

```json
{"seq": 1, "error": {"kind": "transient", "message": "GPU OOM"}}
```

`kind` is one of:

- `transient` — udoc retries the page once with a fresh process if
  `Config.hooks_config.retry_on_transient` is set (off by default).
- `fatal` — udoc marks the hook dead, drops it from the chain, and
  surfaces the error on `Diagnostics`.
- `unsupported` — udoc passes the page through unchanged. Use this
  when the hook explicitly opts out of a page (wrong language, image
  too small, etc.) without flagging it as a bug.
- `provider_failure` — for hooks wrapping external services. Recorded
  on `Diagnostics`; extraction proceeds with the un-augmented page.

A hook that crashes, hangs past the per-request timeout, or emits
malformed JSON is killed by udoc; subsequent pages are processed with
the hook removed from the chain. udoc emits a `HookCrashed`,
`HookTimeout`, or `HookProtocolError` diagnostic per affected page so
the failure surfaces in your pipeline rather than being silently
absorbed.

The aggregate `Diagnostics` view tells you exactly which pages were
affected and why; nothing about a hook failure is silent.

## When not to use hooks

Hooks are the right tool when:

- The model needs to look at the rendered page or its image content
  (OCR, layout detection, image classification).
- The model wants per-page context, not the whole document.
- You want udoc's reliable parsing of structure to feed your model.

Hooks are the wrong tool when:

- You want to summarise an already-extracted document. Just call your
  model on `udoc.extract(...).content` directly. Hooks add round-trip
  overhead you do not need.
- You want a one-shot transformation. Pipe `udoc -j` into your script.
