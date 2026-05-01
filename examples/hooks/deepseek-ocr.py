#!/usr/bin/env python3
"""DeepSeek-OCR hook for udoc. Uses transformers directly (requires transformers==4.46.3).

Prerequisites:
  pip install "transformers==4.46.3" torch torchvision pillow accelerate

Usage:
  udoc scanned.pdf --ocr ./deepseek-ocr.py

Environment variables:
  CUDA_DEVICE     GPU device (default: cuda:0, set to cpu for CPU inference)

Protocol: reads JSONL page requests from stdin, writes JSONL responses to
stdout. Each request has an "image_path" field. Responds with text spans.

For v1, the full markdown output from the model is emitted as a single text
span covering the full page. Structured word-level parsing can be added later.

Note: this model requires transformers==4.46.3 exactly and trust_remote_code=True.
Do not upgrade transformers without verifying compatibility with DeepSeek-OCR.
"""

import json
import os
import sys

try:
    from transformers import AutoTokenizer, AutoModelForCausalLM
    from PIL import Image
    import torch
except ImportError as exc:
    print(
        f"deepseek-ocr.py: missing dependency: {exc} "
        '(pip install "transformers==4.46.3" torch torchvision pillow accelerate)',
        file=sys.stderr,
    )
    sys.exit(1)

DEVICE = os.environ.get("CUDA_DEVICE", "cuda:0")
MODEL_ID = "deepseek-ai/DeepSeek-OCR"
PROMPT = "Convert the document to markdown."

# Load model once at startup.
try:
    os.makedirs("/tmp/hf_modules_deepseek", exist_ok=True)
    os.environ.setdefault("HF_MODULES_CACHE", "/tmp/hf_modules_deepseek")

    print("deepseek-ocr.py: loading DeepSeek-OCR model...", file=sys.stderr)
    tokenizer = AutoTokenizer.from_pretrained(
        MODEL_ID, trust_remote_code=True
    )
    model = AutoModelForCausalLM.from_pretrained(
        MODEL_ID,
        trust_remote_code=True,
        torch_dtype="auto",
        device_map=DEVICE,
        _attn_implementation="eager",
    )
    print(f"deepseek-ocr.py: model loaded on {DEVICE}", file=sys.stderr)
except Exception as exc:
    print(f"deepseek-ocr.py: failed to load model: {exc}", file=sys.stderr)
    model = None
    tokenizer = None

# Handshake
print(
    json.dumps(
        {
            "protocol": "udoc-hook-v1",
            "capabilities": ["ocr"],
            "needs": ["image"],
            "provides": ["spans"],
        }
    ),
    flush=True,
)


def run_ocr(image_path, page_w, page_h):
    """Run DeepSeek-OCR inference on a single page image."""
    image = Image.open(image_path).convert("RGB")
    base_size = max(page_w, page_h)
    image_size = (int(page_w), int(page_h))
    result = model.infer(
        tokenizer,
        PROMPT,
        image,
        output_path=None,
        base_size=base_size,
        image_size=image_size,
    )
    return result


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue

    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        print(json.dumps({"spans": []}), flush=True)
        continue

    image_path = req.get("image_path", "")
    page_w = req.get("width", 612.0)
    page_h = req.get("height", 792.0)

    if not image_path or not os.path.isfile(image_path) or model is None:
        print(json.dumps({"spans": []}), flush=True)
        continue

    try:
        text = run_ocr(image_path, page_w, page_h)
    except Exception as exc:
        print(f"deepseek-ocr.py: inference failed: {exc}", file=sys.stderr)
        print(json.dumps({"spans": []}), flush=True)
        continue

    if text and text.strip():
        spans = [{"text": text.strip(), "bbox": [0.0, 0.0, page_w, page_h]}]
    else:
        spans = []

    print(json.dumps({"spans": spans}), flush=True)
