//! Text extraction output types and reading order reconstruction.
//!
//! [`TextSpan`] and [`TextLine`] are the primary output types for text
//! extraction. They are returned by [`Page::raw_spans()`](crate::Page::raw_spans),
//! [`Page::text_lines()`](crate::Page::text_lines), and
//! [`Page::extract()`](crate::Page::extract).

// Used by fuzz_reading_order
#[cfg(any(test, feature = "test-internals", fuzzing))]
pub mod coherence;
#[cfg(not(any(test, feature = "test-internals", fuzzing)))]
pub(crate) mod coherence;

pub mod cluster;
pub mod layout;
pub(crate) mod order;
pub mod types;
mod xy_cut;

pub use layout::{render_layout, LayoutOptions};
pub use types::{TextLine, TextSpan};

// Used by fuzz_reading_order
#[cfg(any(test, feature = "test-internals"))]
pub use order::order_spans;
