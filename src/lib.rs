#[path = "frontends/lua/mod.rs"]
mod lua;
pub mod runtime;
mod source_loader;
mod typescript;

pub use typescript::{
    LUA_PLUGIN, TYPESCRIPT_PLUGIN, TYPESCRIPT_REFERENCE_ARRAY_PLUGIN,
    collect_reference_array_type_hints_at_path_with_options,
    collect_reference_array_type_hints_with_options, collect_typescript_type_hints,
    collect_typescript_type_hints_at_path_with_options, collect_typescript_type_hints_with_options,
    compile_options, compile_options_with_catalog, lua_flavor, plugins,
    reference_array_compile_options, reference_array_effective_catalog, typescript_flavor,
};
pub use typescript::{LuaPlugin, TypeScriptPlugin, TypeScriptReferenceArrayPlugin};
