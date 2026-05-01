//! Internal backend trait for format-agnostic dispatch.
//!
//! The [`InternalBackend`] trait is the facade's internal dispatch mechanism.
//! It replaces the old `BackendInner` enum with object-safe dynamic dispatch
//! via `Box<dyn InternalBackend>`. Each format backend provides an adapter
//! struct implementing this trait (see `pdf_backend.rs` for the PDF adapter).
//!
//! This trait is `pub(crate)` -- it is NOT part of the public API. The public
//! traits (`FormatBackend`, `PageExtractor`) in udoc-core remain unchanged
//! for low-level direct access.
//!
//! Design rationale: .

use udoc_core::document::{Document, DocumentMetadata};
use udoc_core::error::Result;
use udoc_core::image::PageImage;
use udoc_core::table::Table;
use udoc_core::text::{TextLine, TextSpan};

use crate::detect::Format;
use crate::Config;

/// Object-safe backend trait for facade dispatch.
///
/// Adding a new format backend requires:
/// 1. A new crate implementing the format-specific parsing.
/// 2. An adapter struct implementing `InternalBackend` (in this crate).
/// 3. One match arm in `Extractor::open_with()` to construct it.
pub(crate) trait InternalBackend: Send {
    /// Number of pages (or logical sections) in the document.
    fn page_count(&self) -> usize;

    /// `true` iff the source document declared encryption.
    /// Default `false`; overridden by backends with encryption support
    /// (currently PDF). Mirrors
    /// [`udoc_core::backend::FormatBackend::is_encrypted`]; the macro
    /// in this file delegates to that.
    fn is_encrypted(&self) -> bool {
        false
    }

    /// Extract full text from a single page.
    fn page_text(&mut self, index: usize) -> Result<String>;

    /// Extract text as positioned lines from a single page.
    fn page_lines(&mut self, index: usize) -> Result<Vec<TextLine>>;

    /// Extract raw text spans from a single page (no reading order).
    fn page_spans(&mut self, index: usize) -> Result<Vec<TextSpan>>;

    /// Extract tables from a single page.
    fn page_tables(&mut self, index: usize) -> Result<Vec<Table>>;

    /// Extract images from a single page.
    fn page_images(&mut self, index: usize) -> Result<Vec<PageImage>>;

    /// Document-level metadata.
    fn metadata(&self) -> DocumentMetadata;

    /// The format of this backend.
    fn format(&self) -> Format;

    /// Materialize the full Document model, consuming this backend.
    fn into_document(self: Box<Self>, config: &Config) -> Result<Document>;

    /// Release document-scoped caches without dropping the backend itself.
    ///
    /// Default impl is a no-op. Backends that hold per-document object
    /// caches, decoded-stream caches, etc. override this so long-running
    /// batch workers can reclaim memory between documents.
    ///
    /// The backend must remain usable after this call; pages opened after
    /// reset should still produce identical output to pre-reset calls.
    /// Wired up for PDF (#T60-MEMBATCH); other backends are
    /// intentional no-ops because their per-doc heap is already modest.
    fn reset_document_caches(&mut self) {}
}

/// Generate a boilerplate `InternalBackend` adapter struct for a format backend.
///
/// All format backends follow the same pattern: wrap a document type, delegate
/// page extraction to `FormatBackend`/`PageExtractor`, and call a convert function
/// for `into_document`. The only differences are the struct name, inner doc type,
/// `Format` variant, convert function path, and the unit noun for error messages
/// ("page" or "slide").
///
/// The five page methods (text, lines, spans, tables, images) all share the same
/// open-page + extract + wrap-error structure. The `@page_method` internal rule
/// generates each one from a compact declaration.
macro_rules! define_internal_backend {
    // Internal rule: generate one page extraction method.
    (@page_method $method:ident, $extractor:ident, $ret:ty, $what:expr, $unit:expr) => {
        fn $method(&mut self, index: usize) -> udoc_core::error::Result<$ret> {
            let mut page = udoc_core::backend::FormatBackend::page(&mut self.doc, index)
                .map_err(|e| {
                    udoc_core::error::Error::with_source(
                        format!(concat!("opening ", $unit, " {index}"), index = index),
                        e,
                    )
                })?;
            udoc_core::backend::PageExtractor::$extractor(&mut page).map_err(|e| {
                udoc_core::error::Error::with_source(
                    format!(
                        concat!("extracting ", $what, " from ", $unit, " {index}"),
                        index = index
                    ),
                    e,
                )
            })
        }
    };

    // Public rule: generate the full adapter struct + InternalBackend impl.
    // Optional `reset = |doc| { ... }` tail-arg lets a backend override the
    // default no-op `reset_document_caches`; omitted for backends whose
    // per-document heap is already modest (T60-MEMBATCH).
    ($name:ident, $doc_type:ty, $format:expr, $convert_fn:path, $unit:expr $(, reset = |$doc:ident| $reset_body:block )? ) => {
        pub(crate) struct $name {
            doc: $doc_type,
        }

        impl $name {
            pub(crate) fn new(doc: $doc_type) -> Self {
                Self { doc }
            }
        }

        impl crate::backend_trait::InternalBackend for $name {
            fn page_count(&self) -> usize {
                udoc_core::backend::FormatBackend::page_count(&self.doc)
            }

            define_internal_backend!(@page_method page_text, text, String, "text", $unit);
            define_internal_backend!(@page_method page_lines, text_lines, Vec<udoc_core::text::TextLine>, "lines", $unit);
            define_internal_backend!(@page_method page_spans, raw_spans, Vec<udoc_core::text::TextSpan>, "spans", $unit);
            define_internal_backend!(@page_method page_tables, tables, Vec<udoc_core::table::Table>, "tables", $unit);
            define_internal_backend!(@page_method page_images, images, Vec<udoc_core::image::PageImage>, "images", $unit);

            fn metadata(&self) -> udoc_core::document::DocumentMetadata {
                udoc_core::backend::FormatBackend::metadata(&self.doc)
            }

            fn is_encrypted(&self) -> bool {
                udoc_core::backend::FormatBackend::is_encrypted(&self.doc)
            }

            fn format(&self) -> crate::detect::Format {
                $format
            }

            fn into_document(
                self: Box<Self>,
                config: &crate::Config,
            ) -> udoc_core::error::Result<udoc_core::document::Document> {
                let mut doc = self.doc;
                $convert_fn(&mut doc, config)
            }

            $(
                fn reset_document_caches(&mut self) {
                    let $doc = &mut self.doc;
                    $reset_body
                }
            )?
        }
    };
}

pub(crate) use define_internal_backend;
