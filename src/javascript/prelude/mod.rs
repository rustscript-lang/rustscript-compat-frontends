//! Generated private guest routines for the managed logical heap.
//!
//! Raw extents, directory pages, root frames and the iterative collector all
//! live in the single certified B1 backing as numeric cells. Every helper
//! here is private, typed, generated from the real B1 schemas and declared
//! with an explicit implementation; none allocate host resources or call
//! back into source activation frames.

pub(crate) mod arguments_key;
pub(crate) mod arguments_object;
pub(crate) mod array_buffer;
pub(crate) mod typed_array;
pub(crate) mod array_constructor;
pub(crate) mod construct;
pub(super) mod date;
pub(super) mod date_exact;
pub(crate) mod function_prototype;
pub(super) mod global_binding;
pub(super) mod heap;
pub(super) mod intrinsics;
pub(super) mod math;
pub(super) mod number_string;
pub(super) mod number_proto_string;
pub(super) mod object;
pub(super) mod regexp;
pub(super) mod string_call;
pub(super) mod string_from_code;
pub(super) mod string_repeat;
pub(super) mod undefined_box;
pub(super) mod uri;
pub(super) mod utf16_bridge;
pub(super) mod utf16_init;
pub(super) mod values;
