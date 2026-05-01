//! Stream order coherence scoring.
//!
//! Measures whether text spans arrive in coherent reading order from the
//! content stream. Used by the reading order module to decide whether to
//! trust stream order or apply reordering heuristics.

use super::types::TextSpan;

/// Measures whether spans arrive in coherent reading order from the content stream.
///
/// Detects column regions via gap-based clustering of span center-X positions,
/// then checks Y-monotonicity (non-increasing Y = reading downward) within each
/// column for spans in their original stream order.
///
/// Returns:
/// - ~1.0 for correct stream order (single-column or columns written sequentially)
/// - ~0.5 for interleaved Y jumps within a single detected column
/// - ~0.0 for reverse order
/// - 1.0 for empty or single-span input (trivially coherent)
pub fn stream_order_coherence(spans: &[TextSpan]) -> f64 {
    if spans.len() <= 1 {
        return 1.0;
    }

    // Compute median font size for gap threshold.
    let median_font_size = {
        let mut sizes: Vec<f64> = spans.iter().map(|s| s.font_size).collect();
        sizes.sort_by(|a, b| a.total_cmp(b));
        sizes[sizes.len() / 2]
    };

    // Cluster span center-X positions using gap-based approach.
    let centers: Vec<f64> = spans.iter().map(|s| s.x + s.width.max(0.0) / 2.0).collect();
    let columns = cluster_x_centers(&centers, median_font_size);

    // Assign each span (by index) to its nearest column cluster.
    let assignments: Vec<usize> = centers
        .iter()
        .map(|cx| nearest_x_cluster(*cx, &columns))
        .collect();

    // For each column, measure Y-monotonicity of stream-ordered spans.
    let mut total_weight = 0usize;
    let mut weighted_score = 0.0;

    for col_idx in 0..columns.len() {
        // Collect Y values in stream order for spans in this column.
        let ys: Vec<f64> = assignments
            .iter()
            .enumerate()
            .filter(|(_, &c)| c == col_idx)
            .map(|(i, _)| spans[i].y)
            .collect();

        if ys.len() < 2 {
            // Single span in column is trivially correct.
            total_weight += ys.len();
            weighted_score += ys.len() as f64;
            continue;
        }

        let pairs = ys.len() - 1;
        let correct = ys
            .windows(2)
            .filter(|w| w[1] <= w[0]) // non-increasing Y = reading downward
            .count();

        let col_score = correct as f64 / pairs as f64;
        total_weight += ys.len();
        weighted_score += col_score * ys.len() as f64;
    }

    if total_weight == 0 {
        return 1.0;
    }

    weighted_score / total_weight as f64
}

/// Cluster sorted center-X values using a gap threshold of 2x median font size.
///
/// Returns a vector of cluster center values.
fn cluster_x_centers(centers: &[f64], median_font_size: f64) -> Vec<f64> {
    if centers.is_empty() {
        return vec![];
    }

    let gap_threshold = 2.0 * median_font_size;

    let mut sorted: Vec<f64> = centers.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));

    let mut clusters: Vec<Vec<f64>> = vec![vec![sorted[0]]];

    for &cx in &sorted[1..] {
        let last_val = clusters
            .last()
            .and_then(|c| c.last().copied())
            .unwrap_or(cx);

        if (cx - last_val) > gap_threshold {
            clusters.push(vec![cx]);
        } else if let Some(last) = clusters.last_mut() {
            last.push(cx);
        }
    }

    clusters
        .iter()
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .collect()
}

