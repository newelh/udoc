//! I/O layer: abstract data source for random access reads.
//!
//! This is the bottom layer. It knows nothing about PDF structure.
//! Most users do not need this module directly; the [`Document`](crate::Document)
//! API handles I/O internally. Use these types if you need to feed a custom
//! data source into the lower-level parser APIs.

mod source;
