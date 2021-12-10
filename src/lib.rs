#![feature(slice_ptr_get)]
#![feature(allocator_api)]
#![feature(ptr_metadata)]
#![feature(unsize)]
#![feature(coerce_unsized)]

mod sync;
mod unsync;

pub use sync::ProjectArc;
pub use unsync::ProjectRc;
