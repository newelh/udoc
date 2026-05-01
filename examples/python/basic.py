"""Smoke usage of the udoc Python module.

Run with:

    pip install udoc
    python examples/python/basic.py path/to/file.pdf
"""
import sys
import udoc


def main(path: str) -> None:
    doc = udoc.extract(path)
    print(f"title:      {doc.metadata.title!r}")
    print(f"author:     {doc.metadata.author!r}")
    print(f"page count: {doc.metadata.page_count}")
    print()

    for i, block in enumerate(doc.content[:10]):
        text = block.text[:80].replace("\n", " ")
        print(f"  [{i}] {block.kind:>10s}  {text}")

    if len(doc.content) > 10:
        print(f"  ... and {len(doc.content) - 10} more blocks")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path>", file=sys.stderr)
        sys.exit(2)
    main(sys.argv[1])
