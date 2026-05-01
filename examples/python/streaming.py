"""Streaming a large PDF page by page without loading the whole document.

Run with:

    pip install udoc
    python examples/python/streaming.py path/to/large.pdf
"""
import sys
import udoc


def main(path: str) -> None:
    with udoc.open(path) as ext:
        print(f"opened {path} with {ext.page_count} pages")
        for i in range(ext.page_count):
            page = ext.page(i)
            text = page.text
            head = text[:80].replace("\n", " ")
            print(f"  page {i:4d} ({len(text):6d} chars): {head}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path>", file=sys.stderr)
        sys.exit(2)
    main(sys.argv[1])
