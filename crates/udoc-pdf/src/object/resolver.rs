//! Object resolver: resolves indirect references to concrete PDF objects.
//!
//! Given an indirect reference like `5 0 R`, the resolver:
//! 1. Checks the cache for a previously resolved result
//! 2. Looks up the object's location in the xref table
//! 3. Seeks to the offset and parses `N M obj <value> endobj`
//! 4. Caches and returns the result
//!
//! Cycle detection prevents infinite loops on circular references.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;

use ahash::AHashMap;
// `Entry` is generic over the hasher; importing through ahash keeps the
// match arm type-correct against `AHashMap`'s entry API.
use std::collections::hash_map::Entry;

use crate::crypt::CryptHandler;
#[cfg(any(test, feature = "test-internals"))]
use crate::diagnostics::NullDiagnostics;
use crate::diagnostics::{DiagnosticsSink, Warning, WarningKind};
use crate::error::{Error, Limit, ResultExt};
use crate::object::stream::{decode_stream, decode_stream_with_globals, DecodeLimits};
use crate::object::{ObjRef, PdfDictionary, PdfObject, PdfStream};
use crate::parse::object_parser::ObjectParser;
use crate::parse::{DocumentStructure, PdfVersion, Token, XrefEntry, XrefTable};
use crate::Result;

/// Default maximum number of cached objects.
const DEFAULT_CACHE_MAX: usize = 1024;

/// Default maximum depth for reference chain resolution.
const DEFAULT_MAX_CHAIN_DEPTH: usize = 64;

/// Maximum number of objects in an ObjStm. Prevents memory bombs
/// from malicious /N values causing huge Vec allocations.
const MAX_OBJSTM_N: usize = 100_000;

/// Maximum number of decoded stream results to cache.
///
/// Form XObjects (headers, footers, watermarks) appear on every page and
/// get re-decoded on each visit without this cache. 64 entries covers the
/// typical case without excessive memory use.
const DECODED_CACHE_MAX: usize = 64;

/// Maximum decoded stream size to cache. Streams larger than this are
/// decompressed fresh each time to avoid holding large buffers in memory.
const DECODED_CACHE_MAX_BYTES: usize = 1024 * 1024; // 1 MB

/// Resolves indirect object references to concrete PdfObject values.
pub struct ObjectResolver<'a> {
    /// Raw PDF data.
    data: &'a [u8],
    /// Cross-reference table mapping object numbers to file locations.
    xref: XrefTable,
    /// Trailer dictionary from the document structure.
    trailer: Option<PdfDictionary>,
    /// PDF version from the document header.
    #[allow(dead_code)] // read by version() accessor, which is test-only
    version: Option<PdfVersion>,
    /// Diagnostics sink for warnings.
    diagnostics: Arc<dyn DiagnosticsSink>,
    /// Cache of resolved objects. Each entry stores the object and a generation
    /// counter indicating when it was last accessed.
    ///
    /// `AHashMap` (per-process seeded ahash) instead of default SipHash on
    /// purpose: PDF object refs are 100% attacker-controlled keys.
    /// `rustc_hash`/FxHash would be HashDoS-vulnerable here; ahash is
    /// DOS-resistant and substantially faster than SipHash on `u32`-shaped
    /// keys.
    cache: AHashMap<ObjRef, (PdfObject, u64)>,
    /// Monotonic counter incremented on every cache access. Used to identify
    /// the least-recently-used entry for eviction (min generation = LRU).
    generation: u64,
    /// Min-heap for O(log n) LRU eviction. Each entry is (generation, ObjRef).
    /// Stale entries (where generation != cache entry's generation) are lazily
    /// skipped during eviction.
    eviction_heap: BinaryHeap<Reverse<(u64, ObjRef)>>,
    /// Maximum cache size.
    cache_max: usize,
    /// Set of objects currently being resolved (for cycle detection).
    resolving: HashSet<ObjRef>,
    /// Current depth of nested resolve() calls. Catches deep non-cyclic
    /// resolution chains (e.g., ObjStm -> ObjStm container) that the
    /// `resolving` set alone can't detect.
    resolve_depth: usize,
    /// Limits applied when decoding stream data.
    decode_limits: DecodeLimits,
    /// Encryption handler for decrypting objects and streams.
    /// None means the document is not encrypted (zero overhead).
    crypt_handler: Option<Arc<CryptHandler>>,
    /// Cache of decoded (decompressed) stream data, keyed by ObjRef.
    ///
    /// Form XObjects that repeat across pages (headers, footers, watermarks)
    /// are decompressed once and served from this cache on subsequent calls.
    /// Capped at DECODED_CACHE_MAX entries; entries larger than
    /// DECODED_CACHE_MAX_BYTES are not cached to bound memory usage.
    /// Only populated when decode_stream_data is called with Some(ObjRef).
    ///
    /// Same `AHashMap` rationale as `cache`: ObjRef keys are
    /// attacker-controlled.
    decoded_cache: AHashMap<ObjRef, Vec<u8>>,
}

