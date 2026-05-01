#![deny(unsafe_code)]

//! Container format parsers for the udoc document extraction toolkit.
//!
//! Provides ZIP archive parsing, a namespace-aware XML pull-parser, CFB/OLE2
//! compound document parsing, and an OPC (Open Packaging Conventions) layer
//! for navigating OOXML packages.
//!
//! # Modules
//!
//! - [`zip`] -- ZIP archive reader (DEFLATE, ZIP64, lenient parsing)
//! - [`xml`] -- XML pull-parser for OOXML/ODF subset (namespace-aware)
//! - [`cfb`] -- CFB/OLE2 compound document reader (FAT chains, mini-stream)
//! - [`opc`] -- OPC package navigator (Content_Types, relationships, parts)

pub mod error;

// Container-format parsers. Pub for sibling-crate consumption (udoc-docx
// / udoc-xlsx / udoc-pptx / udoc-doc / udoc-xls / udoc-ppt) but
// doc-hidden because they expose backend-author-shaped APIs rather than
// downstream-library-user APIs.
#[doc(hidden)]
pub mod cfb;
#[doc(hidden)]
pub mod opc;
#[doc(hidden)]
pub mod xml;
#[doc(hidden)]
pub mod zip;

#[cfg(any(test, feature = "test-internals"))]
pub mod test_util;

pub use error::{Error, Result, ResultExt};
