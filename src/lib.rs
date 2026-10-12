#[path = "frontends/lua/mod.rs"]
mod lua;
mod source_loader;
mod typescript;

pub use typescript::{
    LUA_PLUGIN, TYPESCRIPT_PLUGIN, collect_typescript_type_hints,
    collect_typescript_type_hints_with_options, compile_options, compile_options_with_catalog,
    lua_flavor, plugins, typescript_array_host_module, typescript_flavor,
};
pub use typescript::{LuaPlugin, TypeScriptPlugin};
