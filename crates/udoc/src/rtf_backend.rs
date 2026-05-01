//! RTF adapter for InternalBackend.
//!
//! Wraps `udoc_rtf::RtfDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::rtf_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    RtfInternalBackend,
    udoc_rtf::RtfDocument,
    Format::Rtf,
    crate::convert::rtf_to_document,
    "page"
);
