use vm::{FrontendIr, ParseError, ParserDialect, SharedParserOptions, parse_source_with_dialect};

struct JavaScriptDialect;

impl ParserDialect for JavaScriptDialect {
    fn is_import_keyword(&self, ident: &str) -> bool {
        ident == "import"
    }

    fn is_from_keyword(&self, ident: &str) -> bool {
        ident == "from"
    }

    fn is_fn_alias_keyword(&self, ident: &str) -> bool {
        ident == "function"
    }

    fn is_let_alias_keyword(&self, ident: &str) -> bool {
        matches!(ident, "const" | "var")
    }

    fn allow_import_stmt(&self) -> bool {
        true
    }

    fn allow_return_stmt(&self) -> bool {
        true
    }

    fn allow_require_declaration(&self) -> bool {
        true
    }

    fn allow_typeof_operator(&self) -> bool {
        true
    }

    fn allow_arrow_closure(&self) -> bool {
        true
    }

    fn allow_dotted_call(&self) -> bool {
        true
    }

    fn allow_namespace_path_separator(&self) -> bool {
        false
    }

    fn allow_plus_equal_operator(&self) -> bool {
        true
    }

    fn allow_increment_operator(&self) -> bool {
        true
    }

    fn allow_parenthesized_for_loop(&self) -> bool {
        true
    }
}

static JAVASCRIPT_DIALECT: JavaScriptDialect = JavaScriptDialect;

pub(crate) fn parser_dialect() -> &'static dyn ParserDialect {
    &JAVASCRIPT_DIALECT
}

pub(crate) fn lower_to_ir(source: &str) -> Result<FrontendIr, ParseError> {
    // File-module `alias.member()` is a MemberExpression-rooted Call. The frozen
    // parser's unknown dotted-call fallback rewinds those sites, so this frontend
    // owns lowering: identify original call spans, parse, then rewrite the
    // produced Call IR / semantic-index entries to qualified `alias::member`
    // names while keeping the original callee spans.
    // Local object members, shadowed aliases, computed/optional chains, and
    // nested `obj.alias.member` are left as ordinary member access.
    let analysis = crate::js_namespace::analyze_file_module_member_calls(source);
    let mut ir = parse_source_with_dialect(
        analysis.parse_source(source).as_ref(),
        parser_dialect(),
        SharedParserOptions {
            allow_implicit_semicolons: true,
            allow_implicit_externs: true,
            ..SharedParserOptions::default()
        },
    )?;
    analysis.lower_ir(&mut ir);
    Ok(ir)
}
