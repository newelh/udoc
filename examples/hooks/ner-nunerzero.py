#!/usr/bin/env python3
"""NER hook using NuNER_Zero for zero-shot entity extraction.

Prerequisites:
  pip install gliner

Usage:
  udoc file.pdf --hook ./ner-nunerzero.py

Protocol: reads JSONL page requests from stdin, writes JSONL responses to
stdout. Each request has a "text" field with the page text. Responds with
entity annotations.

NuNER_Zero is a zero-shot NER model from NuMind built on top of GLiNER.
It requires no fine-tuning and handles arbitrary label sets at inference time.

The model is loaded once at startup. If the load fails (e.g., package missing
or network unavailable), the hook emits empty entity lists for all pages and
prints a warning to stderr rather than crashing the pipeline.
"""

import json
import sys

LABELS = ["PERSON", "ORG", "LOCATION", "DATE", "MONEY"]

# Load model at startup. Failure is non-fatal.
_model = None
try:
    from gliner import GLiNER
    _model = GLiNER.from_pretrained("numind/NuNER_Zero")
except ImportError:
    print(
        "ner-nunerzero.py: missing dependency: gliner (pip install gliner)",
        file=sys.stderr,
    )
except Exception as exc:
    print(f"ner-nunerzero.py: failed to load NuNER_Zero model: {exc}", file=sys.stderr)

# Handshake
print(json.dumps({
    "protocol": "udoc-hook-v1",
    "capabilities": ["annotate"],
    "needs": ["text"],
    "provides": ["entities"],
}), flush=True)

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        print(json.dumps({"entities": []}), flush=True)
        continue

    text = req.get("text", "")
    entities = []

    if _model and text:
        try:
            raw = _model.predict_entities(text, LABELS)
            for ent in raw:
                entities.append({
                    "text": ent["text"],
                    "label": ent["label"],
                    "start": ent["start"],
                    "end": ent["end"],
                })
        except Exception as exc:
            print(f"ner-nunerzero.py: prediction failed: {exc}", file=sys.stderr)

    print(json.dumps({"entities": entities}), flush=True)
