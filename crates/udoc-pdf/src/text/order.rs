//! Reading order reconstruction from raw text spans.
//!
//! Takes unordered spans from content stream interpretation and produces
//! lines in reading order: top-to-bottom, left-to-right within each line.
//!
//! Algorithm (v4, tiered cascade):
//! Tier 0. Structure tree ordering (tagged PDFs with MCIDs, handled upstream)
//! Tier 1. Stream order coherence check: if score > 0.85, skip X-Y cut
//! Tier 2. X-Y cut with pre-masking (column detection + per-column ordering)
//! Tier 3. Y-X fallback (single-column ordering as leaf within Tier 2)
//!
//! Per-column/single-column steps:
//! 1. Group spans by baseline Y coordinate (within tolerance)
//! 2. Sort spans within each line by X coordinate
//! 3. Detect word boundaries (gap > threshold -> insert space)
//! 4. Merge adjacent same-font spans
//! 5. Sort lines top-to-bottom (descending Y in PDF coordinates)

use super::coherence::stream_order_coherence;
use super::types::{TextLine, TextSpan};
use super::xy_cut::{
    reinsert_full_width_lines, reorder_partitions_breuel, separate_full_width_spans,
    xy_cut_recursive, MAX_BASELINES,
};
use crate::content::marked_content::{HasMcid, PageStructureOrder};
use crate::diagnostics::{DiagnosticsSink, Warning, WarningContext, WarningKind, WarningLevel};

/// Optional diagnostics context for reading order functions.
/// When present, tier selection info is emitted via the sink.
pub(crate) struct OrderDiagnostics<'a> {
    pub sink: &'a dyn DiagnosticsSink,
    pub page_index: usize,
}

impl HasMcid for TextSpan {
    fn mcid(&self) -> Option<u32> {
        self.mcid
    }
}

/// Default baseline tolerance in points. Spans within this Y distance
/// are considered to be on the same line.
pub(super) const BASELINE_TOLERANCE: f64 = 2.0;

/// Normalized gap below this is definitely intra-word (kerning, ligature adjustment).
/// Used in Tier 2 of the word boundary algorithm when font space width is unavailable.
const SAME_WORD_MAX: f64 = 0.03;

/// Normalized gap above this is definitely a word break.
/// Used in Tier 2 of the word boundary algorithm when font space width is unavailable.
const WORD_BREAK_MIN: f64 = 0.15;

/// Rotation threshold in degrees. Spans with absolute rotation above
/// this are considered "rotated" and grouped separately.
const ROTATION_THRESHOLD: f64 = 5.0;

/// Tolerance for clustering X positions of vertical spans into columns.
/// Spans within this horizontal distance are considered part of the same column.
const VERTICAL_COLUMN_TOLERANCE: f64 = 5.0;

// -- Tiered cascade constants --

/// Minimum stream-order coherence score to trust the PDF's content stream order.
/// Below this threshold, X-Y cut geometric reordering is applied (Tier 2).
/// Above it, stream-sequential ordering preserves content stream order (Tier 1).
///
/// Lowered from 0.90 to 0.75: MuPDF ground truth uses raw content stream order,
/// so matching it requires trusting stream order rather than overriding with
/// geometric analysis. PDFium achieves 0.962 order_corr by always trusting
/// stream order. Most well-generated PDFs (LaTeX, Word, InDesign) have
/// coherence > 0.95. At 0.75, up to 25% of consecutive span pairs can have
/// inverted Y order and we still trust the stream.
///
/// Validated on 20K corpus (MuPDF GT: char_acc 0.870, word_f1 0.923),
/// pypdf (word_f1 0.881), and realworld (char_acc 0.861). No regressions
/// observed vs the 0.90 threshold on any corpus.
const STREAM_COHERENCE_THRESHOLD: f64 = 0.75;

/// Reconstruct reading order from raw content-stream-order spans.
///
/// Detects multi-column layouts and orders each column independently
/// (top-to-bottom within column, columns left-to-right). Falls back to
/// single-column ordering when no column structure is detected.
///
/// Output order: horizontal text first, then vertical CJK, then rotated.
// Used by fuzz_reading_order
#[cfg(any(test, feature = "test-internals"))]
pub fn order_spans(spans: Vec<TextSpan>) -> Vec<TextLine> {
    order_spans_with_diagnostics(spans, None, None)
}

/// Output order: horizontal text first, then vertical CJK, then rotated.
#[cfg(not(any(test, feature = "test-internals")))]
pub(crate) fn order_spans(spans: Vec<TextSpan>) -> Vec<TextLine> {
    order_spans_with_diagnostics(spans, None, None)
}

/// Order spans with diagnostics context (emits tier selection info).
pub(crate) fn order_spans_with_diagnostics(
    spans: Vec<TextSpan>,
    structure_order: Option<&PageStructureOrder>,
    diag: Option<&OrderDiagnostics<'_>>,
) -> Vec<TextLine> {
    order_spans_with_structure_and_diagnostics(spans, structure_order, diag)
}

/// Reconstruct reading order, optionally using structure tree ordering.
///
/// If `structure_order` is provided and contains MCIDs matching spans,
/// spans are first reordered according to the document's logical structure
/// (from the structure tree) before geometric ordering is applied.
/// This produces correct reading order for tagged PDFs where content
/// stream order may not match logical order.
///
/// Falls back to pure geometric ordering when no structure order is
/// available or when spans have no MCIDs.
#[cfg(test)]
pub(crate) fn order_spans_with_structure(
    spans: Vec<TextSpan>,
    structure_order: Option<&PageStructureOrder>,
) -> Vec<TextLine> {
    order_spans_with_structure_and_diagnostics(spans, structure_order, None)
}

fn order_spans_with_structure_and_diagnostics(
    mut spans: Vec<TextSpan>,
    structure_order: Option<&PageStructureOrder>,
    diag: Option<&OrderDiagnostics<'_>>,
) -> Vec<TextLine> {
    if spans.is_empty() {
        return Vec::new();
    }

    // If structure ordering is available, reorder spans by MCID before
    // geometric processing. Structured spans come first (in logical order),
    // unstructured spans retain their relative position after.
    if let Some(order) = structure_order {
        // Count spans with MCIDs in the structure order for diagnostics
        let structured_count = if diag.is_some() {
            spans
                .iter()
                .filter(|s| {
                    s.mcid
                        .map(|m| order.mcid_order.contains(&m))
                        .unwrap_or(false)
                })
                .count()
        } else {
            0
        };

        crate::content::marked_content::reorder_by_structure(&mut spans, order);

        // Emit Tier 0 diagnostic with count of structure-reordered spans
        if structured_count > 0 {
            emit_tier0_diagnostic(diag, structured_count);
        }
    }

    // Partition into three groups emitted in this order:
    // 1. Horizontal (the common case, with multi-column detection)
    // 2. Vertical CJK (columns right-to-left)
    // 3. Rotated (grouped by angle)
    let mut horizontal = Vec::new();
    let mut rotated = Vec::new();
    let mut vertical = Vec::new();

    for span in spans {
        // Vertical CJK takes precedence over rotation. A vertical font
        // (CMap ending in -V) should use column-based ordering even if
        // the text rendering matrix has non-zero rotation. Vertical fonts
        // with large rotation are rare; if they arise, the vertical
        // algorithm (top-to-bottom, R-to-L columns) is still more
        // appropriate than the generic rotated-text fallback.
        if span.is_vertical {
            vertical.push(span);
        } else if span.rotation.abs() >= ROTATION_THRESHOLD {
            rotated.push(span);
        } else {
            horizontal.push(span);
        }
    }

    let mut lines = order_horizontal_spans(horizontal, structure_order, diag);

    // Vertical CJK text: top-to-bottom within columns, columns right-to-left.
    if !vertical.is_empty() {
        let vertical_lines = order_vertical_spans(vertical);
        lines.extend(vertical_lines);
    }

    // Process rotated spans: group by similar rotation angle, order
    // each group independently, append after horizontal and vertical text.
    if !rotated.is_empty() {
        let rotated_lines = order_rotated_spans(rotated);
        lines.extend(rotated_lines);
    }

    lines
}

