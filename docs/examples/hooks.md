# Hook implementations

Worked hook implementations live in the repository under
[`examples/hooks/`](https://github.com/newelh/udoc/tree/main/examples/hooks).
**Coming soon** to this section: prose walkthroughs of each,
what they cover, and how to adapt them.

Hooks already in the repo:

- `tesseract-hook` — CPU OCR via Tesseract 5.
- `glm-ocr-hook` — GPU OCR via GLM-OCR.
- `deepseek-ocr-hook` — GPU OCR via DeepSeek-OCR.
- `doclayout-yolo-hook` — layout detection.
- `ner-hook` — entity extraction.
- `cloud-ocr-mock` — template for cloud OCR providers
  (Textract, Document AI, Azure Form Recognizer) with the
  poll-for-result pattern.

The [hooks chapter](../hooks.md) covers the protocol with
worked code; the [hook protocol reference](../reference/hooks.md)
is the strict wire format.
