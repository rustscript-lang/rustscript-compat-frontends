mod javascript;
#[path = "frontends/lua/mod.rs"]
mod lua;
mod source_loader;

use std::path::Path;

use vm::{
    FrontendImportSyntax, FrontendIr, ModuleImport, ParseError, ParserDialect, SourceFlavor,
    SourcePathError, SourcePlugin,
};

pub struct JavaScriptPlugin;
pub struct LuaPlugin;

pub static JAVASCRIPT_PLUGIN: JavaScriptPlugin = JavaScriptPlugin;
pub static LUA_PLUGIN: LuaPlugin = LuaPlugin;

pub fn plugins() -> [&'static dyn SourcePlugin; 2] {
    [&JAVASCRIPT_PLUGIN, &LUA_PLUGIN]
}

impl SourcePlugin for JavaScriptPlugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::JavaScript
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "mjs"]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::JavaScript
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        javascript::lower_to_ir(source)
    }

    fn parser_dialect(&self) -> Option<&'static dyn ParserDialect> {
        Some(javascript::parser_dialect())
    }

    fn parse_module_imports(
        &self,
        source: &str,
        _path: &Path,
    ) -> Result<Vec<ModuleImport>, SourcePathError> {
        Ok(source_loader::parse_js_imports(source))
    }
}

impl SourcePlugin for LuaPlugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::Lua
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["lua"]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::Lua
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        lua::lower_to_ir(source)
    }

    fn parse_module_imports(
        &self,
        source: &str,
        _path: &Path,
    ) -> Result<Vec<ModuleImport>, SourcePathError> {
        Ok(source_loader::parse_lua_imports(source))
    }
}

pub fn compile_options() -> vm::CompileSourceFileOptions {
    plugins()
        .into_iter()
        .fold(vm::CompileSourceFileOptions::new(), |options, plugin| {
            options.with_source_plugin(plugin)
        })
}
