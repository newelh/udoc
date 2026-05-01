//! regression test for #202 (replace `pub use udoc_render::*`
//! wildcard with named re-exports).
//!
//! The original wildcard re-exported everything in `udoc_render`, which
//! committed the facade to all of that crate's internal types as part of
//! its SemVer surface. The fix in commit 86d52528 replaced the wildcard
//! with an explicit named list in `crates/udoc/src/render.rs`.
//!
//! This test locks the named list. Adding a new public type to
//! `udoc_render` should NOT silently extend the facade's surface — the
//! addition must be deliberate (add the name to the explicit re-export
//! list AND extend the assertions below).
//!
//! For the hard SemVer-frozen surface check, see the cargo public-api
//! baseline gate landing in.

#[test]
fn explicit_render_re_exports_present() {
    // Functions — fn-item coercion to fn-pointer asserts each function
    // exists with a public signature, without locking us to specific
    // parameter types (which can change behind the SemVer surface).
    let _ = udoc::render::render_page as *const ();
    let _ = udoc::render::render_page_rgb as *const ();
    let _ = udoc::render::render_page_with_profile as *const ();
    let _ = udoc::render::render_page_rgb_with_profile as *const ();

    // Types
    let _: udoc::render::RenderingProfile = udoc::render::RenderingProfile::Visual;
    let _: udoc::render::RenderingProfile = udoc::render::RenderingProfile::OcrFriendly;

    // Constants
    let _: u32 = udoc::render::DEFAULT_DPI;

    // Modules — touching the path is enough to assert pub re-export.
    #[allow(unused_imports)]
    use udoc::render::{font_cache, inspect, png};
    let _ = std::any::type_name::<font_cache::FontCache>();
    // inspect + png are util modules; keep the use-import as the assertion
    // since their internal item names are not part of the facade contract.
    let _ = std::any::type_name::<inspect::OutlineDump>();
    let _ = png::encode_rgb_png as *const ();
}
