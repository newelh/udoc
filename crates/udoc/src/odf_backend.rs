//! ODF adapters for InternalBackend.
//!
//! Wraps `udoc_odf::OdfDocument` and delegates to `FormatBackend`/`PageExtractor`
//! for page-level extraction. Three `define_internal_backend!` invocations cover
//! ODT (page), ODS (page), and ODP (slide).

use crate::backend_trait::define_internal_backend;
use crate::detect::Format;

define_internal_backend!(
    OdtInternalBackend,
    udoc_odf::OdfDocument,
    Format::Odt,
    crate::convert::odt_to_document,
    "page"
);

define_internal_backend!(
    OdsInternalBackend,
    udoc_odf::OdfDocument,
    Format::Ods,
    crate::convert::ods_to_document,
    "sheet"
);

define_internal_backend!(
    OdpInternalBackend,
    udoc_odf::OdfDocument,
    Format::Odp,
    crate::convert::odp_to_document,
    "slide"
);
