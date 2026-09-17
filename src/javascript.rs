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
    // Frozen dotted-call parsing covers builtin/host namespaces only. File-module
    // `alias.member()` calls are recognized from the JS token stream and lowered
    // onto qualified `alias::member` names in IR so the loader can keep namespace
    // provenance. Local object members and shadowed aliases are left untouched.
    let aliases = crate::js_namespace::file_module_namespace_aliases(source);
    let (source, renames) =
        crate::js_namespace::lower_file_module_namespace_calls(source, &aliases);
    let mut ir = parse_source_with_dialect(
        &source,
        parser_dialect(),
        SharedParserOptions {
            allow_implicit_semicolons: true,
            allow_implicit_externs: true,
            ..SharedParserOptions::default()
        },
    )?;
    crate::js_namespace::apply_file_module_call_renames(&mut ir, &renames);
    Ok(ir)
}
