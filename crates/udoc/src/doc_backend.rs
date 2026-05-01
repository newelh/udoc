//! DOC adapter for InternalBackend.
//!
//! Wraps `udoc_doc::DocDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::doc_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    DocInternalBackend,
    udoc_doc::DocDocument,
    Format::Doc,
    crate::convert::doc_to_document,
    "page"
);
