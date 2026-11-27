//! Empty stand-in for the heap harness.
//!
//! The typed published-head transaction surface is consumed only by the
//! compiler harness. `heap/table.rs` resolves its `#[path]` to the real module
//! in `tests/compiler/heap_file_entry/tests/luajit_heap_runtime/`; from the
//! heap harness the same relative path lands here, so the heap harness
//! compiles no extra item and needs no dead-code allowance.
