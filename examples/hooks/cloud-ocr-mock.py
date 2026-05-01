#!/usr/bin/env python3
"""Mock cloud OCR hook for udoc. Serves fixture data simulating AWS Textract
and Google Cloud Vision OCR API responses.

Prerequisites: none beyond the standard library.

Usage:
  udoc scanned.pdf --ocr ./cloud-ocr-mock.py

Environment variables:
  CLOUD_OCR_MOCK_ENGINE   "textract" (default) or "vision"
  CLOUD_OCR_FIXTURE_DIR   Directory containing fixture JSON files
                          (default: tests/fixtures/cloud-ocr relative to CWD,
                           then relative to this script's directory)

Protocol: reads JSONL page requests from stdin, writes JSONL responses to
stdout. Each request has an "image_path", "width", and "height" field.
Responds with text spans derived from the fixture data.

Coordinate conventions:
  Textract: BoundingBox uses Left/Top/Width/Height as ratios [0,1]. These are
    multiplied by the page pixel dimensions from the request to get absolute
    pixel coords, then converted to points via: pts = pixels * 72 / dpi.
    When the request lacks dpi, a default of 150 dpi is assumed.

  Vision: normalizedVertices use x/y as ratios [0,1]. The bounding box is
    derived from the min/max of the four vertices and converted the same way.

Both engines emit word-level spans. Textract uses WORD blocks; Vision uses
the words array inside each paragraph.

This hook is intended for development and testing pipelines where a live
cloud API is unavailable.
"""

import json
import os
import sys

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

ENGINE = os.environ.get("CLOUD_OCR_MOCK_ENGINE", "textract").strip().lower()
if ENGINE not in ("textract", "vision"):
    print(
        f"cloud-ocr-mock.py: unknown engine '{ENGINE}', use 'textract' or 'vision'",
        file=sys.stderr,
    )
    sys.exit(1)

_SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
_DEFAULT_FIXTURE_DIR_REL = "tests/fixtures/cloud-ocr"

_fixture_dir = os.environ.get("CLOUD_OCR_FIXTURE_DIR", "").strip()
if not _fixture_dir:
    # Try CWD first, then fall back to script directory.
    candidate = os.path.join(os.getcwd(), _DEFAULT_FIXTURE_DIR_REL)
    if os.path.isdir(candidate):
        _fixture_dir = candidate
    else:
        _fixture_dir = os.path.join(_SCRIPT_DIR, "..", "..", _DEFAULT_FIXTURE_DIR_REL)

_fixture_dir = os.path.normpath(_fixture_dir)

_FIXTURE_FILES = {
    "textract": os.path.join(_fixture_dir, "textract-response.json"),
    "vision": os.path.join(_fixture_dir, "vision-response.json"),
}

# ---------------------------------------------------------------------------
# Load fixture
# ---------------------------------------------------------------------------

_fixture_path = _FIXTURE_FILES[ENGINE]
try:
    with open(_fixture_path) as f:
        _FIXTURE = json.load(f)
except FileNotFoundError:
    print(
        f"cloud-ocr-mock.py: fixture file not found: {_fixture_path}",
        file=sys.stderr,
    )
    sys.exit(1)
except json.JSONDecodeError as exc:
    print(
        f"cloud-ocr-mock.py: fixture JSON parse error in {_fixture_path}: {exc}",
        file=sys.stderr,
    )
    sys.exit(1)

# ---------------------------------------------------------------------------
# Span extraction
# ---------------------------------------------------------------------------

def _pts(pixels, dpi):
    """Convert pixel measurement to points."""
    return pixels * 72.0 / dpi


def extract_spans_textract(fixture, page_w, page_h, dpi):
    """Extract word-level spans from a Textract response fixture.

    Textract BoundingBox fields (Left, Top, Width, Height) are ratios in [0,1].
    We multiply by the page pixel dimensions, then convert to points.
    """
    spans = []
    for block in fixture.get("Blocks", []):
        if block.get("BlockType") != "WORD":
            continue
        text = block.get("Text", "").strip()
        if not text:
            continue
        bb = block.get("Geometry", {}).get("BoundingBox", {})
        left_px = bb.get("Left", 0.0) * page_w
        top_px = bb.get("Top", 0.0) * page_h
        w_px = bb.get("Width", 0.0) * page_w
        h_px = bb.get("Height", 0.0) * page_h
        x_min = _pts(left_px, dpi)
        y_min = _pts(top_px, dpi)
        x_max = _pts(left_px + w_px, dpi)
        y_max = _pts(top_px + h_px, dpi)
        spans.append({"text": text, "bbox": [x_min, y_min, x_max, y_max]})
    return spans


def _vertices_bbox(vertices, page_w, page_h, dpi):
    """Convert normalizedVertices list to a [x_min,y_min,x_max,y_max] bbox in points."""
    xs = [v.get("x", 0.0) * page_w for v in vertices]
    ys = [v.get("y", 0.0) * page_h for v in vertices]
    return [
        _pts(min(xs), dpi),
        _pts(min(ys), dpi),
        _pts(max(xs), dpi),
        _pts(max(ys), dpi),
    ]


def extract_spans_vision(fixture, page_w, page_h, dpi):
    """Extract word-level spans from a Google Cloud Vision response fixture.

    normalizedVertices use x/y as ratios in [0,1]. We derive the bounding box
    from the min/max of the four vertices, then convert to points.
    """
    spans = []
    responses = fixture.get("responses", [])
    if not responses:
        return spans
    pages = responses[0].get("fullTextAnnotation", {}).get("pages", [])
    if not pages:
        return spans
    for block in pages[0].get("blocks", []):
        for para in block.get("paragraphs", []):
            for word in para.get("words", []):
                symbols = word.get("symbols", [])
                text = "".join(s.get("text", "") for s in symbols).strip()
                if not text:
                    continue
                vertices = word.get("boundingBox", {}).get("normalizedVertices", [])
                if not vertices:
                    continue
                bbox = _vertices_bbox(vertices, page_w, page_h, dpi)
                spans.append({"text": text, "bbox": bbox})
    return spans

# ---------------------------------------------------------------------------
# Handshake
# ---------------------------------------------------------------------------

print(json.dumps({
    "protocol": "udoc-hook-v1",
    "capabilities": ["ocr"],
    "needs": ["image"],
    "provides": ["spans"],
}), flush=True)

# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        print(json.dumps({"spans": []}), flush=True)
        continue

    # Page pixel dimensions from the request. The fixture BoundingBox ratios
    # are applied to these to get absolute pixel coordinates.
    dpi = float(req.get("dpi", 150))
    # width/height from the request are in points; convert to pixels.
    page_w_pts = float(req.get("width", 612.0))
    page_h_pts = float(req.get("height", 792.0))
    page_w_px = page_w_pts * dpi / 72.0
    page_h_px = page_h_pts * dpi / 72.0

    if ENGINE == "textract":
        spans = extract_spans_textract(_FIXTURE, page_w_px, page_h_px, dpi)
    else:
        spans = extract_spans_vision(_FIXTURE, page_w_px, page_h_px, dpi)

    print(json.dumps({"spans": spans}), flush=True)
