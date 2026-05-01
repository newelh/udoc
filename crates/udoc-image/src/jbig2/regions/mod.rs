//! JBIG2 region decoders (ISO 14492 §6.2-§6.7).
//!
//! Each region flavor lives in its own sub-module.

pub mod generic;
pub mod halftone;
pub mod pattern_dict;
pub mod refinement;
pub mod symbol;
pub mod text;
