#!/usr/bin/env python3
"""GLM-OCR hook for udoc. Uses transformers directly (0.9B model, fits on any GPU).

Prerequisites:
  pip install transformers torch torchvision pillow accelerate

Usage:
  udoc scanned.pdf --ocr ./glm-ocr.py --hook-image-dir /tmp/pages

Environment variables:
  GLM_OCR_MODE    "text" (default) or "table" for table recognition
  CUDA_DEVICE     GPU device (default: cuda:0, set to cpu for CPU inference)

Protocol: reads JSONL page requests from stdin, writes JSONL responses to
stdout. Each request has an "image_path" field. Responds with text spans.

The model outputs structured markdown for tables and clean text for
paragraphs. Output is emitted as a single text span covering the page.
"""

import json
import os
import sys

try:
    from transformers import AutoProcessor, AutoModelForImageTextToText
    from PIL import Image
    import torch
except ImportError as exc:
    print(
        f"glm-ocr.py: missing dependency: {exc} "
        "(pip install transformers torch torchvision pillow accelerate)",
        file=sys.stderr,
    )
    sys.exit(1)

MODE = os.environ.get("GLM_OCR_MODE", "text")
DEVICE = os.environ.get("CUDA_DEVICE", "cuda:0")

PROMPTS = {
    "text": "Text Recognition:",
    "table": "Table Recognition:",
    "formula": "Formula Recognition:",
}
PROMPT_TEXT = PROMPTS.get(MODE, PROMPTS["text"])

# Load model once at startup.
try:
    os.makedirs("/tmp/hf_modules_glm", exist_ok=True)
    os.environ.setdefault("HF_MODULES_CACHE", "/tmp/hf_modules_glm")

    print("glm-ocr.py: loading GLM-OCR model...", file=sys.stderr)
    processor = AutoProcessor.from_pretrained("zai-org/GLM-OCR")
    model = AutoModelForImageTextToText.from_pretrained(
        "zai-org/GLM-OCR", torch_dtype="auto", device_map=DEVICE
    )
    print(f"glm-ocr.py: model loaded on {DEVICE}", file=sys.stderr)
except Exception as exc:
    print(f"glm-ocr.py: failed to load model: {exc}", file=sys.stderr)
    model = None
    processor = None

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
        image = Image.open(image_path).convert("RGB")
        messages = [
            {
                "role": "user",
                "content": [
                    {"type": "image"},
                    {"type": "text", "text": PROMPT_TEXT},
                ],
            }
        ]
        text_input = processor.apply_chat_template(
            messages, tokenize=False, add_generation_prompt=True
        )
        inputs = processor(
            text=[text_input], images=[image], return_tensors="pt"
        ).to(model.device)
        generated_ids = model.generate(**inputs, max_new_tokens=4096)
        output = processor.decode(
            generated_ids[0][inputs["input_ids"].shape[1] :],
            skip_special_tokens=True,
        )
    except Exception as exc:
        print(f"glm-ocr.py: inference failed: {exc}", file=sys.stderr)
        print(json.dumps({"spans": []}), flush=True)
        continue

    if output.strip():
        spans = [{"text": output.strip(), "bbox": [0.0, 0.0, page_w, page_h]}]
    else:
        spans = []

    print(json.dumps({"spans": spans}), flush=True)