/// Order horizontal (non-rotated) spans with tiered reading order cascade.
///
/// Tier 0 (structure tree): handled upstream in order_spans_with_structure
///   via MCID reordering before this function is called.
///
/// Tier 1 (stream order): if the content stream's Y order is coherent
///   (score > STREAM_COHERENCE_THRESHOLD), use single-column ordering
///   directly. This avoids unnecessary X-Y cut reordering on well-generated
///   single-column PDFs.
///
/// Tier 2 (X-Y cut): recursive X-Y cut with pre-masking to partition spans
///   into reading-order regions. Used when stream order is unreliable.
///
/// Tier 3 (Y-X fallback): single-column ordering is the leaf operation
///   within Tier 2, providing basic top-to-bottom, left-to-right order.
fn order_horizontal_spans(
    mut spans: Vec<TextSpan>,
    structure_order: Option<&PageStructureOrder>,
    diag: Option<&OrderDiagnostics<'_>>,
) -> Vec<TextLine> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Tier 1: check stream order coherence before doing geometric work.
    // If the content stream already has a coherent top-to-bottom ordering,
    // we can skip X-Y cut entirely. This preserves correct ordering on
    // well-generated PDFs and avoids false column splits.
    //
    // Key insight: MuPDF ground truth uses raw content stream order (no
    // column segmentation). PDFium achieves 0.962 order correlation by
    // trusting stream order and only sorting within lines. Our X-Y cut
    // often makes ordering WORSE by overriding a correct stream order
    // with geometric analysis. Trust coherence unconditionally: if the
    // stream order is good, don't override it regardless of how many
    // geometric partitions the X-Y cut would find.
    let coherence = stream_order_coherence(&spans);

    // Emit info diagnostic when coherence is near the tier threshold.
    // Scores within 0.10 of the boundary could plausibly go either way.
    const AMBIGUOUS_LOW: f64 = STREAM_COHERENCE_THRESHOLD - 0.10;
    const AMBIGUOUS_HIGH: f64 = STREAM_COHERENCE_THRESHOLD + 0.10;
    if (AMBIGUOUS_LOW..=AMBIGUOUS_HIGH).contains(&coherence) {
        if let Some(d) = diag {
            d.sink.info(Warning {
                offset: None,
                kind: WarningKind::ReadingOrder,
                level: WarningLevel::Info,
                context: WarningContext {
                    page_index: Some(d.page_index),
                    ..Default::default()
                },
                message: format!(
                    "coherence score {:.3} is ambiguous (between tier thresholds), \
                     tier choice may be uncertain",
                    coherence
                ),
            });
        }
    }

    if coherence > STREAM_COHERENCE_THRESHOLD {
        // Stream order is coherent. Use sequential baseline clustering:
        // walk spans in stream order with a limited lookback window.
        // This naturally separates columns (old lines expire from the
        // active set before the next column's spans arrive) while keeping
        // table rows intact (cells at same Y stay on the same active line).
        emit_tier_diagnostic(diag, 1, coherence, 1);
        return order_stream_sequential(spans);
    }

    // Pre-mask: separate full-width spans for X-Y cut.
    let full_width_spans = separate_full_width_spans(&mut spans);

    if spans.is_empty() {
        return order_single_column(full_width_spans, structure_order);
    }

    // Tier 2: X-Y cut reordering for disordered content streams.
    // Recursive X-Y cut on non-full-width spans.
    let partitions = xy_cut_recursive(spans, 0, diag);

    // Breuel spatial ordering ( step 3): reorder leaf partitions
    // using pairwise spatial rules + topological sort.
    let partitions = reorder_partitions_breuel(partitions, diag);

    emit_tier_diagnostic(diag, 2, coherence, partitions.len());

    // Order each leaf partition independently (Tier 3: single-column Y-X sort)
    let mut all_lines = Vec::new();
    for partition in partitions {
        if !partition.is_empty() {
            let mut lines = order_single_column(partition, structure_order);
            all_lines.append(&mut lines);
        }
    }

    // Reinsert full-width spans at their correct Y positions
    reinsert_full_width_lines(&mut all_lines, full_width_spans, structure_order);

    all_lines
}

fn emit_tier_diagnostic(
    diag: Option<&OrderDiagnostics<'_>>,
    tier: u8,
    coherence: f64,
    partitions: usize,
) {
    if let Some(d) = diag {
        let ctx = WarningContext {
            page_index: Some(d.page_index),
            obj_ref: None,
        };
        d.sink.info(Warning::info_with_context(
            WarningKind::TierSelection,
            ctx,
            format!("coherence={coherence:.3}, partitions={partitions}, tier={tier}"),
        ));
    }
}

fn emit_tier0_diagnostic(diag: Option<&OrderDiagnostics<'_>>, structured_count: usize) {
    if let Some(d) = diag {
        let ctx = WarningContext {
            page_index: Some(d.page_index),
            obj_ref: None,
        };
        d.sink.info(Warning::info_with_context(
            WarningKind::TierSelection,
            ctx,
            format!("Tier 0: {structured_count} spans reordered by structure tree MCIDs"),
        ));
    }
}

/// Single-column reading order (the original v1 algorithm).
///
/// 1. Cluster by baseline
/// 2. Sort L-to-R within each line
/// 3. Insert word spaces
/// 4. Merge adjacent same-font spans
/// 5. Sort lines top-to-bottom
pub(super) fn order_single_column(
    spans: Vec<TextSpan>,
    structure_order: Option<&PageStructureOrder>,
) -> Vec<TextLine> {
    // Step 1: Group spans into lines by baseline clustering
    let mut lines = cluster_by_baseline(spans);

    // Step 2: Sort spans within each line by X coordinate
    for line in &mut lines {
        line.spans.sort_by(|a, b| a.x.total_cmp(&b.x));
    }

    // Step 3: Detect word boundaries and insert spaces
    for line in &mut lines {
        insert_word_spaces(&mut line.spans);
    }

    // Step 4: Merge adjacent spans with same font
    for line in &mut lines {
        let spans = std::mem::take(&mut line.spans);
        line.spans = merge_adjacent_spans(spans);
    }

    // Step 5: Sort lines top-to-bottom by Y coordinate.
    // Geometric Y-sort preferred over MCID order within leaf partitions:
    // MCID sequences follow content stream order, not visual reading
    // order, causing reversed paragraphs in multi-column tagged PDFs.
    // Structure tree reordering is applied at span level upstream (Tier 0).
    let _ = structure_order;
    lines.sort_by(|a, b| b.baseline.total_cmp(&a.baseline));

    lines
}

/// Stream-order-preserving variant of single-column ordering.
///
/// Instead of globally clustering all spans by baseline (which mixes
/// columns at the same Y level), walks spans in content stream order
/// with a limited lookback window. Spans only match against recently-
/// seen lines. When the content stream transitions from one column to
/// another (large Y jump), the old column's lines have expired from
/// the active set, so new-column spans create fresh lines.
///
/// This naturally preserves column ordering for well-generated PDFs:
/// left column lines output first, then right column lines. Matches
/// mupdf's content-stream-order output. Tables (row-by-row emission)
/// also work correctly: cells at the same Y match the same active line.
///
/// Only used in Tier 1 (high coherence) where the stream order is trusted.
fn order_stream_sequential(spans: Vec<TextSpan>) -> Vec<TextLine> {
    // Maximum active lines. Small enough that column transitions naturally
    // expire old lines (a column with 25+ lines will have its top lines
    // evicted by the time the next column starts), large enough for normal
    // inline elements within a few consecutive lines.
    const MAX_ACTIVE: usize = 5;

    let mut active: Vec<TextLine> = Vec::new();
    let mut completed: Vec<TextLine> = Vec::new();

    for span in spans {
        // Find a matching active line (baseline within tolerance).
        let match_idx = active
            .iter()
            .position(|line| (line.baseline - span.y).abs() <= BASELINE_TOLERANCE);

        match match_idx {
            Some(idx) => {
                let y = span.y;
                active[idx].spans.push(span);
                // Update baseline as running average to handle slight Y drift.
                let line = &mut active[idx];
                let n = line.spans.len() as f64;
                line.baseline = line.baseline * ((n - 1.0) / n) + y / n;
            }
            None => {
                // No match: evict oldest active line if at capacity.
                if active.len() >= MAX_ACTIVE {
                    completed.push(active.remove(0));
                }
                active.push(TextLine {
                    baseline: span.y,
                    spans: vec![span],
                    is_vertical: false,
                });
            }
        }
    }

    // Flush remaining active lines in order.
    completed.extend(active);

    // X-sort within each line.
    for line in &mut completed {
        line.spans.sort_by(|a, b| a.x.total_cmp(&b.x));
    }

    // Recompute baseline as median Y of all spans.
    for line in &mut completed {
        if line.spans.len() > 1 {
            let mut ys: Vec<f64> = line.spans.iter().map(|s| s.y).collect();
            ys.sort_by(|a, b| a.total_cmp(b));
            line.baseline = ys[ys.len() / 2];
        }
    }

    // Insert word spaces.
    for line in &mut completed {
        insert_word_spaces(&mut line.spans);
    }

    // Merge adjacent spans.
    for line in &mut completed {
        let spans = std::mem::take(&mut line.spans);
        line.spans = merge_adjacent_spans(spans);
    }

    completed
}

/// Group spans into lines by baseline Y coordinate proximity,
/// then split lines at column boundaries (large X gaps).
///
fn cluster_by_baseline(mut spans: Vec<TextSpan>) -> Vec<TextLine> {
    // Sort by Y coordinate (ascending, but order doesn't matter for clustering)
    spans.sort_by(|a, b| a.y.total_cmp(&b.y));

    let mut lines: Vec<TextLine> = Vec::new();

    for span in spans {
        // Note: first-match greedy clustering. Spans very close together (e.g., 0.2pt apart)
        // can land in different lines if an earlier seed captured one of them. Acceptable
        // at BASELINE_TOLERANCE of 2.0pt for typical documents.
        let target = lines
            .iter()
            .position(|line| (line.baseline - span.y).abs() <= BASELINE_TOLERANCE);

        match target {
            Some(idx) => lines[idx].spans.push(span),
            None => {
                if lines.len() < MAX_BASELINES {
                    lines.push(TextLine {
                        baseline: span.y,
                        spans: vec![span],
                        is_vertical: false,
                    });
                } else {
                    // Beyond MAX_BASELINES: assign to nearest existing baseline
                    // to prevent O(n*m) from becoming O(n^2).
                    let nearest = lines
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| {
                            let da = (a.baseline - span.y).abs();
                            let db = (b.baseline - span.y).abs();
                            da.total_cmp(&db)
                        })
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    lines[nearest].spans.push(span);
                }
            }
        }
    }

    // Recompute baseline as median Y of all spans in each line
    for line in &mut lines {
        if !line.spans.is_empty() {
            let mut ys: Vec<f64> = line.spans.iter().map(|s| s.y).collect();
            ys.sort_by(|a, b| a.total_cmp(b));
            line.baseline = ys[ys.len() / 2]; // median
        }
    }

    lines
}

