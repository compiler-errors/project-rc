#![cfg_attr(feature = "unsize", feature(unsize, coerce_unsized))]

#[macro_use]
mod common_impls;
mod metadata;
mod sync;
mod unsync;

pub use sync::ProjectArc;
pub use unsync::ProjectRc;
