//! Render text spans onto a monospace grid using their PDF coordinates.
//!
//! This is a position-faithful alternative to reading-order extraction
//! (`Page::text()`). For tabular and multi-column documents whose
//! structure *is* the visual layout, projecting glyphs to a terminal
//! grid preserves column boundaries that reading-order would flatten
//! into prose.
//!
//! Equivalent to poppler's `pdftotext -layout`.

use udoc_core::geometry::BoundingBox;

use super::types::TextSpan;

/// Options for layout-mode rendering.
///
/// `columns` drives horizontal density; `points_per_row` drives
/// vertical. They're independent so one knob doesn't squeeze the
/// other -- a wider line cap doesn't cause rows to collapse on top
/// of each other.
#[derive(Debug, Clone)]
pub struct LayoutOptions {
    /// Page bounding box in PDF user-space.
    pub page_bbox: BoundingBox,
    /// Target output width in monospace cells. Horizontal scale is
    /// `page_bbox.width() / columns`; the page's full horizontal
    /// extent fills this many cells. Higher = tighter horizontal,
    /// fewer collisions on dense text.
    pub columns: usize,
    /// PDF points per output row. 12.0 matches typical 11pt body text
    /// with default leading, putting one source line on one output
    /// row regardless of column count.
    pub points_per_row: f64,
    /// Skip rotated spans entirely. v1 default. v2 may render them
    /// oriented as in the source.
    pub skip_rotated: bool,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            page_bbox: BoundingBox::new(0.0, 0.0, 612.0, 792.0),
            // 120 cols / letter ≈ 5.1 pt/col, comfortably below a
            // typical 11pt char's ~5.5pt advance. Most modern terminals
            // are at least this wide.
            columns: 120,
            points_per_row: 12.0,
            skip_rotated: true,
        }
    }
}

