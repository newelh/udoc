//! Per-page LRU for character-code -> Unicode lookups.
//!
//! Wraps the per-glyph hot path in [`ContentInterpreter::decode_string`].
//! Each entry is a `(font_obj_ref, packed_code)` -> decoded `String`. The
//! cache is constructed when a [`ContentInterpreter`] is created (i.e. at
//! page-extract entry) and dropped with it (page exit).
//!
//! Why per-page, not per-doc:
//! 1. Doc-scope would break the memory-budget contract on multi-thousand-page
//!    reports ( convoy regression -- a 5K-page report would carry
//!    five thousand glyph caches simultaneously).
//! 2. font_obj_ref is stable across pages (FontBundle is Arc-shared via
//!    the resolver), so a per-doc cache wouldn't even produce significantly
//!    higher hit rates than per-page on real corpora; almost all glyph reuse
//!    is intra-page (a paragraph hits the same codes back-to-back).
//!
//! Capacity 256 was picked to:
//! - Cover Latin text comfortably (95+ printable ASCII + accents + symbols).
//! - Cover a couple of mixed CJK paragraphs without thrashing.
//! - Stay well under any plausible per-page memory budget (256 * ~32B ~= 8KB).
//! - Keep eviction scans cheap (linear over 256 u64 timestamps is sub-microsecond).
//!
//! Eviction is approximate-LRU via a per-entry monotonic counter; on capacity
//! hit we evict the entry with the smallest counter. This is O(N) per eviction
//! but eviction is rare on realistic workloads (most pages stay well under 256
//! distinct codes).

use std::collections::HashMap;

use crate::object::ObjRef;

/// Maximum entries before we start evicting. See module docs for sizing rationale.
const CAPACITY: usize = 256;

/// LRU entry: decoded text plus monotonic access stamp for eviction ordering.
#[derive(Debug)]
struct Entry {
    text: String,
    stamp: u64,
}

/// Per-page (font_id, glyph_code) -> decoded text cache.
///
/// `font_id` is the loaded font's [`ObjRef`] (stable per page; FontBundle is
/// Arc-shared so two pages using the same font dict get the same ObjRef).
/// `glyph_code` is the raw character-code packed big-endian into a u32. PDF
/// codes are 1-4 bytes; the packing matches `code_to_u32` in udoc-font.
#[derive(Debug, Default)]
pub(crate) struct DecodeCache {
    entries: HashMap<(ObjRef, u32), Entry>,
    /// Monotonic access counter. Wraps after 2^64 lookups (will not happen).
    next_stamp: u64,
}

impl DecodeCache {
    /// Construct an empty cache. Lazy: no allocation until the first insert.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            next_stamp: 0,
        }
    }

    /// Look up a cached decode for `(font_id, code)`. Returns the cached text
    /// (cloned) on hit; `None` on miss. Hits also bump the access stamp so the
    /// entry stays warm.
    pub fn get(&mut self, font_id: ObjRef, code: u32) -> Option<String> {
        let stamp = self.bump_stamp();
        let entry = self.entries.get_mut(&(font_id, code))?;
        entry.stamp = stamp;
        Some(entry.text.clone())
    }

    /// Insert a decoded string under `(font_id, code)`. Evicts the
    /// least-recently-accessed entry when the cache is at capacity.
    pub fn insert(&mut self, font_id: ObjRef, code: u32, text: String) {
        if self.entries.len() >= CAPACITY && !self.entries.contains_key(&(font_id, code)) {
            self.evict_oldest();
        }
        let stamp = self.bump_stamp();
        self.entries.insert((font_id, code), Entry { text, stamp });
    }

    /// Number of entries currently cached.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn bump_stamp(&mut self) -> u64 {
        let s = self.next_stamp;
        self.next_stamp = self.next_stamp.wrapping_add(1);
        s
    }

    fn evict_oldest(&mut self) {
        // Linear scan: cheap at CAPACITY=256, no extra data structure needed.
        if let Some(victim) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.stamp)
            .map(|(k, _)| *k)
        {
            self.entries.remove(&victim);
        }
    }
}

/// Pack a PDF character-code byte slice into a u32 (big-endian).
///
/// PDF character codes are 1-4 bytes (1 for simple/Type3, 2-4 for composite
/// CMap-encoded). Anything longer than 4 bytes returns `None` (callers fall
/// back to the uncached path; this is exceedingly rare in practice and only
/// reachable on malformed/exotic CMaps).
pub(crate) fn pack_code(code: &[u8]) -> Option<u32> {
    if code.is_empty() || code.len() > 4 {
        return None;
    }
    let mut val = 0u32;
    for &b in code {
        val = (val << 8) | b as u32;
    }
    Some(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(num: u32) -> ObjRef {
        ObjRef::new(num, 0)
    }

    #[test]
    fn miss_then_hit() {
        let mut c = DecodeCache::new();
        assert!(c.get(r(1), 0x41).is_none());
        c.insert(r(1), 0x41, "A".to_string());
        assert_eq!(c.get(r(1), 0x41).as_deref(), Some("A"));
    }

    #[test]
    fn distinct_fonts_distinct_keys() {
        let mut c = DecodeCache::new();
        c.insert(r(1), 0x41, "A".to_string());
        c.insert(r(2), 0x41, "B".to_string());
        assert_eq!(c.get(r(1), 0x41).as_deref(), Some("A"));
        assert_eq!(c.get(r(2), 0x41).as_deref(), Some("B"));
    }

    #[test]
    fn eviction_at_capacity() {
        let mut c = DecodeCache::new();
        // Fill to capacity.
        for i in 0..CAPACITY as u32 {
            c.insert(r(1), i, format!("c{i}"));
        }
        assert_eq!(c.len(), CAPACITY);
        // Touch a known entry so it's not the oldest.
        let _ = c.get(r(1), 0);
        // Insert one more; should evict the oldest (NOT the one we just touched).
        c.insert(r(1), CAPACITY as u32, "new".to_string());
        assert_eq!(c.len(), CAPACITY);
        assert!(
            c.get(r(1), 0).is_some(),
            "recently-used entry should survive eviction"
        );
        assert_eq!(c.get(r(1), CAPACITY as u32).as_deref(), Some("new"));
    }

    #[test]
    fn pack_code_roundtrip() {
        assert_eq!(pack_code(&[0x41]), Some(0x41));
        assert_eq!(pack_code(&[0x81, 0x40]), Some(0x8140));
        assert_eq!(pack_code(&[0x12, 0x34, 0x56, 0x78]), Some(0x1234_5678));
        assert_eq!(pack_code(&[]), None);
        assert_eq!(pack_code(&[0; 5]), None);
    }
}
