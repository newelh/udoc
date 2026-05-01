//! Shared helpers for PostScript-derived fonts (Type1, CFF).
//!
//! Re-exports the "discovered stem" machinery inspired by FreeType's
//! `ps_hinter_table_build` / `ps_hints_stem` path. See
//! [`discovered_stems`] for the full algorithm.

pub mod discovered_stems;
