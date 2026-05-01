//! Object model layer: PDF object types, resolver, and stream decoding.
//!
//! This layer builds on the parsing layer and provides typed PDF objects,
//! lazy object resolution with caching, and stream filter decoding. Most
//! users do not need this module directly; the [`Document`](crate::Document)
//! API uses it internally.
//!
//! Key types: [`PdfObject`] (the PDF value enum), [`PdfDictionary`] (ordered
//! key-value pairs), [`ObjectResolver`] (lazy loading with LRU cache and
//! cycle detection), [`ObjRef`] (indirect object reference).

/// Colorspace classification (ISO 32000-2 §8.6). Narrow surface used
/// by the content interpreter to decide operand popping and detect
/// Pattern colorspace fills.
pub mod colorspace;
pub mod resolver;
pub mod stream;
mod types;

// Re-exported for external consumers when test-internals is enabled (fuzz targets, integration
// tests). Without that feature, the modules are pub(crate) and these re-exports are unused.
#[cfg(any(test, feature = "test-internals"))]
pub use resolver::ObjectResolver;
#[cfg(any(test, feature = "test-internals"))]
pub use stream::{decode_stream, DecodeLimits};
pub use types::{ObjRef, PdfDictionary, PdfObject, PdfStream, PdfString};
