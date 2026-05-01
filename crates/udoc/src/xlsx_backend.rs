//! XLSX adapter for InternalBackend.
//!
//! Wraps `udoc_xlsx::XlsxDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. The `into_document` method calls
//! `convert::xlsx_to_document()` for Document model construction.

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    XlsxInternalBackend,
    udoc_xlsx::XlsxDocument,
    Format::Xlsx,
    crate::convert::xlsx_to_document,
    "sheet"
);
