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
    // Frozen `try_parse_js_dotted_call` rewinds unknown file-module dotted
    // calls. Fold those callee spans to collision-free same-length identifiers
    // (byte length and `\n`/`\r` positions unchanged), parse the folded source,
    // then rewrite only the implicit-extern Call IR / semantic-index entries
    // that match the original spans to qualified `alias::member` names.
    // Local object members, shadowed aliases, computed/optional chains, and
    // nested `obj.alias.member` stay ordinary member access. Multiline callees
    // cannot be an identifier without moving line boundaries, so they fail
    // closed before parse.
    let analysis = crate::js_namespace::analyze_file_module_member_calls(source)?;
    let folded = analysis.parse_source(source)?;
    let mut ir = parse_source_with_dialect(
        folded.as_ref(),
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
