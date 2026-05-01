//! DOCX adapter for InternalBackend.
//!
//! Wraps `udoc_docx::DocxDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::docx_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    DocxInternalBackend,
    udoc_docx::DocxDocument,
    Format::Docx,
    crate::convert::docx_to_document,
    "page"
);
