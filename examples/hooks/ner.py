#!/usr/bin/env python3
"""ner.py -- NER hook for udoc wrapping spaCy.

Prerequisites: python3, spacy, en_core_web_sm model
  pip install spacy
  python -m spacy download en_core_web_sm

Usage: udoc file.pdf --hook ./ner.py

Protocol: reads JSONL page requests from stdin, writes JSONL
responses to stdout. Each request has a "text" field with the
page text. Responds with entity annotations as overlay data.
"""

import json
import sys

try:
    import spacy
    nlp = spacy.load("en_core_web_sm")
except ImportError:
    nlp = None

# Handshake
print(json.dumps({
    "protocol": "udoc-hook-v1",
    "capabilities": ["annotate"],
    "needs": ["text"],
    "provides": ["entities"]
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

    if nlp and text:
        doc = nlp(text)
        for ent in doc.ents:
            entities.append({
                "text": ent.text,
                "label": ent.label_,
                "start": ent.start_char,
                "end": ent.end_char,
            })

    print(json.dumps({"entities": entities}), flush=True)
