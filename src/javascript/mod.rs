//! JavaScript ES5 static-script frontend (P0 capability bridge).
//!
//! Independent `javascript-es5` [`SourcePlugin`][vm::SourcePlugin] under
//! Strict typing with ownership checking. User JS bytes enter the ES Script
//! parser (never the TypeScript parser); the actual SWC ES AST drives the
//! emitted [`FrontendIr`][vm::FrontendIr] through [`lowerer`].
//!
//! Parsing happens in two stages with a hard boundary between them:
//!
//! * [`parser`] produces the SWC ES Script and runs the
//!   recursive ES5.1 grammar/admission validator ([`admission`]). Grammar
//!   acceptance here means the source is ES5.1 Script syntax with its
//!   strict-mode early errors satisfied — nothing more.
//! * [`lowerer`] then lowers the admitted AST to typed IR. Admitted ES5 forms
//!   that P0 cannot execute yet are refused there with lowering diagnostics.
//!
//! The physical [`FrontendTypingPolicy::Strict`][vm::FrontendTypingPolicy::Strict]
//! never activates JavaScript strict mode; only an actual source-spelling
//! `"use strict"` directive prologue does.
//!
//! P0 admits a small honest subset: `var` bindings, the `undefined` and
//! `null` primitives, numeric expressions with `+ - * /`, parentheses, unary
//! minus, identifier reads and tag-aware `===` / `!==` comparisons. Values
//! lower to the private guest record built by [`representation`]. Anything
//! else is precisely rejected until later scopes.
//!
//! Realm sources additionally admit immutable string literals and aliases
//! with lossless UTF-16 storage and content equality; see [`lowerer`].

mod admission;
pub(super) mod heap;
mod lowerer;
mod parser;
mod prelude;
mod representation;
mod source_map;
mod string_literal;
#[cfg(test)]
mod syntax_tests;

use std::path::Path;

use vm::{
    CompileSourceFileOptions, FrontendId, FrontendIr, FrontendTypeExport, FrontendTypingPolicy,
    ParseError, SourceFlavor, SourceFrontendContext, SourceFrontendDescriptor, SourcePathError,
    SourcePlugin,
};

pub struct JavaScriptEs5Plugin;

pub static JAVASCRIPT_ES5_PLUGIN: JavaScriptEs5Plugin = JavaScriptEs5Plugin;

pub fn javascript_es5_flavor() -> SourceFlavor {
    SourceFlavor::Plugin(FrontendId::new("javascript-es5"))
}

pub fn javascript_es5_compile_options() -> CompileSourceFileOptions {
    CompileSourceFileOptions::new().with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
}

impl SourcePlugin for JavaScriptEs5Plugin {
    fn descriptor(&self) -> SourceFrontendDescriptor {
        SourceFrontendDescriptor {
            id: FrontendId::new("javascript-es5"),
            extensions: &["js"],
            typing_policy: FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, ParseError> {
        let (script, base) = parser::parse_script(source, context.source_id)?;
        lowerer::lower(&script, source, context.source_id, base, context)
    }

    fn module_type_exports(
        &self,
        _source: &str,
        _context: &SourceFrontendContext<'_>,
        _ir: &FrontendIr,
    ) -> Result<Vec<FrontendTypeExport>, ParseError> {
        Ok(Vec::new())
    }

    fn validate_typed_ir(
        &self,
        _typed_ir: &vm::compiler::FrontendTypedIr<'_>,
    ) -> Result<(), vm::compiler::CompileError> {
        Ok(())
    }

    fn parse_module_imports(
        &self,
        _source: &str,
        _path: &Path,
        _context: &SourceFrontendContext<'_>,
    ) -> Result<Vec<vm::ModuleImport>, SourcePathError> {
        Ok(Vec::new())
    }

    fn strip_import_directives(&self, source: &str) -> String {
        source.to_owned()
    }
}
