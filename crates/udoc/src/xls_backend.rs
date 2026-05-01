//! XLS adapter for InternalBackend.
//!
//! Wraps `udoc_xls::XlsDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::xls_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    XlsInternalBackend,
    udoc_xls::XlsDocument,
    Format::Xls,
    crate::convert::xls_to_document,
    "sheet"
);
