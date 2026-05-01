#!/usr/bin/env python3
"""Generate a synthetic XLSX test corpus with ground-truth text output.

Uses xlsxwriter to create diverse XLSX files and writes the expected
tab-separated text output alongside each file. The ground truth matches
the format produced by udoc's XLSX page text extraction (tab-separated
columns, newline-separated rows, trailing empties trimmed).

Usage:
    python3 generate_corpus.py [--count N] [--output-dir DIR] [--gt-dir DIR]

Default: 5000 files into tests/xlsx-corpus/corpus/ with ground truth in
tests/xlsx-corpus/ground-truth/.
"""

import argparse
import hashlib
import math
import os
import random
import string
import sys

import xlsxwriter


# ---------------------------------------------------------------------------
# Deterministic RNG for reproducible corpus
# ---------------------------------------------------------------------------
RNG = random.Random(42)


def rand_string(min_len=1, max_len=30):
    length = RNG.randint(min_len, max_len)
    return "".join(RNG.choices(string.ascii_letters + string.digits + " _-.", k=length))


def rand_int(lo=-10000, hi=10000):
    return RNG.randint(lo, hi)


def rand_float():
    return round(RNG.uniform(-1e6, 1e6), RNG.randint(0, 6))


def rand_date_serial():
    """Return (serial, iso_string) for a random date (1900 epoch)."""
    # Range: 1 (Jan 1 1900) to 55000 (~2050)
    serial = RNG.randint(1, 55000)
    # Skip the Lotus bug serial 60 for simplicity in ground truth
    if serial == 60:
        serial = 61
    return serial


def serial_to_iso(serial):
    """Convert Excel serial (1900 epoch) to ISO date string.

    Must match udoc's serial_to_iso_date exactly.
    """
    if serial < 1:
        return str(serial)
    if serial == 60:
        return "1900-02-29"

    adjusted = serial - 1 if serial <= 60 else serial - 2
    # Days since 1899-12-31 (serial 1 = Jan 1, 1900)
    import datetime
    base = datetime.date(1899, 12, 31)
    d = base + datetime.timedelta(days=adjusted + 1)
    return d.strftime("%Y-%m-%d")


# ---------------------------------------------------------------------------
# Generator catalog: each generator produces (rows, ground_truth_text)
# where rows is a list of lists suitable for xlsxwriter, and
# ground_truth_text is the expected tab-separated output.
# ---------------------------------------------------------------------------


def gen_simple_numbers(idx):
    """Grid of integers."""
    nrows = RNG.randint(1, 20)
    ncols = RNG.randint(1, 8)
    rows = []
    for _ in range(nrows):
        row = [rand_int() for _ in range(ncols)]
        rows.append(row)
    gt_lines = []
    for row in rows:
        gt_lines.append("\t".join(str(v) for v in row))
    return rows, "\n".join(gt_lines), {}


def gen_simple_strings(idx):
    """Grid of strings."""
    nrows = RNG.randint(1, 15)
    ncols = RNG.randint(1, 6)
    rows = []
    for _ in range(nrows):
        row = [rand_string() for _ in range(ncols)]
        rows.append(row)
    gt_lines = []
    for row in rows:
        gt_lines.append("\t".join(row))
    return rows, "\n".join(gt_lines), {}


def gen_mixed_types(idx):
    """Mix of strings, ints, floats, booleans."""
    nrows = RNG.randint(2, 15)
    ncols = RNG.randint(2, 6)
    rows = []
    gt_lines = []
    for _ in range(nrows):
        row = []
        gt_row = []
        for _ in range(ncols):
            t = RNG.choice(["str", "int", "float", "bool"])
            if t == "str":
                v = rand_string(1, 15)
                row.append(v)
                gt_row.append(v)
            elif t == "int":
                v = rand_int(-100, 100)
                row.append(v)
                gt_row.append(str(v))
            elif t == "float":
                v = round(RNG.uniform(-100, 100), 2)
                row.append(v)
                # xlsxwriter writes floats; udoc strips trailing zeros
                gt_row.append(format_number(v))
            else:
                v = RNG.choice([True, False])
                row.append(v)
                gt_row.append("TRUE" if v else "FALSE")
        rows.append(row)
        gt_lines.append("\t".join(gt_row))
    return rows, "\n".join(gt_lines), {}


def gen_single_cell(idx):
    """Single cell with a value."""
    t = RNG.choice(["str", "int", "float"])
    if t == "str":
        v = rand_string(1, 50)
        return [[v]], v, {}
    elif t == "int":
        v = rand_int()
        return [[v]], str(v), {}
    else:
        v = round(RNG.uniform(-1000, 1000), 4)
        return [[v]], format_number(v), {}


