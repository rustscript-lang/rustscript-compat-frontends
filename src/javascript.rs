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
    // JavaScript now lowers directly through the shared parser with JS dialect behavior.
    // No RustScript text rewriting layer is used.
    parse_source_with_dialect(
        source,
        parser_dialect(),
        SharedParserOptions {
            allow_implicit_semicolons: true,
            ..SharedParserOptions::default()
        },
    )
}
