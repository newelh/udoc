//! Markdown adapter for InternalBackend.
//!
//! Wraps `udoc_markdown::MdDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::md_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    MdInternalBackend,
    udoc_markdown::MdDocument,
    Format::Md,
    crate::convert::md_to_document,
    "page"
);
