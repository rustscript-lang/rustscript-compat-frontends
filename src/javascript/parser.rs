//! ES5 Script parsing over the locked SWC ES parser.
//!
//! Sources always enter as ES [`Script`][ast::Script] (never TypeScript,
//! never modules): the explicit [`EsSyntax`] default keeps JSX, decorators
//! and bind expressions off, and the recursive [`admission`] whitelist owns
//! every version judgment because the parse target alone does not gate all
//! modern AST shapes. Raw source bytes, SWC base positions and the caller's
//! `source_id` flow through to every diagnostic via [`source_map`].

use swc_common::{FileName, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast as ast;
use swc_ecma_parser::{EsSyntax, Lexer, Parser, StringInput, Syntax};
use vm::ParseError;

use super::admission::admit_script;
use super::source_map::parse_error;

/// Parse ES Script source and run ES5.1 grammar admission.
///
/// Returns the admitted script plus the scratch-file base the lowerer uses
/// for line mapping. Admission failure and raw parse failure both surface as
/// [`ParseError`] with the caller's `source_id` and file-relative byte spans.
pub(super) fn parse_script(source: &str, source_id: u32) -> Result<(ast::Script, u32), ParseError> {
    let source_map: Lrc<SourceMap> = Default::default();
    let file = source_map.new_source_file(
        FileName::Custom("<javascript-es5>".to_string()).into(),
        source.to_string(),
    );
    let syntax = EsSyntax {
        jsx: false,
        fn_bind: false,
        decorators: false,
        decorators_before_export: false,
        export_default_from: false,
        import_attributes: false,
        allow_super_outside_method: false,
        allow_return_outside_function: false,
        auto_accessors: false,
        explicit_resource_management: false,
    };
    let lexer = Lexer::new(
        Syntax::Es(syntax),
        Default::default(),
        StringInput::from(&*file),
        None,
    );
    let mut parser = Parser::new_from(lexer);
    let base = file.start_pos.0;
    let script = parser.parse_script().map_err(|error| {
        parse_error(
            source_id,
            base,
            error.span(),
            format!("JavaScript ES5 parse error: {error:?}"),
        )
    })?;
    // Every recoverable lexer/parser diagnostic stays a rejection: no
    // wholesale suppression. The locked parser emits no newer-rule diagnostic
    // that ES5.1 admission must override (duplicate `__proto__` keys parse as
    // ordinary data keys; strict `with`/octal/delete diagnostics agree with
    // ES5.1), so any exact future override would need its own identified
    // diagnostic plus an independent ES5 validation test.
    if let Some(error) = parser.take_errors().into_iter().next() {
        return Err(parse_error(
            source_id,
            base,
            error.span(),
            format!("JavaScript ES5 parse error: {error:?}"),
        ));
    }
    admit_script(&script, source, source_id, base)?;
    Ok((script, base))
}
