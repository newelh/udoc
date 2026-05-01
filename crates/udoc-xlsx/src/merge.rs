//! Merged cell region parser for XLSX.
//!
//! Parses `<mergeCells>` sections from worksheet XML to extract merge
//! regions. Each region is a range like "A1:C3" specifying the top-left
//! and bottom-right cells of the merged area.

use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::cell_ref::parse_cell_ref;

/// Maximum number of merge regions we'll process (safety limit).
const MAX_MERGE_REGIONS: usize = 10_000;

/// Overlap detection threshold. For region counts above this, the O(n^2) overlap
/// scan is skipped (it's purely diagnostic). 1K^2 = 500K comparisons is fine;
/// 10K^2 = 50M is too slow for debug builds.
const OVERLAP_SCAN_LIMIT: usize = 1_000;

/// A rectangular merge region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeRegion {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

impl MergeRegion {
    /// Number of rows this merge spans.
    pub fn row_span(&self) -> usize {
        self.end_row - self.start_row + 1
    }

    /// Number of columns this merge spans.
    pub fn col_span(&self) -> usize {
        self.end_col - self.start_col + 1
    }

    /// Whether this is a single-cell "merge" (no-op).
    pub fn is_single_cell(&self) -> bool {
        self.start_row == self.end_row && self.start_col == self.end_col
    }

    /// Whether a (row, col) position is covered by this merge but is NOT
    /// the top-left anchor cell.
    #[cfg(test)]
    pub fn covers(&self, row: usize, col: usize) -> bool {
        row >= self.start_row
            && row <= self.end_row
            && col >= self.start_col
            && col <= self.end_col
            && !(row == self.start_row && col == self.start_col)
    }

    /// Whether this region overlaps with another.
    #[cfg(test)]
    pub fn overlaps(&self, other: &MergeRegion) -> bool {
        self.start_row <= other.end_row
            && self.end_row >= other.start_row
            && self.start_col <= other.end_col
            && self.end_col >= other.start_col
    }
}

