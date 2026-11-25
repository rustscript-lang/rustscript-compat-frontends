#![allow(clippy::duplicate_mod)]

// A direct #[path] to src/frontends/lua/heap.rs would compile that file's
// cfg(test) observation harness into this binary. The link is deep enough
// that those relative paths land on empty stubs. handle, resource, and table
// still resolve to the real heap sources.
#[allow(dead_code, unused_imports)]
#[path = "compiler/heap_file_entry/link/pad1/pad2/heap.rs"]
mod heap;

#[path = "compiler/compiler_javascript_tests.rs"]
mod compiler_javascript_tests;

#[path = "compiler/compiler_lua_tests.rs"]
mod compiler_lua_tests;

#[path = "compiler/compiler_lua_static_import_tests.rs"]
mod compiler_lua_static_import_tests;

#[path = "compiler/compiler_luajit_oracle_tests.rs"]
mod compiler_luajit_oracle_tests;

#[path = "compiler/compiler_lua_control_tests.rs"]
mod compiler_lua_control_tests;

#[path = "compiler/compiler_luajit_gc_tests.rs"]
mod compiler_luajit_gc_tests;