/// Insert space characters between spans using three-tier word boundary detection.
///
/// Tier 1: Font-derived space width (most accurate when available).
///   Uses the space glyph width from TextSpan.space_width, threshold = space_width * 0.5.
///
/// Tier 2: Font-size-relative thresholds (reliable fallback).
///   gap/font_size < SAME_WORD_MAX (0.03) -> no space
///   gap/font_size > WORD_BREAK_MIN (0.15) -> space
///   Gray zone (0.03..0.15): adaptive threshold from char width (PDFium-style).
///
/// Tier 3: Script-aware filtering.
///   Suppresses false spaces between CJK characters that don't use word spacing.
fn insert_word_spaces(spans: &mut [TextSpan]) {
    if spans.len() < 2 {
        return;
    }

    for i in 1..spans.len() {
        let prev_right_edge = spans[i - 1].x + spans[i - 1].width;
        let gap = spans[i].x - prev_right_edge;

        if !should_add_space(&spans[i - 1], &spans[i], gap) {
            continue;
        }

        // Tier 3: script-aware filtering
        let prev_last = spans[i - 1].text.chars().next_back();
        let curr_first = spans[i].text.chars().next();
        if let (Some(p), Some(c)) = (prev_last, curr_first) {
            if !should_insert_space_for_scripts(p, c) {
                continue;
            }
        }

        spans[i].text.insert(0, ' ');
    }
}

/// Determine whether a gap between two spans constitutes a word boundary.
///
/// Implements Tier 1 (font space width) and Tier 2 (size-relative) logic.
pub(crate) fn should_add_space(prev: &TextSpan, curr: &TextSpan, gap: f64) -> bool {
    // Negative or zero gap = overlapping or touching. Not a space.
    if gap <= 0.0 {
        return false;
    }

    let font_size = (prev.font_size + curr.font_size) / 2.0;
    if font_size <= 0.0 {
        return false;
    }

    // Tier 1: font-derived space width (most accurate)
    if let Some(sw) = prev.space_width.or(curr.space_width) {
        return gap > sw * 0.5;
    }

    // Tier 2: size-relative thresholds.
    // Use max(font_size, avg_char_width) as the effective size. Some PDFs
    // (XeLaTeX) set font_size=1.0 and do all scaling through the text matrix,
    // making font_size unreliable. In those cases avg_char_width is a better
    // proxy for the visual character size.
    let prev_cw = estimate_char_width(prev);
    let curr_cw = estimate_char_width(curr);
    let avg_cw = (prev_cw + curr_cw) / 2.0;
    let effective_size = font_size.max(avg_cw);

    let normalized = gap / effective_size;

    if normalized < SAME_WORD_MAX {
        return false;
    }

    if normalized > WORD_BREAK_MIN {
        return true;
    }

    // Gray zone (0.03..0.15): adaptive char-width normalization (PDFium-style).
    // Map average char width (in thousandths of effective size) to a divisor
    // that scales the threshold for different font widths.
    let avg_thousandths = avg_cw / effective_size * 1000.0;

    let divisor = if avg_thousandths < 300.0 {
        2.0
    } else if avg_thousandths < 500.0 {
        4.0
    } else if avg_thousandths < 700.0 {
        5.0
    } else {
        6.0 // wide glyphs (CJK, decorative)
    };

    let adaptive_threshold = avg_cw / divisor;
    gap > adaptive_threshold
}

/// Returns true if a space should be inserted between characters of the given scripts.
///
/// CJK ideographs, kana, and hangul do not use inter-word spaces, so inserting
/// spaces between adjacent CJK characters would be wrong. A space is only inserted
/// when at least one side is from a script that uses word spacing (Latin, etc.).
fn should_insert_space_for_scripts(prev: char, next: char) -> bool {
    // Suppress space when both sides are non-spacing scripts (CJK, kana, hangul)
    !(is_non_spacing_script(prev) && is_non_spacing_script(next))
}

/// Returns true if the character belongs to a script that does NOT use inter-word spaces.
fn is_non_spacing_script(c: char) -> bool {
    let cp = c as u32;
    matches!(cp,
        // CJK Unified Ideographs
        0x4E00..=0x9FFF |
        // CJK Extension A
        0x3400..=0x4DBF |
        // CJK Extension B
        0x20000..=0x2A6DF |
        // CJK Extension C-G (subsumes CJK Compat Ideographs Supplement 2F800-2FA1F).
        // Intentionally covers some unassigned blocks between extensions;
        // over-inclusive is safer than missing CJK characters for spacing.
        0x2A700..=0x323AF |
        // CJK Compatibility Ideographs
        0xF900..=0xFAFF |
        // Hiragana
        0x3040..=0x309F |
        // Katakana
        0x30A0..=0x30FF |
        // Katakana Phonetic Extensions
        0x31F0..=0x31FF |
        // Hangul Syllables
        0xAC00..=0xD7AF |
        // Hangul Jamo
        0x1100..=0x11FF |
        // Hangul Compatibility Jamo
        0x3130..=0x318F |
        // CJK Symbols and Punctuation
        0x3000..=0x303F |
        // Halfwidth and Fullwidth Forms (includes fullwidth Latin/digits
        // FF10-FF5A, but these are used in CJK contexts so treating them
        // as non-spacing is correct for CJK-adjacent positioning)
        0xFF00..=0xFFEF |
        // Bopomofo
        0x3100..=0x312F |
        // CJK Radicals Supplement
        0x2E80..=0x2EFF |
        // Kangxi Radicals
        0x2F00..=0x2FDF |
        // Ideographic Description Characters
        0x2FF0..=0x2FFF
    )
}

/// Estimate the average character width of a span from its width and text length.
/// Falls back to font_size if text is empty.
pub(crate) fn estimate_char_width(span: &TextSpan) -> f64 {
    let char_count = span.text.chars().count();
    if char_count > 0 && span.width > 0.0 {
        span.width / char_count as f64
    } else {
        span.font_size
    }
}

/// Merge adjacent spans that share the same font name and size.
///
/// This reduces fragmentation from PDF content streams that split words
/// into multiple Tj operations (common with kerning and ligatures).
/// Only merges if the gap is small enough that no word boundary was detected.
pub(crate) fn merge_adjacent_spans(spans: Vec<TextSpan>) -> Vec<TextSpan> {
    let mut iter = spans.into_iter();
    let Some(first) = iter.next() else {
        return Vec::new();
    };
    let mut merged: Vec<TextSpan> = vec![first];

    for span in iter {
        // merged always has at least one element (pushed above).
        let Some(last) = merged.last_mut() else {
            break;
        };
        let same_font = last.font_name == span.font_name;
        // Font sizes within 0.1pt are considered the same
        let same_size = (last.font_size - span.font_size).abs() < 0.1;
        // Only merge if spans are close together (no word boundary).
        // For positive gaps: delegate to should_add_space (three-tier algorithm).
        // For negative gaps (overlap): limit how much overlap we allow.
        // Large overlaps often mean duplicate rendering (bold simulation, etc.)
        // and should not be merged.
        let right_edge = last.x + last.width;
        let gap = span.x - right_edge;
        let char_width = estimate_char_width(last);
        let small_gap = if gap < 0.0 {
            gap.abs() < char_width * 0.5
        } else {
            !should_add_space(last, &span, gap)
        };

        if same_font && same_size && small_gap {
            last.text.push_str(&span.text);
            last.width = (span.x + span.width) - last.x;
        } else {
            merged.push(span);
        }
    }

    merged
}

/// Order rotated spans into lines.
///
/// Groups spans by similar rotation angle (within ROTATION_THRESHOLD), then
/// orders each group as a separate set of lines. Rotated groups are emitted
/// in order of their rotation angle.
///
/// Rotated text ordering is best-effort. For axis-aligned rotations (90, 180, 270),
/// device-space coordinates map cleanly to reading order. For arbitrary angles,
/// the baseline clustering and L-to-R sort may not produce correct reading order.
/// Use raw_spans() for precise control over rotated text.
///
/// Note: For 90/270-degree rotated text, spans at different device-space Y
/// positions will land on separate "lines" since baseline clustering uses Y.
/// This fragments sideways text into one-span lines. The result is still
/// correct (all text present, in roughly spatial order) but not line-grouped.
fn order_rotated_spans(spans: Vec<TextSpan>) -> Vec<TextLine> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Group by similar rotation angle
    let mut groups: Vec<(f64, Vec<TextSpan>)> = Vec::new();
    for span in spans {
        let target = groups
            .iter()
            .position(|(angle, _)| (angle - span.rotation).abs() < ROTATION_THRESHOLD);
        match target {
            Some(idx) => groups[idx].1.push(span),
            None => groups.push((span.rotation, vec![span])),
        }
    }

    // Sort groups by rotation angle
    groups.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Order each rotation group independently using single-column algorithm.
    // Structure ordering is not threaded here: rotated spans are rare and
    // unlikely to carry MCIDs. Geometric ordering is always used.
    let mut all_lines = Vec::new();
    for (_angle, group_spans) in groups {
        let mut lines = order_single_column(group_spans, None);
        all_lines.append(&mut lines);
    }
    all_lines
}