/// Parse merge cell references from collected `mergeCell/@ref` strings.
pub(crate) fn parse_merge_cells(
    refs: &[String],
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<MergeRegion> {
    let mut regions = Vec::new();

    for ref_str in refs {
        if regions.len() >= MAX_MERGE_REGIONS {
            diag.warning(Warning::new(
                "XlsxMergeLimit",
                format!("more than {MAX_MERGE_REGIONS} merge regions, truncating"),
            ));
            break;
        }
        match parse_merge_ref(ref_str) {
            Ok(region) => {
                if region.is_single_cell() {
                    // Single-cell merges are no-ops, skip silently.
                    continue;
                }
                // Check for overlap with existing regions (diagnostic only).
                // Skip when region count is large to avoid O(n^2) blowup.
                if regions.len() < OVERLAP_SCAN_LIMIT {
                    let overlapping = regions.iter().any(|r: &MergeRegion| {
                        r.start_row <= region.end_row
                            && r.end_row >= region.start_row
                            && r.start_col <= region.end_col
                            && r.end_col >= region.start_col
                    });
                    if overlapping {
                        diag.warning(Warning::new(
                            "XlsxOverlappingMerge",
                            format!("overlapping merge region: {ref_str}"),
                        ));
                    }
                }
                regions.push(region);
            }
            Err(_) => {
                diag.warning(Warning::new(
                    "XlsxInvalidMergeRef",
                    format!("invalid merge cell reference: {ref_str}"),
                ));
            }
        }
    }

    regions
}

/// Parse a merge reference like "A1:C3" into a MergeRegion.
fn parse_merge_ref(ref_str: &str) -> Result<MergeRegion, ()> {
    let parts: Vec<&str> = ref_str.split(':').collect();
    if parts.len() != 2 {
        return Err(());
    }

    let (start_row, start_col) = parse_cell_ref(parts[0]).map_err(|_| ())?;
    let (end_row, end_col) = parse_cell_ref(parts[1]).map_err(|_| ())?;

    // Normalize so start <= end.
    Ok(MergeRegion {
        start_row: start_row.min(end_row),
        start_col: start_col.min(end_col),
        end_row: start_row.max(end_row),
        end_col: start_col.max(end_col),
    })
}

/// Find the merge region anchored at (row, col), if any.
#[cfg(test)]
pub(crate) fn find_merge_at(
    regions: &[MergeRegion],
    row: usize,
    col: usize,
) -> Option<&MergeRegion> {
    regions
        .iter()
        .find(|r| r.start_row == row && r.start_col == col)
}

/// Check if (row, col) is a covered (non-anchor) cell in any merge region.
///
/// For single lookups. For batch lookups (iterating all cells), use
/// [`build_covered_set`] for O(1) per-cell checks instead of O(regions).
#[cfg(test)]
pub(crate) fn is_covered_by_merge(regions: &[MergeRegion], row: usize, col: usize) -> bool {
    regions.iter().any(|r| r.covers(row, col))
}

/// Pre-computed merge lookup tables for O(1) per-cell checks.
///
/// Combines the covered set (for skipping non-anchor cells) and an anchor
/// index (for O(1) merge-span lookups in tables()).
pub(crate) struct MergeCache {
    /// All (row, col) positions covered by merge regions (non-anchor cells).
    pub covered: std::collections::HashSet<(usize, usize)>,
    /// Anchor (row, col) -> index into the regions slice. For O(1)
    /// merge-span lookups in tables() instead of O(regions) linear scan.
    pub anchors: std::collections::HashMap<(usize, usize), usize>,
    /// Rows touched by any merge region (as anchor OR as covered cell).
    /// Used by sparse iteration paths to skip empty rows that no merge
    /// region reaches, so text()/tables() can O(1)-confirm a row is empty
    /// instead of walking `0..=max_col` just to find nothing.
    rows_with_coverage: std::collections::HashSet<usize>,
}

impl MergeCache {
    /// O(1) merge-anchor lookup using the cached anchor index.
    pub fn find_anchor<'a>(
        &self,
        regions: &'a [MergeRegion],
        row: usize,
        col: usize,
    ) -> Option<&'a MergeRegion> {
        self.anchors.get(&(row, col)).map(|&idx| &regions[idx])
    }

    /// True if any merge region touches `row` (as anchor or covered cell).
    /// Sparse-iteration paths use this to short-circuit truly-empty rows.
    pub fn row_has_coverage(&self, row: usize) -> bool {
        self.rows_with_coverage.contains(&row)
    }
}

/// Maximum total covered cells across all merge regions.
/// Prevents pathological inputs from causing massive HashSet allocations.
const MAX_TOTAL_COVERED: usize = 500_000;

/// Build a set of all (row, col) positions covered by merge regions.
///
/// O(total_covered_cells) construction, O(1) per lookup. Use this when
/// iterating all cells in a sheet to avoid O(cells * regions) linear scans.
/// Regions with area > 100,000 cells are skipped (pathological input guard).
fn build_covered_set(regions: &[MergeRegion]) -> std::collections::HashSet<(usize, usize)> {
    let mut covered = std::collections::HashSet::new();
    for r in regions {
        let area = r.row_span().saturating_mul(r.col_span());
        if area > 100_000 {
            continue; // pathological single merge, skip
        }
        if covered.len().saturating_add(area) > MAX_TOTAL_COVERED {
            break; // total covered cells would exceed cap
        }
        for row in r.start_row..=r.end_row {
            for col in r.start_col..=r.end_col {
                if row == r.start_row && col == r.start_col {
                    continue; // anchor cell is not "covered"
                }
                covered.insert((row, col));
            }
        }
    }
    covered
}

/// Build the complete merge cache: covered set + anchor index + row-coverage set.
pub(crate) fn build_merge_cache(regions: &[MergeRegion]) -> MergeCache {
    let covered = build_covered_set(regions);
    let mut anchors = std::collections::HashMap::with_capacity(regions.len());
    let mut rows_with_coverage = std::collections::HashSet::new();
    for (i, r) in regions.iter().enumerate() {
        anchors.insert((r.start_row, r.start_col), i);
        for row in r.start_row..=r.end_row {
            rows_with_coverage.insert(row);
        }
    }
    MergeCache {
        covered,
        anchors,
        rows_with_coverage,
    }
}

