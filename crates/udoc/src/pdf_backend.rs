//! PDF adapter for InternalBackend.
//!
//! Wraps `udoc_pdf::Document` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::pdf_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    PdfInternalBackend,
    udoc_pdf::Document,
    Format::Pdf,
    crate::convert::pdf_to_document,
    "page",
    reset = |doc| {
        doc.reset_document_caches();
    }
);
