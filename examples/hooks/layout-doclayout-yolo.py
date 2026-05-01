#!/usr/bin/env python3
"""Document layout detection hook using DocLayout-YOLO.

Detects document regions (text, titles, tables, figures, formulas, captions)
and returns bounding boxes with region types. This is a "layout" phase hook
in the udoc pipeline (OCR -> layout -> annotate).

Prerequisites:
  pip install doclayout-yolo
  # Weights auto-download from HuggingFace on first run

Usage:
  udoc document.pdf --hook ./layout-doclayout-yolo.py --hook-image-dir /tmp/pages

Environment variables:
  DOCLAYOUT_MODEL    Path to model weights (default: auto-download from HF)
  DOCLAYOUT_DEVICE   Inference device (default: cpu, set to cuda:0 for GPU)
  DOCLAYOUT_CONF     Confidence threshold (default: 0.25)
  DOCLAYOUT_IMGSZ    Input image size (default: 1024)
"""

import json
import os
import sys

try:
    from doclayout_yolo import YOLOv10
except ImportError:
    print(
        "layout-doclayout-yolo.py: missing dependency: doclayout-yolo "
        "(pip install doclayout-yolo)",
        file=sys.stderr,
    )
    sys.exit(1)

MODEL_PATH = os.environ.get("DOCLAYOUT_MODEL", "")
DEVICE = os.environ.get("DOCLAYOUT_DEVICE", "cpu")
CONF = float(os.environ.get("DOCLAYOUT_CONF", "0.25"))
IMGSZ = int(os.environ.get("DOCLAYOUT_IMGSZ", "1024"))

# Load model once at startup.
try:
    if MODEL_PATH:
        model = YOLOv10(MODEL_PATH)
    else:
        from huggingface_hub import hf_hub_download

        path = hf_hub_download(
            "juliozhao/DocLayout-YOLO-DocStructBench",
            "doclayout_yolo_docstructbench_imgsz1024.pt",
        )
        model = YOLOv10(path)
    print("layout-doclayout-yolo.py: model loaded", file=sys.stderr)
except Exception as exc:
    print(f"layout-doclayout-yolo.py: failed to load model: {exc}", file=sys.stderr)
    model = None

# Handshake: layout phase hook, needs page images, provides region annotations.
print(
    json.dumps(
        {
            "protocol": "udoc-hook-v1",
            "capabilities": ["layout"],
            "needs": ["image"],
            "provides": ["regions"],
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
        print(json.dumps({"regions": []}), flush=True)
        continue

    image_path = req.get("image_path", "")
    dpi = req.get("dpi", 150)
    page_w = req.get("width", 612.0)
    page_h = req.get("height", 792.0)

    if not image_path or not os.path.isfile(image_path) or model is None:
        print(json.dumps({"regions": []}), flush=True)
        continue

    try:
        results = model.predict(
            image_path, imgsz=IMGSZ, conf=CONF, device=DEVICE, verbose=False
        )
    except Exception as exc:
        print(
            f"layout-doclayout-yolo.py: inference failed: {exc}", file=sys.stderr
        )
        print(json.dumps({"regions": []}), flush=True)
        continue

    regions = []
    scale = 72.0 / dpi  # pixels to points

    for box in results[0].boxes:
        cls_id = int(box.cls[0])
        confidence = float(box.conf[0])
        x1, y1, x2, y2 = box.xyxy[0].tolist()

        region_type = results[0].names.get(cls_id, f"class_{cls_id}")

        regions.append(
            {
                "type": region_type,
                "confidence": round(confidence, 3),
                "bbox": [
                    round(x1 * scale, 1),
                    round(y1 * scale, 1),
                    round(x2 * scale, 1),
                    round(y2 * scale, 1),
                ],
            }
        )

    print(json.dumps({"regions": regions}), flush=True)
