# udoc hook examples

These are example hooks for udoc's external processing protocol (udoc-hook-v1).
Each hook is a subprocess that reads JSONL page requests from stdin and writes
JSONL responses to stdout. The first line written by a hook is the handshake,
which declares its capabilities, needs, and provides.

For the full protocol specification see `crates/udoc/src/hooks/`.

---

## tesseract-ocr.sh

OCR hook wrapping Tesseract in TSV mode. Emits per-word text spans with bounding
boxes in points.

**Prerequisites:** bash, jq, tesseract >= 4.0

**Usage:**

    udoc scanned.pdf --ocr ./examples/hooks/tesseract-ocr.sh

**Details:**

- Uses `--psm 1` (automatic page segmentation with OSD).
- Reads the `dpi` field from each request and converts pixel coordinates to
  points: `pts = pixels * 72 / dpi`.
- Skips rows with `conf < 0` (empty block markers) and `conf < 30` (low
  confidence words).
- Bbox format: `[x_min, y_min, x_max, y_max]` in points, top-left origin.

---

## deepseek-ocr.py

OCR hook using DeepSeek-OCR via direct transformers inference. Loads the model
in-process; no server required.

**Prerequisites:** python3, transformers==4.46.3 (exact), torch, torchvision, pillow, accelerate

    pip install "transformers==4.46.3" torch torchvision pillow accelerate

**Version constraint:** transformers must be exactly 4.46.3. The model uses
`trust_remote_code=True` and `_attn_implementation='eager'`; newer transformers
versions may break the custom model code. Do not upgrade without verifying.

**Usage:**

    udoc scanned.pdf --ocr ./examples/hooks/deepseek-ocr.py

**Environment variables:**

| Variable | Default | Description |
|---|---|---|
| `CUDA_DEVICE` | `cuda:0` | GPU device for inference (set to `cpu` for CPU-only) |

**Details:**

- Model: `deepseek-ai/DeepSeek-OCR` loaded via `AutoModelForCausalLM`.
- Uses `model.infer(tokenizer, prompt, image, ...)` with `base_size` and
  `image_size` derived from the page dimensions in the request.
- For v1, emits the full model output as a single text span covering the page.
- Inference errors emit empty spans and a warning to stderr; the pipeline
  continues.

---

## glm-ocr.py

OCR hook using GLM-OCR (zai-org/GLM-OCR, 0.9B model) via direct transformers
inference. Loads the model in-process; no server required. Fits on any modern
GPU, or runs on CPU for low-volume use.

**Prerequisites:** python3, transformers, torch, torchvision, pillow, accelerate

    pip install transformers torch torchvision pillow accelerate

Ollama is also supported if you prefer a server-based setup:

    ollama pull zai-org/glm-ocr
    # Then point a custom hook at http://localhost:11434 using the Ollama API.

**Usage:**

    udoc scanned.pdf --ocr ./examples/hooks/glm-ocr.py --hook-image-dir /tmp/pages

**Environment variables:**

| Variable | Default | Description |
|---|---|---|
| `GLM_OCR_MODE` | `text` | `text` for plain OCR, `table` for tabular pages, `formula` for math |
| `CUDA_DEVICE` | `cuda:0` | GPU device (set to `cpu` for CPU-only inference) |

**Details:**

- Model: `zai-org/GLM-OCR` loaded via `AutoModelForImageTextToText`.
- Text mode prompt: `Text Recognition:`
- Table mode prompt: `Table Recognition:` -- better suited to pages that are
  primarily tabular data.
- Formula mode prompt: `Formula Recognition:` -- for math-heavy pages.
- For v1, emits the full model output as a single text span covering the page.
- Inference errors emit empty spans and a warning to stderr; the pipeline
  continues.

---

## ner-nunerzero.py

NER annotation hook using NuNER_Zero, a zero-shot named entity recognition
model from NuMind built on GLiNER. No fine-tuning required.

**Prerequisites:** python3, gliner

    pip install gliner

**Usage:**

    udoc file.pdf --hook ./examples/hooks/ner-nunerzero.py

**Details:**

- Default label set: PERSON, ORG, LOCATION, DATE, MONEY.
- The model is downloaded from HuggingFace on first use (numind/NuNER_Zero).
- If the model fails to load, the hook emits empty entity lists for all pages
  and prints a warning to stderr rather than crashing the pipeline.
- Entities are stored in document metadata under `hook.entities.page.N`.
- Response format per entity: `{"text": "...", "label": "PERSON", "start": 0, "end": 5}`
  where `start`/`end` are character offsets into the page text.

---

## cloud-ocr-mock.py

Mock OCR hook that serves fixture data simulating AWS Textract and Google Cloud
Vision responses. Intended for development and testing pipelines where a live
cloud API is unavailable.

**Prerequisites:** python3 (standard library only)

**Usage:**

    udoc scanned.pdf --ocr ./examples/hooks/cloud-ocr-mock.py

    # Use Vision fixture instead of Textract
    CLOUD_OCR_MOCK_ENGINE=vision udoc scanned.pdf --ocr ./examples/hooks/cloud-ocr-mock.py

**Environment variables:**

| Variable | Default | Description |
|---|---|---|
| `CLOUD_OCR_MOCK_ENGINE` | `textract` | `textract` or `vision` |
| `CLOUD_OCR_FIXTURE_DIR` | `tests/fixtures/cloud-ocr` | Directory with fixture JSON files |

**Fixture files** (relative to repository root):

- `tests/fixtures/cloud-ocr/textract-response.json` -- sample Textract
  DetectDocumentText response with two LINE blocks and four WORD blocks.
- `tests/fixtures/cloud-ocr/vision-response.json` -- sample Cloud Vision
  annotateImage response with two paragraphs and four words.

**Coordinate conversion:**

- Textract: `BoundingBox.Left/Top/Width/Height` are ratios in [0,1]. Multiplied
  by page pixel dimensions (derived from the request `width`/`height` in points
  and `dpi`), then converted to points.
- Vision: `normalizedVertices` are x/y ratios in [0,1]. Bounding box is the
  min/max of the four vertices, converted the same way.

Both engines emit word-level spans in the udoc wire format:
`{"text": "...", "bbox": [x_min, y_min, x_max, y_max]}` in points.

---

## ner.py (legacy)

Original NER hook wrapping spaCy. Superseded by ner-nunerzero.py for new
deployments. Kept for backwards compatibility.

**Prerequisites:** python3, spacy, en_core_web_sm

    pip install spacy
    python -m spacy download en_core_web_sm

**Usage:**

    udoc file.pdf --hook ./examples/hooks/ner.py
