//! PPT adapter for InternalBackend.
//!
//! Wraps `udoc_ppt::PptDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::ppt_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    PptInternalBackend,
    udoc_ppt::PptDocument,
    Format::Ppt,
    crate::convert::ppt_to_document,
    "slide"
);
