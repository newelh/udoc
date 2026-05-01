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

/// Project text spans onto a monospace grid sized to `opts.columns`
/// and emit the grid as a string. Each row is right-trimmed.
///
/// Spans are first clustered into visual lines by baseline proximity
/// (so tightly-leaded text doesn't collide on a fixed row grid), then
/// within each line sorted left-to-right. Adjacent spans concatenate
/// without a forced inter-span gap when their PDF positions are
/// touching, so a word fragmented across spans (e.g. "Pro" + "vided")
/// reads as one word; spans with real PDF whitespace between them
/// still appear separated.
///
/// Spans flagged `is_invisible` are skipped. With `opts.skip_rotated`,
/// spans whose rotation differs from horizontal are also skipped.
pub fn render_layout(spans: &[TextSpan], opts: &LayoutOptions) -> String {
    use std::cmp::Ordering;

    let page_w = opts.page_bbox.width();
    let cols = opts.columns.max(1);
    let pts_per_col = page_w / cols as f64;

    if !pts_per_col.is_finite() || pts_per_col <= 0.0 {
        return String::new();
    }

    // Filter once. Iterating a `Vec<&TextSpan>` keeps the renderer
    // allocation-light without re-checking flags on each visit.
    let mut filtered: Vec<&TextSpan> = spans
        .iter()
        .filter(|s| !s.is_invisible && !s.text.is_empty())
        .filter(|s| !opts.skip_rotated || s.rotation.abs() <= 0.5)
        .collect();
    if filtered.is_empty() {
        return String::new();
    }

    // Sort by y descending so we walk the page from top to bottom.
    // PDF y-axis is bottom-origin: higher y = closer to top.
    filtered.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(Ordering::Equal));

    // Cluster into visual lines. Two spans are on the same line if
    // their baselines are within `tolerance` PDF points -- a fraction
    // of the smaller font size. This handles superscripts and
    // mixed-size lines without collapsing adjacent prose lines.
    let mut lines: Vec<Vec<&TextSpan>> = Vec::new();
    let mut current: Vec<&TextSpan> = Vec::new();
    let mut current_y = f64::NAN;
    for span in filtered {
        let tolerance = (span.font_size.max(6.0)) * 0.4;
        if current.is_empty() || (current_y - span.y).abs() <= tolerance {
            current_y = if current.is_empty() {
                span.y
            } else {
                // Smooth the cluster's reference y as it grows so a long
                // run of small descenders doesn't drift past tolerance.
                (current_y * current.len() as f64 + span.y) / (current.len() + 1) as f64
            };
            current.push(span);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push(span);
            current_y = span.y;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }

    // Compute the row stride from the median observed line gap so the
    // output's vertical density matches the document's actual leading.
    // Falls back to `opts.points_per_row` for single-line pages.
    let pts_per_row = if lines.len() < 2 {
        opts.points_per_row.max(1.0)
    } else {
        let mut gaps: Vec<f64> = lines
            .windows(2)
            .filter_map(|w| {
                let a = w[0].first()?.y;
                let b = w[1].first()?.y;
                let g = a - b;
                if g.is_finite() && g > 0.0 {
                    Some(g)
                } else {
                    None
                }
            })
            .collect();
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let median = gaps
            .get(gaps.len() / 2)
            .copied()
            .unwrap_or(opts.points_per_row);
        median.max(1.0)
    };

    // Map each line to a row index by y delta from the page top.
    let mut output: Vec<String> = Vec::new();
    for line in lines.iter_mut() {
        line.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal));

        let line_y = line[0].y;
        let y_top = opts.page_bbox.y_max - line_y;
        let target_row_f = y_top / pts_per_row;
        if !target_row_f.is_finite() || target_row_f < 0.0 {
            continue;
        }
        let target_row = target_row_f.round() as usize;

        // Pad with blank rows up to the target.
        while output.len() < target_row {
            output.push(String::new());
        }

        // Lay out spans on this line.
        let mut chars: Vec<char> = Vec::new();
        let mut next_min: usize = 0;
        let mut prev_end_pdf: Option<f64> = None;
        for span in line.iter() {
            let span_chars: Vec<char> = span.text.chars().collect();
            if span_chars.is_empty() {
                continue;
            }

            let x_rel = span.x - opts.page_bbox.x_min;
            let col_f = x_rel / pts_per_col;
            if !col_f.is_finite() || col_f < 0.0 {
                continue;
            }
            // Distinguish two cases of consecutive spans:
            //   * Word fragments ("Pro" + "vided"): the next span's
            //     start touches or sits inside the previous span's
            //     advance. Concatenate -- a forced gap fragments the
            //     word.
            //   * Word breaks ("Provided" + "proper"): there's a real
            //     PDF whitespace gap between the spans. Force at
            //     least one cell of space so the words read distinct.
            // Word-spaces in real PDFs are roughly 0.25 * font_size
            // (~2.75pt at 11pt body); 1pt is a conservative absolute
            // floor that catches them while still letting kerning
            // micro-adjustments fall through to concatenation.
            let mut start = col_f.round() as usize;
            if let Some(prev_end) = prev_end_pdf {
                let pdf_gap = span.x - prev_end;
                let min_safe = if pdf_gap > 1.0 {
                    next_min + 1
                } else {
                    next_min
                };
                start = start.max(min_safe);
            }

            for (i, ch) in span_chars.iter().enumerate() {
                write_at(&mut chars, start + i, *ch);
            }
            next_min = start + span_chars.len();
            prev_end_pdf = Some(span.x + span.width);
        }

        let row_str: String = chars.iter().collect::<String>().trim_end().to_string();
        if output.len() == target_row {
            output.push(row_str);
        } else {
            // Multiple visual lines hashed to the same row index --
            // happens when median gap rounding differs from a specific
            // line's actual gap. Append to the existing row instead of
            // overwriting it.
            let existing = output.last_mut().unwrap();
            if !existing.is_empty() && !row_str.is_empty() {
                existing.push(' ');
            }
            existing.push_str(&row_str);
        }
    }

    output.join("\n")
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

    #[test]
    fn touching_spans_concatenate_without_gap() {
        // PDF often emits a single word as multiple Tj operations:
        // "Pro" + "vided" both at the same baseline, second span
        // starting where the first ends. Output should read
        // "Provided", not "Pro vided".
        // span1: x=60, width=18 (3 chars * 6pt advance)
        // span2: x=78 (touching), width=30 (5 chars * 6pt advance)
        let spans = vec![
            span_at("Pro", 60.0, 780.0, 18.0),
            span_at("vided", 78.0, 780.0, 30.0),
        ];
        let out = render_layout(&spans, &letter_opts());
        assert!(
            out.contains("Provided"),
            "touching spans must concatenate: {out:?}",
        );
        assert!(!out.contains("Pro vided"), "no artificial gap: {out:?}",);
    }

    #[test]
    fn spans_with_real_pdf_gap_get_at_least_one_space() {
        // Two distinct words separated by a real PDF whitespace gap
        // must read as two words on output.
        // span1 "Provided": x=60, width=48 (8*6) → ends at 108
        // span2 "proper":   x=114 (6pt gap = real space), width=36
        let spans = vec![
            span_at("Provided", 60.0, 780.0, 48.0),
            span_at("proper", 114.0, 780.0, 36.0),
        ];
        let out = render_layout(&spans, &letter_opts());
        assert!(
            out.contains("Provided proper") || out.contains("Provided  proper"),
            "real PDF gap must produce visible whitespace: {out:?}",
        );
    }

    #[test]
    fn tight_leading_does_not_collapse_lines() {
        // Two lines just 8pt apart (tighter than the 12pt default
        // points_per_row). Adaptive line clustering should still
        // separate them rather than collapsing to one row.
        let spans = vec![
            span_at("Line one", 60.0, 780.0, 48.0),
            span_at("Line two", 60.0, 772.0, 48.0),
        ];
        let out = render_layout(&spans, &letter_opts());
        let one = out.lines().find(|l| l.contains("one")).unwrap_or("");
        let two = out.lines().find(|l| l.contains("two")).unwrap_or("");
        // They must be on separate output lines.
        assert!(!one.contains("two"), "lines collapsed: {out:?}");
        assert!(!two.contains("one"), "lines collapsed: {out:?}");
    }

    #[test]
    fn baseline_clustering_groups_superscripts() {
        // A footnote marker rendered above the baseline (e.g. ∗)
        // must stay on the same visual line as its anchor word, not
        // be promoted to a separate row.
        let star = span_at("∗", 108.0, 783.0, 4.0); // ~3pt above baseline 780
        let mut star_span = star;
        star_span.font_size = 8.0; // smaller font for the star
        let spans = vec![span_at("Vaswani", 60.0, 780.0, 48.0), star_span];
        let out = render_layout(&spans, &letter_opts());
        let line = out
            .lines()
            .find(|l| l.contains("Vaswani"))
            .expect("found Vaswani line");
        assert!(
            line.contains('∗'),
            "superscript ∗ must stay on the same row as Vaswani: {line:?}",
        );
    }
}