/// Find the index of the nearest cluster center to a given x value.
fn nearest_x_cluster(x: f64, cluster_centers: &[f64]) -> usize {
    cluster_centers
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (x - **a).abs();
            let db = (x - **b).abs();
            da.total_cmp(&db)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use udoc_core::text::FontResolution;

    fn make_span(text: &str, x: f64, y: f64, width: f64) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            width,
            font_name: Arc::from("Test"),
            font_size: 12.0,
            rotation: 0.0,
            is_vertical: false,
            mcid: None,
            space_width: None,
            has_font_metrics: false,
            is_invisible: false,
            is_annotation: false,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
            glyph_bboxes: None,
            active_clips: Vec::new(),
        }
    }

    #[test]
    fn stream_coherence_empty_input() {
        assert!((stream_order_coherence(&[]) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stream_coherence_single_span() {
        let spans = vec![make_span("hello", 72.0, 700.0, 50.0)];
        assert!((stream_order_coherence(&spans) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stream_coherence_single_column_correct() {
        // Y decreasing = reading downward = correct order
        let spans = vec![
            make_span("line1", 72.0, 700.0, 100.0),
            make_span("line2", 72.0, 686.0, 100.0),
            make_span("line3", 72.0, 672.0, 100.0),
            make_span("line4", 72.0, 658.0, 100.0),
            make_span("line5", 72.0, 644.0, 100.0),
        ];
        let score = stream_order_coherence(&spans);
        assert!(score > 0.95, "correct order should be ~1.0, got {}", score);
    }

    #[test]
    fn stream_coherence_single_column_reverse() {
        // Y increasing = reading upward = wrong order
        let spans = vec![
            make_span("line5", 72.0, 644.0, 100.0),
            make_span("line4", 72.0, 658.0, 100.0),
            make_span("line3", 72.0, 672.0, 100.0),
            make_span("line2", 72.0, 686.0, 100.0),
            make_span("line1", 72.0, 700.0, 100.0),
        ];
        let score = stream_order_coherence(&spans);
        assert!(score < 0.15, "reverse order should be ~0.0, got {}", score);
    }

    #[test]
    fn stream_coherence_two_column_sequential() {
        // Left column then right column (correct two-column stream order)
        let spans = vec![
            make_span("L1", 72.0, 700.0, 100.0),
            make_span("L2", 72.0, 686.0, 100.0),
            make_span("L3", 72.0, 672.0, 100.0),
            make_span("L4", 72.0, 658.0, 100.0),
            make_span("R1", 350.0, 700.0, 100.0),
            make_span("R2", 350.0, 686.0, 100.0),
            make_span("R3", 350.0, 672.0, 100.0),
            make_span("R4", 350.0, 658.0, 100.0),
        ];
        let score = stream_order_coherence(&spans);
        assert!(
            score > 0.95,
            "sequential two-column should be ~1.0, got {}",
            score
        );
    }

    #[test]
    fn stream_coherence_interleaved() {
        // Alternating left/right columns (bad stream order)
        let spans = vec![
            make_span("a", 72.0, 700.0, 100.0),
            make_span("b", 72.0, 658.0, 100.0),
            make_span("c", 72.0, 686.0, 100.0),
            make_span("d", 72.0, 644.0, 100.0),
            make_span("e", 72.0, 672.0, 100.0),
            make_span("f", 72.0, 630.0, 100.0),
        ];
        let score = stream_order_coherence(&spans);
        assert!(
            score > 0.4 && score < 0.7,
            "interleaved should be ~0.5, got {}",
            score
        );
    }

    #[test]
    fn stream_coherence_same_line_spans() {
        // Same Y = non-increasing = counted as correct
        let spans = vec![
            make_span("word1", 72.0, 700.0, 50.0),
            make_span("word2", 130.0, 700.0, 50.0),
            make_span("word3", 72.0, 686.0, 50.0),
            make_span("word4", 130.0, 686.0, 50.0),
        ];
        let score = stream_order_coherence(&spans);
        assert!(
            score > 0.95,
            "same-line spans should be coherent, got {}",
            score
        );
    }

    #[test]
    fn stream_coherence_font_size_zero() {
        // When all spans have font_size=0, median is 0, gap_threshold is 0.
        // The function should still return a valid score without panicking.
        let spans = vec![
            TextSpan {
                text: "a".to_string(),
                x: 72.0,
                y: 700.0,
                width: 50.0,
                font_name: Arc::from("Test"),
                font_size: 0.0,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            },
            TextSpan {
                text: "b".to_string(),
                x: 72.0,
                y: 686.0,
                width: 50.0,
                font_name: Arc::from("Test"),
                font_size: 0.0,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            },
            TextSpan {
                text: "c".to_string(),
                x: 350.0,
                y: 700.0,
                width: 50.0,
                font_name: Arc::from("Test"),
                font_size: 0.0,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            },
        ];
        let score = stream_order_coherence(&spans);
        assert!(
            (0.0..=1.0).contains(&score),
            "score should be in [0,1], got {}",
            score
        );
    }
}
