//! PPTX adapter for InternalBackend.
//!
//! Wraps `udoc_pptx::PptxDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::pptx_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    PptxInternalBackend,
    udoc_pptx::PptxDocument,
    Format::Pptx,
    crate::convert::pptx_to_document,
    "slide"
);