#[cfg(test)]
mod tests {
    use udoc_core::diagnostics::NullDiagnostics;

    use super::*;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    #[test]
    fn parse_simple_merge() {
        let refs = vec!["A1:C3".to_string()];
        let regions = parse_merge_cells(&refs, &null_diag());
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].start_row, 0);
        assert_eq!(regions[0].start_col, 0);
        assert_eq!(regions[0].end_row, 2);
        assert_eq!(regions[0].end_col, 2);
        assert_eq!(regions[0].row_span(), 3);
        assert_eq!(regions[0].col_span(), 3);
    }

    #[test]
    fn single_cell_merge_skipped() {
        let refs = vec!["A1:A1".to_string()];
        let regions = parse_merge_cells(&refs, &null_diag());
        assert!(regions.is_empty());
    }

    #[test]
    fn invalid_merge_ref_warned() {
        let refs = vec!["INVALID".to_string()];
        let collecting = Arc::new(udoc_core::diagnostics::CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let regions = parse_merge_cells(&refs, &diag);
        assert!(regions.is_empty());
        let warnings = collecting.warnings();
        assert!(warnings.iter().any(|w| w.kind == "XlsxInvalidMergeRef"));
    }

    #[test]
    fn overlapping_merge_warned() {
        let refs = vec!["A1:B2".to_string(), "B2:C3".to_string()];
        let collecting = Arc::new(udoc_core::diagnostics::CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let regions = parse_merge_cells(&refs, &diag);
        assert_eq!(regions.len(), 2);
        let warnings = collecting.warnings();
        assert!(warnings.iter().any(|w| w.kind == "XlsxOverlappingMerge"));
    }

    #[test]
    fn covers_detects_non_anchor() {
        let region = MergeRegion {
            start_row: 0,
            start_col: 0,
            end_row: 2,
            end_col: 2,
        };
        // Anchor cell is NOT covered
        assert!(!region.covers(0, 0));
        // Interior cells are covered
        assert!(region.covers(0, 1));
        assert!(region.covers(1, 0));
        assert!(region.covers(2, 2));
        // Outside cells are not covered
        assert!(!region.covers(3, 0));
        assert!(!region.covers(0, 3));
    }

    #[test]
    fn find_merge_at_anchor() {
        let regions = vec![MergeRegion {
            start_row: 0,
            start_col: 0,
            end_row: 1,
            end_col: 1,
        }];
        assert!(find_merge_at(&regions, 0, 0).is_some());
        assert!(find_merge_at(&regions, 0, 1).is_none());
        assert!(find_merge_at(&regions, 1, 0).is_none());
    }

    #[test]
    fn is_covered_check() {
        let regions = vec![MergeRegion {
            start_row: 0,
            start_col: 0,
            end_row: 1,
            end_col: 1,
        }];
        assert!(!is_covered_by_merge(&regions, 0, 0)); // anchor
        assert!(is_covered_by_merge(&regions, 0, 1)); // covered
        assert!(is_covered_by_merge(&regions, 1, 0)); // covered
        assert!(is_covered_by_merge(&regions, 1, 1)); // covered
        assert!(!is_covered_by_merge(&regions, 2, 0)); // outside
    }

    #[test]
    fn overlap_detection() {
        let r1 = MergeRegion {
            start_row: 0,
            start_col: 0,
            end_row: 1,
            end_col: 1,
        };
        let r2 = MergeRegion {
            start_row: 1,
            start_col: 1,
            end_row: 2,
            end_col: 2,
        };
        let r3 = MergeRegion {
            start_row: 3,
            start_col: 3,
            end_row: 4,
            end_col: 4,
        };
        assert!(r1.overlaps(&r2));
        assert!(!r1.overlaps(&r3));
    }
}