/// Project text spans onto a monospace grid sized to `opts.columns` and
/// emit the grid as a string. Each row is right-trimmed; trailing blank
/// rows on the page are dropped.
///
/// Spans are bucketed by row (computed from their baseline) and within
/// each row sorted left-to-right. Each span's chars occupy consecutive
/// cells starting at the span's PDF-derived column, with a minimum
/// one-cell gap from the previous span on the same row. This preserves
/// column boundaries for tabular content and prevents adjacent spans
/// from colliding.
///
/// Spans flagged `is_invisible` are skipped. With `opts.skip_rotated`,
/// spans whose rotation differs from horizontal are also skipped.
pub fn render_layout(spans: &[TextSpan], opts: &LayoutOptions) -> String {
    use std::collections::BTreeMap;

    let page_w = opts.page_bbox.width();
    let cols = opts.columns.max(1);
    let pts_per_col = page_w / cols as f64;
    let pts_per_row = opts.points_per_row.max(1.0);

    if !pts_per_col.is_finite() || pts_per_col <= 0.0 || !pts_per_row.is_finite() {
        return String::new();
    }

    // Bucket spans by row. Within a row the original order is unstable
    // (content stream order isn't visual order), so we resort by x below.
    let mut by_row: BTreeMap<usize, Vec<&TextSpan>> = BTreeMap::new();
    for span in spans {
        if span.is_invisible || span.text.is_empty() {
            continue;
        }
        if opts.skip_rotated && span.rotation.abs() > 0.5 {
            continue;
        }

        // PDF y-axis is bottom-origin; flip to top-origin row index.
        let y_top = opts.page_bbox.y_max - span.y;
        let row_f = y_top / pts_per_row;
        if !row_f.is_finite() || row_f < 0.0 {
            continue;
        }
        by_row.entry(row_f.round() as usize).or_default().push(span);
    }

    let max_row = by_row.keys().copied().max().unwrap_or(0);
    let mut grid: Vec<Vec<char>> = vec![Vec::new(); max_row + 1];

    for (row, mut row_spans) in by_row {
        row_spans.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

        let line = &mut grid[row];
        let mut next_min_col: usize = 0;

        for span in row_spans {
            let chars: Vec<char> = span.text.chars().collect();
            if chars.is_empty() {
                continue;
            }

            let x_rel = span.x - opts.page_bbox.x_min;
            let col_f = x_rel / pts_per_col;
            if !col_f.is_finite() || col_f < 0.0 {
                continue;
            }
            // Honor the PDF position when there's room, otherwise push
            // past the previous span. The +1 keeps at least a single
            // space between adjacent spans even when their PDF positions
            // round to the same cell.
            let start = (col_f.round() as usize).max(next_min_col);

            for (i, ch) in chars.iter().enumerate() {
                write_at(line, start + i, *ch);
            }
            next_min_col = start + chars.len() + 1;
        }
    }

    grid.iter()
        .map(|line| line.iter().collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_at(line: &mut Vec<char>, pos: usize, ch: char) {
    while line.len() <= pos {
        line.push(' ');
    }
    line[pos] = ch;
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::geometry::BoundingBox;

    fn span_at(text: &str, x: f64, y: f64, width: f64) -> TextSpan {
        // TextSpan::new in udoc-pdf takes more args; use the simplest path
        // that lets us probe the renderer.
        let mut s = TextSpan::new(
            text.to_string(),
            x,
            y,
            width,
            std::sync::Arc::from("test"),
            12.0,
        );
        // Ensure rotation is 0 so spans aren't filtered out.
        s.rotation = 0.0;
        s
    }

    fn letter_opts() -> LayoutOptions {
        LayoutOptions {
            page_bbox: BoundingBox::new(0.0, 0.0, 612.0, 792.0),
            columns: 100,
            points_per_row: 12.0,
            skip_rotated: true,
        }
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(render_layout(&[], &letter_opts()), "");
    }

    #[test]
    fn single_span_appears_in_output() {
        let span = span_at("Hello", 60.0, 780.0, 30.0);
        let out = render_layout(&[span], &letter_opts());
        assert!(out.contains("Hello"), "output: {out:?}");
    }

    #[test]
    fn invisible_spans_are_skipped() {
        let mut span = span_at("Hidden", 60.0, 780.0, 30.0);
        span.is_invisible = true;
        let out = render_layout(&[span], &letter_opts());
        assert!(!out.contains("Hidden"));
    }

    #[test]
    fn rotated_spans_skipped_when_flag_set() {
        let mut span = span_at("Sideways", 60.0, 780.0, 30.0);
        span.rotation = 90.0;
        let out = render_layout(&[span], &letter_opts());
        assert!(!out.contains("Sideways"));
    }

    #[test]
    fn rotated_spans_kept_when_flag_unset() {
        let mut span = span_at("Sideways", 60.0, 780.0, 30.0);
        span.rotation = 90.0;
        let opts = LayoutOptions {
            skip_rotated: false,
            ..letter_opts()
        };
        let out = render_layout(&[span], &opts);
        // Even kept, the chars project horizontally; we just confirm they
        // weren't filtered out.
        assert!(out.contains('S') && out.contains('y'));
    }

    #[test]
    fn two_columns_stay_separated_on_same_row() {
        // Reading-order would flatten these to "Left Right text here".
        // Layout mode keeps them as distinct columns on the same line.
        let spans = vec![
            span_at("Left", 60.0, 780.0, 24.0),
            span_at("Right", 320.0, 780.0, 30.0),
            span_at("text", 60.0, 768.0, 24.0),
            span_at("here", 320.0, 768.0, 24.0),
        ];
        let out = render_layout(&spans, &letter_opts());
        let line = out
            .lines()
            .find(|l| l.contains("Left"))
            .expect("a row contains 'Left'");
        assert!(
            line.contains("Right"),
            "Left and Right should share a row: {line:?}",
        );
        let l = line.find("Left").unwrap();
        let r = line.find("Right").unwrap();
        assert!(
            r >= l + 4 + 4,
            "Right must follow Left with whitespace: {line:?}",
        );
    }

    #[test]
    fn each_line_has_no_trailing_whitespace() {
        let spans = vec![
            span_at("alpha", 60.0, 780.0, 30.0),
            span_at("beta", 60.0, 768.0, 24.0),
        ];
        let out = render_layout(&spans, &letter_opts());
        for line in out.lines() {
            assert_eq!(
                line.len(),
                line.trim_end().len(),
                "trailing whitespace on line: {line:?}",
            );
        }
    }

    #[test]
    fn span_chars_placed_consecutively() {
        // Within a single span, characters land in adjacent cells
        // regardless of glyph_bboxes. This trades intra-span position
        // fidelity for word integrity (PDF coordinates are sub-cell
        // precise; rounding glyphs individually causes collisions).
        let span = span_at("Hello", 60.0, 780.0, 30.0);
        let out = render_layout(&[span], &letter_opts());
        let line = out.lines().find(|l| l.contains("Hello")).unwrap();
        // The substring should appear contiguous, not "H e l l o".
        assert!(line.contains("Hello"), "chars must be adjacent: {line:?}");
    }

    #[test]
    fn ligature_bbox_does_not_panic() {
        // Ligature: 1 bbox for 2 chars. Renderer ignores glyph_bboxes
        // for placement (we always use consecutive cells), but the
        // input shape must not panic.
        let mut span = span_at("fi", 60.0, 780.0, 12.0);
        span.glyph_bboxes = Some(vec![BoundingBox::new(60.0, 770.0, 72.0, 782.0)]);
        let out = render_layout(&[span], &letter_opts());
        assert!(out.contains("fi"));
    }

    #[test]
    fn offset_page_bbox_is_respected() {
        // Some PDFs declare a /CropBox or /MediaBox not anchored at the
        // origin. The renderer must subtract the bbox origin so spans
        // land at their relative position.
        let opts = LayoutOptions {
            page_bbox: BoundingBox::new(100.0, 100.0, 712.0, 892.0),
            ..letter_opts()
        };
        let span = span_at("Hi", 100.0, 880.0, 12.0);
        let out = render_layout(&[span], &opts);
        let line = out.lines().find(|l| l.contains("Hi")).unwrap();
        // Span at x = bbox.x_min should sit at col 0
        assert_eq!(line.find("Hi"), Some(0), "line: {line:?}");
    }

    #[test]
    fn empty_text_span_is_no_op() {
        let span = span_at("", 60.0, 780.0, 0.0);
        assert_eq!(render_layout(&[span], &letter_opts()), "");
    }

    #[test]
    fn negative_columns_clamped_safely() {
        // Pathological: columns=0 would divide by zero. Must not panic.
        let opts = LayoutOptions {
            columns: 0,
            ..letter_opts()
        };
        let span = span_at("Hi", 60.0, 780.0, 12.0);
        // Either renders (after clamping) or returns empty; never panics.
        let _ = render_layout(&[span], &opts);
    }
}