def gen_empty_sheet(idx):
    """Empty sheet."""
    return [], "", {}


def gen_sparse_grid(idx):
    """Sparse grid with gaps between populated cells."""
    # We'll create a grid but only populate some cells.
    nrows = RNG.randint(3, 10)
    ncols = RNG.randint(3, 8)
    grid = [[""] * ncols for _ in range(nrows)]
    # Fill about 30% of cells
    for r in range(nrows):
        for c in range(ncols):
            if RNG.random() < 0.3:
                v = rand_int(0, 100)
                grid[r][c] = v
    # Build gt: trim trailing empty cells per row, trim trailing empty rows
    gt_lines = []
    for row in grid:
        str_row = [str(v) if v != "" else "" for v in row]
        # Trim trailing empty
        while str_row and str_row[-1] == "":
            str_row.pop()
        gt_lines.append("\t".join(str_row))
    # Trim trailing empty lines
    while gt_lines and gt_lines[-1] == "":
        gt_lines.pop()
    return grid, "\n".join(gt_lines), {}


def gen_wide_row(idx):
    """Single row with many columns."""
    ncols = RNG.randint(20, 50)
    row = [rand_int(0, 999) for _ in range(ncols)]
    gt = "\t".join(str(v) for v in row)
    return [row], gt, {}


def gen_tall_column(idx):
    """Single column with many rows."""
    nrows = RNG.randint(50, 200)
    rows = [[rand_int(0, 999)] for _ in range(nrows)]
    gt = "\n".join(str(row[0]) for row in rows)
    return rows, gt, {}


def gen_unicode_strings(idx):
    """Strings with Unicode characters."""
    samples = [
        "cafe\u0301",  # decomposed e-acute
        "\u4e16\u754c",  # Chinese: "world"
        "\u0410\u0411\u0412",  # Cyrillic ABC
        "\u00e9\u00e8\u00ea",  # French accented
        "\u2603\u2764\u263a",  # snowman, heart, smiley
        "na\u00efve",
        "\u00df\u00fc\u00f6\u00e4",  # German
        "\ud55c\uad6d\uc5b4",  # Korean
        "\u3053\u3093\u306b\u3061\u306f",  # Japanese hiragana
        "\u0639\u0631\u0628\u064a",  # Arabic
    ]
    nrows = RNG.randint(2, 8)
    ncols = RNG.randint(1, 3)
    rows = []
    gt_lines = []
    for _ in range(nrows):
        row = [RNG.choice(samples) for _ in range(ncols)]
        rows.append(row)
        gt_lines.append("\t".join(row))
    return rows, "\n".join(gt_lines), {}


def gen_date_cells(idx):
    """Cells formatted as dates using built-in format 14 (mm-dd-yy)."""
    nrows = RNG.randint(1, 10)
    rows = []
    gt_lines = []
    for _ in range(nrows):
        serial = rand_date_serial()
        rows.append([serial])
        gt_lines.append(serial_to_iso(serial))
    return rows, "\n".join(gt_lines), {"date_format": True}


def gen_percentage_cells(idx):
    """Cells formatted as percentages (numFmtId 9 = 0%)."""
    nrows = RNG.randint(1, 8)
    rows = []
    gt_lines = []
    for _ in range(nrows):
        # Percentage values stored as decimals (0.75 = 75%)
        v = round(RNG.uniform(0, 1), 4)
        rows.append([v])
        # Use round-half-away-from-zero to match Rust's f64::round()
        pct = v * 100.0
        rounded = int(pct + 0.5) if pct >= 0 else int(pct - 0.5)
        gt_lines.append(f"{rounded}%")
    return rows, "\n".join(gt_lines), {"pct_format": True}


def gen_header_data(idx):
    """Table with a header row and data rows."""
    ncols = RNG.randint(2, 6)
    headers = [f"Col{i+1}" for i in range(ncols)]
    nrows = RNG.randint(3, 15)
    data_rows = []
    for _ in range(nrows):
        row = [rand_int(0, 1000) for _ in range(ncols)]
        data_rows.append(row)
    all_rows = [headers] + data_rows
    gt_lines = ["\t".join(headers)]
    for row in data_rows:
        gt_lines.append("\t".join(str(v) for v in row))
    return all_rows, "\n".join(gt_lines), {}


