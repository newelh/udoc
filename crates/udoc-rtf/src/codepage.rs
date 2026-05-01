//! Re-exports codepage utilities from udoc-core.
//!
//! The canonical implementation lives in [`udoc_core::codepage`]. This module
//! re-exports everything for backward compatibility within the RTF crate.

pub(crate) use udoc_core::codepage::{
    encoding_for_ansicpg, encoding_for_charset, is_approximate_codepage, CodepageDecoder,
};
