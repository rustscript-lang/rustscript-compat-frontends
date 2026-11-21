#![allow(clippy::duplicate_mod)]

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