def gen_formulas_cached(idx):
    """Cells with formulas (xlsxwriter writes cached results)."""
    a = rand_int(1, 100)
    b = rand_int(1, 100)
    rows = [[a, b, None]]  # None placeholder for formula cell
    gt = f"{a}\t{b}\t{a + b}"
    return rows, gt, {"formula": (a, b)}


def gen_large_grid(idx):
    """Larger grid (100-500 rows) to stress test."""
    nrows = RNG.randint(100, 500)
    ncols = RNG.randint(3, 8)
    rows = []
    gt_lines = []
    for _ in range(nrows):
        row = [rand_int(0, 9999) for _ in range(ncols)]
        rows.append(row)
        gt_lines.append("\t".join(str(v) for v in row))
    return rows, "\n".join(gt_lines), {}


def gen_multisheet(idx):
    """Workbook with multiple sheets."""
    nsheets = RNG.randint(2, 5)
    sheets = []
    for s in range(nsheets):
        nrows = RNG.randint(1, 5)
        ncols = RNG.randint(1, 3)
        rows = []
        for _ in range(nrows):
            row = [rand_int(0, 100) for _ in range(ncols)]
            rows.append(row)
        sheets.append((f"Sheet{s+1}", rows))
    return sheets, None, {"multisheet": True}


def gen_special_chars(idx):
    """Strings with XML-special characters."""
    specials = [
        '<tag>',
        'A & B',
        '"quoted"',
        "it's",
        'a < b > c',
        '&amp; literal',
        'line\nnewline',  # embedded newline
        'tab\there',
    ]
    nrows = RNG.randint(2, 6)
    rows = []
    gt_lines = []
    for _ in range(nrows):
        v = RNG.choice(specials)
        rows.append([v])
        # Embedded newlines in cell values: xlsxwriter preserves them
        gt_lines.append(v)
    return rows, "\n".join(gt_lines), {}


def gen_negative_numbers(idx):
    """Negative integers and floats."""
    nrows = RNG.randint(2, 10)
    ncols = RNG.randint(1, 4)
    rows = []
    gt_lines = []
    for _ in range(nrows):
        row = []
        gt_row = []
        for _ in range(ncols):
            if RNG.random() < 0.5:
                v = rand_int(-9999, -1)
                row.append(v)
                gt_row.append(str(v))
            else:
                v = round(RNG.uniform(-999, -0.01), 2)
                row.append(v)
                gt_row.append(format_number(v))
        rows.append(row)
        gt_lines.append("\t".join(gt_row))
    return rows, "\n".join(gt_lines), {}


def gen_long_strings(idx):
    """Cells with long string values."""
    nrows = RNG.randint(1, 5)
    rows = []
    gt_lines = []
    for _ in range(nrows):
        v = rand_string(100, 500)
        rows.append([v])
        gt_lines.append(v)
    return rows, "\n".join(gt_lines), {}


def gen_zero_values(idx):
    """Cells with zero, empty string, and near-zero floats."""
    rows = [[0, "", 0.0, "zero"]]
    gt = "0\t\t0\tzero"
    return rows, gt, {}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def format_number(v):
    """Format a number the way udoc does: strip trailing zeros."""
    if isinstance(v, int):
        return str(v)
    if v == int(v) and abs(v) < 2**53:
        return str(int(v))
    # Python's str(float) is close to Rust's format!("{}", f64)
    s = f"{v}"
    return s


def multisheet_gt(sheets):
    """Build ground truth for multisheet files (one sheet per 'page')."""
    page_gts = []
    for name, rows in sheets:
        if not rows:
            page_gts.append("")
            continue
        lines = []
        for row in rows:
            lines.append("\t".join(str(v) for v in row))
        page_gts.append("\n".join(lines))
    return page_gts


# ---------------------------------------------------------------------------
# Generators list with weights (higher = more common in corpus)
# ---------------------------------------------------------------------------
GENERATORS = [
    (gen_simple_numbers, 20),
    (gen_simple_strings, 15),
    (gen_mixed_types, 15),
    (gen_single_cell, 10),
    (gen_empty_sheet, 3),
    (gen_sparse_grid, 10),
    (gen_wide_row, 5),
    (gen_tall_column, 5),
    (gen_unicode_strings, 8),
    (gen_date_cells, 8),
    (gen_percentage_cells, 5),
    (gen_header_data, 10),
    (gen_formulas_cached, 5),
    (gen_large_grid, 3),
    (gen_multisheet, 8),
    (gen_special_chars, 5),
    (gen_negative_numbers, 8),
    (gen_long_strings, 3),
    (gen_zero_values, 3),
]

# Flatten to weighted list
WEIGHTED_GENERATORS = []
for gen, weight in GENERATORS:
    WEIGHTED_GENERATORS.extend([gen] * weight)


