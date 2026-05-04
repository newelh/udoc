# Hooks protocol reference

The strict reference for the `udoc-hook-v1` JSONL protocol:
handshake fields, per-phase request and response shapes, error
kinds, and resource controls. Companion to the [Hooks
chapter](../hooks.md), which is the narrative tutorial. If you are
implementing a hook from scratch this is the page to read; if you
are picking up `examples/hooks/` to adapt, the tutorial is
friendlier.

## Pipeline

```text
udoc parse ─▶ [OCR phase] ─▶ [Layout phase] ─▶ [Annotate phase] ─▶ Document
                hook[0]        hook[0]          hook[0]
                hook[1]        hook[1]          hook[1]
                ...            ...              ...
```

Each phase is optional. Each phase may have any number of hooks
chained — the output of hook *N* feeds the input of hook *N+1*
within the same phase. The output of the last hook in a phase
feeds the first hook of the next.

Phase order is fixed and not configurable: OCR runs before layout,
layout runs before annotate. The order is what the model usually
needs (text before regions, regions before entities), and changing
it would invalidate downstream hooks' contracts.

## Per-phase dispatch

Whether a hook is invoked for a given page depends on the phase
and the runner configuration:

| Phase      | Default dispatch                                                                   | Override                                                            |
|------------|------------------------------------------------------------------------------------|---------------------------------------------------------------------|
| `ocr`      | Per-page, only when extracted text has fewer than `min_words_to_skip_ocr` (10) words. | `HookConfig.ocr_all_pages = true` or `min_words_to_skip_ocr = 0` (CLI: `--ocr-all`) bypasses the gate. |
| `layout`   | Per-page, every page.                                                              | None at the runner level. Hook can opt out of a page by returning `error.kind = "unsupported"`. |
| `annotate` | Per-page, every page.                                                              | Same as `layout`.                                                   |

