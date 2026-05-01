#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Format-agnostic font parsing and hinting.
//!
//! Extracted from `udoc-pdf` to decouple the font subsystem from PDF-specific
//! object types. Parses TrueType, CFF, and Type1 font programs; decodes
//! encoding tables and ToUnicode CMaps; grid-fits glyphs via the TrueType
//! hinting VM.
//!
//! PDF-specific font loading (font dictionaries, CharProcs) stays in
//! `udoc-pdf::font::loader` and `udoc-pdf::font::type3_pdf`.
//!
//! # Stability
//!
//! All modules in this crate are `#[doc(hidden)]` for the 0.1.0-alpha.1
//! release. The font engine is consumed exclusively by `udoc-pdf` and
//! `udoc-render` and is not part of the SemVer surface that downstream
//! library users should depend on. A proper internal-only re-export
//! boundary lands in 0.1.0-alpha.2.
//!
//! # Example
//!
//! ```
//! // Strip the standard PDF font-subset prefix.
//! assert_eq!(udoc_font::types::strip_subset_prefix("ABCDEF+Helvetica"), "Helvetica");
//! // Names without a subset prefix are returned unchanged.
//! assert_eq!(udoc_font::types::strip_subset_prefix("Helvetica"), "Helvetica");
//! ```

#[doc(hidden)]
pub mod cff;
#[doc(hidden)]
pub mod cmap;
#[doc(hidden)]
pub mod cmap_parser;
#[doc(hidden)]
pub mod encoding;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod hinting;
#[doc(hidden)]
pub mod math_encodings;
#[doc(hidden)]
pub mod otf;
#[doc(hidden)]
pub mod postscript;
#[doc(hidden)]
pub mod standard_widths;
#[doc(hidden)]
pub mod tounicode;
#[doc(hidden)]
pub mod ttf;
#[doc(hidden)]
pub mod type1;
#[doc(hidden)]
pub mod type3_outline;
#[doc(hidden)]
pub mod types;