def write_xlsx(path, rows, opts):
    """Write an XLSX file using xlsxwriter."""
    wb = xlsxwriter.Workbook(path, {"strings_to_numbers": False})

    if opts.get("multisheet"):
        # rows is actually list of (name, rows) tuples
        for name, sheet_rows in rows:
            ws = wb.add_worksheet(name)
            for r, row in enumerate(sheet_rows):
                for c, val in enumerate(row):
                    if isinstance(val, bool):
                        ws.write_boolean(r, c, val)
                    elif isinstance(val, (int, float)):
                        ws.write_number(r, c, val)
                    elif isinstance(val, str):
                        ws.write_string(r, c, val)
            # empty sheets are just created with no writes
        wb.close()
        return

    ws = wb.add_worksheet()

    date_fmt = None
    if opts.get("date_format"):
        date_fmt = wb.add_format({"num_format": 14})  # mm-dd-yy / maps to built-in 14

    pct_fmt = None
    if opts.get("pct_format"):
        pct_fmt = wb.add_format({"num_format": "0%"})  # numFmtId 9

    for r, row in enumerate(rows):
        for c, val in enumerate(row):
            if opts.get("formula") and r == 0 and c == 2:
                a, b = opts["formula"]
                ws.write_formula(r, c, f"=A1+B1", None, a + b)
                continue
            if val is None:
                continue
            if isinstance(val, bool):
                ws.write_boolean(r, c, val)
            elif isinstance(val, (int, float)):
                if date_fmt:
                    ws.write_number(r, c, val, date_fmt)
                elif pct_fmt:
                    ws.write_number(r, c, val, pct_fmt)
                else:
                    ws.write_number(r, c, val)
            elif isinstance(val, str):
                if val == "":
                    ws.write_blank(r, c, None)
                else:
                    ws.write_string(r, c, val)

    wb.close()


def main():
    parser = argparse.ArgumentParser(description="Generate synthetic XLSX test corpus")
    parser.add_argument("--count", type=int, default=8000, help="Number of files to generate")
    parser.add_argument(
        "--output-dir",
        default="tests/xlsx-corpus/corpus",
        help="Directory for XLSX files",
    )
    parser.add_argument(
        "--gt-dir",
        default="tests/xlsx-corpus/ground-truth",
        help="Directory for ground truth text files",
    )
    args = parser.parse_args()

    os.makedirs(args.output_dir, exist_ok=True)
    os.makedirs(args.gt_dir, exist_ok=True)

    print(f"Generating {args.count} XLSX files...")

    gen_counts = {}
    for i in range(args.count):
        gen = RNG.choice(WEIGHTED_GENERATORS)
        gen_name = gen.__name__

        gen_counts[gen_name] = gen_counts.get(gen_name, 0) + 1

        result = gen(i)
        rows, gt_text, opts = result

        filename = f"{i:05d}_{gen_name}.xlsx"
        filepath = os.path.join(args.output_dir, filename)

        if opts.get("multisheet"):
            write_xlsx(filepath, rows, opts)
            # Write per-sheet ground truth
            page_gts = multisheet_gt(rows)
            for sheet_idx, page_gt in enumerate(page_gts):
                gt_filename = f"{i:05d}_{gen_name}_sheet{sheet_idx}.txt"
                gt_path = os.path.join(args.gt_dir, gt_filename)
                with open(gt_path, "w", encoding="utf-8") as f:
                    f.write(page_gt)
            # Also write a manifest
            manifest_path = os.path.join(args.gt_dir, f"{i:05d}_{gen_name}_manifest.txt")
            with open(manifest_path, "w", encoding="utf-8") as f:
                f.write(f"{len(rows)}\n")  # number of sheets
                for name, _ in rows:
                    f.write(f"{name}\n")
        else:
            write_xlsx(filepath, rows, opts)
            gt_filename = f"{i:05d}_{gen_name}_sheet0.txt"
            gt_path = os.path.join(args.gt_dir, gt_filename)
            with open(gt_path, "w", encoding="utf-8") as f:
                f.write(gt_text if gt_text else "")

        if (i + 1) % 500 == 0:
            print(f"  {i + 1}/{args.count} files generated...")

    print(f"\nDone. {args.count} files in {args.output_dir}")
    print(f"Ground truth in {args.gt_dir}")
    print("\nGenerator distribution:")
    for name, count in sorted(gen_counts.items(), key=lambda x: -x[1]):
        print(f"  {name}: {count}")


if __name__ == "__main__":
    main()