impl<'a> ObjectResolver<'a> {
    /// Create a resolver from raw data and xref table.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn new(data: &'a [u8], xref: XrefTable) -> Self {
        Self {
            data,
            xref,
            trailer: None,
            version: None,
            diagnostics: Arc::new(NullDiagnostics),
            cache: AHashMap::new(),
            generation: 0,
            eviction_heap: BinaryHeap::new(),
            cache_max: DEFAULT_CACHE_MAX,
            resolving: HashSet::new(),
            resolve_depth: 0,
            decode_limits: DecodeLimits::default(),
            crypt_handler: None,
            decoded_cache: AHashMap::new(),
        }
    }

    /// Create a resolver from a fully parsed DocumentStructure.
    ///
    /// Stores the trailer dictionary and PDF version for downstream access.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn from_document(data: &'a [u8], doc: DocumentStructure) -> Self {
        Self {
            data,
            xref: doc.xref,
            trailer: Some(doc.trailer),
            version: Some(doc.version),
            diagnostics: Arc::new(NullDiagnostics),
            cache: AHashMap::new(),
            generation: 0,
            eviction_heap: BinaryHeap::new(),
            cache_max: DEFAULT_CACHE_MAX,
            resolving: HashSet::new(),
            resolve_depth: 0,
            decode_limits: DecodeLimits::default(),
            crypt_handler: None,
            decoded_cache: AHashMap::new(),
        }
    }

    /// Create a resolver from a DocumentStructure with a custom diagnostics sink.
    pub fn from_document_with_diagnostics(
        data: &'a [u8],
        doc: DocumentStructure,
        diagnostics: Arc<dyn DiagnosticsSink>,
    ) -> Self {
        Self {
            data,
            xref: doc.xref,
            trailer: Some(doc.trailer),
            version: Some(doc.version),
            diagnostics,
            cache: AHashMap::new(),
            generation: 0,
            eviction_heap: BinaryHeap::new(),
            cache_max: DEFAULT_CACHE_MAX,
            resolving: HashSet::new(),
            resolve_depth: 0,
            decode_limits: DecodeLimits::default(),
            crypt_handler: None,
            decoded_cache: AHashMap::new(),
        }
    }

    /// Create a resolver with a custom diagnostics sink.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn with_diagnostics(
        data: &'a [u8],
        xref: XrefTable,
        diagnostics: Arc<dyn DiagnosticsSink>,
    ) -> Self {
        Self {
            data,
            xref,
            trailer: None,
            version: None,
            diagnostics,
            cache: AHashMap::new(),
            generation: 0,
            eviction_heap: BinaryHeap::new(),
            cache_max: DEFAULT_CACHE_MAX,
            resolving: HashSet::new(),
            resolve_depth: 0,
            decode_limits: DecodeLimits::default(),
            crypt_handler: None,
            decoded_cache: AHashMap::new(),
        }
    }

    /// Get a reference to the diagnostics sink.
    #[must_use]
    pub fn diagnostics(&self) -> &dyn DiagnosticsSink {
        &*self.diagnostics
    }

    /// Set the maximum number of objects to cache.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn set_cache_max(&mut self, max: usize) {
        self.cache_max = max;
    }

    /// Set the decode limits used for stream decompression.
    pub fn set_decode_limits(&mut self, limits: DecodeLimits) {
        self.decode_limits = limits;
    }

    /// Set the encryption handler for decrypting objects and streams.
    pub(crate) fn set_crypt_handler(&mut self, handler: Arc<CryptHandler>) {
        self.crypt_handler = Some(handler);
    }

    /// Resolve an indirect reference to a concrete object.
    ///
    /// Returns a cached result if available, otherwise parses the object
    /// from the file and caches it.
    pub fn resolve(&mut self, obj_ref: ObjRef) -> Result<PdfObject> {
        // Check cache first (O(1) hit via generation bump)
        if let Some(entry) = self.cache.get_mut(&obj_ref) {
            self.generation += 1;
            entry.1 = self.generation;
            self.eviction_heap.push(Reverse((self.generation, obj_ref)));
            let obj = entry.0.clone();
            // Compact the eviction heap if it has too many stale entries.
            // Threshold: 4x cache_max. Amortized O(1) per resolve.
            if self.eviction_heap.len() > self.cache_max * 4 {
                self.compact_eviction_heap();
            }
            return Ok(obj);
        }

        // Check depth BEFORE incrementing -- no decrement needed on error
        if self.resolve_depth >= DEFAULT_MAX_CHAIN_DEPTH {
            return Err(Error::resource_limit(Limit::RecursionDepth(
                DEFAULT_MAX_CHAIN_DEPTH,
            )));
        }

        // Single increment/decrement pair with no early returns between them
        self.resolve_depth += 1;
        let result = self.resolve_with_tracking(obj_ref);
        self.resolve_depth -= 1;

        // Cache successful results with LRU eviction
        if let Ok(ref obj) = result {
            self.cache_object(obj_ref, obj.clone());
        }

        result
    }

    /// Handle cycle detection and dispatch to resolve_uncached.
    /// Called within the depth-tracked scope of resolve().
    fn resolve_with_tracking(&mut self, obj_ref: ObjRef) -> Result<PdfObject> {
        if !self.resolving.insert(obj_ref) {
            return Err(Error::structure(format!(
                "circular reference detected while resolving {obj_ref}"
            )));
        }
        let result = self.resolve_uncached(obj_ref);
        self.resolving.remove(&obj_ref);
        result
    }

    /// Resolve a PdfObject that might be a Reference, returning the
    /// concrete object. Non-reference objects are returned as-is.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn resolve_if_ref(&mut self, obj: PdfObject) -> Result<PdfObject> {
        match obj {
            PdfObject::Reference(r) => self.resolve(r),
            other => Ok(other),
        }
    }

    /// Follow a chain of indirect references until a non-Reference object
    /// is reached, up to `max_depth` hops. Returns an error if the chain
    /// exceeds the depth limit.
    ///
    /// This provides defense-in-depth against long (but non-cyclic) reference
    /// chains that the cycle detector alone wouldn't catch.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn resolve_chain(&mut self, obj: PdfObject, max_depth: usize) -> Result<PdfObject> {
        let mut current = obj;
        for _ in 0..max_depth {
            match current {
                PdfObject::Reference(r) => {
                    current = self.resolve(r)?;
                }
                other => return Ok(other),
            }
        }
        // If we're still on a Reference after max_depth hops, that's too deep.
        if matches!(current, PdfObject::Reference(_)) {
            Err(Error::structure(format!(
                "reference chain exceeded depth limit of {max_depth}"
            )))
        } else {
            Ok(current)
        }
    }

    /// Follow a chain of indirect references using the default depth limit.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn resolve_chain_default(&mut self, obj: PdfObject) -> Result<PdfObject> {
        self.resolve_chain(obj, DEFAULT_MAX_CHAIN_DEPTH)
    }

    /// Look up a key in a dictionary and resolve it if it's an indirect reference.
    ///
    /// Returns `Ok(None)` if the key is absent. Returns the resolved object if
    /// the value is a reference, or the value itself if it's direct.
    pub fn get_and_resolve(
        &mut self,
        dict: &PdfDictionary,
        key: &[u8],
    ) -> Result<Option<PdfObject>> {
        match dict.get(key) {
            None => Ok(None),
            Some(PdfObject::Reference(r)) => {
                let resolved = self.resolve(*r).context(format!(
                    "resolving /{} reference",
                    String::from_utf8_lossy(key)
                ))?;
                Ok(Some(resolved))
            }
            Some(obj) => Ok(Some(obj.clone())),
        }
    }

    /// Look up a key in a dictionary, resolve if indirect, and expect a dictionary.
    ///
    /// Returns `Ok(None)` if the key is absent. Streams are accepted (returns
    /// their dictionary). Returns an error for other types.
    pub fn get_resolved_dict(
        &mut self,
        dict: &PdfDictionary,
        key: &[u8],
    ) -> Result<Option<PdfDictionary>> {
        match self.get_and_resolve(dict, key)? {
            None => Ok(None),
            Some(PdfObject::Dictionary(d)) => Ok(Some(d)),
            Some(PdfObject::Stream(s)) => Ok(Some(s.dict)),
            Some(other) => Err(Error::structure(format!(
                "expected dictionary for key /{}, got {}",
                String::from_utf8_lossy(key),
                other.type_name()
            ))),
        }
    }

    /// Look up a key in a dictionary, resolve if indirect, and expect a name.
    ///
    /// Returns `Ok(None)` if the key is absent or resolves to something other
    /// than a name (unlike `get_resolved_dict` which errors on type mismatch,
    /// this is lenient for name-or-missing cases which are common in font
    /// dicts). Some PDFs (seen from older pdfTeX / hyperref) write
    /// `/BaseFont 47 0 R` where the target is a standalone Name object; the
    /// direct `PdfDictionary::get_name` accessor can't see through the
    /// indirection and returns None, which downstream turns into "unknown"
    /// font names and breaks renderer font lookups.
    pub fn get_resolved_name(
        &mut self,
        dict: &PdfDictionary,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        match self.get_and_resolve(dict, key)? {
            None => Ok(None),
            Some(PdfObject::Name(n)) => Ok(Some(n)),
            Some(_) => Ok(None),
        }
    }

    /// Look up a key in a dictionary, resolve if indirect, and expect an array.
    ///
    /// Returns `Ok(None)` if the key is absent. Returns an error if the resolved
    /// value is not an array.
    pub fn get_resolved_array(
        &mut self,
        dict: &PdfDictionary,
        key: &[u8],
    ) -> Result<Option<Vec<PdfObject>>> {
        match self.get_and_resolve(dict, key)? {
            None => Ok(None),
            Some(PdfObject::Array(a)) => Ok(Some(a)),
            Some(other) => Err(Error::structure(format!(
                "expected array for key /{}, got {}",
                String::from_utf8_lossy(key),
                other.type_name()
            ))),
        }
    }

    /// Look up a key in a dictionary, resolve if indirect, and expect a stream.
    ///
    /// Returns `Ok(None)` if the key is absent. Returns an error if the resolved
    /// value is not a stream.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn get_resolved_stream(
        &mut self,
        dict: &PdfDictionary,
        key: &[u8],
    ) -> Result<Option<PdfStream>> {
        match self.get_and_resolve(dict, key)? {
            None => Ok(None),
            Some(PdfObject::Stream(s)) => Ok(Some(s)),
            Some(other) => Err(Error::structure(format!(
                "expected stream for key /{}, got {}",
                String::from_utf8_lossy(key),
                other.type_name()
            ))),
        }
    }

    /// Resolve an indirect reference and expect a dictionary.
    ///
    /// Returns an error if the object is not a dictionary. Streams are
    /// accepted (returns their dictionary).
    pub fn resolve_dict(&mut self, obj_ref: ObjRef) -> Result<PdfDictionary> {
        let obj = self.resolve(obj_ref)?;
        Self::expect_dict(obj).context(format!(
            "resolving {} {} R as dictionary",
            obj_ref.num, obj_ref.gen
        ))
    }

    /// Resolve an indirect reference and expect an array.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn resolve_array(&mut self, obj_ref: ObjRef) -> Result<Vec<PdfObject>> {
        let obj = self.resolve(obj_ref)?;
        Self::expect_array(obj).context(format!(
            "resolving {} {} R as array",
            obj_ref.num, obj_ref.gen
        ))
    }

    /// Resolve an indirect reference and expect a stream.
    pub fn resolve_stream(&mut self, obj_ref: ObjRef) -> Result<PdfStream> {
        let obj = self.resolve(obj_ref)?;
        Self::expect_stream(obj).context(format!(
            "resolving {} {} R as stream",
            obj_ref.num, obj_ref.gen
        ))
    }

    /// If `obj` is an indirect reference, resolve it; then expect a dictionary.
    ///
    /// Handles both direct dictionaries and indirect references to dictionaries,
    /// so callers don't need to branch on the object type.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn resolve_as_dict(&mut self, obj: PdfObject) -> Result<PdfDictionary> {
        let resolved = self.resolve_if_ref(obj)?;
        Self::expect_dict(resolved)
    }

    /// If `obj` is an indirect reference, resolve it; then expect an array.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn resolve_as_array(&mut self, obj: PdfObject) -> Result<Vec<PdfObject>> {
        let resolved = self.resolve_if_ref(obj)?;
        Self::expect_array(resolved)
    }

    /// If `obj` is an indirect reference, resolve it; then expect a stream.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn resolve_as_stream(&mut self, obj: PdfObject) -> Result<PdfStream> {
        let resolved = self.resolve_if_ref(obj)?;
        Self::expect_stream(resolved)
    }

    /// Extract a dictionary from a resolved object, accepting streams too.
    fn expect_dict(obj: PdfObject) -> Result<PdfDictionary> {
        match obj {
            PdfObject::Dictionary(d) => Ok(d),
            PdfObject::Stream(s) => Ok(s.dict),
            other => Err(Error::structure(format!(
                "expected dictionary, got {}",
                other.type_name()
            ))),
        }
    }

    /// Extract an array from a resolved object.
    #[cfg(any(test, feature = "test-internals"))]
    fn expect_array(obj: PdfObject) -> Result<Vec<PdfObject>> {
        match obj {
            PdfObject::Array(a) => Ok(a),
            other => Err(Error::structure(format!(
                "expected array, got {}",
                other.type_name()
            ))),
        }
    }

    /// Extract a stream from a resolved object.
    fn expect_stream(obj: PdfObject) -> Result<PdfStream> {
        match obj {
            PdfObject::Stream(s) => Ok(s),
            other => Err(Error::structure(format!(
                "expected stream, got {}",
                other.type_name()
            ))),
        }
    }

    /// Decode a stream's data, applying the filter chain from its dictionary.
    ///
    /// Slices the raw bytes from the resolver's source data, clamping to EOF
    /// if the stream extends past the end of the file (with a warning).
    ///
    /// # Encryption
    ///
    /// `obj_ref` controls per-object decryption:
    /// - `Some(ref)`: decrypt using this object's key (normal case for all
    ///   top-level streams: content, XObject, ToUnicode, ObjStm, etc.)
    /// - `None`: skip decryption entirely. Used for inline images (BI/ID/EI),
    ///   whose data lives inside the already-decrypted content stream, and
    ///   in tests where encryption is not involved.
    pub fn decode_stream_data(
        &mut self,
        stream: &PdfStream,
        obj_ref: Option<ObjRef>,
    ) -> Result<Vec<u8>> {
        // Cache hit: serve from decoded_cache when we have an ObjRef.
        if let Some(r) = obj_ref {
            if let Some(cached) = self.decoded_cache.get(&r) {
                return Ok(cached.clone());
            }
        }

        let start = usize::try_from(stream.data_offset).map_err(|_| {
            Error::structure_at(
                stream.data_offset,
                format!(
                    "stream data offset {} exceeds address space",
                    stream.data_offset
                ),
            )
        })?;
        let length = usize::try_from(stream.data_length).map_err(|_| {
            Error::structure_at(
                stream.data_offset,
                format!(
                    "stream data length {} exceeds address space",
                    stream.data_length
                ),
            )
        })?;

        if start >= self.data.len() {
            return Err(Error::structure_at(
                stream.data_offset,
                format!(
                    "stream data offset {} is beyond EOF ({})",
                    stream.data_offset,
                    self.data.len()
                ),
            ));
        }

        let end = start.saturating_add(length);
        let actual_end = if end > self.data.len() {
            self.diagnostics.warning(Warning::new(
                Some(stream.data_offset),
                WarningKind::StreamExtendsPastEof,
                format!(
                    "stream extends past EOF (offset {} + length {} > file size {}), clamping",
                    stream.data_offset,
                    stream.data_length,
                    self.data.len()
                ),
            ));
            self.data.len()
        } else {
            end
        };

        let raw = &self.data[start..actual_end];

        // Decrypt stream data before applying the filter chain.
        let decrypted;
        let data = if let (Some(handler), Some(obj_ref)) = (&self.crypt_handler, obj_ref) {
            let stream_type = stream.dict.get_name(b"Type");
            if handler.should_skip(obj_ref, stream_type) {
                raw
            } else {
                decrypted = handler.decrypt_stream_data(raw, obj_ref);
                &decrypted
            }
        } else {
            raw
        };

        // Resolve JBIG2Globals if the stream uses JBIG2Decode filter.
        let jbig2_globals = self.resolve_jbig2_globals(&stream.dict);

        if std::env::var_os("UDOC_JBIG2_DEBUG").is_some() {
            let filter_name = stream.dict.get(b"Filter").map(|f| format!("{:?}", f));
            eprintln!(
                "decode_stream_data: obj_ref={:?} data.len={} filter={:?}",
                obj_ref,
                data.len(),
                filter_name
            );
        }

        let result = decode_stream_with_globals(
            data,
            &stream.dict,
            &self.decode_limits,
            self.diagnostics.as_ref(),
            stream.data_offset,
            jbig2_globals,
        )
        .context("decoding stream data through filter chain")?;

        // Cache miss: store the decoded result when we have an ObjRef and the
        // result fits within the per-entry size limit. Evict one arbitrary entry
        // when the cache is full (oldest entries are not tracked; for this
        // workload -- form XObjects that repeat every page -- any eviction
        // strategy is fine because the hot set is small).
        if let Some(r) = obj_ref {
            if result.len() <= DECODED_CACHE_MAX_BYTES {
                if self.decoded_cache.len() >= DECODED_CACHE_MAX {
                    // Pop an arbitrary entry to make room. The HashMap iterator
                    // order is non-deterministic but consistent within a run.
                    if let Some(evict_key) = self.decoded_cache.keys().next().copied() {
                        self.decoded_cache.remove(&evict_key);
                    }
                }
                self.decoded_cache.insert(r, result.clone());
            }
        }

        Ok(result)
    }

    /// Resolve JBIG2Globals from a stream's DecodeParms dictionary.
    /// Returns the decoded global segment bytes, or None if not present.
    fn resolve_jbig2_globals(&mut self, dict: &PdfDictionary) -> Option<Vec<u8>> {
        // Check if the stream uses JBIG2Decode filter
        let has_jbig2 = match dict.get(b"Filter") {
            Some(PdfObject::Name(n)) => n == b"JBIG2Decode",
            Some(PdfObject::Array(arr)) => arr.iter().any(|o| o.as_name() == Some(b"JBIG2Decode")),
            _ => false,
        };
        if !has_jbig2 {
            return None;
        }

        // Get the DecodeParms dict and look for JBIG2Globals reference
        let dp = match dict.get(b"DecodeParms") {
            Some(PdfObject::Dictionary(d)) => d.clone(),
            Some(PdfObject::Array(arr)) => arr
                .first()
                .and_then(|o| o.as_dict().cloned())
                .unwrap_or_default(),
            _ => return None,
        };

        let globals_ref = match dp.get(b"JBIG2Globals") {
            Some(PdfObject::Reference(r)) => *r,
            _ => return None,
        };

        // Resolve the globals stream and decode it
        let globals_stream = self.resolve_stream(globals_ref).ok()?;
        let raw_start = globals_stream.data_offset as usize;
        let raw_len = globals_stream.data_length as usize;
        if raw_start >= self.data.len() {
            return None;
        }
        let raw_end = (raw_start + raw_len).min(self.data.len());
        let raw = &self.data[raw_start..raw_end];

        // Decode the globals stream (it may have its own filters like Flate)
        decode_stream(
            raw,
            &globals_stream.dict,
            &self.decode_limits,
            self.diagnostics.as_ref(),
            globals_stream.data_offset,
        )
        .ok()
    }

    /// Insert an object into the cache with O(log n) LRU eviction.
    fn cache_object(&mut self, obj_ref: ObjRef, obj: PdfObject) {
        if self.cache_max == 0 {
            return;
        }
        // Evict until we're under the limit.
        while self.cache.len() >= self.cache_max {
            match self.eviction_heap.pop() {
                Some(Reverse((gen, candidate))) => {
                    // Check if this heap entry is still current (not stale).
                    if let Entry::Occupied(e) = self.cache.entry(candidate) {
                        if e.get().1 == gen {
                            e.remove();
                        }
                        // Otherwise: stale entry (object was re-accessed since this
                        // heap entry was pushed). Skip and try next.
                    }
                }
                None => {
                    // Heap empty but cache full -- shouldn't happen, but break to
                    // avoid infinite loop.
                    break;
                }
            }
        }
        self.generation += 1;
        self.eviction_heap.push(Reverse((self.generation, obj_ref)));
        self.cache.insert(obj_ref, (obj, self.generation));
    }

    /// Rebuild the eviction heap, discarding stale entries.
    ///
    /// Called when the heap grows to more than 4x cache_max due to repeated
    /// cache hits pushing new entries without removing old ones.
    fn compact_eviction_heap(&mut self) {
        let fresh: Vec<_> = self
            .cache
            .iter()
            .map(|(obj_ref, (_, gen))| Reverse((*gen, *obj_ref)))
            .collect();
        self.eviction_heap = BinaryHeap::from(fresh);
    }

    /// Resolve an object stored in a compressed object stream (ObjStm).
    ///
    /// Decodes the entire ObjStm and bulk-caches all objects inside it,
    /// so subsequent accesses to sibling objects don't re-decompress.
    fn resolve_from_objstm(
        &mut self,
        obj_ref: ObjRef,
        stream_obj: u32,
        xref_index: u32,
    ) -> Result<PdfObject> {
        // 2a. Nested ObjStm prohibition: the container stream must be at a
        // file offset, never inside another ObjStm.
        if let Some(entry) = self.xref.get(stream_obj) {
            if matches!(entry, XrefEntry::Compressed { .. }) {
                return Err(Error::structure(format!(
                    "ObjStm container {stream_obj} is itself compressed (nested ObjStm prohibited)"
                )));
            }
        }

        // 2b. Resolve the ObjStm stream object. This re-enters resolve(),
        // which handles caching, cycle detection, and depth tracking.
        let stream_object = self
            .resolve(ObjRef::new(stream_obj, 0))
            .context(format!("resolving ObjStm container {stream_obj}"))?;

        // 2c. Extract and validate the PdfStream.
        let stream = match stream_object {
            PdfObject::Stream(s) => s,
            other => {
                return Err(Error::structure(format!(
                    "ObjStm {stream_obj} is {}, expected stream",
                    other.type_name()
                )));
            }
        };

        // Validate /Type is /ObjStm
        match stream.dict.get_name(b"Type") {
            Some(b"ObjStm") => {}
            Some(other) => {
                return Err(Error::structure(format!(
                    "ObjStm {stream_obj} has /Type /{}, expected /ObjStm",
                    String::from_utf8_lossy(other)
                )));
            }
            None => {
                return Err(Error::structure(format!(
                    "ObjStm {stream_obj} missing required /Type entry"
                )));
            }
        }

        // /N: number of objects
        let n_raw = stream.dict.get_i64(b"N").ok_or_else(|| {
            Error::structure(format!("ObjStm {stream_obj} missing required /N entry"))
        })?;
        if n_raw < 1 || n_raw as u64 > MAX_OBJSTM_N as u64 {
            return Err(Error::structure(format!(
                "ObjStm {stream_obj} /N = {n_raw} out of valid range 1..={MAX_OBJSTM_N}",
            )));
        }
        let n = n_raw as usize;

        // /First: byte offset of first object within decoded stream
        let first = stream.dict.get_i64(b"First").ok_or_else(|| {
            Error::structure(format!("ObjStm {stream_obj} missing required /First entry"))
        })?;
        if first < 0 {
            return Err(Error::structure(format!(
                "ObjStm {stream_obj} /First = {first} is negative"
            )));
        }
        let first = first as usize;

        // 2d. Decode the stream body.
        // ObjStm uses the container stream's obj number for decryption.
        let decoded = self
            .decode_stream_data(&stream, Some(ObjRef::new(stream_obj, 0)))
            .context(format!("decoding ObjStm {stream_obj}"))?;

        if first > decoded.len() {
            return Err(Error::structure(format!(
                "ObjStm {stream_obj} /First ({first}) exceeds decoded data length ({})",
                decoded.len()
            )));
        }

        // 2e. Parse the header section: N pairs of (obj_num, offset).
        let header_data = &decoded[..first];
        let mut header_parser = ObjectParser::new(header_data);
        let mut entries: Vec<(u32, usize)> = Vec::with_capacity(n);

        for i in 0..n {
            let obj_num = match header_parser.parse_object() {
                Ok(PdfObject::Integer(num)) if num >= 0 => num as u32,
                Ok(other) => {
                    return Err(Error::structure(format!(
                        "ObjStm {stream_obj} header entry {i}: expected object number, got {other}"
                    )));
                }
                Err(e) => {
                    return Err(e.context(format!(
                        "ObjStm {stream_obj} header entry {i}: reading object number"
                    )));
                }
            };

            let offset = match header_parser.parse_object() {
                Ok(PdfObject::Integer(off)) if off >= 0 => off as usize,
                Ok(other) => {
                    return Err(Error::structure(format!(
                        "ObjStm {stream_obj} header entry {i}: expected offset, got {other}"
                    )));
                }
                Err(e) => {
                    return Err(e.context(format!(
                        "ObjStm {stream_obj} header entry {i}: reading offset"
                    )));
                }
            };

            let abs_offset = first + offset;
            if abs_offset > decoded.len() {
                return Err(Error::structure(format!(
                    "ObjStm {stream_obj} header entry {i}: offset {offset} + /First {first} = \
                     {abs_offset} exceeds decoded data length ({})",
                    decoded.len()
                )));
            }

            entries.push((obj_num, abs_offset));
        }

        // Check xref index consistency: the xref entry's index should match
        // the position of obj_ref.num in the ObjStm header. A mismatch
        // indicates a corrupt xref but isn't fatal (we look up by obj_num).
        let xref_idx = xref_index as usize;
        if xref_idx < entries.len() && entries[xref_idx].0 != obj_ref.num {
            self.diagnostics.warning(Warning::new(
                Some(stream.data_offset),
                WarningKind::ObjectHeaderMismatch,
                format!(
                    "ObjStm {stream_obj} xref index {xref_index} maps to obj {} \
                     but expected obj {}",
                    entries[xref_idx].0, obj_ref.num
                ),
            ));
        }

        // 2f. Parse all objects and bulk-cache them.
        // If encrypted, strings inside ObjStm objects must be decrypted using
        // the ObjStm's own object number for per-object key derivation (not
        // the individual object's number). Stream-level decryption (above)
        // only decrypts the raw bytes; string-level decryption is separate.
        let objstm_ref = ObjRef::new(stream_obj, 0);
        let stream_offset = stream.data_offset;
        for (i, &(obj_num, abs_offset)) in entries.iter().enumerate() {
            let obj_data = &decoded[abs_offset..];
            let mut obj_parser = ObjectParser::new(obj_data);
            match obj_parser.parse_object() {
                Ok(obj) => {
                    let obj = if let Some(handler) = &self.crypt_handler {
                        handler.decrypt_object(obj, objstm_ref)
                    } else {
                        obj
                    };
                    let cached_ref = ObjRef::new(obj_num, 0);
                    self.cache_object(cached_ref, obj);
                }
                Err(e) if obj_num == obj_ref.num => {
                    return Err(e.context(format!(
                        "parsing target object {obj_num} in ObjStm {stream_obj}"
                    )));
                }
                Err(e) => {
                    self.diagnostics.warning(Warning::new(
                        Some(stream_offset),
                        WarningKind::DecodeError,
                        format!("ObjStm {stream_obj} object {i} (obj {obj_num}): parse error: {e}"),
                    ));
                }
            }
        }

        // 2g. Return the requested object from cache.
        if let Some(entry) = self.cache.get_mut(&obj_ref) {
            self.generation += 1;
            entry.1 = self.generation;
            Ok(entry.0.clone())
        } else {
            Err(Error::structure(format!(
                "object {obj_ref} not found in ObjStm {stream_obj} \
                 ({n} objects parsed)"
            )))
        }
    }

    /// Actual resolution logic (no cache, no cycle tracking).
    fn resolve_uncached(&mut self, obj_ref: ObjRef) -> Result<PdfObject> {
        let entry = self.xref.get(obj_ref.num).ok_or_else(|| {
            Error::structure(format!("object {} not found in xref table", obj_ref.num))
        })?;

        match *entry {
            XrefEntry::Uncompressed { offset, gen } => {
                if gen != obj_ref.gen {
                    self.diagnostics.warning(Warning::new(
                        Some(offset),
                        WarningKind::ObjectHeaderMismatch,
                        format!("generation mismatch for {obj_ref}: xref has gen {gen}"),
                    ));
                }
                let obj = self
                    .parse_object_at(offset, obj_ref)
                    .context(format!("resolving {obj_ref}"))?;

                // Decrypt strings within the object if encrypted
                if let Some(handler) = &self.crypt_handler {
                    Ok(handler.decrypt_object(obj, obj_ref))
                } else {
                    Ok(obj)
                }
            }
            XrefEntry::Free { .. } => Err(Error::structure(format!(
                "object {obj_ref} is marked as free in xref"
            ))),
            XrefEntry::Compressed { stream_obj, index } => {
                // No decrypt_object call here: resolve_from_objstm handles both
                // stream-level decryption (raw bytes, via decode_stream_data) and
                // string-level decryption (via decrypt_object on each parsed object,
                // using the ObjStm's obj number for per-object key derivation).
                self.resolve_from_objstm(obj_ref, stream_obj, index)
                    .context(format!("resolving {obj_ref} from ObjStm {stream_obj}"))
            }
        }
    }

    /// Parse an object definition at the given byte offset.
    /// Expected format: `<obj_num> <gen_num> obj <object> endobj`
    fn parse_object_at(&self, offset: u64, expected_ref: ObjRef) -> Result<PdfObject> {
        let start: usize = usize::try_from(offset).map_err(|_| {
            Error::structure_at(
                offset,
                format!("object offset {offset} exceeds address space on this platform"),
            )
        })?;
        if start >= self.data.len() {
            return Err(Error::structure_at(
                offset,
                format!(
                    "object offset {offset} is beyond end of file ({})",
                    self.data.len()
                ),
            ));
        }

        let mut parser =
            ObjectParser::with_diagnostics(&self.data[start..], self.diagnostics.clone());

        let lexer = parser.lexer_mut();

        // Read: <obj_num> <gen_num> obj
        let obj_num_token = lexer.next_token();
        let obj_num = match obj_num_token {
            Token::Integer(n) => n,
            other => {
                return Err(Error::parse(offset, "object number", format!("{other:?}")));
            }
        };

        let gen_token = lexer.next_token();
        let gen = match gen_token {
            Token::Integer(n) => n,
            other => {
                return Err(Error::parse(
                    offset,
                    "generation number",
                    format!("{other:?}"),
                ));
            }
        };

        let obj_keyword = lexer.next_token();
        if obj_keyword != Token::Obj {
            return Err(Error::parse(
                offset,
                "'obj' keyword",
                format!("{obj_keyword:?}"),
            ));
        }

        // Warn if the object header doesn't match what the xref says.
        // Use try_from to safely handle negative or out-of-range values
        // instead of truncating casts (obj_num as u32, gen as u16).
        let header_matches = u32::try_from(obj_num)
            .ok()
            .zip(u16::try_from(gen).ok())
            .is_some_and(|(n, g)| n == expected_ref.num && g == expected_ref.gen);
        if !header_matches {
            self.diagnostics.warning(Warning::new(
                Some(offset),
                WarningKind::ObjectHeaderMismatch,
                format!(
                    "object header says {} {} obj but xref says {}",
                    obj_num, gen, expected_ref
                ),
            ));
        }

        // Parse the actual object content
        let mut obj = parser
            .parse_object()
            .context(format!("parsing object body at offset {offset}"))?;

        // Fix stream data_offset: the parser computed it relative to the
        // slice &data[start.], but we need an absolute file offset.
        if let PdfObject::Stream(ref mut stream) = obj {
            stream.data_offset += offset;
        }

        // Consume trailing `endobj` keyword. Missing endobj is common in
        // malformed PDFs so we warn rather than fail.
        let trailing = parser.lexer_mut().peek_token();
        if trailing == Token::EndObj {
            parser.lexer_mut().next_token();
        } else if trailing != Token::Eof {
            self.diagnostics.warning(Warning::new(
                Some(offset),
                WarningKind::MissingEndObj,
                format!("expected 'endobj' after object body, got {trailing:?}"),
            ));
        }

        Ok(obj)
    }

    /// Number of cached objects.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Clear the object cache and eviction heap.
    ///
    /// Also resets the generation counter. After this call, the resolver
    /// behaves as if no objects were ever cached.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.eviction_heap.clear();
        self.generation = 0;
    }

    /// Release all document-scoped caches: resolved objects, eviction heap,
    /// and decoded-stream cache. Shrinks the underlying `HashMap` capacity so
    /// the allocator can reclaim pages instead of hanging on to the
    /// high-water-mark bucket count.
    ///
    /// Used by [`crate::Document::reset_document_caches`] (T60-MEMBATCH) for
    /// long-running batch workers that want to cap RSS between operations
    /// without dropping the whole document.
    pub fn reset_caches(&mut self) {
        self.cache = AHashMap::new();
        self.eviction_heap = BinaryHeap::new();
        self.decoded_cache = AHashMap::new();
        self.generation = 0;
        // resolving + resolve_depth are zero outside an active call; no
        // need to touch them.
    }

    /// Get a reference to the xref table.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn xref(&self) -> &XrefTable {
        &self.xref
    }

    /// Get the trailer dictionary, if this resolver was built from a DocumentStructure.
    #[must_use]
    pub fn trailer(&self) -> Option<&PdfDictionary> {
        self.trailer.as_ref()
    }

    /// Get the PDF version, if this resolver was built from a DocumentStructure.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn version(&self) -> Option<&PdfVersion> {
        self.version.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ResourceLimitError;
    use crate::parse::{DocumentParser, XrefEntry};
    use crate::CollectingDiagnostics;

    /// Build a minimal PDF with the given object bodies.
    /// Each entry is (obj_num, gen, body_bytes).
    /// Returns (raw_data, XrefTable).
    fn build_pdf_with_objects(objects: &[(u32, u16, &[u8])]) -> (Vec<u8>, XrefTable) {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let mut xref = XrefTable::new();
        // Object 0 is always free
        xref.insert_if_absent(
            0,
            XrefEntry::Free {
                next_free: 0,
                gen: 65535,
            },
        );

        for &(num, gen, body) in objects {
            let offset = data.len() as u64;
            data.extend_from_slice(format!("{num} {gen} obj\n").as_bytes());
            data.extend_from_slice(body);
            data.extend_from_slice(b"\nendobj\n");
            xref.insert_if_absent(num, XrefEntry::Uncompressed { offset, gen });
        }

        (data, xref)
    }

    // -- Basic resolution --

    #[test]
    fn test_resolve_integer() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));
    }

    #[test]
    fn test_resolve_dictionary() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"<< /Type /Catalog /Pages 2 0 R >>")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        let dict = obj.as_dict().expect("expected dictionary");
        assert_eq!(
            dict.get(b"Type"),
            Some(&PdfObject::Name(b"Catalog".to_vec()))
        );
        assert_eq!(
            dict.get(b"Pages"),
            Some(&PdfObject::Reference(ObjRef::new(2, 0)))
        );
    }

    #[test]
    fn test_resolve_array() {
        let (data, xref) = build_pdf_with_objects(&[(5, 0, b"[1 2 3]")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(5, 0)).unwrap();
        let arr = obj.as_array().expect("expected array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0], PdfObject::Integer(1));
    }

    #[test]
    fn test_resolve_string() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"(Hello World)")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        let s = obj.as_pdf_string().expect("expected string");
        assert_eq!(s.as_bytes(), b"Hello World");
    }

    #[test]
    fn test_resolve_null() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"null")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert!(obj.is_null());
    }

    #[test]
    fn test_resolve_multiple_objects() {
        let (data, xref) = build_pdf_with_objects(&[
            (1, 0, b"<< /Type /Catalog >>"),
            (2, 0, b"<< /Type /Pages >>"),
            (3, 0, b"42"),
        ]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj1 = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        let obj2 = resolver.resolve(ObjRef::new(2, 0)).unwrap();
        let obj3 = resolver.resolve(ObjRef::new(3, 0)).unwrap();

        assert!(obj1.as_dict().is_some());
        assert!(obj2.as_dict().is_some());
        assert_eq!(obj3, PdfObject::Integer(42));
    }

    // -- Error cases --

    #[test]
    fn test_resolve_missing_object() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let err = resolver.resolve(ObjRef::new(99, 0)).unwrap_err();
        assert!(err.to_string().contains("not found in xref"));
    }

    #[test]
    fn test_resolve_free_object() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        // Object 0 is free
        let err = resolver.resolve(ObjRef::new(0, 65535)).unwrap_err();
        assert!(err.to_string().contains("free"));
    }

    #[test]
    fn test_resolve_offset_beyond_eof() {
        let data = b"%PDF-1.4\n".to_vec();
        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            1,
            XrefEntry::Uncompressed {
                offset: 99999,
                gen: 0,
            },
        );
        let mut resolver = ObjectResolver::new(&data, xref);

        let err = resolver.resolve(ObjRef::new(1, 0)).unwrap_err();
        assert!(err.to_string().contains("beyond end of file"));
    }

    #[test]
    fn test_resolve_malformed_object_header() {
        // Put garbage where an object definition should be
        let mut data = b"%PDF-1.4\n".to_vec();
        let offset = data.len() as u64;
        data.extend_from_slice(b"not an object definition");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(1, XrefEntry::Uncompressed { offset, gen: 0 });

        let mut resolver = ObjectResolver::new(&data, xref);
        assert!(resolver.resolve(ObjRef::new(1, 0)).is_err());
    }

    #[test]
    fn test_resolve_compressed_missing_container_errors() {
        // Compressed entry pointing to non-existent stream object
        let data = b"%PDF-1.5\n".to_vec();
        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            1,
            XrefEntry::Compressed {
                stream_obj: 10,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(1, 0)).unwrap_err();
        assert!(err.to_string().contains("not found in xref"));
    }

    // -- Cycle detection --

    #[test]
    fn test_circular_reference_detected() {
        // Object 1 contains a reference to object 2, object 2 references object 1.
        // Direct cycle detection happens at the resolver level: if we're already
        // resolving obj 1 and encounter it again, we detect the cycle.
        //
        // However, parse_object just returns PdfObject::Reference without resolving.
        // Cycles are detected when the caller explicitly resolves reference chains.
        // We test the resolver's own cycle detection here.
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        // Manually simulate: start resolving obj 1, then try to resolve it again
        resolver.resolving.insert(ObjRef::new(1, 0));
        let err = resolver.resolve(ObjRef::new(1, 0)).unwrap_err();
        assert!(err.to_string().contains("circular reference"));
        resolver.resolving.remove(&ObjRef::new(1, 0));
    }

    #[test]
    fn test_resolve_after_cycle_error_works() {
        // After a cycle error, the resolver should still work for other objects
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42"), (2, 0, b"99")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        // Force a cycle error on object 1
        resolver.resolving.insert(ObjRef::new(1, 0));
        assert!(resolver.resolve(ObjRef::new(1, 0)).is_err());
        resolver.resolving.remove(&ObjRef::new(1, 0));

        // Object 2 should still resolve fine
        let obj = resolver.resolve(ObjRef::new(2, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(99));

        // Object 1 should also work now that the resolving set is clear
        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));
    }

    #[test]
    fn test_cache_hit() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        // First resolve: cache miss
        let obj1 = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 1);

        // Second resolve: cache hit (returns same value)
        let obj2 = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj1, obj2);
        assert_eq!(resolver.cache_len(), 1); // no new entry
    }

    #[test]
    fn test_cache_lru_eviction() {
        let objects: Vec<(u32, u16, &[u8])> = vec![
            (1, 0, b"10" as &[u8]),
            (2, 0, b"20"),
            (3, 0, b"30"),
            (4, 0, b"40"),
            (5, 0, b"50"),
        ];
        let (data, xref) = build_pdf_with_objects(&objects);

        let mut resolver = ObjectResolver::new(&data, xref);
        resolver.set_cache_max(3);

        // Resolve objects 1, 2, 3 -- cache is now [1, 2, 3]
        for n in 1..=3 {
            resolver.resolve(ObjRef::new(n, 0)).unwrap();
        }
        assert_eq!(resolver.cache_len(), 3);

        // Resolve object 4 -- should evict object 1 (LRU)
        resolver.resolve(ObjRef::new(4, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 3);
        assert!(!resolver.cache.contains_key(&ObjRef::new(1, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(2, 0)));

        // Access object 2 to make it most recently used
        resolver.resolve(ObjRef::new(2, 0)).unwrap();

        // Resolve object 5 -- should evict object 3 (now the LRU), not 2
        resolver.resolve(ObjRef::new(5, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 3);
        assert!(!resolver.cache.contains_key(&ObjRef::new(3, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(2, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(4, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(5, 0)));
    }

    #[test]
    fn test_clear_cache() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 1);
        assert!(!resolver.eviction_heap.is_empty());

        resolver.clear_cache();
        assert_eq!(resolver.cache_len(), 0);
        assert!(resolver.eviction_heap.is_empty());
        assert_eq!(resolver.generation, 0);
    }

    #[test]
    fn test_clear_cache_then_refill_evicts_correctly() {
        // Regression: clear_cache must reset the eviction heap, otherwise
        // stale heap entries from before the clear could collide with new
        // generation counters after the reset and evict wrong entries.
        let objects: Vec<(u32, u16, &[u8])> = vec![
            (1, 0, b"10" as &[u8]),
            (2, 0, b"20"),
            (3, 0, b"30"),
            (4, 0, b"40"),
        ];
        let (data, xref) = build_pdf_with_objects(&objects);

        let mut resolver = ObjectResolver::new(&data, xref);
        resolver.set_cache_max(2);

        // Fill cache with obj 1 and 2
        resolver.resolve(ObjRef::new(1, 0)).unwrap();
        resolver.resolve(ObjRef::new(2, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 2);

        // Clear and refill with obj 3 and 4
        resolver.clear_cache();
        resolver.resolve(ObjRef::new(3, 0)).unwrap();
        resolver.resolve(ObjRef::new(4, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 2);

        // Re-access obj 4 to make it MRU
        resolver.resolve(ObjRef::new(4, 0)).unwrap();

        // Resolve obj 1 -- should evict obj 3 (LRU), not obj 4
        resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 2);
        assert!(resolver.cache.contains_key(&ObjRef::new(4, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(1, 0)));
        assert!(!resolver.cache.contains_key(&ObjRef::new(3, 0)));
    }

    #[test]
    fn test_eviction_heap_compaction() {
        // With cache_max=2, the heap compaction threshold is 2*4=8.
        // Repeatedly resolving cached objects should trigger compaction
        // and keep the heap bounded.
        let objects: Vec<(u32, u16, &[u8])> = vec![(1, 0, b"42"), (2, 0, b"99")];
        let (data, xref) = build_pdf_with_objects(&objects);
        let mut resolver = ObjectResolver::new(&data, xref);
        resolver.set_cache_max(2);

        // Fill cache
        resolver.resolve(ObjRef::new(1, 0)).unwrap();
        resolver.resolve(ObjRef::new(2, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 2);

        // Hit the cache many times to grow the heap with stale entries
        for _ in 0..20 {
            resolver.resolve(ObjRef::new(1, 0)).unwrap();
            resolver.resolve(ObjRef::new(2, 0)).unwrap();
        }

        // Heap should have been compacted (threshold = 8), so it should
        // be close to cache size, not 40+
        assert!(
            resolver.eviction_heap.len() <= 8,
            "eviction heap should be compacted, got {} entries",
            resolver.eviction_heap.len()
        );
    }

    // -- resolve_if_ref --

    #[test]
    fn test_resolve_if_ref_with_reference() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = PdfObject::Reference(ObjRef::new(1, 0));
        let resolved = resolver.resolve_if_ref(obj).unwrap();
        assert_eq!(resolved, PdfObject::Integer(42));
    }

    #[test]
    fn test_resolve_if_ref_with_direct_object() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = PdfObject::Integer(42);
        let resolved = resolver.resolve_if_ref(obj).unwrap();
        assert_eq!(resolved, PdfObject::Integer(42));
    }

    // -- Generation number handling --

    #[test]
    fn test_generation_mismatch_warns() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        // Request gen 5, but xref has gen 0
        let obj = resolver.resolve(ObjRef::new(1, 5)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));

        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.message.contains("generation mismatch")));
    }

    // -- Integration: full document parse then resolve --

    #[test]
    fn test_integration_parse_and_resolve() {
        // Build a complete PDF and resolve objects through the full pipeline
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let obj2_offset = data.len();
        data.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 3\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             {obj2_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 3 /Root 1 0 R >>\n"
        );
        data.extend_from_slice(xref.as_bytes());
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        // Parse document structure
        let doc_parser = DocumentParser::new(&data);
        let doc = doc_parser.parse().unwrap();

        // Build resolver from document structure
        let mut resolver = ObjectResolver::from_document(&data, doc);

        // Resolve the catalog
        let catalog = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        let catalog_dict = catalog.as_dict().expect("catalog should be a dict");
        assert_eq!(
            catalog_dict.get(b"Type"),
            Some(&PdfObject::Name(b"Catalog".to_vec()))
        );

        // Follow the /Pages reference
        let pages_ref = catalog_dict
            .get_ref(b"Pages")
            .expect("should have /Pages ref");
        let pages_dict = resolver.resolve_dict(pages_ref).unwrap();
        assert_eq!(
            pages_dict.get(b"Type"),
            Some(&PdfObject::Name(b"Pages".to_vec()))
        );

        // Both should be cached now
        assert_eq!(resolver.cache_len(), 2);

        // Trailer and version should be accessible
        let trailer = resolver.trailer().expect("trailer should be set");
        assert_eq!(
            trailer.get(b"Root"),
            Some(&PdfObject::Reference(ObjRef::new(1, 0)))
        );
        assert_eq!(trailer.get(b"Size"), Some(&PdfObject::Integer(3)));

        let version = resolver.version().expect("version should be set");
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 4);
    }

    #[test]
    fn test_new_resolver_has_no_trailer_or_version() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let resolver = ObjectResolver::new(&data, xref);
        assert!(resolver.trailer().is_none());
        assert!(resolver.version().is_none());
    }

    // -- XrefTable access --

    #[test]
    fn test_xref_accessor() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let resolver = ObjectResolver::new(&data, xref);
        assert!(resolver.xref().get(1).is_some());
        assert!(resolver.xref().get(99).is_none());
    }

    // -- endobj validation --

    #[test]
    fn test_missing_endobj_warns() {
        // Object body without trailing endobj
        let mut data = b"%PDF-1.4\n".to_vec();
        let offset = data.len() as u64;
        data.extend_from_slice(b"1 0 obj\n42\n");
        // No endobj here, just EOF

        let mut xref = XrefTable::new();
        xref.insert_if_absent(1, XrefEntry::Uncompressed { offset, gen: 0 });

        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        // Should still resolve successfully
        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));

        // No warning for EOF (missing endobj at EOF is benign)
        let warnings = diag.warnings();
        assert!(
            !warnings.iter().any(|w| w.message.contains("endobj")),
            "EOF after object body should not warn"
        );
    }

    #[test]
    fn test_well_formed_object_no_spurious_warnings() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert!(
            diag.warnings().is_empty(),
            "well-formed object should emit no warnings"
        );
    }

    // -- Cast safety --

    #[test]
    fn test_negative_obj_num_in_header_warns() {
        // Object header has a negative object number, which can't match any
        // valid ObjRef (u32). The old `as u32` cast would silently wrap;
        // try_from correctly detects the mismatch.
        let mut data = b"%PDF-1.4\n".to_vec();
        let offset = data.len() as u64;
        data.extend_from_slice(b"-1 0 obj\n42\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(1, XrefEntry::Uncompressed { offset, gen: 0 });

        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        // Resolves (lenient), but warns about header mismatch
        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("object header says")),
            "negative obj_num should trigger header mismatch warning"
        );
    }

    // -- Realistic cycle detection --

    #[test]
    fn test_follow_reference_chain() {
        // Object 1 -> ref to object 2 -> integer 99.
        // resolve_if_ref should follow the chain.
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"2 0 R"), (2, 0, b"99")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj1 = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj1, PdfObject::Reference(ObjRef::new(2, 0)));

        // Follow the reference
        let resolved = resolver.resolve_if_ref(obj1).unwrap();
        assert_eq!(resolved, PdfObject::Integer(99));
    }

    #[test]
    fn test_mutual_references_resolve_individually() {
        // Object 1 -> ref to object 2, object 2 -> ref to object 1.
        // Each resolves individually to a Reference (no cycle in a single call).
        // Cycle detection only triggers when following a chain that revisits
        // an object already on the resolve stack.
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"2 0 R"), (2, 0, b"1 0 R")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj1 = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj1, PdfObject::Reference(ObjRef::new(2, 0)));

        let obj2 = resolver.resolve(ObjRef::new(2, 0)).unwrap();
        assert_eq!(obj2, PdfObject::Reference(ObjRef::new(1, 0)));
    }

    // -- resolve_chain --

    #[test]
    fn test_resolve_chain_follows_refs() {
        // Object 1 -> ref to 2 -> ref to 3 -> integer 42
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"2 0 R"), (2, 0, b"3 0 R"), (3, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        let resolved = resolver.resolve_chain(obj, 10).unwrap();
        assert_eq!(resolved, PdfObject::Integer(42));
    }

    #[test]
    fn test_resolve_chain_direct_object() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let resolved = resolver.resolve_chain(PdfObject::Integer(7), 10).unwrap();
        assert_eq!(resolved, PdfObject::Integer(7));
    }

    #[test]
    fn test_resolve_chain_depth_limit_exceeded() {
        // Chain: obj 1 -> obj 2 -> obj 3, with max_depth=1
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"2 0 R"), (2, 0, b"3 0 R"), (3, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        let err = resolver.resolve_chain(obj, 1).unwrap_err();
        assert!(err.to_string().contains("depth limit"));
    }

    #[test]
    fn test_resolve_chain_default_succeeds() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"2 0 R"), (2, 0, b"99")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        let resolved = resolver.resolve_chain_default(obj).unwrap();
        assert_eq!(resolved, PdfObject::Integer(99));
    }

    // -- get_and_resolve / get_resolved_dict --

    #[test]
    fn test_get_and_resolve_direct() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Count".to_vec(), PdfObject::Integer(5));

        let result = resolver.get_and_resolve(&dict, b"Count").unwrap();
        assert_eq!(result, Some(PdfObject::Integer(5)));
    }

    #[test]
    fn test_get_and_resolve_indirect() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Value".to_vec(), PdfObject::Reference(ObjRef::new(1, 0)));

        let result = resolver.get_and_resolve(&dict, b"Value").unwrap();
        assert_eq!(result, Some(PdfObject::Integer(42)));
    }

    #[test]
    fn test_get_and_resolve_missing() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let dict = PdfDictionary::new();
        let result = resolver.get_and_resolve(&dict, b"Nope").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_resolved_dict_direct() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut inner = PdfDictionary::new();
        inner.insert(b"Type".to_vec(), PdfObject::Name(b"Pages".to_vec()));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Pages".to_vec(), PdfObject::Dictionary(inner));

        let result = resolver
            .get_resolved_dict(&dict, b"Pages")
            .unwrap()
            .unwrap();
        assert_eq!(result.get_name(b"Type"), Some(b"Pages".as_slice()));
    }

    #[test]
    fn test_get_resolved_dict_indirect() {
        let (data, xref) = build_pdf_with_objects(&[(2, 0, b"<< /Type /Pages /Count 0 >>")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Pages".to_vec(), PdfObject::Reference(ObjRef::new(2, 0)));

        let result = resolver
            .get_resolved_dict(&dict, b"Pages")
            .unwrap()
            .unwrap();
        assert_eq!(result.get_name(b"Type"), Some(b"Pages".as_slice()));
    }

    #[test]
    fn test_get_resolved_dict_missing() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let dict = PdfDictionary::new();
        assert!(resolver
            .get_resolved_dict(&dict, b"Nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_get_resolved_dict_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Pages".to_vec(), PdfObject::Integer(42));

        let err = resolver.get_resolved_dict(&dict, b"Pages").unwrap_err();
        assert!(err.to_string().contains("expected dictionary"));
    }

    // -- resolve_dict / resolve_array / resolve_stream --

    #[test]
    fn test_resolve_dict_success() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"<< /Type /Catalog >>")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        let dict = resolver.resolve_dict(ObjRef::new(1, 0)).unwrap();
        assert_eq!(dict.get_name(b"Type"), Some(b"Catalog".as_slice()));
    }

    #[test]
    fn test_resolve_dict_from_stream() {
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"<< /Length 5 >> stream\nhello\nendstream")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        let dict = resolver.resolve_dict(ObjRef::new(1, 0)).unwrap();
        assert_eq!(dict.get_i64(b"Length"), Some(5));
    }

    #[test]
    fn test_resolve_dict_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve_dict(ObjRef::new(1, 0)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected dictionary"));
        assert!(msg.contains("1 0 R"));
    }

    #[test]
    fn test_resolve_array_success() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"[1 2 3]")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        let arr = resolver.resolve_array(ObjRef::new(1, 0)).unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn test_resolve_array_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve_array(ObjRef::new(1, 0)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected array"));
        assert!(msg.contains("1 0 R"));
    }

    #[test]
    fn test_resolve_stream_success() {
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"<< /Length 5 >> stream\nhello\nendstream")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        let stream = resolver.resolve_stream(ObjRef::new(1, 0)).unwrap();
        assert_eq!(stream.dict.get_i64(b"Length"), Some(5));
    }

    #[test]
    fn test_resolve_stream_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"<< /Type /Catalog >>")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve_stream(ObjRef::new(1, 0)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected stream"));
        assert!(msg.contains("1 0 R"));
    }

    // -- get_resolved_array / get_resolved_stream --

    #[test]
    fn test_get_resolved_array_direct() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(
            b"Kids".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(1), PdfObject::Integer(2)]),
        );

        let arr = resolver
            .get_resolved_array(&dict, b"Kids")
            .unwrap()
            .unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_get_resolved_array_indirect() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"[10 20 30]")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Kids".to_vec(), PdfObject::Reference(ObjRef::new(1, 0)));

        let arr = resolver
            .get_resolved_array(&dict, b"Kids")
            .unwrap()
            .unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn test_get_resolved_array_missing() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let dict = PdfDictionary::new();
        assert!(resolver
            .get_resolved_array(&dict, b"Nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_get_resolved_array_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Kids".to_vec(), PdfObject::Integer(42));

        let err = resolver.get_resolved_array(&dict, b"Kids").unwrap_err();
        assert!(err.to_string().contains("expected array"));
    }

    #[test]
    fn test_get_resolved_stream_direct() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut stream_dict = PdfDictionary::new();
        stream_dict.insert(b"Length".to_vec(), PdfObject::Integer(0));
        let stream = PdfStream {
            dict: stream_dict,
            data_offset: 0,
            data_length: 0,
        };

        let mut dict = PdfDictionary::new();
        dict.insert(b"Data".to_vec(), PdfObject::Stream(stream));

        let result = resolver
            .get_resolved_stream(&dict, b"Data")
            .unwrap()
            .unwrap();
        assert_eq!(result.dict.get_i64(b"Length"), Some(0));
    }

    #[test]
    fn test_get_resolved_stream_missing() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let dict = PdfDictionary::new();
        assert!(resolver
            .get_resolved_stream(&dict, b"Nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_get_resolved_stream_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Data".to_vec(), PdfObject::Integer(42));

        let err = resolver.get_resolved_stream(&dict, b"Data").unwrap_err();
        assert!(err.to_string().contains("expected stream"));
    }

    // -- resolve_as_dict / resolve_as_array / resolve_as_stream --

    #[test]
    fn test_resolve_as_dict_direct() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut inner = PdfDictionary::new();
        inner.insert(b"Type".to_vec(), PdfObject::Name(b"Catalog".to_vec()));

        let dict = resolver
            .resolve_as_dict(PdfObject::Dictionary(inner))
            .unwrap();
        assert_eq!(dict.get_name(b"Type"), Some(b"Catalog".as_slice()));
    }

    #[test]
    fn test_resolve_as_dict_indirect() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"<< /Type /Catalog >>")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let dict = resolver
            .resolve_as_dict(PdfObject::Reference(ObjRef::new(1, 0)))
            .unwrap();
        assert_eq!(dict.get_name(b"Type"), Some(b"Catalog".as_slice()));
    }

    #[test]
    fn test_resolve_as_dict_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let err = resolver
            .resolve_as_dict(PdfObject::Integer(42))
            .unwrap_err();
        assert!(err.to_string().contains("expected dictionary"));
    }

    #[test]
    fn test_resolve_as_array_direct() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let arr = resolver
            .resolve_as_array(PdfObject::Array(vec![PdfObject::Integer(1)]))
            .unwrap();
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn test_resolve_as_array_indirect() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"[1 2 3]")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let arr = resolver
            .resolve_as_array(PdfObject::Reference(ObjRef::new(1, 0)))
            .unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn test_resolve_as_array_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let err = resolver
            .resolve_as_array(PdfObject::Integer(42))
            .unwrap_err();
        assert!(err.to_string().contains("expected array"));
    }

    #[test]
    fn test_resolve_as_stream_direct() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut stream_dict = PdfDictionary::new();
        stream_dict.insert(b"Length".to_vec(), PdfObject::Integer(0));
        let stream = PdfStream {
            dict: stream_dict,
            data_offset: 0,
            data_length: 0,
        };

        let result = resolver
            .resolve_as_stream(PdfObject::Stream(stream))
            .unwrap();
        assert_eq!(result.dict.get_i64(b"Length"), Some(0));
    }

    #[test]
    fn test_resolve_as_stream_indirect() {
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"<< /Length 5 >> stream\nhello\nendstream")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let stream = resolver
            .resolve_as_stream(PdfObject::Reference(ObjRef::new(1, 0)))
            .unwrap();
        assert_eq!(stream.dict.get_i64(b"Length"), Some(5));
    }

    #[test]
    fn test_resolve_as_stream_wrong_type() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let err = resolver
            .resolve_as_stream(PdfObject::Integer(42))
            .unwrap_err();
        assert!(err.to_string().contains("expected stream"));
    }

    // -- from_document_with_diagnostics --

    #[test]
    fn test_from_document_with_diagnostics() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let obj2_offset = data.len();
        data.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 3\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             {obj2_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 3 /Root 1 0 R >>\n"
        );
        data.extend_from_slice(xref.as_bytes());
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let doc_parser = DocumentParser::new(&data);
        let doc = doc_parser.parse().unwrap();

        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag.clone());

        // Should have trailer and version
        assert!(resolver.trailer().is_some());
        assert_eq!(resolver.version().unwrap().major, 1);

        // Resolve with warnings going to our sink
        let catalog = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert!(catalog.as_dict().is_some());
    }

    // -- decode_stream_data --

    #[test]
    fn test_decode_stream_data_offset_beyond_eof_no_spurious_warnings() {
        let data = b"%PDF-1.4\nsome content here".to_vec();
        let xref = XrefTable::new();
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: 99999,
            data_length: 10,
        };

        let err = resolver.decode_stream_data(&stream, None).unwrap_err();
        assert!(err.to_string().contains("beyond EOF"));
        // No "clamping" warning should fire for an invalid offset
        assert!(
            diag.warnings().is_empty(),
            "offset-beyond-EOF should produce error with no warnings, got: {:?}",
            diag.warnings()
        );
    }

    #[test]
    fn test_decode_stream_data_decodes_raw() {
        // Embed raw (unfiltered) stream data in the resolver's source
        let mut data = b"%PDF-1.4\n".to_vec();
        let stream_offset = data.len() as u64;
        let stream_content = b"Hello, stream!";
        data.extend_from_slice(stream_content);

        let xref = XrefTable::new();
        let mut resolver = ObjectResolver::new(&data, xref);

        let stream = PdfStream {
            dict: PdfDictionary::new(), // no /Filter = raw passthrough
            data_offset: stream_offset,
            data_length: stream_content.len() as u64,
        };

        let decoded = resolver.decode_stream_data(&stream, None).unwrap();
        assert_eq!(decoded, stream_content);
    }

    #[test]
    fn test_decode_stream_data_with_flate_filter() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"compressed stream content";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut data = b"%PDF-1.4\n".to_vec();
        let stream_offset = data.len() as u64;
        data.extend_from_slice(&compressed);

        let xref = XrefTable::new();
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));

        let stream = PdfStream {
            dict,
            data_offset: stream_offset,
            data_length: compressed.len() as u64,
        };

        let decoded = resolver.decode_stream_data(&stream, None).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_decode_stream_data_clamps_past_eof() {
        let mut data = b"%PDF-1.4\n".to_vec();
        let stream_offset = data.len() as u64;
        data.extend_from_slice(b"short");

        let xref = XrefTable::new();
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: stream_offset,
            data_length: 1000, // way past EOF
        };

        let decoded = resolver.decode_stream_data(&stream, None).unwrap();
        assert_eq!(decoded, b"short");
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("clamping")));
    }

    // -- ObjStm resolution --

    /// Build a synthetic PDF containing an ObjStm with the given objects.
    ///
    /// `stream_obj_num`: object number for the ObjStm stream itself.
    /// `objects`: list of (obj_num, body_bytes) for objects inside the ObjStm.
    ///
    /// Returns (raw_data, XrefTable) with:
    /// - Uncompressed entry for the ObjStm container
    /// - Compressed entries for each object inside it
    fn build_pdf_with_objstm(
        stream_obj_num: u32,
        objects: &[(u32, &[u8])],
    ) -> (Vec<u8>, XrefTable) {
        // Build the decoded ObjStm content: header + object data
        let n = objects.len();

        // First pass: compute offsets relative to start of object data section
        let mut obj_data = Vec::new();
        let mut header_entries: Vec<(u32, usize)> = Vec::new();
        for &(obj_num, body) in objects {
            header_entries.push((obj_num, obj_data.len()));
            obj_data.extend_from_slice(body);
            obj_data.push(b' '); // separator between objects
        }

        // Build header string: "obj_num offset obj_num offset ..."
        let header: String = header_entries
            .iter()
            .map(|(num, off)| format!("{num} {off} "))
            .collect();
        let first = header.len();

        // Combine header + object data = decoded stream content
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header.as_bytes());
        decoded.extend_from_slice(&obj_data);

        // Build the PDF file with the ObjStm as an uncompressed stream
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let stream_offset = data.len() as u64;
        // Write: <stream_obj_num> 0 obj\n<< ... >> stream\n<decoded>\nendstream\nendobj\n
        data.extend_from_slice(format!("{stream_obj_num} 0 obj\n").as_bytes());
        data.extend_from_slice(
            format!(
                "<< /Type /ObjStm /N {n} /First {first} /Length {} >>\n",
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(b"stream\n");
        data.extend_from_slice(&decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        // Build xref table
        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            0,
            XrefEntry::Free {
                next_free: 0,
                gen: 65535,
            },
        );
        xref.insert_if_absent(
            stream_obj_num,
            XrefEntry::Uncompressed {
                offset: stream_offset,
                gen: 0,
            },
        );
        for (i, &(obj_num, _)) in objects.iter().enumerate() {
            xref.insert_if_absent(
                obj_num,
                XrefEntry::Compressed {
                    stream_obj: stream_obj_num,
                    index: i as u32,
                },
            );
        }

        (data, xref)
    }

    #[test]
    fn test_objstm_basic_resolution() {
        // ObjStm containing: obj 10 = (Hello), obj 11 = << /X 1 >>
        let (data, xref) = build_pdf_with_objstm(5, &[(10, b"(Hello)"), (11, b"<< /X 1 >>")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj10 = resolver.resolve(ObjRef::new(10, 0)).unwrap();
        let s = obj10.as_pdf_string().expect("expected string");
        assert_eq!(s.as_bytes(), b"Hello");

        let obj11 = resolver.resolve(ObjRef::new(11, 0)).unwrap();
        let dict = obj11.as_dict().expect("expected dictionary");
        assert_eq!(dict.get_i64(b"X"), Some(1));
    }

    #[test]
    fn test_objstm_all_cached() {
        let (data, xref) = build_pdf_with_objstm(5, &[(10, b"(Hello)"), (11, b"<< /X 1 >>")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        // Resolve obj 10 -- should also cache obj 11
        resolver.resolve(ObjRef::new(10, 0)).unwrap();

        // ObjStm container (5) is cached + both objects inside (10, 11)
        assert!(resolver.cache.contains_key(&ObjRef::new(5, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(10, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(11, 0)));
    }

    #[test]
    fn test_objstm_nested_prohibited() {
        // stream_obj 5's xref entry is Compressed (nested ObjStm)
        let data = b"%PDF-1.5\n".to_vec();
        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            5,
            XrefEntry::Compressed {
                stream_obj: 99,
                index: 0,
            },
        );
        xref.insert_if_absent(
            1,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(1, 0)).unwrap_err();
        assert!(err.to_string().contains("nested ObjStm prohibited"));
    }

    #[test]
    fn test_objstm_missing_type_errors() {
        // Build a stream without /Type /ObjStm
        let decoded = b"10 0 (Hello) ";
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let stream_offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /N 1 /First 4 /Length {} >>\nstream\n",
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            5,
            XrefEntry::Uncompressed {
                offset: stream_offset,
                gen: 0,
            },
        );
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("missing required /Type"));
    }

    #[test]
    fn test_objstm_bad_n_errors() {
        // /N = 0 is out of valid range
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let stream_offset = data.len() as u64;
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );

        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            5,
            XrefEntry::Uncompressed {
                offset: stream_offset,
                gen: 0,
            },
        );
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("out of valid range"));
    }

    #[test]
    fn test_objstm_missing_first_errors() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let stream_offset = data.len() as u64;
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /ObjStm /N 1 /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );

        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            5,
            XrefEntry::Uncompressed {
                offset: stream_offset,
                gen: 0,
            },
        );
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("missing required /First"));
    }

    #[test]
    fn test_resolve_depth_limit() {
        // Artificially set resolve_depth to the limit and verify the next
        // resolve() call triggers the depth error. The check is >= before
        // incrementing, so depth == limit is already too deep.
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        resolver.resolve_depth = DEFAULT_MAX_CHAIN_DEPTH;
        let err = resolver.resolve(ObjRef::new(1, 0)).unwrap_err();
        assert!(
            matches!(
                err,
                Error::ResourceLimit(ResourceLimitError {
                    limit: Limit::RecursionDepth(64),
                    ..
                })
            ),
            "expected RecursionDepth(64), got: {err}"
        );

        // Depth should not leak after error (check fires before increment)
        assert_eq!(resolver.resolve_depth, DEFAULT_MAX_CHAIN_DEPTH);
    }

    // -- Cache with max = 0 (disabled) --

    #[test]
    fn test_cache_max_zero_disables_caching() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        resolver.set_cache_max(0);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));
        // Nothing cached when cache_max is 0
        assert_eq!(resolver.cache_len(), 0);
    }

    // -- Cache eviction: stale heap entries --

    #[test]
    fn test_cache_eviction_skips_stale_entries() {
        // When an object is re-accessed, its generation is bumped but the old
        // heap entry remains. Eviction must skip stale entries.
        let objects: Vec<(u32, u16, &[u8])> =
            vec![(1, 0, b"10" as &[u8]), (2, 0, b"20"), (3, 0, b"30")];
        let (data, xref) = build_pdf_with_objects(&objects);

        let mut resolver = ObjectResolver::new(&data, xref);
        resolver.set_cache_max(2);

        // Resolve obj 1 and 2 -- cache full at [1, 2]
        resolver.resolve(ObjRef::new(1, 0)).unwrap();
        resolver.resolve(ObjRef::new(2, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 2);

        // Re-access obj 1, making it MRU (creates a stale heap entry for old gen)
        resolver.resolve(ObjRef::new(1, 0)).unwrap();

        // Resolve obj 3 -- must evict obj 2 (LRU), not obj 1 (despite old heap entry)
        resolver.resolve(ObjRef::new(3, 0)).unwrap();
        assert_eq!(resolver.cache_len(), 2);
        assert!(resolver.cache.contains_key(&ObjRef::new(1, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(3, 0)));
        assert!(!resolver.cache.contains_key(&ObjRef::new(2, 0)));
    }

    // -- Cache eviction: empty heap but cache full (degenerate case) --

    #[test]
    fn test_cache_eviction_breaks_on_empty_heap() {
        // Manually populate the cache without pushing heap entries,
        // then verify cache_object doesn't loop forever.
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);
        resolver.set_cache_max(1);

        // Directly insert into cache without heap entry
        resolver
            .cache
            .insert(ObjRef::new(99, 0), (PdfObject::Null, 0));
        assert_eq!(resolver.cache_len(), 1);

        // Resolving obj 1 should attempt eviction, find empty heap, break,
        // and still insert (cache may grow to 2 briefly).
        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));
    }

    // -- ObjStm: container is not a stream --

    #[test]
    fn test_objstm_container_not_stream_errors() {
        // Container obj 5 is a dictionary, not a stream
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(b"5 0 obj\n<< /Type /ObjStm >>\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("expected stream"));
    }

    // -- ObjStm: wrong /Type --

    #[test]
    fn test_objstm_wrong_type_errors() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /XRef /N 1 /First 4 /Length 5 >>\nstream\n10 0 \nendstream\nendobj\n",
        );

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("expected /ObjStm"));
    }

    // -- ObjStm: missing /N --

    #[test]
    fn test_objstm_missing_n_errors() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /ObjStm /First 4 /Length 5 >>\nstream\n10 0 \nendstream\nendobj\n",
        );

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("missing required /N"));
    }

    // -- ObjStm: negative /First --

    #[test]
    fn test_objstm_negative_first_errors() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /ObjStm /N 1 /First -1 /Length 5 >>\nstream\n10 0 \nendstream\nendobj\n",
        );

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("negative"));
    }

    // -- ObjStm: /First exceeds decoded data --

    #[test]
    fn test_objstm_first_exceeds_decoded_errors() {
        // /First = 9999, but decoded data is tiny
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /ObjStm /N 1 /First 9999 /Length 5 >>\nstream\n10 0 \nendstream\nendobj\n",
        );

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("exceeds decoded data length"));
    }

    // -- ObjStm: header entry with bad object number type --

    #[test]
    fn test_objstm_header_bad_obj_num_type_errors() {
        // Header has a name instead of an integer for the object number
        let decoded = b"(bad) 0 42 ";
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {} /Length {} >>\nstream\n",
                decoded.len(),
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("expected object number"));
    }

    // -- ObjStm: header entry with bad offset type --

    #[test]
    fn test_objstm_header_bad_offset_type_errors() {
        // Header has int for obj_num but name for offset
        let decoded = b"10 /bad 42 ";
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {} /Length {} >>\nstream\n",
                decoded.len(),
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("expected offset"));
    }

    // -- ObjStm: header offset exceeds decoded data --

    #[test]
    fn test_objstm_header_offset_exceeds_data_errors() {
        // Header claims offset 9999, but the data is short
        let header = b"10 9999 ";
        let first = header.len();
        let body = b"42 ";
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header);
        decoded.extend_from_slice(body);

        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("exceeds decoded data length"));
    }

    // -- ObjStm: xref index mismatch warning --

    #[test]
    fn test_objstm_xref_index_mismatch_warns() {
        // ObjStm has objects [10, 11], but xref index=0 points to obj 10
        // while we request obj 11 at index 0 (mismatch)
        let (data, xref) = build_pdf_with_objstm(5, &[(10, b"(Hello)"), (11, b"(World)")]);

        // Override obj 11's xref entry to have index=0 (points to obj 10 in header)
        // Remove and re-insert with wrong index
        let mut new_xref = XrefTable::new();
        new_xref.insert_if_absent(
            0,
            XrefEntry::Free {
                next_free: 0,
                gen: 65535,
            },
        );
        // Copy the ObjStm container entry
        if let Some(entry) = xref.get(5) {
            new_xref.insert_if_absent(5, *entry);
        }
        // Obj 10 at correct index 0
        new_xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );
        // Obj 11 with WRONG index 0 (should be 1)
        new_xref.insert_if_absent(
            11,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, new_xref, diag.clone());

        // Resolve obj 11 with wrong index -- index 0 maps to obj 10, not 11
        let obj = resolver.resolve(ObjRef::new(11, 0)).unwrap();
        let s = obj.as_pdf_string().expect("expected string");
        assert_eq!(s.as_bytes(), b"World");

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("xref index") && w.message.contains("expected obj")),
            "expected xref index mismatch warning, got: {:?}",
            warnings
        );
    }

    // -- ObjStm: parse error for non-target object emits warning --

    #[test]
    fn test_objstm_parse_error_non_target_warns() {
        // ObjStm with obj 10 = valid, obj 11 = invalid (empty body = EOF error).
        // The second object's data points past all actual content.
        let obj10_body = b"(Hello)";
        let header = format!("10 0 11 {} ", obj10_body.len());
        let first = header.len();
        // Body section: valid first obj, nothing after (obj 11's offset -> EOF)
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header.as_bytes());
        decoded.extend_from_slice(obj10_body);

        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 2 /First {first} /Length {} >>\nstream\n",
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );
        xref.insert_if_absent(
            11,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 1,
            },
        );

        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        // Resolve obj 10 (valid) -- obj 11 parse error should warn, not fail
        let obj = resolver.resolve(ObjRef::new(10, 0)).unwrap();
        let s = obj.as_pdf_string().expect("expected string");
        assert_eq!(s.as_bytes(), b"Hello");

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("parse error")),
            "expected parse error warning for non-target object, got: {:?}",
            warnings
        );
    }

    // -- ObjStm: parse error for target object is fatal --

    #[test]
    fn test_objstm_parse_error_target_object_errors() {
        // ObjStm with obj 10 = empty body (EOF = parse error).
        // The header says offset 0, but the body section is empty.
        let header = b"10 0 ";
        let first = header.len();
        // No body data at all: offset 0 in a zero-length body -> EOF
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header);

        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("parsing target object"));
    }

    // -- ObjStm: object not found after parsing --

    #[test]
    fn test_objstm_target_not_found_errors() {
        // ObjStm claims to contain obj 10, but xref says obj 20 is at index 0
        // After parsing, obj 20 won't be in the ObjStm's header
        let header = b"10 0 ";
        let first = header.len();
        let body = b"42 ";
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header);
        decoded.extend_from_slice(body);

        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        // Obj 20 claims to be in ObjStm 5, but ObjStm only has obj 10
        xref.insert_if_absent(
            20,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(20, 0)).unwrap_err();
        assert!(err.to_string().contains("not found in ObjStm"));
    }

    // -- parse_object_at: bad generation token --

    #[test]
    fn test_parse_object_bad_gen_token_errors() {
        let mut data = b"%PDF-1.4\n".to_vec();
        let offset = data.len() as u64;
        // Object number is fine, but gen is a name instead of integer
        data.extend_from_slice(b"1 /bad obj\n42\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(1, XrefEntry::Uncompressed { offset, gen: 0 });

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(1, 0)).unwrap_err();
        assert!(err.to_string().contains("generation number"));
    }

    // -- parse_object_at: missing 'obj' keyword --

    #[test]
    fn test_parse_object_missing_obj_keyword_errors() {
        let mut data = b"%PDF-1.4\n".to_vec();
        let offset = data.len() as u64;
        // Two integers but then a name instead of 'obj'
        data.extend_from_slice(b"1 0 stream\n42\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(1, XrefEntry::Uncompressed { offset, gen: 0 });

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(1, 0)).unwrap_err();
        assert!(err.to_string().contains("obj"));
    }

    // -- parse_object_at: trailing junk warns about missing endobj --

    #[test]
    fn test_parse_object_trailing_junk_warns() {
        let mut data = b"%PDF-1.4\n".to_vec();
        let offset = data.len() as u64;
        // Object body followed by junk instead of endobj
        data.extend_from_slice(b"1 0 obj\n42\n/SomeKey\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(1, XrefEntry::Uncompressed { offset, gen: 0 });

        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("expected 'endobj'")),
            "trailing junk should warn about missing endobj, got: {:?}",
            warnings
        );
    }

    // -- parse_object_at: object header mismatch (obj_num != expected) --

    #[test]
    fn test_parse_object_header_obj_num_mismatch_warns() {
        let mut data = b"%PDF-1.4\n".to_vec();
        let offset = data.len() as u64;
        // Object header says "99 0 obj" but xref says this is object 1
        data.extend_from_slice(b"99 0 obj\n42\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(1, XrefEntry::Uncompressed { offset, gen: 0 });

        let diag = Arc::new(CollectingDiagnostics::new());
        let mut resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(42));

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("object header says")),
            "obj_num mismatch should trigger warning"
        );
    }

    // -- get_resolved_dict: accepts stream and returns dict --

    #[test]
    fn test_get_resolved_dict_stream_returns_dict() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut stream_dict = PdfDictionary::new();
        stream_dict.insert(b"Type".to_vec(), PdfObject::Name(b"XRef".to_vec()));
        let stream = PdfStream {
            dict: stream_dict,
            data_offset: 0,
            data_length: 0,
        };

        let mut dict = PdfDictionary::new();
        dict.insert(b"XRef".to_vec(), PdfObject::Stream(stream));

        let result = resolver.get_resolved_dict(&dict, b"XRef").unwrap().unwrap();
        assert_eq!(result.get_name(b"Type"), Some(b"XRef".as_slice()));
    }

    // -- get_resolved_stream: indirect reference --

    #[test]
    fn test_get_resolved_stream_indirect() {
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"<< /Length 5 >> stream\nhello\nendstream")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Data".to_vec(), PdfObject::Reference(ObjRef::new(1, 0)));

        let result = resolver
            .get_resolved_stream(&dict, b"Data")
            .unwrap()
            .unwrap();
        assert_eq!(result.dict.get_i64(b"Length"), Some(5));
    }

    // -- set_decode_limits --

    #[test]
    fn test_set_decode_limits() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let limits = DecodeLimits {
            max_decompressed_size: 1024,
            ..DecodeLimits::default()
        };
        resolver.set_decode_limits(limits);

        // Verify the limits are stored (indirectly by using them)
        // Build a raw stream that would exceed 1024 bytes after "decoding"
        // (no filter, just raw pass-through)
        let mut stream_data = b"%PDF-1.4\n".to_vec();
        let start = stream_data.len() as u64;
        let big_content = vec![b'A'; 2048];
        stream_data.extend_from_slice(&big_content);

        let mut resolver2 = ObjectResolver::new(&stream_data, XrefTable::new());
        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: start,
            data_length: 2048,
        };
        // Without filters, raw passthrough doesn't check decode limits,
        // so just verify the setter doesn't panic.
        let decoded = resolver2.decode_stream_data(&stream, None).unwrap();
        assert_eq!(decoded.len(), 2048);
    }

    // -- ObjStm: /N very large (above MAX_OBJSTM_N) --

    #[test]
    fn test_objstm_n_too_large_errors() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /ObjStm /N 200000 /First 4 /Length 5 >>\nstream\n10 0 \nendstream\nendobj\n",
        );

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("out of valid range"));
    }

    // -- ObjStm: negative /N --

    #[test]
    fn test_objstm_negative_n_errors() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /ObjStm /N -5 /First 4 /Length 5 >>\nstream\n10 0 \nendstream\nendobj\n",
        );

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("out of valid range"));
    }

    // -- resolve_chain: exactly at depth limit with non-Reference result --

    #[test]
    fn test_resolve_chain_at_exact_depth_limit() {
        // Chain of length 2: obj1 -> obj2 -> 42, with max_depth=2
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"2 0 R"), (2, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        // max_depth=2: hop 1 resolves ref to obj2, hop 2 resolves to 42 (not a ref, returns)
        let resolved = resolver.resolve_chain(obj, 2).unwrap();
        assert_eq!(resolved, PdfObject::Integer(42));
    }

    // -- resolve_chain: at depth limit and still a Reference --

    #[test]
    fn test_resolve_chain_resolves_to_non_ref_at_boundary() {
        // Chain: obj1 -> obj2 -> obj3, max_depth=2
        // After 2 hops: resolve obj1's ref -> obj2's ref = PdfObject::Reference(3,0)
        // Still a reference after max_depth hops -> error
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"2 0 R"), (2, 0, b"3 0 R"), (3, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        // After 2 hops we're at obj3 = 42 (not a ref), so it should succeed
        let resolved = resolver.resolve_chain(obj, 2).unwrap();
        assert_eq!(resolved, PdfObject::Integer(42));
    }

    // -- resolve_as_dict: stream returns dict --

    #[test]
    fn test_resolve_as_dict_from_stream() {
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"<< /Length 5 >> stream\nhello\nendstream")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let dict = resolver
            .resolve_as_dict(PdfObject::Reference(ObjRef::new(1, 0)))
            .unwrap();
        assert_eq!(dict.get_i64(b"Length"), Some(5));
    }

    // -- Multiple resolutions with eviction and re-parse --

    #[test]
    fn test_cache_eviction_and_reparse() {
        let objects: Vec<(u32, u16, &[u8])> =
            vec![(1, 0, b"10" as &[u8]), (2, 0, b"20"), (3, 0, b"30")];
        let (data, xref) = build_pdf_with_objects(&objects);

        let mut resolver = ObjectResolver::new(&data, xref);
        resolver.set_cache_max(2);

        // Fill cache with 1 and 2
        resolver.resolve(ObjRef::new(1, 0)).unwrap();
        resolver.resolve(ObjRef::new(2, 0)).unwrap();

        // Evict 1 by resolving 3
        resolver.resolve(ObjRef::new(3, 0)).unwrap();
        assert!(!resolver.cache.contains_key(&ObjRef::new(1, 0)));

        // Re-resolve 1 (cache miss, must re-parse)
        let obj = resolver.resolve(ObjRef::new(1, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(10));
        assert!(resolver.cache.contains_key(&ObjRef::new(1, 0)));
    }

    // -- decode_stream_data: stream with data_offset at exact EOF boundary --

    #[test]
    fn test_decode_stream_data_offset_at_exact_eof() {
        // data_offset == data.len() means it's at EOF (empty read area)
        let data = b"%PDF-1.4\n1234567890".to_vec();
        let xref = XrefTable::new();
        let mut resolver = ObjectResolver::new(&data, xref);

        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: data.len() as u64, // exactly at EOF
            data_length: 5,
        };

        let err = resolver.decode_stream_data(&stream, None).unwrap_err();
        assert!(err.to_string().contains("beyond EOF"));
    }

    // -- ObjStm: header parse error for object number --

    #[test]
    fn test_objstm_header_parse_error_obj_num() {
        // Header that causes a parse error when reading the object number
        // An unterminated string literal will cause a parse error
        let decoded = b"(unterminated";
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {} /Length {} >>\nstream\n",
                decoded.len(),
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        // Should hit the parse error path for reading object number
        assert!(err.to_string().contains("ObjStm"));
    }

    // -- ObjStm: header parse error for offset --

    #[test]
    fn test_objstm_header_parse_error_offset() {
        // Header has a valid obj num but then a parse error for offset
        // "10 " followed by unterminated string
        let decoded = b"10 (bad";
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {} /Length {} >>\nstream\n",
                decoded.len(),
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("ObjStm"));
    }

    // -- diagnostics accessor --

    #[test]
    fn test_diagnostics_accessor() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let diag = Arc::new(CollectingDiagnostics::new());
        let resolver = ObjectResolver::with_diagnostics(&data, xref, diag.clone());

        // Just verify the accessor doesn't panic
        let _d = resolver.diagnostics();
    }

    // -- get_resolved_dict: indirect resolves to stream -> returns stream's dict --

    #[test]
    fn test_get_resolved_dict_indirect_stream_returns_dict() {
        let (data, xref) =
            build_pdf_with_objects(&[(1, 0, b"<< /Type /XRef /Length 0 >> stream\n\nendstream")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let mut dict = PdfDictionary::new();
        dict.insert(b"XRef".to_vec(), PdfObject::Reference(ObjRef::new(1, 0)));

        let result = resolver.get_resolved_dict(&dict, b"XRef").unwrap().unwrap();
        assert_eq!(result.get_name(b"Type"), Some(b"XRef".as_slice()));
    }

    // -- resolve_as_stream: wrong type --

    #[test]
    fn test_resolve_as_stream_from_dict_errors() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"<< /Type /Catalog >>")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        let err = resolver
            .resolve_as_stream(PdfObject::Reference(ObjRef::new(1, 0)))
            .unwrap_err();
        assert!(err.to_string().contains("expected stream"));
    }

    // -- ObjStm: multiple objects, resolve second one directly --

    #[test]
    fn test_objstm_resolve_second_object() {
        let (data, xref) = build_pdf_with_objstm(5, &[(10, b"100"), (11, b"200"), (12, b"300")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        // Directly resolve the second object (index 1)
        let obj = resolver.resolve(ObjRef::new(11, 0)).unwrap();
        assert_eq!(obj, PdfObject::Integer(200));

        // All three should be cached
        assert!(resolver.cache.contains_key(&ObjRef::new(10, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(11, 0)));
        assert!(resolver.cache.contains_key(&ObjRef::new(12, 0)));
    }

    // -- ObjStm: resolve_from_objstm when container xref entry is missing --

    #[test]
    fn test_objstm_container_xref_missing() {
        // Container object 5 has no xref entry at all, but obj 1 is Compressed
        // pointing to it. The nested-check should pass (no entry = not compressed),
        // but resolving the container will fail with "not found in xref".
        let data = b"%PDF-1.5\n".to_vec();
        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            1,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(1, 0)).unwrap_err();
        assert!(err.to_string().contains("not found in xref"));
    }

    // -- decode_stream_data with zero length --

    #[test]
    fn test_decode_stream_data_zero_length() {
        let mut data = b"%PDF-1.4\n".to_vec();
        let stream_offset = data.len() as u64;
        data.extend_from_slice(b"some extra data");

        let xref = XrefTable::new();
        let mut resolver = ObjectResolver::new(&data, xref);

        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: stream_offset,
            data_length: 0,
        };

        let decoded = resolver.decode_stream_data(&stream, None).unwrap();
        assert!(decoded.is_empty());
    }

    // -- resolve_chain with zero max_depth on a Reference --

    #[test]
    fn test_resolve_chain_zero_depth_with_ref_errors() {
        let (data, xref) = build_pdf_with_objects(&[(1, 0, b"42")]);
        let mut resolver = ObjectResolver::new(&data, xref);

        // max_depth=0, and the object is a reference. The for loop body never
        // executes, so we exit with a reference still in hand -> error.
        let err = resolver
            .resolve_chain(PdfObject::Reference(ObjRef::new(1, 0)), 0)
            .unwrap_err();
        assert!(err.to_string().contains("depth limit"));
    }

    // -- resolve_chain with zero max_depth on a non-Reference --

    #[test]
    fn test_resolve_chain_zero_depth_with_direct_ok() {
        let (data, xref) = build_pdf_with_objects(&[]);
        let mut resolver = ObjectResolver::new(&data, xref);

        // max_depth=0 but already a concrete object -> returns it after loop
        let resolved = resolver.resolve_chain(PdfObject::Integer(7), 0).unwrap();
        assert_eq!(resolved, PdfObject::Integer(7));
    }

    // -- from_document constructor stores trailer and version --

    #[test]
    fn test_from_document_stores_trailer_and_version() {
        let mut trailer = PdfDictionary::new();
        trailer.insert(b"Size".to_vec(), PdfObject::Integer(10));

        let doc = DocumentStructure {
            version: PdfVersion { major: 2, minor: 0 },
            xref: XrefTable::new(),
            trailer,
        };

        let data = b"%PDF-2.0\n".to_vec();
        let resolver = ObjectResolver::from_document(&data, doc);

        let v = resolver.version().unwrap();
        assert_eq!(v.major, 2);
        assert_eq!(v.minor, 0);

        let t = resolver.trailer().unwrap();
        assert_eq!(t.get_i64(b"Size"), Some(10));
    }

    // -- ObjStm: header entry with negative offset --

    #[test]
    fn test_objstm_header_negative_offset_errors() {
        // Header has obj_num 10 and offset -1 (negative -> not a valid usize)
        let header = b"10 -1 ";
        let first = header.len();
        let body = b"42 ";
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header);
        decoded.extend_from_slice(body);

        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("expected offset"));
    }

    // -- ObjStm: header entry with negative object number --

    #[test]
    fn test_objstm_header_negative_obj_num_errors() {
        // Header has obj_num -1 (negative -> not a valid obj num)
        let header = b"-1 0 ";
        let first = header.len();
        let body = b"42 ";
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header);
        decoded.extend_from_slice(body);

        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let offset = data.len() as u64;
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                decoded.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&decoded);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        let mut xref = XrefTable::new();
        xref.insert_if_absent(5, XrefEntry::Uncompressed { offset, gen: 0 });
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 0,
            },
        );

        let mut resolver = ObjectResolver::new(&data, xref);
        let err = resolver.resolve(ObjRef::new(10, 0)).unwrap_err();
        assert!(err.to_string().contains("expected object number"));
    }

    // -- decoded stream cache tests --

    #[test]
    fn test_decoded_cache_hit() {
        // Verify that calling decode_stream_data twice with the same ObjRef
        // returns the same result (cache hit on second call).
        let mut data = b"%PDF-1.4\n".to_vec();
        let stream_offset = data.len() as u64;
        let content = b"cached stream content";
        data.extend_from_slice(content);

        let xref = XrefTable::new();
        let mut resolver = ObjectResolver::new(&data, xref);

        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: stream_offset,
            data_length: content.len() as u64,
        };

        let obj_ref = ObjRef::new(1, 0);

        // First call: cache miss, populates cache.
        let result1 = resolver.decode_stream_data(&stream, Some(obj_ref)).unwrap();
        assert_eq!(result1, content);

        // Second call: cache hit, returns same data.
        let result2 = resolver.decode_stream_data(&stream, Some(obj_ref)).unwrap();
        assert_eq!(result2, content);
        assert_eq!(result1, result2);
    }

    #[test]
    fn test_decoded_cache_no_obj_ref_skips_cache() {
        // When obj_ref is None, decoding should succeed but not populate cache.
        let mut data = b"%PDF-1.4\n".to_vec();
        let stream_offset = data.len() as u64;
        let content = b"no-cache stream";
        data.extend_from_slice(content);

        let xref = XrefTable::new();
        let mut resolver = ObjectResolver::new(&data, xref);

        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: stream_offset,
            data_length: content.len() as u64,
        };

        // Decode with None -- should work but not cache.
        let result = resolver.decode_stream_data(&stream, None).unwrap();
        assert_eq!(result, content);
        assert!(
            resolver.decoded_cache.is_empty(),
            "cache should be empty when obj_ref is None"
        );
    }

    #[test]
    fn test_decoded_cache_eviction() {
        // Fill the cache beyond DECODED_CACHE_MAX and verify it does not grow
        // past the limit (eviction kicks in).
        let mut data = b"%PDF-1.4\n".to_vec();

        // Create DECODED_CACHE_MAX + 5 distinct streams.
        let count = DECODED_CACHE_MAX + 5;
        let mut streams = Vec::new();
        for i in 0..count {
            let offset = data.len() as u64;
            let content = format!("stream-{i}");
            data.extend_from_slice(content.as_bytes());
            streams.push((
                ObjRef::new(i as u32 + 1, 0),
                PdfStream {
                    dict: PdfDictionary::new(),
                    data_offset: offset,
                    data_length: content.len() as u64,
                },
            ));
        }

        let xref = XrefTable::new();
        let mut resolver = ObjectResolver::new(&data, xref);

        for (obj_ref, stream) in &streams {
            resolver.decode_stream_data(stream, Some(*obj_ref)).unwrap();
        }

        // Cache should be capped at DECODED_CACHE_MAX.
        assert!(
            resolver.decoded_cache.len() <= DECODED_CACHE_MAX,
            "cache should not exceed DECODED_CACHE_MAX ({}), got {}",
            DECODED_CACHE_MAX,
            resolver.decoded_cache.len()
        );
    }

    #[test]
    fn test_decoded_cache_skips_large_entries() {
        // Streams larger than DECODED_CACHE_MAX_BYTES should not be cached.
        let mut data = b"%PDF-1.4\n".to_vec();
        let offset = data.len() as u64;
        let large_content = vec![b'X'; DECODED_CACHE_MAX_BYTES + 1];
        data.extend_from_slice(&large_content);

        let xref = XrefTable::new();
        let mut resolver = ObjectResolver::new(&data, xref);

        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: offset,
            data_length: large_content.len() as u64,
        };

        let obj_ref = ObjRef::new(1, 0);
        let result = resolver.decode_stream_data(&stream, Some(obj_ref)).unwrap();
        assert_eq!(result.len(), large_content.len());

        // Large entry should not have been cached.
        assert!(
            resolver.decoded_cache.is_empty(),
            "streams exceeding DECODED_CACHE_MAX_BYTES should not be cached"
        );
    }
}
