//! Span merging for table detection.
//!
//! Merges per-glyph/per-token TextSpans into word-level spans before table
//! column detection. This is a prerequisite for the Nurminen text-edge
//! algorithm, which operates on word-level chunks.
//!
//! Reuses the word-boundary logic from `text::order` (should_add_space,
//! estimate_char_width) rather than reimplementing it.

use crate::text::order::{estimate_char_width, should_add_space};
use crate::text::types::TextSpan;

/// Merge per-glyph spans into word-level spans for table detection.
///
/// Sorts spans by baseline (Y descending, X ascending), groups by baseline
/// proximity, then merges consecutive same-font spans within each baseline
/// when no word boundary is detected.
///
/// The returned spans have word-level granularity suitable for column
/// detection algorithms (Nurminen text-edge, etc.).
pub(crate) fn merge_spans_for_table(spans: &[TextSpan]) -> Vec<TextSpan> {
    if spans.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<TextSpan> = spans.to_vec();
    // Sort by Y descending (top of page first in PDF coords), then X ascending.
    sorted.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));

    let mut result: Vec<TextSpan> = Vec::with_capacity(spans.len());

    // Walk sorted spans, merging consecutive ones on the same baseline.
    let mut iter = sorted.into_iter();
    let Some(first) = iter.next() else {
        return Vec::new();
    };
    let mut current = first;

    for span in iter {
        // Check if on the same baseline.
        let avg_fs = (current.font_size + span.font_size) / 2.0;
        let baseline_tolerance = (avg_fs * 0.3).max(1.0);
        let same_baseline = (current.y - span.y).abs() <= baseline_tolerance;

        // Check if same font.
        let same_font = current.font_name == span.font_name;
        let same_size = (current.font_size - span.font_size).abs() < 0.1;

        if same_baseline && same_font && same_size {
            let right_edge = current.x + current.width;
            let gap = span.x - right_edge;

            // For overlapping or touching spans, check overlap isn't too large.
            let should_merge = if gap < 0.0 {
                let cw = estimate_char_width(&current);
                gap.abs() < cw * 0.5
            } else {
                !should_add_space(&current, &span, gap)
            };

            if should_merge {
                current.text.push_str(&span.text);
                // Guard against negative width from malformed spans where
                // span.x + span.width < current.x.
                current.width = ((span.x + span.width) - current.x).max(current.width);
                continue;
            }
        }

        result.push(current);
        current = span;
    }
    result.push(current);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_span(text: &str, x: f64, y: f64, width: f64, font_size: f64) -> TextSpan {
        TextSpan::new(
            text.to_string(),
            x,
            y,
            width,
            "TestFont".to_string(),
            font_size,
        )
    }

    #[test]
    fn test_merge_per_glyph_spans() {
        // Simulate per-glyph Tj operators: "Hello" as 5 separate spans
        let spans = vec![
            make_span("H", 100.0, 500.0, 6.0, 12.0),
            make_span("e", 106.0, 500.0, 5.5, 12.0),
            make_span("l", 111.5, 500.0, 3.0, 12.0),
            make_span("l", 114.5, 500.0, 3.0, 12.0),
            make_span("o", 117.5, 500.0, 6.0, 12.0),
        ];
        let merged = merge_spans_for_table(&spans);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Hello");
    }

    #[test]
    fn test_merge_preserves_word_boundaries() {
        // Two words with a clear gap between them
        let spans = vec![
            make_span("Hello", 100.0, 500.0, 30.0, 12.0),
            make_span("World", 145.0, 500.0, 30.0, 12.0),
        ];
        let merged = merge_spans_for_table(&spans);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "Hello");
        assert_eq!(merged[1].text, "World");
    }

    #[test]
    fn test_merge_different_baselines() {
        // Spans on different baselines should never merge
        let spans = vec![
            make_span("Row1", 100.0, 500.0, 30.0, 12.0),
            make_span("Row2", 100.0, 485.0, 30.0, 12.0),
        ];
        let merged = merge_spans_for_table(&spans);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_empty_input() {
        let merged = merge_spans_for_table(&[]);
        assert!(merged.is_empty());
    }

    #[test]
    fn test_merge_single_span() {
        let spans = vec![make_span("Only", 100.0, 500.0, 24.0, 12.0)];
        let merged = merge_spans_for_table(&spans);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Only");
    }
}
