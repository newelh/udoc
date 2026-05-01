//! Page rendering module. Renders document pages to PNG images for OCR
//! hooks and layout detection models.
//!
//! Thin re-export shim over the `udoc-render` crate. External callers that
//! used `udoc::render::*` before the  extraction continue to work
//! through this module.
//!
//! Re-exports are explicit (not `pub use udoc_render::*`) so that new
//! `pub` items added inside `udoc-render` do not silently extend the
//! facade's SemVer surface. New public API must be listed below on
//! purpose. See issue #202.

pub use udoc_render::{
    font_cache, inspect, png, render_page, render_page_rgb, render_page_rgb_with_profile,
    render_page_with_profile, RenderingProfile, DEFAULT_DPI,
};

// `auto_hinter` and `rasterizer` are only `pub` in `udoc-render` under the
// `test-internals` feature (they are `pub(crate)` in the default build).
// Mirror that gating here so the facade's public surface matches whatever
// `udoc-render` exposes at build time. Downstream tool consumers that
// use these modules (`glyph-diff`, `bench-compare`, `cursor-pair-diagnose`)
// already enable the corresponding feature when building.
#[cfg(feature = "test-internals")]
pub use udoc_render::{auto_hinter, rasterizer};