Hooks declaring `"needs": ["document"]` swap per-page dispatch for
one whole-document request per extraction (see [`needs`](#needs-array-of-string-required-non-empty)
below). The runner does not invoke per-page dispatch for those
hooks at all; the document-level request goes out once and the
response carries per-page payloads in a `pages` array.

The OCR gate uses the page text udoc has *already* extracted from
the content stream. A scanned page contributes zero or near-zero
words and falls under the threshold; a born-digital page sits well
above it. Adjust `min_words_to_skip_ocr` if your inputs cluster
near the boundary (legal filings with mostly-blank cover pages,
form-heavy documents with sparse extracted text).

## Process lifecycle

A hook is a long-lived OS process. Lifecycle:

1. **Spawn.** udoc launches the hook executable. `argv[0]` is the
   command string the caller configured; udoc passes no extra
   arguments by default.
2. **Handshake.** The hook writes exactly one line of JSON to
   stdout (see [Handshake](#handshake)). udoc reads that line, with
   a configurable startup timeout (default 5 s).
3. **Request loop.** udoc writes one JSON request per line on the
   hook's stdin and reads one JSON response per line on stdout.
   Requests are tagged with a sequence number; responses must
   match.
4. **Shutdown.** udoc closes the hook's stdin when there is nothing
   left to send. The hook drains pending work, writes the final
   responses, and exits cleanly.

A hook process is reused across every page of the document being
extracted. Model setup amortises across the document, not per
page. Running udoc on a second document spawns a fresh hook
process.

### Spawn environment

- **stdin / stdout / stderr** are the communication channels. udoc
  captures stderr (truncated to a configurable cap) and surfaces it
  on diagnostics; do not put load-bearing output there.
- **PATH lookup.** A bare command string is resolved against the
  parent process's `$PATH`. Absolute paths are honoured verbatim.
- **Inherited environment.** Hooks inherit the calling process's
  environment by default. Restrict explicitly via
  `Hook(env={...})` on the Rust side / Python config (`Hooks(...)`).
- **No sandboxing.** udoc does not sandbox the hook. If you run
  hooks from untrusted sources, sandbox them yourself (seccomp,
  container, separate user, minimum-rights service account).

## Handshake

The first line the hook writes to stdout. Required, exactly one
line, four required fields, each value drawn from a fixed enum.

```json
{
  "protocol":     "udoc-hook-v1",
  "capabilities": ["ocr"],
  "needs":        ["image"],
  "provides":     ["spans"]
}
```

A handshake that fails to parse, omits a field, or names an
unrecognised value is rejected. udoc emits a `WrongProtocol` /
`HookProtocolError` diagnostic and does not call into the hook.

### `protocol` (string, required)

Exactly `"udoc-hook-v1"`. The wire-format version. There is no
auto-negotiation: a future incompatible revision would bump the
suffix and udoc would only accept hooks that name the version it
was built against.

### `capabilities` (array of string, required, non-empty)

Which phase(s) the hook participates in. When more than one is
declared, the earliest in this priority order wins:

| Order | Value         | Phase      |
|-------|---------------|------------|
| 1     | `"ocr"`       | OCR        |
| 2     | `"layout"`    | Layout     |
| 3     | `"annotate"`  | Annotate   |

A hook declaring `["ocr", "layout"]` runs in the OCR phase. The
ability to claim multiple capabilities exists for hooks that genuinely
do dual-purpose work (e.g. an OCR engine that also emits region
labels), but each such hook still runs in exactly one phase per
extraction.

### `needs` (array of string, required, non-empty)

What inputs the hook wants. udoc only sends the fields the hook
asks for; everything else is omitted to keep request payloads
small.

| Value         | Meaning                                                                  |
|---------------|--------------------------------------------------------------------------|
| `"image"`     | The rendered page as base64 PNG, plus its DPI.                           |
| `"spans"`     | Positioned text spans (whatever was extracted or produced by earlier OCR).|
| `"blocks"`    | The current `Block` tree for the page.                                   |
| `"text"`      | Plain text reconstruction of the page.                                   |
| `"document"`  | The whole document in one request rather than per-page (see below).      |

`"document"` is special. When the hook declares it, udoc switches
from per-page request loop to one-shot delivery: a single request
shaped `{"document_path": "...", "page_count": N, "format": "pdf"}`,
expecting a response shaped
`{"pages": [{"page_index": 0, "spans": [...]}, ...]}`. Use sparingly
— it forces the hook to handle the whole document in one shot and
disables streaming.

### `provides` (array of string, required, non-empty)

What the hook produces. udoc routes the response fields back into
the document model based on these.

| Value         | Where it lands                                                       |
|---------------|----------------------------------------------------------------------|
| `"spans"`     | New positioned text spans on the page (typical OCR output).          |
| `"regions"`   | Labelled regions on the presentation overlay (typical layout output).|
| `"tables"`    | Detected table structures.                                           |
| `"blocks"`    | New blocks inserted into the content spine.                          |
| `"overlays"`  | Arbitrary key-value annotations on the presentation overlay.         |
| `"entities"`  | Entity stamps on the relationships overlay (NER output).             |
| `"labels"`    | Free-form labels on slide / page metadata.                           |

A hook that does multiple things declares them all; udoc routes
each response field to the corresponding destination.

### Worked handshake examples

```json
// Tesseract OCR wrapper.
{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["image"],"provides":["spans"]}

// DocLayout-YOLO layout-detection model.
{"protocol":"udoc-hook-v1","capabilities":["layout"],"needs":["image","spans"],"provides":["regions"]}

// Cross-page entity extractor.
{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text","blocks"],"provides":["entities"]}

// Whole-document table reconciler.
{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["document"],"provides":["tables"]}
```

## Request / response shapes

Every per-page request carries a sequence number, the phase, and
the page index. Responses must echo the sequence number udoc
sent.

### OCR phase

```jsonc
// Request
{
  "seq":   1,                   // sequence number
  "phase": "ocr",
  "page":  0,                   // 0-indexed page number
  "image": "<base64 PNG>",
  "dpi":   150                  // DPI the image was rendered at
}

// Response (success)
{
  "seq":   1,
  "text":  "...full page text...",
  "spans": [
    {"text": "hello", "x": 10.0, "y": 20.0, "w": 40.0, "h": 15.0}
  ]
}
```

`spans` is optional — a hook may return `text` only. When `spans`
are returned, x/y/w/h are in PDF user-space coordinates (origin
lower-left, points). Each span object accepts an optional
`baseline` and an optional `font_size` if your model produces them.

### Layout phase

```jsonc
// Request
{
  "seq":   1,
  "phase": "layout",
  "page":  0,
  "spans": [/* positioned spans from extraction or earlier OCR phase */],
  "image": "<optional base64 PNG>"
}

// Response (success)
{
  "seq":     1,
  "regions": [
    {"label": "title",     "x": 10.0, "y":  20.0, "w": 400.0, "h":  50.0},
    {"label": "figure",    "x": 10.0, "y":  80.0, "w": 400.0, "h": 200.0},
    {"label": "paragraph", "x": 10.0, "y": 290.0, "w": 400.0, "h": 100.0}
  ]
}
```

Region labels are open-ended strings; common values are `"title"`,
`"paragraph"`, `"list"`, `"figure"`, `"table"`, `"caption"`,
`"footnote"`, `"header"`, `"footer"`. udoc preserves whatever the
model emits and stamps labels onto the corresponding blocks.

### Annotate phase

```jsonc
// Request
{
  "seq":     1,
  "phase":   "annotate",
  "page":    0,
  "content": [/* block tree */],
  "regions": [/* labelled regions from layout phase */]
}

// Response (success)
{
  "seq":         1,
  "annotations": [
    {"kind": "entity", "type": "PERSON",       "text": "Ada Lovelace",  "span": [12, 24]},
    {"kind": "entity", "type": "ORGANIZATION", "text": "Royal Society", "span": [80, 93]}
  ]
}
```

Annotation `kind` values currently understood by udoc: `"entity"`,
`"label"`, `"classification"`. `span` is a `[start, end]` byte
range into the page text. Unknown kinds round-trip through
`Block.metadata` so downstream consumers can opt in.

### Whole-document mode

For hooks declaring `"needs": ["document"]`:

```jsonc
// Request (one per extraction)
{
  "seq":           1,
  "phase":         "annotate",
  "document_path": "/tmp/...",   // path the hook can mmap
  "page_count":    42,
  "format":        "pdf"
}

// Response (one per extraction)
{
  "seq":   1,
  "pages": [
    {"page_index": 0, "spans":  [...], "regions": [...], "annotations": [...]},
    {"page_index": 1, "spans":  [...]},
    ...
  ]
}
```

Pages omitted from the response are passed through unchanged.

## Errors

Hooks signal per-page failures by writing an error response with
the matching sequence number:

```json
{"seq": 1, "error": {"kind": "transient", "message": "GPU OOM"}}
```

| `kind`              | udoc behaviour                                                                  |
|---------------------|---------------------------------------------------------------------------------|
| `"transient"`       | Retry the page once with a fresh process if `Config.hooks_config.retry_on_transient` is set (off by default). Otherwise drop and surface as a `HookTransientError` diagnostic. |
| `"fatal"`           | Mark the hook dead, drop it from the chain, surface the error on diagnostics. The remaining pages of the extraction proceed without this hook. |
| `"unsupported"`     | Pass the page through unchanged. Use this when the hook explicitly opts out of a page (wrong language, image too small, no scope) without flagging it as a bug. |
| `"provider_failure"`| Hook wrapping an external service returned a provider error. Recorded on diagnostics; extraction proceeds with the un-augmented page. |

A hook that crashes (process exits before responding), hangs past
the per-request timeout, or emits malformed JSON is killed by
udoc; subsequent pages are processed with the hook removed from
the chain. udoc emits one of `HookCrashed`, `HookTimeout`, or
`HookProtocolError` per affected page; nothing about a hook
failure is silent.

## Resource controls

| Setting                          | Default | Notes                                                                 |
|----------------------------------|---------|-----------------------------------------------------------------------|
| Startup timeout                  | 5 s     | Time allowed for the handshake line.                                  |
| Per-request timeout              | 60 s    | Configurable on `Config.hooks.timeout` / `HooksConfig::request_timeout`. |
| Per-line size cap                | 16 MiB  | Caps a single JSON request or response. Defends against runaway hooks.|
| Per-page response budget         | 10 MiB  | Caps total response bytes for one page.                               |
| Stderr cap                       | 64 KiB  | Captured stderr beyond this is truncated.                             |
| Retry on transient               | off     | When on, transient errors retry the page once with a fresh process.   |

Bumping the per-request timeout is the right knob for slow cloud
OCR providers. Bumping the per-line cap is rarely the right
answer — if a single request or response is over 16 MiB, the
protocol shape is probably wrong; consider splitting per region or
using whole-document mode.

## Diagnostics emitted by the hook subsystem

| `kind`                 | Level    | When                                                                          |
|------------------------|----------|-------------------------------------------------------------------------------|
| `WrongProtocol`        | warning  | Handshake parsed but `protocol` was not `"udoc-hook-v1"`.                     |
| `HookProtocolError`    | warning  | Handshake malformed, missing field, or unrecognised enum value.               |
| `HookStartupTimeout`   | warning  | Hook did not write a handshake within the startup timeout.                    |
| `HookCrashed`          | warning  | Hook process exited before responding to the page in flight.                  |
| `HookTimeout`          | warning  | Hook exceeded the per-request timeout for the page in flight.                 |
| `HookTransientError`   | warning  | Hook returned a `transient` error (recorded; page un-augmented unless retried). |
| `HookFatalError`       | warning  | Hook returned a `fatal` error; hook dropped from the chain.                   |
| `HookProviderFailure`  | warning  | Hook returned a `provider_failure` error.                                     |
| `HookStderr`           | info     | Hook wrote to stderr; the message is the captured (truncated) stream.         |

Filter on `kind` from the diagnostics sink to surface hook
failures in your pipeline. Hook failures never abort the
extraction by themselves; the `Document` you get back will lack
the augmentations from the failed hook but is otherwise usable.

## Source

- Wire-format types and parsing: [`crates/udoc/src/hooks/protocol.rs`](https://github.com/newelh/udoc/blob/main/crates/udoc/src/hooks/protocol.rs)
- Process supervision and lifecycle: [`crates/udoc/src/hooks/process.rs`](https://github.com/newelh/udoc/blob/main/crates/udoc/src/hooks/process.rs)
- Request building: [`crates/udoc/src/hooks/request.rs`](https://github.com/newelh/udoc/blob/main/crates/udoc/src/hooks/request.rs)
- Response routing: [`crates/udoc/src/hooks/response.rs`](https://github.com/newelh/udoc/blob/main/crates/udoc/src/hooks/response.rs)
- Worked example hooks: [`examples/hooks/`](https://github.com/newelh/udoc/tree/main/examples/hooks)