/// Order vertical CJK spans into lines.
///
/// Vertical text flows top-to-bottom within a column, and columns are
/// ordered right-to-left (the traditional CJK reading direction).
///
/// Algorithm:
/// 1. Group spans by X proximity into columns
/// 2. Within each column, sort spans top-to-bottom (descending Y)
/// 3. Sort columns right-to-left (descending X)
/// 4. Each column becomes one TextLine
///
/// Note: we skip `insert_word_spaces` and `merge_adjacent_spans` here
/// because both use X-axis gap logic designed for horizontal text.
/// Vertical CJK text does not typically have word spaces, and the spans
/// are concatenated by `TextLine::text()` regardless of merge state.
fn order_vertical_spans(mut spans: Vec<TextSpan>) -> Vec<TextLine> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Sort by X before clustering so that first-match grouping is deterministic,
    // consistent with how cluster_by_baseline sorts by Y before grouping.
    spans.sort_by(|a, b| a.x.total_cmp(&b.x));

    // Group spans into columns by X proximity.
    // Note: column representative X is the first span's X, not updated as spans
    // are added. This means the R-to-L sort uses the first-seen X, not the mean.
    // Acceptable for typical vertical CJK where columns are well-separated.
    let mut columns: Vec<(f64, Vec<TextSpan>)> = Vec::new();

    for span in spans {
        let target = columns
            .iter()
            .position(|(col_x, _)| (*col_x - span.x).abs() <= VERTICAL_COLUMN_TOLERANCE);

        match target {
            Some(idx) => columns[idx].1.push(span),
            None => {
                if columns.len() < MAX_BASELINES {
                    columns.push((span.x, vec![span]));
                }
                // Beyond cap: skip. Adversarial input with 10K+ unique X
                // positions is not a real vertical CJK document.
            }
        }
    }

    // Sort columns right-to-left (descending X)
    columns.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut all_lines = Vec::new();

    for (col_x, mut col_spans) in columns {
        // Sort spans top-to-bottom within column (descending Y in PDF coords)
        col_spans.sort_by(|a, b| b.y.total_cmp(&a.y));

        all_lines.push(TextLine {
            baseline: col_x, // Use column X as "baseline" for vertical text
            spans: col_spans,
            is_vertical: true,
        });
    }

    all_lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use udoc_core::text::FontResolution;

    fn span(text: &str, x: f64, y: f64, width: f64, font_size: f64) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            width,
            font_name: Arc::from("Helvetica"),
            font_size,
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

    fn span_font(text: &str, x: f64, y: f64, width: f64, font_name: &str) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            width,
            font_name: Arc::from(font_name),
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

    fn span_vertical(text: &str, x: f64, y: f64, width: f64) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            width,
            font_name: Arc::from("MS-Mincho"),
            font_size: 12.0,
            rotation: 0.0,
            is_vertical: true,
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
    fn test_empty_input() {
        let result = order_spans(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_single_span() {
        let result = order_spans(vec![span("Hello", 100.0, 700.0, 40.0, 12.0)]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text(), "Hello");
        assert_eq!(result[0].baseline, 700.0);
    }

    #[test]
    fn test_baseline_clustering() {
        // Three spans: two on same baseline (within tolerance), one on different line
        let spans = vec![
            span("World", 150.0, 700.5, 40.0, 12.0), // same line as Hello (0.5pt diff)
            span("Hello", 100.0, 700.0, 40.0, 12.0),
            span("Below", 100.0, 680.0, 40.0, 12.0), // different line (20pt diff)
        ];
        let result = order_spans(spans);
        assert_eq!(result.len(), 2);
        // Top line first (700 > 680)
        assert_eq!(result[0].spans.len(), 2);
        assert_eq!(result[1].spans.len(), 1);
    }

    #[test]
    fn test_left_to_right_sort() {
        // Spans out of order on same baseline
        let spans = vec![
            span("World", 200.0, 700.0, 40.0, 12.0),
            span("Hello", 100.0, 700.0, 40.0, 12.0),
        ];
        let result = order_spans(spans);
        assert_eq!(result.len(), 1);
        // After merging/spacing, first span should be Hello
        assert!(result[0].text().starts_with("Hello"));
    }

    #[test]
    fn test_top_to_bottom_sort() {
        // Lines out of order
        let spans = vec![
            span("Line3", 100.0, 660.0, 40.0, 12.0),
            span("Line1", 100.0, 700.0, 40.0, 12.0),
            span("Line2", 100.0, 680.0, 40.0, 12.0),
        ];
        let result = order_spans(spans);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text(), "Line1"); // y=700 (top)
        assert_eq!(result[1].text(), "Line2"); // y=680
        assert_eq!(result[2].text(), "Line3"); // y=660 (bottom)
    }

    #[test]
    fn test_word_gap_detection() {
        // Two spans with a gap larger than 0.25 * font_size (3pt > 12*0.25=3pt)
        let spans = vec![
            span("Hello", 100.0, 700.0, 40.0, 12.0),
            span("World", 144.0, 700.0, 40.0, 12.0), // gap = 144 - 140 = 4pt > 3pt
        ];
        let result = order_spans(spans);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].text().contains(' '),
            "expected space between words: {:?}",
            result[0].text()
        );
    }

    #[test]
    fn test_no_space_for_small_gap() {
        // Two spans that are adjacent (no gap)
        let spans = vec![
            span("Hel", 100.0, 700.0, 20.0, 12.0),
            span("lo", 120.0, 700.0, 14.0, 12.0), // gap = 120 - 120 = 0
        ];
        let result = order_spans(spans);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text(), "Hello"); // merged, no space
    }

    #[test]
    fn test_span_merging_same_font() {
        // Adjacent spans with same font should merge
        let spans = vec![
            span("Hel", 100.0, 700.0, 18.0, 12.0),
            span("lo", 118.0, 700.0, 12.0, 12.0),
            span("Wor", 130.0, 700.0, 18.0, 12.0),
            span("ld", 148.0, 700.0, 12.0, 12.0),
        ];
        let result = order_spans(spans);
        assert_eq!(result.len(), 1);
        // All should merge into one span since same font and adjacent
        assert_eq!(result[0].text(), "HelloWorld");
    }

    #[test]
    fn test_different_font_not_merged() {
        // Adjacent spans with different fonts should not merge
        let spans = vec![
            span_font("Hello", 100.0, 700.0, 40.0, "Helvetica"),
            span_font("World", 140.0, 700.0, 40.0, "Times"),
        ];
        let result = order_spans(spans);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].spans.len(), 2);
        assert_eq!(result[0].spans[0].text, "Hello");
        assert_eq!(result[0].spans[1].text, "World");
    }

    #[test]
    fn test_multiline_document() {
        let spans = vec![
            span("This", 72.0, 720.0, 30.0, 12.0),
            span("is", 108.0, 720.0, 12.0, 12.0),
            span("line", 126.0, 720.0, 24.0, 12.0),
            span("one.", 156.0, 720.0, 24.0, 12.0),
            span("This", 72.0, 700.0, 30.0, 12.0),
            span("is", 108.0, 700.0, 12.0, 12.0),
            span("line", 126.0, 700.0, 24.0, 12.0),
            span("two.", 156.0, 700.0, 24.0, 12.0),
        ];
        let result = order_spans(spans);
        assert_eq!(result.len(), 2);
        assert!(result[0].text().contains("one"));
        assert!(result[1].text().contains("two"));
    }

    // -- Corpus integration tests --

    use crate::content::interpreter::{get_page_content, ContentInterpreter};
    use crate::object::resolver::ObjectResolver;
    use crate::object::PdfObject;
    use crate::parse::DocumentParser;
    use crate::CollectingDiagnostics;

    const CORPUS_DIR: &str = "tests/corpus/minimal";

    /// Extract ordered text lines from the first page of a corpus PDF.
    fn extract_lines(filename: &str) -> Vec<TextLine> {
        let path = format!("{CORPUS_DIR}/{filename}");
        let data = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let diag = Arc::new(CollectingDiagnostics::new());
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag.clone());
        let trailer = resolver.trailer().unwrap().clone();
        let root_ref = trailer.get_ref(b"Root").unwrap();
        let catalog = resolver.resolve_dict(root_ref).unwrap();
        let pages_ref = catalog.get_ref(b"Pages").unwrap();
        let pages_dict = resolver.resolve_dict(pages_ref).unwrap();
        let kids = pages_dict.get_array(b"Kids").unwrap();
        let page_ref = match &kids[0] {
            PdfObject::Reference(r) => *r,
            _ => panic!("expected reference in /Kids"),
        };
        let page_dict = resolver.resolve_dict(page_ref).unwrap();
        let resources = resolver
            .get_resolved_dict(&page_dict, b"Resources")
            .unwrap()
            .unwrap();
        let content = get_page_content(&mut resolver, &page_dict, None).unwrap();
        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        let spans = interp.interpret(&content).unwrap();
        order_spans(spans)
    }

    #[test]
    fn test_corpus_winansi_type1() {
        let lines = extract_lines("winansi_type1.pdf");
        assert!(!lines.is_empty(), "expected at least one line");
        let full_text: String = lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            full_text.contains("Hello"),
            "expected 'Hello' in text: {full_text:?}"
        );
    }

    #[test]
    fn test_corpus_flate_content() {
        let lines = extract_lines("flate_content.pdf");
        assert!(!lines.is_empty());
        let full_text: String = lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            full_text.contains("Hello World"),
            "expected 'Hello World': {full_text:?}"
        );
    }

    #[test]
    fn test_corpus_xelatex_reading_order() {
        let lines = extract_lines("xelatex.pdf");
        assert!(
            lines.len() > 5,
            "expected multiple lines from xelatex.pdf, got {}",
            lines.len()
        );

        // First line should contain the title
        let first_line = lines[0].text();
        assert!(
            first_line.contains("Problem"),
            "expected 'Problem' in first line: {first_line:?}"
        );

        // With stream-sequential ordering, line order follows the content
        // stream rather than strict geometric Y order. Large Y inversions
        // (e.g., footnotes, references) are expected. Verify that the
        // output is mostly top-to-bottom with at most a few inversions.
        let inversions = (1..lines.len())
            .filter(|&i| lines[i - 1].baseline < lines[i].baseline - 5.0)
            .count();
        assert!(
            inversions <= 3,
            "too many Y inversions ({inversions}): stream-sequential order \
             should be mostly top-to-bottom"
        );

        // Spot-check: should contain recognizable words
        let full_text: String = lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            full_text.contains("processor"),
            "expected 'processor' in text"
        );
    }

    #[test]
    fn test_corpus_multipage() {
        let lines = extract_lines("multipage.pdf");
        assert!(!lines.is_empty());
        let full_text: String = lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            full_text.contains("Page 1"),
            "expected 'Page 1': {full_text:?}"
        );
    }

    #[test]
    fn test_corpus_macroman_type1() {
        let lines = extract_lines("macroman_type1.pdf");
        assert!(!lines.is_empty());
        let full_text: String = lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !full_text.contains('\u{FFFD}'),
            "should not contain replacement chars: {full_text:?}"
        );
    }

    #[test]
    fn test_corpus_form_xobject() {
        let lines = extract_lines("form_xobject.pdf");
        let full_text: String = lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n");
        // Should contain text from page content stream
        assert!(
            full_text.contains("Page text"),
            "expected 'Page text' from page content: {full_text:?}"
        );
        // Should contain text from Form XObject (via Do operator)
        assert!(
            full_text.contains("XObject"),
            "expected text from Form XObject: {full_text:?}"
        );
    }

    // -- Multi-column detection tests (O-001, O-002) --

    /// Build a two-column layout: left column at x~72, right column at x~320,
    /// with a large gap (~180pt) between them across multiple lines.
    /// Uses realistic flowing text (5+ words per line) to distinguish from tables.
    fn two_column_spans() -> Vec<TextSpan> {
        // Simulates a disordered content stream where left and right column
        // spans are NOT in a coherent Y-descending order. This triggers Tier 2
        // (X-Y cut) column detection. Spans are emitted in shuffled order:
        // right column bottom-to-top, then left column bottom-to-top.
        // This gives low coherence because Y increases within each group.
        let font_size = 10.0;
        let char_w = 4.5;
        let mut spans = Vec::new();

        let left_texts = [
            "The quick brown fox jumps over the lazy dog today",
            "Meanwhile the rain continued falling on the fields",
            "Several researchers proposed new approaches to solve",
            "In the following section we describe the methods used",
            "The experimental results clearly show an improvement",
            "Furthermore the analysis reveals several key trends",
        ];
        let right_texts = [
            "On the other hand some critics argued against this",
            "Nevertheless the evidence supports the original claim",
            "Additional experiments were conducted to verify results",
            "The data collected from multiple sources confirms the",
            "These findings have significant implications for future",
            "In conclusion we have demonstrated a novel technique",
        ];

        // Emit right column bottom-to-top (reverse Y order = low coherence)
        for i in (0..6).rev() {
            let y = 700.0 - (i as f64 * 14.0);
            let right_text = right_texts[i];
            let right_w = right_text.len() as f64 * char_w;
            spans.push(TextSpan {
                text: right_text.to_string(),
                x: 340.0,
                y,
                width: right_w,
                font_name: Arc::from("Helvetica"),
                font_size,
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
            });
        }

        // Emit left column bottom-to-top (reverse Y order = low coherence)
        for i in (0..6).rev() {
            let y = 700.0 - (i as f64 * 14.0);
            let left_text = left_texts[i];
            let left_w = left_text.len() as f64 * char_w;
            spans.push(TextSpan {
                text: left_text.to_string(),
                x: 72.0,
                y,
                width: left_w,
                font_name: Arc::from("Helvetica"),
                font_size,
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
            });
        }

        spans
    }

    #[test]
    fn test_two_column_reading_order() {
        let spans = two_column_spans();
        let lines = order_spans(spans);

        let texts: Vec<String> = lines.iter().map(|l| l.text()).collect();

        let left_start = texts.iter().position(|t| t.contains("quick brown fox"));
        let left_end = texts.iter().position(|t| t.contains("key trends"));
        let right_start = texts.iter().position(|t| t.contains("critics argued"));
        let right_end = texts.iter().position(|t| t.contains("novel technique"));

        assert!(
            left_start.is_some() && left_end.is_some(),
            "missing left column lines in output: {texts:?}"
        );
        assert!(
            right_start.is_some() && right_end.is_some(),
            "missing right column lines in output: {texts:?}"
        );

        // Left column lines should all precede right column lines
        assert!(
            left_end.unwrap() < right_start.unwrap(),
            "left column should finish before right column starts: {texts:?}"
        );

        // Within left column, lines should be in order (top to bottom)
        assert!(
            left_start.unwrap() < left_end.unwrap(),
            "left column lines should be top-to-bottom: {texts:?}"
        );

        // Within right column, lines should be in order (top to bottom)
        assert!(
            right_start.unwrap() < right_end.unwrap(),
            "right column lines should be top-to-bottom: {texts:?}"
        );
    }

    #[test]
    fn test_single_column_order_unchanged() {
        let spans = vec![
            span("Line1", 72.0, 700.0, 30.0, 12.0),
            span("Line2", 72.0, 680.0, 30.0, 12.0),
            span("Line3", 72.0, 660.0, 30.0, 12.0),
        ];
        let lines = order_spans(spans);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text(), "Line1");
        assert_eq!(lines[1].text(), "Line2");
        assert_eq!(lines[2].text(), "Line3");
    }

    #[test]
    fn test_full_width_header_with_two_columns() {
        // Simulates a disordered content stream with a full-width title
        // and two columns. Spans emitted bottom-to-top within each column
        // to produce low coherence, triggering X-Y cut (Tier 2).
        let font_size = 10.0;
        let char_w = 4.5;
        let mut spans = Vec::new();

        let left_body = [
            "The left column begins with a paragraph of flowing text",
            "Continuing the discussion about the experimental methodology",
            "Several key observations were made during the initial trials",
            "The participants reported consistent improvements in accuracy",
            "In conclusion the left column analysis supports our thesis",
        ];
        let right_body = [
            "Meanwhile the right column presents alternative viewpoints",
            "Critics have pointed out several limitations in the approach",
            "Additional data from external sources corroborates findings",
            "The statistical analysis reveals a significant positive trend",
            "Overall the right column provides complementary evidence here",
        ];

        // Emit right column bottom-to-top (low coherence)
        for i in (0..5).rev() {
            let y = 720.0 - (i as f64 * 14.0);
            let right_text = right_body[i];
            spans.push(TextSpan {
                text: right_text.to_string(),
                x: 420.0,
                y,
                width: right_text.len() as f64 * char_w,
                font_name: Arc::from("Helvetica"),
                font_size,
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
            });
        }

        // Emit left column bottom-to-top (low coherence)
        for i in (0..5).rev() {
            let y = 720.0 - (i as f64 * 14.0);
            let left_text = left_body[i];
            spans.push(TextSpan {
                text: left_text.to_string(),
                x: 72.0,
                y,
                width: left_text.len() as f64 * char_w,
                font_name: Arc::from("Helvetica"),
                font_size,
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
            });
        }

        // Full-width title at top (emitted last to further disorder stream)
        let title = "This Is A Full Width Title Spanning Both Columns";
        spans.push(TextSpan {
            text: title.to_string(),
            x: 72.0,
            y: 750.0,
            width: title.len() as f64 * char_w,
            font_name: Arc::from("Helvetica"),
            font_size: 14.0,
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
        });

        let lines = order_spans(spans);
        let texts: Vec<String> = lines.iter().map(|l| l.text()).collect();

        // Title should be the very first line (highest Y, left column)
        assert!(
            texts[0].contains("Full Width Title"),
            "title should be first line, got: {:?}",
            texts[0]
        );

        // Left body lines should come before right body lines
        let left_last = texts
            .iter()
            .rposition(|t| t.contains("left column analysis"));
        let right_first = texts
            .iter()
            .position(|t| t.contains("right column presents"));
        assert!(
            left_last.unwrap() < right_first.unwrap(),
            "left column should finish before right column: {texts:?}"
        );
    }

    // -- Vertical CJK writing mode tests (O-005) --

    #[test]
    fn test_vertical_single_column() {
        // Single vertical column: three characters top to bottom at same X.
        let spans = vec![
            span_vertical("A", 300.0, 700.0, 12.0),
            span_vertical("B", 300.0, 680.0, 12.0),
            span_vertical("C", 300.0, 660.0, 12.0),
        ];
        let lines = order_vertical_spans(spans);
        assert_eq!(lines.len(), 1, "expected 1 column, got {}", lines.len());
        // Spans should be top-to-bottom (A, B, C)
        assert_eq!(lines[0].text(), "ABC");
    }

    #[test]
    fn test_vertical_two_columns_right_to_left() {
        // Two vertical columns. Right column (x=300) should come first.
        let spans = vec![
            span_vertical("L1", 200.0, 700.0, 12.0),
            span_vertical("L2", 200.0, 680.0, 12.0),
            span_vertical("R1", 300.0, 700.0, 12.0),
            span_vertical("R2", 300.0, 680.0, 12.0),
        ];
        let lines = order_vertical_spans(spans);
        assert_eq!(lines.len(), 2, "expected 2 columns, got {}", lines.len());
        // Right column first (higher X)
        assert_eq!(lines[0].text(), "R1R2");
        assert_eq!(lines[1].text(), "L1L2");
    }

    #[test]
    fn test_vertical_spans_separated_from_horizontal() {
        // Mix of horizontal and vertical spans. Vertical should appear after horizontal.
        let spans = vec![
            span("Horizontal text", 72.0, 700.0, 80.0, 12.0),
            span_vertical("V1", 400.0, 700.0, 12.0),
            span_vertical("V2", 400.0, 680.0, 12.0),
        ];
        let lines = order_spans(spans);
        assert_eq!(lines.len(), 2, "expected 2 lines, got {}", lines.len());
        assert_eq!(lines[0].text(), "Horizontal text");
        assert_eq!(lines[1].text(), "V1V2");
    }

    #[test]
    fn test_vertical_empty_input() {
        let lines = order_vertical_spans(vec![]);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_vertical_three_columns() {
        // Three columns at x=400, x=300, x=200. Should order 400, 300, 200.
        let spans = vec![
            span_vertical("A1", 200.0, 700.0, 12.0),
            span_vertical("A2", 200.0, 680.0, 12.0),
            span_vertical("B1", 300.0, 700.0, 12.0),
            span_vertical("B2", 300.0, 680.0, 12.0),
            span_vertical("C1", 400.0, 700.0, 12.0),
            span_vertical("C2", 400.0, 680.0, 12.0),
        ];
        let lines = order_vertical_spans(spans);
        assert_eq!(lines.len(), 3);
        // Rightmost column first
        assert_eq!(lines[0].text(), "C1C2");
        assert_eq!(lines[1].text(), "B1B2");
        assert_eq!(lines[2].text(), "A1A2");
    }

    #[test]
    fn test_vertical_column_x_tolerance() {
        // Spans at slightly different X should still group into one column
        // (within VERTICAL_COLUMN_TOLERANCE = 5pt)
        let spans = vec![
            span_vertical("A", 300.0, 700.0, 12.0),
            span_vertical("B", 302.0, 680.0, 12.0), // 2pt offset
            span_vertical("C", 299.0, 660.0, 12.0), // 1pt offset
        ];
        let lines = order_vertical_spans(spans);
        assert_eq!(
            lines.len(),
            1,
            "spans within tolerance should form one column"
        );
        assert_eq!(lines[0].text(), "ABC");
    }

    fn span_rotated(text: &str, x: f64, y: f64, width: f64, rotation: f64) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            width,
            font_name: Arc::from("Helvetica"),
            font_size: 12.0,
            rotation,
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
    fn test_rotated_spans_grouped_by_angle() {
        // Two spans at 90 degrees on the same baseline, one at -90 degrees.
        // Produces two groups sorted by angle (-90 first, then 90).
        // Each group goes through baseline clustering, so same-baseline
        // spans merge into one line.
        let spans = vec![
            span_rotated("Up1", 100.0, 300.0, 12.0, 90.0),
            span_rotated("Up2", 150.0, 300.0, 12.0, 90.0), // same baseline as Up1
            span_rotated("Down1", 200.0, 300.0, 12.0, -90.0),
        ];
        let lines = order_rotated_spans(spans);
        assert!(
            !lines.is_empty(),
            "rotated spans should produce at least one line"
        );
        // -90 group first, then 90 group
        let all_text: Vec<_> = lines.iter().map(|l| l.text()).collect();
        assert_eq!(all_text.len(), 2);
        assert_eq!(all_text[0], "Down1");
        assert!(
            all_text[1].contains("Up1") && all_text[1].contains("Up2"),
            "second group should contain both 90-degree spans, got: {}",
            all_text[1]
        );
    }

    #[test]
    fn test_rotated_spans_empty() {
        let lines = order_rotated_spans(vec![]);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_mixed_horizontal_rotated_vertical() {
        // All three categories in one order_spans call.
        let spans = vec![
            span("Horizontal", 72.0, 700.0, 60.0, 12.0),
            span_rotated("Rotated", 72.0, 500.0, 30.0, 45.0),
            span_vertical("Vertical", 400.0, 700.0, 12.0),
        ];
        let lines = order_spans(spans);
        assert_eq!(lines.len(), 3);
        // Order: horizontal, vertical, rotated
        assert_eq!(lines[0].text(), "Horizontal");
        assert_eq!(lines[1].text(), "Vertical");
        assert_eq!(lines[2].text(), "Rotated");
    }

    #[test]
    fn test_vertical_line_has_is_vertical_flag() {
        let spans = vec![
            span("Horizontal", 72.0, 700.0, 60.0, 12.0),
            span_vertical("Vertical", 400.0, 700.0, 12.0),
        ];
        let lines = order_spans(spans);
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].is_vertical);
        assert!(lines[1].is_vertical);
    }

    #[test]
    fn test_nan_infinity_spans_no_panic() {
        // Verify that NaN and infinity values in span fields do not cause panics.
        // total_cmp handles NaN deterministically; we just need no crashes.
        let spans = vec![
            span("Normal", 72.0, 700.0, 50.0, 12.0),
            TextSpan {
                text: "NaN-x".to_string(),
                x: f64::NAN,
                y: 680.0,
                width: 50.0,
                font_name: Arc::from("Helvetica"),
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
            },
            TextSpan {
                text: "Inf-y".to_string(),
                x: 72.0,
                y: f64::INFINITY,
                width: 50.0,
                font_name: Arc::from("Helvetica"),
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
            },
            TextSpan {
                text: "NaN-width".to_string(),
                x: 72.0,
                y: 660.0,
                width: f64::NAN,
                font_name: Arc::from("Helvetica"),
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
            },
            TextSpan {
                text: "NegInf".to_string(),
                x: f64::NEG_INFINITY,
                y: 640.0,
                width: 50.0,
                font_name: Arc::from("Helvetica"),
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
            },
        ];
        // Should not panic
        let lines = order_spans(spans);
        // All spans should appear somewhere in the output
        let all_text: String = lines.iter().map(|l| l.text()).collect::<Vec<_>>().join(" ");
        assert!(all_text.contains("Normal"), "Normal span should appear");
        assert!(all_text.contains("NaN-x"), "NaN-x span should appear");
        assert!(all_text.contains("Inf-y"), "Inf-y span should appear");
        assert!(
            all_text.contains("NaN-width"),
            "NaN-width span should appear"
        );
        assert!(all_text.contains("NegInf"), "NegInf span should appear");
    }

    #[test]
    fn test_vertical_spans_through_order_spans() {
        // Verify vertical CJK spans go through the full order_spans pipeline
        // and produce lines with is_vertical=true, columns ordered R-to-L.
        let spans = vec![
            // Right column (x=400)
            span_vertical("R1", 400.0, 700.0, 12.0),
            span_vertical("R2", 400.0, 680.0, 12.0),
            // Left column (x=200)
            span_vertical("L1", 200.0, 700.0, 12.0),
            span_vertical("L2", 200.0, 680.0, 12.0),
        ];
        let lines = order_spans(spans);
        // All vertical lines
        let vertical_lines: Vec<_> = lines.iter().filter(|l| l.is_vertical).collect();
        assert_eq!(
            vertical_lines.len(),
            2,
            "expected 2 vertical columns, got {}",
            vertical_lines.len()
        );
        // Right column first (higher X)
        assert_eq!(vertical_lines[0].text(), "R1R2");
        assert_eq!(vertical_lines[1].text(), "L1L2");
        // Baseline should be column X
        assert!(
            vertical_lines[0].baseline > vertical_lines[1].baseline,
            "right column baseline ({}) should be greater than left ({})",
            vertical_lines[0].baseline,
            vertical_lines[1].baseline
        );
    }

    // ---------------------------------------------------------------
    // Structure ordering tests
    // ---------------------------------------------------------------

    #[test]
    fn structure_order_does_not_override_geometric_line_sort() {
        // Two spans at different Y positions. Structure order says "bottom"
        // (MCID 0) first, but we always use geometric Y-sort for line ordering
        // since MCID sequences often follow content stream order rather than
        // visual reading order.
        use crate::content::marked_content::PageStructureOrder;

        let spans = vec![
            TextSpan {
                text: "top".to_string(),
                x: 100.0,
                y: 700.0,
                width: 50.0,
                font_name: Arc::from("Test"),
                font_size: 12.0,
                rotation: 0.0,
                is_vertical: false,
                mcid: Some(1),
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
                text: "bottom".to_string(),
                x: 100.0,
                y: 500.0,
                width: 80.0,
                font_name: Arc::from("Test"),
                font_size: 12.0,
                rotation: 0.0,
                is_vertical: false,
                mcid: Some(0),
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

        // Structure order: MCID 0 before MCID 1
        let structure_order = PageStructureOrder {
            mcid_order: vec![0, 1],
        };

        // Both with and without structure: geometric order (top first)
        let lines_geo = order_spans(spans.clone());
        assert_eq!(lines_geo[0].text(), "top");
        assert_eq!(lines_geo[1].text(), "bottom");

        let lines_struct = order_spans_with_structure(spans, Some(&structure_order));
        assert_eq!(lines_struct[0].text(), "top");
        assert_eq!(lines_struct[1].text(), "bottom");
    }

    #[test]
    fn structure_order_uses_geometric_for_line_ordering() {
        // All lines are now sorted by geometric Y position regardless of
        // MCID presence. Structure tree ordering is used only for span
        // reordering upstream, not for line sorting.
        use crate::content::marked_content::PageStructureOrder;

        let spans = vec![
            TextSpan {
                text: "no-mcid-top".to_string(),
                x: 100.0,
                y: 800.0,
                width: 80.0,
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
            },
            TextSpan {
                text: "structured".to_string(),
                x: 100.0,
                y: 500.0,
                width: 80.0,
                font_name: Arc::from("Test"),
                font_size: 12.0,
                rotation: 0.0,
                is_vertical: false,
                mcid: Some(0),
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
                text: "no-mcid-bottom".to_string(),
                x: 100.0,
                y: 300.0,
                width: 80.0,
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
            },
        ];

        let structure_order = PageStructureOrder {
            mcid_order: vec![0],
        };

        let lines = order_spans_with_structure(spans, Some(&structure_order));
        // All lines in geometric order (descending Y)
        assert_eq!(lines[0].text(), "no-mcid-top");
        assert_eq!(lines[1].text(), "structured");
        assert_eq!(lines[2].text(), "no-mcid-bottom");
    }

    // -- Three-tier spacing algorithm tests (ACC-033) --

    /// Helper: create a span with explicit space_width for Tier 1 testing.
    fn span_with_sw(
        text: &str,
        x: f64,
        width: f64,
        font_size: f64,
        space_width: Option<f64>,
    ) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y: 700.0,
            width,
            font_name: Arc::from("Helvetica"),
            font_size,
            rotation: 0.0,
            is_vertical: false,
            mcid: None,
            space_width,
            has_font_metrics: space_width.is_some(),
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
    fn test_tier1_space_width_detects_word_boundary() {
        // Helvetica at 12pt: space glyph = 278/1000 * 12 = 3.336
        // "Hello" + gap of 3.5pt (> 3.336*0.5=1.668) -> word break
        let mut spans = vec![
            span_with_sw("Hello", 100.0, 30.0, 12.0, Some(3.336)),
            span_with_sw("World", 133.5, 30.0, 12.0, Some(3.336)),
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, " World");
    }

    #[test]
    fn test_tier1_small_gap_no_space() {
        // Gap of 0.5pt (< 3.336*0.5=1.668) -> no space (kerning)
        let mut spans = vec![
            span_with_sw("He", 100.0, 12.0, 12.0, Some(3.336)),
            span_with_sw("llo", 112.5, 18.0, 12.0, Some(3.336)),
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, "llo");
    }

    #[test]
    fn test_tier2_clear_word_break() {
        // No space_width (Tier 2). Gap/font_size = 2.0/12.0 = 0.167 > 0.15 -> word break
        let mut spans = vec![
            span("Hello", 100.0, 700.0, 30.0, 12.0),
            span("World", 132.0, 700.0, 30.0, 12.0),
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, " World");
    }

    #[test]
    fn test_tier2_clear_same_word() {
        // No space_width. Gap/font_size = 0.2/12.0 = 0.017 < 0.03 -> same word
        let mut spans = vec![
            span("He", 100.0, 700.0, 12.0, 12.0),
            span("llo", 112.2, 700.0, 18.0, 12.0),
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, "llo");
    }

    #[test]
    fn test_tier2_xelatex_tfs_1_uses_char_width_fallback() {
        // XeLaTeX: font_size=1.0 but glyphs are ~6pt wide (font metrics scaled up).
        // Gap=0.28, avg_cw≈6, effective_size=max(1.0, 6.0)=6.0
        // normalized=0.28/6.0=0.047 -> gray zone -> adaptive threshold
        // avg_thousandths=6/6*1000=1000 -> divisor=6 -> threshold=6/6=1.0
        // 0.28 < 1.0 -> no space
        let mut spans = vec![
            span("pro", 100.0, 700.0, 18.0, 1.0),
            span("cessor", 118.28, 700.0, 36.0, 1.0),
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, "cessor");
    }

    #[test]
    fn test_tier2_xelatex_real_word_gap() {
        // XeLaTeX: font_size=1.0, avg_cw≈6, effective_size=6.0
        // Gap=3.3, normalized=3.3/6.0=0.55 > 0.15 -> word break
        let mut spans = vec![
            span("hello", 100.0, 700.0, 30.0, 1.0),
            span("world", 133.3, 700.0, 30.0, 1.0),
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, " world");
    }

    #[test]
    fn test_negative_gap_no_space() {
        // Overlapping spans -> no space
        let mut spans = vec![
            span("Hel", 100.0, 700.0, 20.0, 12.0),
            span("lo", 118.0, 700.0, 12.0, 12.0),
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, "lo");
    }

    #[test]
    fn test_tier3_cjk_no_space() {
        // Two CJK characters with a gap that would normally trigger a space,
        // but Tier 3 suppresses it because both sides are CJK.
        let mut spans = vec![
            span_with_sw("\u{4E16}", 100.0, 12.0, 12.0, Some(3.336)), // CJK "shi"
            span_with_sw("\u{754C}", 114.0, 12.0, 12.0, Some(3.336)), // CJK "jie"
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, "\u{754C}"); // no space inserted
    }

    #[test]
    fn test_tier3_cjk_latin_gets_space() {
        // CJK followed by Latin with a gap -> space IS inserted
        // (at least one side is Latin, which uses word spacing)
        let mut spans = vec![
            span_with_sw("\u{4E16}", 100.0, 12.0, 12.0, Some(3.336)),
            span_with_sw("World", 114.0, 30.0, 12.0, Some(3.336)),
        ];
        insert_word_spaces(&mut spans);
        assert_eq!(spans[1].text, " World");
    }

    #[test]
    fn test_is_non_spacing_script() {
        // CJK Unified Ideographs
        assert!(is_non_spacing_script('\u{4E00}')); // first CJK unified
        assert!(is_non_spacing_script('\u{9FFF}')); // last CJK unified
                                                    // Hiragana
        assert!(is_non_spacing_script('\u{3042}')); // a
                                                    // Katakana
        assert!(is_non_spacing_script('\u{30A2}')); // a
                                                    // Hangul
        assert!(is_non_spacing_script('\u{AC00}')); // first hangul syllable
                                                    // Latin -- should NOT be non-spacing
        assert!(!is_non_spacing_script('A'));
        assert!(!is_non_spacing_script('z'));
        // Numbers
        assert!(!is_non_spacing_script('0'));
        // Fullwidth forms
        assert!(is_non_spacing_script('\u{FF01}')); // fullwidth !
    }

    #[test]
    fn test_merge_respects_word_boundaries() {
        // Two same-font spans with a word-boundary gap should NOT merge
        let spans = vec![
            span_with_sw("Hello", 100.0, 30.0, 12.0, Some(3.336)),
            span_with_sw(" World", 133.5, 30.0, 12.0, Some(3.336)),
        ];
        let merged = merge_adjacent_spans(spans);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_joins_kerned_fragments() {
        // Two same-font spans with a tiny gap (kerning) -> merge
        let spans = vec![
            span("He", 100.0, 700.0, 12.0, 12.0),
            span("llo", 112.2, 700.0, 18.0, 12.0),
        ];
        let merged = merge_adjacent_spans(spans);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Hello");
    }

    // -- X-Y cut algorithm unit tests --

    #[test]
    fn test_tier_selection_diagnostic_tier1() {
        // Single-column coherent spans should trigger Tier 1, confirmed by
        // X-Y cut probe finding only 1 partition.
        use crate::diagnostics::CollectingDiagnostics;
        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 3,
        };
        let spans = vec![
            span("Line 1", 72.0, 700.0, 100.0, 12.0),
            span("Ref", 400.0, 700.0, 30.0, 12.0), // widens bbox
            span("Line 2", 72.0, 680.0, 110.0, 12.0),
            span("Line 3", 72.0, 660.0, 105.0, 12.0),
            span("Line 4", 72.0, 640.0, 100.0, 12.0),
            span("Line 5", 72.0, 620.0, 108.0, 12.0),
        ];
        let _lines = order_spans_with_diagnostics(spans, None, Some(&diag));
        let warnings = diag_sink.warnings();
        let tier_msgs: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind == crate::diagnostics::WarningKind::TierSelection)
            .collect();
        assert_eq!(tier_msgs.len(), 1, "expected one TierSelection diagnostic");
        assert!(
            tier_msgs[0].message.contains("tier=1"),
            "expected tier=1, got: {}",
            tier_msgs[0].message
        );
        assert_eq!(
            tier_msgs[0].context.page_index,
            Some(3),
            "page_index should be 3"
        );
        assert_eq!(
            tier_msgs[0].level,
            crate::diagnostics::WarningLevel::Info,
            "tier diagnostic should be Info level"
        );
    }

    #[test]
    fn test_tier_selection_diagnostic_tier2() {
        // Two-column spans with scrambled stream order: spans bounce between
        // left and right columns, creating low coherence that triggers Tier 2.
        use crate::diagnostics::CollectingDiagnostics;
        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 0,
        };
        // Scrambled order: L0 at top, R3 at middle, L5 at bottom, R1 at top.
        // This creates Y-jumps in both directions, killing coherence.
        let scrambled_spans = vec![
            span(
                "L0 text here in left column first line",
                72.0,
                700.0,
                150.0,
                12.0,
            ),
            span(
                "R3 text here in right column fourth line",
                350.0,
                640.0,
                150.0,
                12.0,
            ),
            span(
                "L5 text here in left column sixth line",
                72.0,
                600.0,
                150.0,
                12.0,
            ),
            span(
                "R1 text here in right column second line",
                350.0,
                680.0,
                150.0,
                12.0,
            ),
            span(
                "L2 text here in left column third line",
                72.0,
                660.0,
                150.0,
                12.0,
            ),
            span(
                "R5 text here in right column sixth line",
                350.0,
                600.0,
                150.0,
                12.0,
            ),
            span(
                "L4 text here in left column fifth line",
                72.0,
                620.0,
                150.0,
                12.0,
            ),
            span(
                "R0 text here in right column first line",
                350.0,
                700.0,
                150.0,
                12.0,
            ),
            span(
                "L1 text here in left column second line",
                72.0,
                680.0,
                150.0,
                12.0,
            ),
            span(
                "R2 text here in right column third line",
                350.0,
                660.0,
                150.0,
                12.0,
            ),
            span(
                "L3 text here in left column fourth line",
                72.0,
                640.0,
                150.0,
                12.0,
            ),
            span(
                "R4 text here in right column fifth line",
                350.0,
                620.0,
                150.0,
                12.0,
            ),
        ];
        let _lines = order_spans_with_diagnostics(scrambled_spans, None, Some(&diag));
        let warnings = diag_sink.warnings();
        let tier_msgs: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind == crate::diagnostics::WarningKind::TierSelection)
            .collect();
        assert_eq!(tier_msgs.len(), 1, "expected one TierSelection diagnostic");
        // Scrambled stream order has low coherence, triggering Tier 2
        // (X-Y cut with multi-algorithm cascade).
        let msg = &tier_msgs[0].message;
        assert!(
            msg.contains("coherence="),
            "should contain coherence: {msg}"
        );
        assert!(
            msg.contains("partitions="),
            "should contain partitions: {msg}"
        );
        assert!(
            msg.contains("tier=2"),
            "scrambled layout should use Tier 2 (X-Y cut), got: {msg}"
        );
    }

    #[test]
    fn test_no_diagnostic_without_sink() {
        // order_spans (no diagnostics) should not panic or emit anything.
        let spans = vec![
            span("Hello", 72.0, 700.0, 40.0, 12.0),
            span("World", 72.0, 680.0, 40.0, 12.0),
        ];
        let lines = order_spans(spans);
        assert_eq!(lines.len(), 2);
    }

    // -- Lowered gap threshold tests --

    // -- Stacked gap detection tests --

    // -- Horizontal cuts at depth >= 1 tests --

    // -- Tier 0 diagnostic tests --

    #[test]
    fn test_tier0_diagnostic_with_mcids() {
        use crate::content::marked_content::PageStructureOrder;
        use crate::diagnostics::CollectingDiagnostics;

        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 5,
        };

        // Spans with MCIDs matching a structure order
        let spans = vec![
            {
                let mut s = span("First", 72.0, 700.0, 40.0, 12.0);
                s.mcid = Some(2);
                s
            },
            {
                let mut s = span("Second", 72.0, 680.0, 50.0, 12.0);
                s.mcid = Some(0);
                s
            },
            {
                let mut s = span("Third", 72.0, 660.0, 45.0, 12.0);
                s.mcid = Some(1);
                s
            },
        ];

        let structure = PageStructureOrder {
            mcid_order: vec![0, 1, 2],
        };

        let _lines =
            order_spans_with_structure_and_diagnostics(spans, Some(&structure), Some(&diag));

        let warnings = diag_sink.warnings();
        let tier0_msgs: Vec<_> = warnings
            .iter()
            .filter(|w| {
                w.kind == crate::diagnostics::WarningKind::TierSelection
                    && w.message.contains("Tier 0")
            })
            .collect();

        assert_eq!(tier0_msgs.len(), 1, "expected one Tier 0 diagnostic");
        assert!(
            tier0_msgs[0].message.contains("3 spans reordered"),
            "should report 3 structured spans, got: {}",
            tier0_msgs[0].message
        );
        assert_eq!(
            tier0_msgs[0].level,
            crate::diagnostics::WarningLevel::Info,
            "Tier 0 diagnostic should be Info level"
        );
        assert_eq!(
            tier0_msgs[0].context.page_index,
            Some(5),
            "page_index should be 5"
        );
    }

    #[test]
    fn test_tier0_diagnostic_no_mcids() {
        use crate::content::marked_content::PageStructureOrder;
        use crate::diagnostics::CollectingDiagnostics;

        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 0,
        };

        // Spans with no MCIDs
        let spans = vec![
            span("First", 72.0, 700.0, 40.0, 12.0),
            span("Second", 72.0, 680.0, 50.0, 12.0),
        ];

        let structure = PageStructureOrder {
            mcid_order: vec![0, 1, 2],
        };

        let _lines =
            order_spans_with_structure_and_diagnostics(spans, Some(&structure), Some(&diag));

        let warnings = diag_sink.warnings();
        let tier0_msgs: Vec<_> = warnings
            .iter()
            .filter(|w| {
                w.kind == crate::diagnostics::WarningKind::TierSelection
                    && w.message.contains("Tier 0")
            })
            .collect();

        // No Tier 0 diagnostic should be emitted when no spans have MCIDs
        assert_eq!(
            tier0_msgs.len(),
            0,
            "should not emit Tier 0 diagnostic when no spans have MCIDs"
        );
    }

    #[test]
    fn test_tier0_diagnostic_without_structure_order() {
        use crate::diagnostics::CollectingDiagnostics;

        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 0,
        };

        let spans = vec![
            span("First", 72.0, 700.0, 40.0, 12.0),
            span("Second", 72.0, 680.0, 50.0, 12.0),
        ];

        // No structure order
        let _lines = order_spans_with_structure_and_diagnostics(spans, None, Some(&diag));

        let warnings = diag_sink.warnings();
        let tier0_msgs: Vec<_> = warnings
            .iter()
            .filter(|w| {
                w.kind == crate::diagnostics::WarningKind::TierSelection
                    && w.message.contains("Tier 0")
            })
            .collect();

        assert_eq!(
            tier0_msgs.len(),
            0,
            "should not emit Tier 0 diagnostic without structure order"
        );
    }

    // -- Breuel partition reordering tests --

    // -- CJK visual width estimation tests --

    // -- Stacked gap merge coverage tests --

    // -- Topological sort cycle-breaking tests --

    // -- Partition reorder diagnostics tests --

    // -- Ambiguous coherence diagnostic test --

    #[test]
    fn test_ambiguous_coherence_diagnostic() {
        // Create spans with coherence near STREAM_COHERENCE_THRESHOLD (0.75).
        // The ambiguous range is [0.65, 0.85]. We need coherence in this range
        // to trigger the info diagnostic.
        //
        // 9 spans, 8 consecutive pairs. 6 coherent (Y decreasing), 2 incoherent.
        // = 6/8 = 0.75. Exactly at threshold boundary.
        use crate::diagnostics::CollectingDiagnostics;
        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 0,
        };

        let spans = vec![
            span(
                "One word here in the flowing text line",
                72.0,
                700.0,
                250.0,
                12.0,
            ),
            span(
                "Two word here in the flowing text line",
                72.0,
                688.0,
                250.0,
                12.0,
            ),
            span(
                "Three word here in flowing text line",
                72.0,
                676.0,
                250.0,
                12.0,
            ),
            span(
                "Four word here in flowing text line",
                72.0,
                690.0,
                250.0,
                12.0,
            ),
            span(
                "Five word here in the flowing text line",
                72.0,
                680.0,
                250.0,
                12.0,
            ),
            span(
                "Six word here in the flowing text line",
                72.0,
                668.0,
                250.0,
                12.0,
            ),
            span(
                "Seven word here in flowing text line",
                72.0,
                656.0,
                250.0,
                12.0,
            ),
            span(
                "Eight word here in flowing text line",
                72.0,
                670.0,
                250.0,
                12.0,
            ),
            span(
                "Nine word here in the flowing text line",
                72.0,
                644.0,
                250.0,
                12.0,
            ),
        ];

        let _lines = order_spans_with_diagnostics(spans, None, Some(&diag));

        let warnings = diag_sink.warnings();
        let ambiguous_msgs: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind == WarningKind::ReadingOrder && w.message.contains("ambiguous"))
            .collect();
        assert_eq!(
            ambiguous_msgs.len(),
            1,
            "should emit exactly one ambiguous coherence diagnostic, got {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
        assert_eq!(
            ambiguous_msgs[0].level,
            WarningLevel::Info,
            "ambiguous coherence diagnostic should be Info level"
        );
    }

    // -- Whitespace grid resource limit diagnostic tests --
}
