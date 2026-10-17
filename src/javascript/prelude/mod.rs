//! Generated private guest routines for the managed logical heap.
//!
//! Raw extents, directory pages, root frames and the iterative collector all
//! live in the single certified B1 backing as numeric cells. Every helper
//! here is private, typed, generated from the real B1 schemas and declared
//! with an explicit implementation; none allocate host resources or call
//! back into source activation frames.

pub(super) mod heap;
pub(super) mod values;
