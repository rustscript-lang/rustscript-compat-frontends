use std::collections::{HashMap, HashSet};

use swc_common::{FileName, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast as ast;
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use vm::compiler::ir::{
    AssignmentKind, CatalogVisibility, FunctionDecl, FunctionDeclSite, FunctionImpl,
    GuestTypeProvenance, HostApiIrMetadata, LocalDeclSite, LocalRefSite, ParsedCallSite,
    ParsedCallTarget, ParsedLexicalScope, ParsedSemanticIndex, SemanticNodeId, StmtSpanSite,
    StructDecl, StructDeclOrigin, StructDeclSite,
};
use vm::compiler::source_map::Span;
use vm::{
    Expr, FrontendImportedDeclaration, FrontendIr, FrontendTypeExport, ParseError,
    SourceFrontendContext, Stmt, TypeSchema,
};

struct ParsedModule {
    module: ast::Module,
    base: u32,
}

type LoweredDefaultArguments = (Vec<u16>, Vec<Stmt>, Vec<Expr>);

fn parse_module(source: &str) -> Result<ParsedModule, ParseError> {
    let source_map: Lrc<SourceMap> = Default::default();
    let file = source_map.new_source_file(
        FileName::Custom("<typescript>".to_string()).into(),
        source.to_string(),
    );
    let lexer = Lexer::new(
        Syntax::Typescript(TsSyntax {
            tsx: false,
            decorators: false,
            dts: false,
            no_early_errors: false,
            disallow_ambiguous_jsx_like: true,
        }),
        Default::default(),
        StringInput::from(&*file),
        None,
    );
    let mut parser = Parser::new_from(lexer);
    let module = parser.parse_typescript_module().map_err(|error| {
        let span = error.span();
        let lo = span.lo.0.saturating_sub(file.start_pos.0) as usize;
        let hi = span.hi.0.saturating_sub(file.start_pos.0) as usize;
        ParseError::at_span(
            Span::new(0, lo, hi),
            format!("TypeScript parse error: {error:?}"),
        )
    })?;
    if let Some(error) = parser.take_errors().into_iter().next() {
        let span = error.span();
        let lo = span.lo.0.saturating_sub(file.start_pos.0) as usize;
        let hi = span.hi.0.saturating_sub(file.start_pos.0) as usize;
        return Err(ParseError::at_span(
            Span::new(0, lo, hi),
            format!("TypeScript parse error: {error:?}"),
        ));
    }
    Ok(ParsedModule {
        module,
        base: file.start_pos.0,
    })
}

pub(crate) fn byte_span(span: swc_common::Span, base: u32, source_id: u32) -> Span {
    Span::new(
        source_id,
        span.lo.0.saturating_sub(base) as usize,
        span.hi.0.saturating_sub(base) as usize,
    )
}

enum TypeDeclaration<'a> {
    Interface(&'a ast::TsInterfaceDecl),
    Alias(&'a ast::TsTypeAliasDecl),
}

struct DeclaredHostType {
    schema: TypeSchema,
    guest_type_names: HashSet<String>,
}

struct DeclaredGuestTypeNames<'map, 'ast> {
    declarations: &'map HashMap<String, StructDecl>,
    aliases: &'map HashMap<String, &'ast ast::TsTypeAliasDecl>,
    expanded_aliases: HashSet<String>,
    generic_params: HashSet<String>,
    names: HashSet<String>,
}

impl<'map, 'ast> DeclaredGuestTypeNames<'map, 'ast> {
    fn new(
        declarations: &'map HashMap<String, StructDecl>,
        aliases: &'map HashMap<String, &'ast ast::TsTypeAliasDecl>,
    ) -> Self {
        Self {
            declarations,
            aliases,
            expanded_aliases: HashSet::new(),
            generic_params: HashSet::new(),
            names: HashSet::new(),
        }
    }

    fn collect(&mut self, ty: &ast::TsType) {
        match ty {
            ast::TsType::TsTypeRef(reference) => {
                let name = entity_name(&reference.type_name);
                if !self.generic_params.contains(&name) {
                    if let Some(declaration) = self.declarations.get(&name) {
                        if declaration.origin == StructDeclOrigin::Guest {
                            self.names.insert(declaration.name.clone());
                        }
                    } else if let Some(alias) = self.aliases.get(&name)
                        && self.expanded_aliases.insert(name)
                    {
                        let previous_params = self.generic_params.clone();
                        if let Some(params) = &alias.type_params {
                            self.generic_params.extend(
                                params
                                    .params
                                    .iter()
                                    .map(|parameter| parameter.name.sym.to_string()),
                            );
                        }
                        self.collect(&alias.type_ann);
                        self.generic_params = previous_params;
                    }
                }
                if let Some(params) = &reference.type_params {
                    for parameter in &params.params {
                        self.collect(parameter);
                    }
                }
            }
            ast::TsType::TsArrayType(array) => self.collect(&array.elem_type),
            ast::TsType::TsTupleType(tuple) => {
                for element in &tuple.elem_types {
                    self.collect(&element.ty);
                }
            }
            ast::TsType::TsTypeLit(literal) => {
                for member in &literal.members {
                    if let ast::TsTypeElement::TsPropertySignature(property) = member
                        && let Some(annotation) = &property.type_ann
                    {
                        self.collect(&annotation.type_ann);
                    }
                }
            }
            ast::TsType::TsParenthesizedType(paren) => self.collect(&paren.type_ann),
            ast::TsType::TsFnOrConstructorType(ast::TsFnOrConstructorType::TsFnType(function)) => {
                for parameter in &function.params {
                    if let ast::TsFnParam::Ident(binding) = parameter
                        && let Some(annotation) = &binding.type_ann
                    {
                        self.collect(&annotation.type_ann);
                    }
                }
                self.collect(&function.type_ann.type_ann);
            }
            ast::TsType::TsUnionOrIntersectionType(union) => match union {
                ast::TsUnionOrIntersectionType::TsUnionType(union) => {
                    for member in &union.types {
                        self.collect(member);
                    }
                }
                ast::TsUnionOrIntersectionType::TsIntersectionType(intersection) => {
                    for member in &intersection.types {
                        self.collect(member);
                    }
                }
            },
            ast::TsType::TsOptionalType(optional) => self.collect(&optional.type_ann),
            ast::TsType::TsRestType(rest) => self.collect(&rest.type_ann),
            _ => {}
        }
    }
}

fn type_declaration_item(item: &ast::ModuleItem) -> Option<TypeDeclaration<'_>> {
    match item {
        ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::TsInterface(interface))) => {
            Some(TypeDeclaration::Interface(interface))
        }
        ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::TsTypeAlias(alias))) => {
            Some(TypeDeclaration::Alias(alias))
        }
        ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDecl(export)) => match &export.decl {
            ast::Decl::TsInterface(interface) => Some(TypeDeclaration::Interface(interface)),
            ast::Decl::TsTypeAlias(alias) => Some(TypeDeclaration::Alias(alias)),
            _ => None,
        },
        _ => None,
    }
}

fn type_parameter_names(
    params: Option<&ast::TsTypeParamDecl>,
    base: u32,
    source_id: u32,
) -> Result<Vec<String>, ParseError> {
    let Some(params) = params else {
        return Ok(Vec::new());
    };
    let mut names = HashSet::new();
    let mut result = Vec::with_capacity(params.params.len());
    for param in &params.params {
        if param.constraint.is_some()
            || param.default.is_some()
            || param.is_in
            || param.is_out
            || param.is_const
        {
            return Err(unsupported(
                param.span,
                base,
                source_id,
                "generic constraints, defaults, variance, or const modifiers",
            ));
        }
        let name = param.name.sym.to_string();
        if !names.insert(name.clone()) {
            return Err(ParseError::at_span(
                byte_span(param.name.span, base, source_id),
                format!("duplicate TypeScript generic parameter '{name}'"),
            ));
        }
        result.push(name);
    }
    Ok(result)
}

fn entity_name(name: &ast::TsEntityName) -> String {
    match name {
        ast::TsEntityName::Ident(ident) => ident.sym.to_string(),
        ast::TsEntityName::TsQualifiedName(qualified) => {
            format!("{}::{}", entity_name(&qualified.left), qualified.right.sym)
        }
    }
}

fn function_item(item: &ast::ModuleItem) -> Option<(&ast::FnDecl, bool)> {
    match item {
        ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::Fn(function))) => Some((function, false)),
        ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDecl(export)) => match &export.decl {
            ast::Decl::Fn(function) => Some((function, true)),
            _ => None,
        },
        _ => None,
    }
}

fn variable_declaration_item(item: &ast::ModuleItem) -> Option<(&ast::VarDecl, bool)> {
    match item {
        ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::Var(declaration))) => {
            Some((declaration, false))
        }
        ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDecl(export)) => match &export.decl {
            ast::Decl::Var(declaration) => Some((declaration, true)),
            _ => None,
        },
        _ => None,
    }
}

fn arrow_function_decl(ident: ast::Ident, arrow: ast::ArrowExpr) -> ast::FnDecl {
    let body = match *arrow.body {
        ast::ArrowFunctionBody::FunctionBody(body) => body,
        ast::ArrowFunctionBody::Expr(expression) => ast::FunctionBody {
            span: arrow.span,
            stmts: vec![ast::Stmt::Return(ast::ReturnStmt {
                span: arrow.span,
                arg: Some(expression),
            })],
        },
    };
    let params = arrow
        .params
        .into_iter()
        .map(|pat| ast::Param {
            span: pat.span(),
            decorators: Vec::new(),
            pat,
        })
        .collect();
    ast::FnDecl {
        ident,
        declare: false,
        function: Box::new(ast::Function {
            this_param: None,
            params,
            decorators: Vec::new(),
            span: arrow.span,
            ctxt: arrow.ctxt,
            body: Some(body),
            is_generator: arrow.is_generator,
            is_async: arrow.is_async,
            type_params: arrow.type_params,
            return_type: arrow.return_type,
        }),
    }
}

pub(crate) fn parameter_binding(
    parameter: &ast::Pat,
) -> Option<(&ast::BindingIdent, Option<&ast::Expr>)> {
    match parameter {
        ast::Pat::Ident(binding) => Some((binding, None)),
        ast::Pat::Assign(assign) => match assign.left.as_ref() {
            ast::Pat::Ident(binding) => Some((binding, Some(assign.right.as_ref()))),
            _ => None,
        },
        _ => None,
    }
}

fn parsed_numeric_literal(expression: &ast::Expr) -> Option<f64> {
    match expression {
        ast::Expr::Lit(ast::Lit::Num(value)) => Some(value.value),
        ast::Expr::Paren(paren) => parsed_numeric_literal(&paren.expr),
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::Minus => {
            parsed_numeric_literal(&unary.arg).map(|value| -value)
        }
        _ => None,
    }
}

pub(crate) fn numeric_literal(value: f64) -> Expr {
    if value == 0.0 && value.is_sign_negative() {
        Expr::Float(value)
    } else if value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value < 9_223_372_036_854_775_808.0
    {
        Expr::Int(value as i64)
    } else {
        Expr::Float(value)
    }
}

fn generic_params_in_schema(schema: &TypeSchema) -> Vec<String> {
    fn visit(schema: &TypeSchema, seen: &mut HashSet<String>, ordered: &mut Vec<String>) {
        match schema {
            TypeSchema::GenericParam(name) => {
                if seen.insert(name.clone()) {
                    ordered.push(name.clone());
                }
            }
            TypeSchema::Optional(inner) | TypeSchema::Array(inner) | TypeSchema::Map(inner) => {
                visit(inner, seen, ordered);
            }
            TypeSchema::Named(_, arguments) | TypeSchema::ArrayTuple(arguments) => {
                for argument in arguments {
                    visit(argument, seen, ordered);
                }
            }
            TypeSchema::ArrayTupleRest { prefix, rest } => {
                for item in prefix {
                    visit(item, seen, ordered);
                }
                visit(rest, seen, ordered);
            }
            TypeSchema::Object(fields) => {
                let mut fields = fields.iter().collect::<Vec<_>>();
                fields.sort_by_key(|(left, _)| *left);
                for (_, field) in fields {
                    visit(field, seen, ordered);
                }
            }
            TypeSchema::Callable { params, result } => {
                for param in params {
                    visit(param, seen, ordered);
                }
                visit(result, seen, ordered);
            }
            TypeSchema::Unknown
            | TypeSchema::Null
            | TypeSchema::Int
            | TypeSchema::Float
            | TypeSchema::Number
            | TypeSchema::Bool
            | TypeSchema::String
            | TypeSchema::Bytes
            | TypeSchema::Resource(_) => {}
        }
    }

    let mut seen = HashSet::new();
    let mut ordered = Vec::new();
    visit(schema, &mut seen, &mut ordered);
    ordered
}

fn substitute_type_schema(
    schema: &TypeSchema,
    bindings: &HashMap<String, TypeSchema>,
) -> TypeSchema {
    match schema {
        TypeSchema::GenericParam(name) => bindings
            .get(name)
            .cloned()
            .unwrap_or_else(|| schema.clone()),
        TypeSchema::Optional(inner) => {
            TypeSchema::Optional(Box::new(substitute_type_schema(inner, bindings)))
        }
        TypeSchema::Named(name, args) => TypeSchema::Named(
            name.clone(),
            args.iter()
                .map(|arg| substitute_type_schema(arg, bindings))
                .collect(),
        ),
        TypeSchema::Array(inner) => {
            TypeSchema::Array(Box::new(substitute_type_schema(inner, bindings)))
        }
        TypeSchema::ArrayTuple(items) => TypeSchema::ArrayTuple(
            items
                .iter()
                .map(|item| substitute_type_schema(item, bindings))
                .collect(),
        ),
        TypeSchema::ArrayTupleRest { prefix, rest } => TypeSchema::ArrayTupleRest {
            prefix: prefix
                .iter()
                .map(|item| substitute_type_schema(item, bindings))
                .collect(),
            rest: Box::new(substitute_type_schema(rest, bindings)),
        },
        TypeSchema::Map(inner) => {
            TypeSchema::Map(Box::new(substitute_type_schema(inner, bindings)))
        }
        TypeSchema::Object(fields) => TypeSchema::Object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), substitute_type_schema(field, bindings)))
                .collect(),
        ),
        TypeSchema::Callable { params, result } => TypeSchema::Callable {
            params: params
                .iter()
                .map(|param| substitute_type_schema(param, bindings))
                .collect(),
            result: Box::new(substitute_type_schema(result, bindings)),
        },
        TypeSchema::Unknown
        | TypeSchema::Null
        | TypeSchema::Int
        | TypeSchema::Float
        | TypeSchema::Number
        | TypeSchema::Bool
        | TypeSchema::String
        | TypeSchema::Bytes
        | TypeSchema::Resource(_) => schema.clone(),
    }
}

pub(crate) fn schema_value_type(schema: &TypeSchema) -> vm::ValueType {
    match schema {
        TypeSchema::Null => vm::ValueType::Null,
        TypeSchema::Int => vm::ValueType::Int,
        TypeSchema::Float => vm::ValueType::Float,
        TypeSchema::Bool => vm::ValueType::Bool,
        TypeSchema::String => vm::ValueType::String,
        TypeSchema::Bytes => vm::ValueType::Bytes,
        TypeSchema::Array(_) | TypeSchema::ArrayTuple(_) | TypeSchema::ArrayTupleRest { .. } => {
            vm::ValueType::Array
        }
        TypeSchema::Map(_) | TypeSchema::Object(_) | TypeSchema::Named(_, _) => vm::ValueType::Map,
        TypeSchema::Callable { .. } => vm::ValueType::Callable,
        TypeSchema::Number
        | TypeSchema::Unknown
        | TypeSchema::GenericParam(_)
        | TypeSchema::Optional(_)
        | TypeSchema::Resource(_) => vm::ValueType::Unknown,
    }
}

pub(crate) fn line_at(source: &str, byte: usize) -> u32 {
    let end = byte.min(source.len());
    let prefix = &source[..end];
    u32::try_from(prefix.bytes().filter(|byte| *byte == b'\n').count() + 1).unwrap_or(u32::MAX)
}

pub(crate) fn unsupported(
    span: swc_common::Span,
    base: u32,
    source_id: u32,
    what: &str,
) -> ParseError {
    ParseError::at_span(
        byte_span(span, base, source_id),
        format!("unsupported TypeScript {what}"),
    )
}

struct InstalledHostImports {
    functions: Vec<FunctionDecl>,
    struct_schemas: HashMap<String, StructDecl>,
    named_types: HashMap<String, TypeSchema>,
    resource_keys: HashSet<String>,
    parameter_passings: HashMap<String, HashMap<u8, Vec<vm::HostParamPassing>>>,
    metadata: Option<HostApiIrMetadata>,
    by_local_name: HashMap<String, HashMap<u8, u16>>,
    declared_functions: HashMap<String, Vec<vm::HostFunctionSchema>>,
    visibility: CatalogVisibility,
    next_function_index: u16,
}

fn empty_frontend_ir() -> FrontendIr {
    FrontendIr {
        stmts: Vec::new(),
        locals: 0,
        local_bindings: Vec::new(),
        struct_schemas: HashMap::new(),
        unknown_type_spans: Vec::new(),
        functions: Vec::new(),
        function_impls: HashMap::new(),
        stmt_sources: Vec::new(),
        function_sources: HashMap::new(),
        use_declarations: Vec::new(),
        implicit_extern_names: Vec::new(),
        host_api_metadata: None,
        semantic_index: None,
        parsed_semantic_index: None,
        catalog_visibility: None,
        lexer_tokens: Vec::new(),
    }
}

fn is_host_import_spec(spec: &str) -> bool {
    !(spec.starts_with('.') || spec.starts_with('/') || spec.contains('/'))
}

fn variable_decl_contains_throw(declaration: &ast::VarDecl) -> bool {
    declaration.decls.iter().any(|declaration| {
        declaration
            .init
            .as_deref()
            .is_some_and(expression_contains_throw)
    })
}

fn function_body_contains_throw(body: &ast::FunctionBody) -> bool {
    body.stmts.iter().any(statement_contains_throw)
}

fn statement_contains_throw(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::Throw(_) => true,
        ast::Stmt::Block(block) => block.stmts.iter().any(statement_contains_throw),
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => variable_decl_contains_throw(declaration),
        ast::Stmt::Decl(ast::Decl::Fn(function)) => function
            .function
            .body
            .as_ref()
            .is_some_and(function_body_contains_throw),
        ast::Stmt::Expr(expression) => expression_contains_throw(&expression.expr),
        ast::Stmt::If(statement) => {
            expression_contains_throw(&statement.test)
                || statement_contains_throw(&statement.cons)
                || statement
                    .alt
                    .as_deref()
                    .is_some_and(statement_contains_throw)
        }
        ast::Stmt::While(statement) => {
            expression_contains_throw(&statement.test) || statement_contains_throw(&statement.body)
        }
        ast::Stmt::For(statement) => {
            let initializer_contains_throw =
                statement
                    .init
                    .as_ref()
                    .is_some_and(|initializer| match initializer {
                        ast::VarDeclOrExpr::VarDecl(declaration) => {
                            variable_decl_contains_throw(declaration)
                        }
                        ast::VarDeclOrExpr::Expr(expression) => {
                            expression_contains_throw(expression)
                        }
                    });
            initializer_contains_throw
                || statement
                    .test
                    .as_deref()
                    .is_some_and(expression_contains_throw)
                || statement
                    .update
                    .as_deref()
                    .is_some_and(expression_contains_throw)
                || statement_contains_throw(&statement.body)
        }
        ast::Stmt::Return(statement) => statement
            .arg
            .as_deref()
            .is_some_and(expression_contains_throw),
        _ => false,
    }
}

fn expression_contains_throw(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Arrow(arrow) => match arrow.body.as_ref() {
            ast::ArrowFunctionBody::FunctionBody(body) => function_body_contains_throw(body),
            ast::ArrowFunctionBody::Expr(expression) => expression_contains_throw(expression),
        },
        ast::Expr::Fn(function) => function
            .function
            .body
            .as_ref()
            .is_some_and(function_body_contains_throw),
        ast::Expr::Call(call) => {
            let callee_contains_throw = match &call.callee {
                ast::Callee::Expr(callee) => expression_contains_throw(callee),
                ast::Callee::Super(_) | ast::Callee::Import(_) => false,
            };
            callee_contains_throw
                || call
                    .args
                    .iter()
                    .any(|argument| expression_contains_throw(&argument.expr))
        }
        ast::Expr::New(expression) => {
            expression_contains_throw(&expression.callee)
                || expression.args.as_ref().is_some_and(|arguments| {
                    arguments
                        .iter()
                        .any(|argument| expression_contains_throw(&argument.expr))
                })
        }
        ast::Expr::Cond(expression) => {
            expression_contains_throw(&expression.test)
                || expression_contains_throw(&expression.cons)
                || expression_contains_throw(&expression.alt)
        }
        ast::Expr::Bin(expression) => {
            expression_contains_throw(&expression.left)
                || expression_contains_throw(&expression.right)
        }
        ast::Expr::Unary(expression) => expression_contains_throw(&expression.arg),
        ast::Expr::Assign(expression) => expression_contains_throw(&expression.right),
        ast::Expr::Paren(expression) => expression_contains_throw(&expression.expr),
        ast::Expr::Seq(expression) => expression
            .exprs
            .iter()
            .any(|expression| expression_contains_throw(expression)),
        _ => false,
    }
}

fn module_contains_throw(module: &ast::Module) -> bool {
    module.body.iter().any(|item| match item {
        ast::ModuleItem::Stmt(statement) => statement_contains_throw(statement),
        ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDecl(export)) => match &export.decl {
            ast::Decl::Var(declaration) => variable_decl_contains_throw(declaration),
            ast::Decl::Fn(function) => function
                .function
                .body
                .as_ref()
                .is_some_and(function_body_contains_throw),
            _ => false,
        },
        _ => false,
    })
}

fn catalog_has_exact_error_adapter(catalog: &vm::HostApiCatalog) -> bool {
    let candidates = catalog.functions_named(crate::runtime::scalar_error::THROW_ERROR_FUNCTION);
    let [candidate] = candidates.as_slice() else {
        return false;
    };
    candidate.name == crate::runtime::scalar_error::THROW_ERROR_FUNCTION
        && candidate.params.len() == 1
        && candidate.params[0].name == "message"
        && candidate.params[0].passing == vm::HostParamPassing::Value
        && matches!(&candidate.params[0].ty, vm::HostTypeSchema::String)
        && matches!(&candidate.return_type, vm::HostTypeSchema::Null)
}

fn collect_host_named_type_names(schema: &vm::HostTypeSchema, names: &mut HashSet<String>) {
    match schema {
        vm::HostTypeSchema::Array(inner)
        | vm::HostTypeSchema::Map(inner)
        | vm::HostTypeSchema::Optional(inner) => collect_host_named_type_names(inner, names),
        vm::HostTypeSchema::Callable { params, result } => {
            for param in params {
                collect_host_named_type_names(param, names);
            }
            collect_host_named_type_names(result, names);
        }
        vm::HostTypeSchema::Named { name, fields } => {
            names.insert(name.clone());
            for field in fields {
                collect_host_named_type_names(&field.ty, names);
            }
        }
        vm::HostTypeSchema::Unknown
        | vm::HostTypeSchema::Null
        | vm::HostTypeSchema::Int
        | vm::HostTypeSchema::Float
        | vm::HostTypeSchema::Number
        | vm::HostTypeSchema::Bool
        | vm::HostTypeSchema::String
        | vm::HostTypeSchema::Bytes
        | vm::HostTypeSchema::Resource(_) => {}
    }
}

fn host_named_types_for_module(catalog: &vm::HostApiCatalog, spec: &str) -> Vec<String> {
    let prefix = format!("{spec}::");
    let mut names = HashSet::new();
    for function in catalog
        .functions()
        .iter()
        .filter(|function| function.name.starts_with(&prefix))
    {
        for parameter in &function.params {
            collect_host_named_type_names(&parameter.ty, &mut names);
        }
        collect_host_named_type_names(&function.return_type, &mut names);
    }
    let mut names = names
        .into_iter()
        .filter(|name| catalog.struct_named(name).is_some())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn intrinsic_type_schema(name: &str) -> Option<TypeSchema> {
    match name {
        "Int" => Some(TypeSchema::Int),
        "Float" => Some(TypeSchema::Float),
        "Bytes" => Some(TypeSchema::Bytes),
        "Resource" => Some(TypeSchema::Named(
            "__rustscript_resource_type".to_owned(),
            Vec::new(),
        )),
        _ => None,
    }
}

fn install_intrinsic_type_imports(
    import: &ast::ImportDecl,
    named_types: &mut HashMap<String, TypeSchema>,
    base: u32,
    source_id: u32,
) -> Result<(), ParseError> {
    for specifier in &import.specifiers {
        match specifier {
            ast::ImportSpecifier::Named(named) => {
                if !import.type_only && !named.is_type_only {
                    return Err(unsupported(
                        named.span,
                        base,
                        source_id,
                        "value import from the virtual type module",
                    ));
                }
                let imported = named
                    .imported
                    .as_ref()
                    .map(|name| match name {
                        ast::ModuleExportName::Ident(ident) => ident.sym.to_string(),
                        ast::ModuleExportName::Str(value) => {
                            value.value.to_string_lossy().into_owned()
                        }
                    })
                    .unwrap_or_else(|| named.local.sym.to_string());
                let local = named.local.sym.to_string();
                let schema = intrinsic_type_schema(&imported).ok_or_else(|| {
                    ParseError::at_span(
                        byte_span(named.span, base, source_id),
                        format!("unknown intrinsic TypeScript type '{imported}'"),
                    )
                })?;
                if named_types.insert(local.clone(), schema).is_some() {
                    return Err(ParseError::at_span(
                        byte_span(named.span, base, source_id),
                        format!("duplicate intrinsic type binding '{local}'"),
                    ));
                }
            }
            ast::ImportSpecifier::Namespace(namespace) => {
                if !import.type_only {
                    return Err(unsupported(
                        namespace.span,
                        base,
                        source_id,
                        "value namespace import from the virtual type module",
                    ));
                }
                for name in ["Int", "Float", "Bytes", "Resource"] {
                    let local = format!("{}::{name}", namespace.local.sym);
                    let schema = intrinsic_type_schema(name)
                        .expect("known intrinsic type names have schemas");
                    named_types.insert(local, schema);
                }
            }
            ast::ImportSpecifier::Default(default) => {
                return Err(unsupported(
                    default.span,
                    base,
                    source_id,
                    "default import from the virtual type module",
                ));
            }
        }
    }
    Ok(())
}

fn host_parameter_passings_for_arity(
    candidates: &[&vm::HostFunctionSchema],
    arity: u8,
    span: swc_common::Span,
    base: u32,
    source_id: u32,
) -> Result<Vec<vm::HostParamPassing>, ParseError> {
    let mut signatures = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.params.len() == usize::from(arity));
    let Some(first) = signatures.next() else {
        return Err(ParseError::at_span(
            byte_span(span, base, source_id),
            "host catalog has no function overload for the selected arity",
        ));
    };
    let passings = first
        .params
        .iter()
        .map(|parameter| parameter.passing)
        .collect::<Vec<_>>();
    if signatures.any(|candidate| {
        candidate
            .params
            .iter()
            .map(|parameter| parameter.passing)
            .ne(passings.iter().cloned())
    }) {
        return Err(ParseError::at_span(
            byte_span(span, base, source_id),
            "same-arity host overloads require different resource passing modes",
        ));
    }
    Ok(passings)
}

fn install_host_imports(
    module: &ast::Module,
    context: &SourceFrontendContext<'_>,
    base: u32,
) -> Result<InstalledHostImports, ParseError> {
    let mut seed = empty_frontend_ir();
    let mut by_local_name = HashMap::<String, HashMap<u8, u16>>::new();
    let mut parameter_passings = HashMap::new();
    let mut named_types = HashMap::new();
    let mut declared_functions = HashMap::new();
    let mut direct_host_call_aliases = Vec::new();
    let mut host_namespace_aliases = Vec::new();
    for item in &module.body {
        let ast::ModuleItem::ModuleDecl(ast::ModuleDecl::Import(import)) = item else {
            continue;
        };
        let spec = import.src.value.to_string_lossy().into_owned();
        if spec == "rustscript:types" {
            install_intrinsic_type_imports(import, &mut named_types, base, context.source_id)?;
            continue;
        }
        if !is_host_import_spec(&spec) {
            continue;
        }
        for specifier in &import.specifiers {
            match specifier {
                ast::ImportSpecifier::Named(named) => {
                    let imported = named
                        .imported
                        .as_ref()
                        .map(|name| match name {
                            ast::ModuleExportName::Ident(ident) => ident.sym.to_string(),
                            ast::ModuleExportName::Str(value) => {
                                value.value.to_string_lossy().into_owned()
                            }
                        })
                        .unwrap_or_else(|| named.local.sym.to_string());
                    let local = named.local.sym.to_string();
                    if import.type_only || named.is_type_only {
                        let span = byte_span(named.span, base, context.source_id);
                        let schema = context
                            .install_host_type(&mut seed, &imported)
                            .map_err(|error| ParseError::at_span(span, error.message))?;
                        if named_types.insert(local.clone(), schema).is_some() {
                            return Err(ParseError::at_span(
                                span,
                                format!("duplicate host type import binding '{local}'"),
                            ));
                        }
                        continue;
                    }
                    let catalog_name = format!("{spec}::{imported}");
                    let catalog = context.host_api_catalog.ok_or_else(|| {
                        ParseError::at_span(
                            byte_span(named.span, base, context.source_id),
                            format!("host import '{catalog_name}' requires the caller's catalog"),
                        )
                    })?;
                    let candidates = catalog.functions_named(&catalog_name);
                    if candidates.is_empty() {
                        return Err(ParseError::at_span(
                            byte_span(named.span, base, context.source_id),
                            format!("host catalog does not declare '{catalog_name}'"),
                        ));
                    }
                    let mut arities = Vec::new();
                    for candidate in &candidates {
                        let arity = u8::try_from(candidate.params.len()).map_err(|_| {
                            unsupported(named.span, base, context.source_id, "host function arity")
                        })?;
                        if !arities.contains(&arity) {
                            arities.push(arity);
                        }
                    }
                    let imported_locals = by_local_name.entry(local.clone()).or_default();
                    for arity in arities {
                        let passings = host_parameter_passings_for_arity(
                            &candidates,
                            arity,
                            named.span,
                            base,
                            context.source_id,
                        )?;
                        let index =
                            context.install_host_function(&mut seed, &catalog_name, arity)?;
                        parameter_passings
                            .entry(local.clone())
                            .or_insert_with(HashMap::new)
                            .insert(arity, passings);
                        if imported_locals.insert(arity, index).is_some() {
                            return Err(ParseError::at_span(
                                byte_span(named.span, base, context.source_id),
                                format!("duplicate host import binding '{local}'"),
                            ));
                        }
                    }
                    direct_host_call_aliases.push((local, catalog_name));
                }
                ast::ImportSpecifier::Namespace(namespace) => {
                    let local = namespace.local.sym.to_string();
                    let catalog = context.host_api_catalog.ok_or_else(|| {
                        ParseError::at_span(
                            byte_span(namespace.span, base, context.source_id),
                            format!("host namespace '{spec}' requires the caller's catalog"),
                        )
                    })?;
                    let type_names = host_named_types_for_module(catalog, &spec);
                    for type_name in &type_names {
                        let local_name = format!("{local}::{type_name}");
                        let span = byte_span(namespace.span, base, context.source_id);
                        let schema = context
                            .install_host_type(&mut seed, type_name)
                            .map_err(|error| ParseError::at_span(span, error.message))?;
                        if named_types.insert(local_name.clone(), schema).is_some() {
                            return Err(ParseError::at_span(
                                span,
                                format!("duplicate host namespace type binding '{local_name}'"),
                            ));
                        }
                    }
                    if import.type_only {
                        if type_names.is_empty() {
                            return Err(ParseError::at_span(
                                byte_span(namespace.span, base, context.source_id),
                                format!("host catalog has no type namespace '{spec}'"),
                            ));
                        }
                        continue;
                    }
                    let prefix = format!("{spec}::");
                    let mut names = Vec::new();
                    for function in catalog.functions() {
                        if function.name.starts_with(&prefix) && !names.contains(&function.name) {
                            names.push(function.name.clone());
                        }
                    }
                    if names.is_empty() {
                        return Err(ParseError::at_span(
                            byte_span(namespace.span, base, context.source_id),
                            format!("host catalog has no function namespace '{spec}'"),
                        ));
                    }
                    for catalog_name in names {
                        let member = catalog_name
                            .strip_prefix(&prefix)
                            .expect("name has the selected namespace prefix");
                        let local_name = format!("{local}::{member}");
                        let candidates = catalog.functions_named(&catalog_name);
                        let mut arities = Vec::new();
                        for candidate in &candidates {
                            let arity = u8::try_from(candidate.params.len()).map_err(|_| {
                                unsupported(
                                    namespace.span,
                                    base,
                                    context.source_id,
                                    "host function arity",
                                )
                            })?;
                            if !arities.contains(&arity) {
                                arities.push(arity);
                            }
                        }
                        let imported_locals = by_local_name.entry(local_name.clone()).or_default();
                        for arity in arities {
                            let passings = host_parameter_passings_for_arity(
                                &candidates,
                                arity,
                                namespace.span,
                                base,
                                context.source_id,
                            )?;
                            let index =
                                context.install_host_function(&mut seed, &catalog_name, arity)?;
                            parameter_passings
                                .entry(local_name.clone())
                                .or_insert_with(HashMap::new)
                                .insert(arity, passings);
                            if imported_locals.insert(arity, index).is_some() {
                                return Err(ParseError::at_span(
                                    byte_span(namespace.span, base, context.source_id),
                                    format!("duplicate host namespace binding '{local_name}'"),
                                ));
                            }
                        }
                    }
                    host_namespace_aliases.push((local, spec.clone()));
                }
                ast::ImportSpecifier::Default(default) => {
                    return Err(unsupported(
                        default.span,
                        base,
                        context.source_id,
                        "default host import",
                    ));
                }
            }
        }
    }
    let mut seen_declarations = HashSet::new();
    for item in &module.body {
        let Some((function, exported)) = function_item(item) else {
            continue;
        };
        if !function.declare {
            continue;
        }
        let name = function.ident.sym.to_string();
        if !seen_declarations.insert(name.clone()) {
            continue;
        }
        if exported {
            return Err(unsupported(
                function.ident.span,
                base,
                context.source_id,
                "exported host declaration",
            ));
        }
        let declaration_span = byte_span(function.ident.span, base, context.source_id);
        if by_local_name.contains_key(&name) {
            return Err(ParseError::at_span(
                declaration_span,
                format!("host declaration '{name}' conflicts with an imported host function"),
            ));
        }
        let catalog = context.host_api_catalog.ok_or_else(|| {
            ParseError::at_span(
                declaration_span,
                format!("host declaration '{name}' requires the caller's catalog"),
            )
        })?;
        let catalog_names = catalog
            .functions()
            .iter()
            .filter(|candidate| {
                candidate.name == name
                    || candidate
                        .name
                        .rsplit_once("::")
                        .is_some_and(|(_, member)| member == name)
            })
            .map(|candidate| candidate.name.clone())
            .collect::<HashSet<_>>();
        let catalog_name = match catalog_names.len() {
            0 => {
                return Err(ParseError::at_span(
                    declaration_span,
                    format!("host catalog does not declare '{name}'"),
                ));
            }
            1 => catalog_names.into_iter().next().expect("one catalog name"),
            _ => {
                return Err(ParseError::at_span(
                    declaration_span,
                    format!("host declaration '{name}' is ambiguous in the caller's catalog"),
                ));
            }
        };
        let candidates = catalog.functions_named(&catalog_name);
        if candidates.is_empty() {
            return Err(ParseError::at_span(
                declaration_span,
                format!("host catalog does not declare '{catalog_name}'"),
            ));
        }
        let mut arities = Vec::new();
        for candidate in &candidates {
            let arity = u8::try_from(candidate.params.len()).map_err(|_| {
                unsupported(
                    function.function.span,
                    base,
                    context.source_id,
                    "host function arity",
                )
            })?;
            if !arities.contains(&arity) {
                arities.push(arity);
            }
        }
        let imported_locals = by_local_name.entry(name.clone()).or_default();
        for arity in arities {
            let passings = host_parameter_passings_for_arity(
                &candidates,
                arity,
                function.function.span,
                base,
                context.source_id,
            )?;
            let index = context.install_host_function(&mut seed, &catalog_name, arity)?;
            parameter_passings
                .entry(name.clone())
                .or_insert_with(HashMap::new)
                .insert(arity, passings);
            if imported_locals.insert(arity, index).is_some() {
                return Err(ParseError::at_span(
                    declaration_span,
                    format!("duplicate host declaration binding '{name}'"),
                ));
            }
        }
        direct_host_call_aliases.push((name.clone(), catalog_name));
        declared_functions.insert(name, candidates.into_iter().cloned().collect());
    }
    if module_contains_throw(module)
        && context
            .host_api_catalog
            .is_some_and(catalog_has_exact_error_adapter)
    {
        let index = context.install_host_function(
            &mut seed,
            crate::runtime::scalar_error::THROW_ERROR_FUNCTION,
            1,
        )?;
        let binding = crate::runtime::scalar_error::THROW_ERROR_BINDING.to_owned();
        by_local_name
            .entry(binding.clone())
            .or_default()
            .insert(1, index);
        parameter_passings
            .entry(binding)
            .or_insert_with(HashMap::new)
            .insert(1, vec![vm::HostParamPassing::Value]);
    }
    let resource_keys = context
        .host_api_catalog
        .map(|catalog| {
            catalog
                .resources()
                .iter()
                .map(|resource| resource.key.as_str().to_owned())
                .collect()
        })
        .unwrap_or_default();
    let next_function_index = u16::try_from(seed.functions.len())
        .map_err(|_| ParseError::new("host function table exceeds TypeScript index range"))?;
    Ok(InstalledHostImports {
        functions: seed.functions,
        struct_schemas: seed.struct_schemas,
        named_types,
        resource_keys,
        parameter_passings,
        metadata: seed.host_api_metadata,
        by_local_name,
        declared_functions,
        visibility: CatalogVisibility {
            host_namespace_aliases,
            direct_host_call_aliases,
            direct_host_wildcard_imports: Vec::new(),
            module_namespace_aliases: Vec::new(),
            use_declarations: Vec::new(),
        },
        next_function_index,
    })
}

pub(crate) struct ClosureCaptureContext {
    pub(crate) local_slots: HashSet<u16>,
    pub(crate) captures: HashMap<u16, u16>,
    pub(crate) capture_copies: Vec<(u16, u16)>,
}

/// Early-return lowering state for one ordinary function or arrow body.
/// `done` flips on the first executed return; `ret` holds valued results.
struct EarlyReturnFix {
    done: u16,
    ret: Option<u16>,
    valued: bool,
}

#[derive(Clone)]
pub(crate) enum ConstCallableOrigin {
    Local {
        name: String,
        index: u16,
        arity: u8,
    },
    Imported {
        name: String,
        symbol: vm::SymbolId,
        arity: u8,
    },
}

impl ConstCallableOrigin {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Local { name, .. } | Self::Imported { name, .. } => name,
        }
    }

    pub(crate) fn arity(&self) -> u8 {
        match self {
            Self::Local { arity, .. } | Self::Imported { arity, .. } => *arity,
        }
    }

    fn reference(&self) -> Expr {
        match self {
            Self::Local { index, .. } => Expr::FunctionRef(*index, Vec::new()),
            Self::Imported { symbol, .. } => Expr::ModuleFunctionRef(*symbol, Vec::new()),
        }
    }
}

pub(crate) struct Lowerer<'a> {
    pub(crate) source: &'a str,
    pub(crate) source_id: u32,
    pub(crate) base: u32,
    pub(crate) next_slot: u16,
    pub(crate) next_node_id: u32,
    pub(crate) next_scope_id: u32,
    pub(crate) next_function_index: u16,
    pub(crate) scope_id: u32,
    /// Explicit reference-array mode: logical `number[]` bindings in the
    /// user region lower to the private B1-backed carrier. Plain mode
    /// never sets this and behaves exactly as before.
    pub(crate) ref_mode: bool,
    /// Number of leading module items that belong to the private
    /// reference-array prelude. Those keep plain array semantics (notably
    /// the factory staging parameter); user items lower to the carrier.
    /// The prelude is parsed separately and merged as AST items, never as
    /// source-text concatenation, so user bytes/spans are untouched.
    pub(crate) ref_prelude_items: usize,
    /// True while lowering a prelude item. Drives `ref_is_user` without
    /// consulting span offsets (the two parses share numeric span space).
    pub(crate) in_ref_prelude: bool,
    /// Statements nested inside function, closure, loop or branch bodies
    /// bind their expression-statement values to hidden locals instead of
    /// leaving them on the stack. The native SSA backend rejects a value
    /// left on the stack at those merge points (FrameMismatch), while top
    /// level keeps evaluating to the stack. Plain and reference modes
    /// share this shape; behavior is identical either way.
    pub(crate) stmt_value_depth: usize,
    pub(crate) locals: HashMap<String, u16>,
    pub(crate) local_types: HashMap<String, TypeSchema>,
    pub(crate) mutable_locals: HashSet<u16>,
    // Source slots are globally unique before Core compaction. Only immutable
    // guest bindings acquire facts; runtime identity is checked before routing.
    pub(crate) const_callable_origins: HashMap<u16, ConstCallableOrigin>,
    pub(crate) closure_captures: Vec<ClosureCaptureContext>,
    pub(crate) imported_functions: HashMap<String, (vm::SymbolId, FunctionDecl)>,
    pub(crate) host_function_indices: HashMap<String, HashMap<u8, u16>>,
    pub(crate) host_parameter_passings: HashMap<String, HashMap<u8, Vec<vm::HostParamPassing>>>,
    pub(crate) declared_host_functions: HashMap<String, Vec<vm::HostFunctionSchema>>,
    pub(crate) host_api_metadata: Option<HostApiIrMetadata>,
    pub(crate) catalog_visibility: CatalogVisibility,
    pub(crate) named_types: HashMap<String, TypeSchema>,
    pub(crate) named_type_declarations: HashMap<String, StructDecl>,
    pub(crate) type_alias_schemas: HashMap<String, (Vec<String>, TypeSchema)>,
    pub(crate) struct_schemas: HashMap<String, StructDecl>,
    pub(crate) resource_keys: HashSet<String>,
    pub(crate) type_provenance: Vec<GuestTypeProvenance>,
    pub(crate) struct_decl_sites: Vec<StructDeclSite>,
    pub(crate) generic_type_params: HashSet<String>,
    pub(crate) function_indices: HashMap<String, (u16, u8)>,
    pub(crate) function_parameters: HashMap<String, Vec<ast::Pat>>,
    pub(crate) generic_function_indices: HashSet<u16>,
    /// Declared (annotation-only) signatures for module functions and
    /// explicitly typed arrows: argument schemas in order plus the declared
    /// return schema. Populated by `predeclare_functions` before any body
    /// lowers, so recursive and forward references observe the truthful
    /// declared facts while the full `FunctionDecl` (with inferred returns)
    /// is appended only after its body. Generic functions are never entered;
    /// inferred-only returns stay `None` until lowered. Consulted only when
    /// the full declaration is absent; never overrides it.
    pub(crate) predeclared_signatures:
        HashMap<String, (Vec<Option<TypeSchema>>, Option<TypeSchema>)>,
    pub(crate) functions: Vec<FunctionDecl>,
    pub(crate) function_impls: HashMap<u16, FunctionImpl>,
    pub(crate) function_sites: Vec<FunctionDeclSite>,
    pub(crate) function_scopes: Vec<ParsedLexicalScope>,
    pub(crate) call_sites: Vec<ParsedCallSite>,
    pub(crate) local_bindings: Vec<(String, u16)>,
    pub(crate) local_decls: Vec<LocalDeclSite>,
    pub(crate) local_refs: Vec<LocalRefSite>,
    pub(crate) stmt_spans: Vec<StmtSpanSite>,
    /// Lazily lowered generator constructors in reference-array mode: maps
    /// each private iterator nominal to its result representation. Plain
    /// mode never populates this; ordinary functions never consult it.
    pub(crate) gen_iterators: HashMap<String, super::generator_lowering::GeneratorInfo>,
    /// Reverse index from private result nominal to iterator nominal, so
    /// result-typed values resolve to their exact representation without
    /// name-based privileges for user shapes.
    pub(crate) gen_results: HashMap<String, String>,
    /// Module-level `const` initializers lowered so far (name to initializer
    /// AST plus declared/inferred schema), in source order. Powers
    /// pure-constant capture for isolated function bodies; shadowing, order
    /// and purity are enforced at use.
    pub(crate) module_const_inits: HashMap<String, (ast::Expr, Option<TypeSchema>)>,
    /// True while lowering a lazy numeric generator body (pre-scan and
    /// state machine). Integer-only arithmetic infers `Int` there, matching
    /// the exact integer backend path; every other context keeps the
    /// historical conservative `number`.
    pub(crate) in_gen_body: bool,
}

impl<'a> Lowerer<'a> {
    pub(crate) fn callable_origin_is_current(&self, origin: &ConstCallableOrigin) -> bool {
        match origin {
            ConstCallableOrigin::Local { name, index, arity } => {
                self.function_indices.get(name) == Some(&(*index, *arity))
            }
            ConstCallableOrigin::Imported {
                name,
                symbol,
                arity,
            } => self
                .imported_functions
                .get(name)
                .is_some_and(|(actual, decl)| actual == symbol && decl.arity == *arity),
        }
    }

    pub(crate) fn immutable_guest_callable_origin(
        &self,
        expression: &ast::Expr,
    ) -> Option<ConstCallableOrigin> {
        if !self.ref_mode || self.in_ref_prelude {
            return None;
        }
        let ident = match expression {
            ast::Expr::Ident(ident) => ident,
            ast::Expr::Paren(paren) => return self.immutable_guest_callable_origin(&paren.expr),
            _ => return None,
        };
        let name = ident.sym.to_string();
        if let Some(slot) = self.locals.get(&name) {
            if self.mutable_locals.contains(slot) {
                return None;
            }
            return self
                .const_callable_origins
                .get(slot)
                .filter(|origin| self.callable_origin_is_current(origin))
                .cloned();
        }
        // Private protocol functions and host values must never obtain guest
        // argument/default adaptations through a value alias.
        if name.starts_with("__compat_") || self.host_function_indices.contains_key(&name) {
            return None;
        }
        if let Some(&(index, arity)) = self.function_indices.get(&name) {
            return (!self.generic_function_indices.contains(&index))
                .then_some(ConstCallableOrigin::Local { name, index, arity });
        }
        self.imported_functions
            .get(&name)
            .and_then(|(symbol, decl)| {
                decl.type_params
                    .is_empty()
                    .then_some(ConstCallableOrigin::Imported {
                        name: name.clone(),
                        symbol: *symbol,
                        arity: decl.arity,
                    })
            })
    }

    pub(crate) fn alloc_node_id(&mut self) -> SemanticNodeId {
        let id = SemanticNodeId(self.next_node_id);
        self.next_node_id = self
            .next_node_id
            .checked_add(1)
            .expect("TypeScript semantic node id overflow");
        id
    }

    /// Lower one declared annotation for predeclare, mirroring the declared
    /// half of `lower_function` (including the user-region number-array
    /// rule). Failures stay `None` here; lowering reports them
    /// authoritatively when the body itself is lowered.
    fn predeclared_annotation(
        &self,
        annotation: &ast::TsType,
        span: swc_common::Span,
    ) -> Option<TypeSchema> {
        self.lower_type(annotation)
            .ok()
            .map(|schema| self.ref_map_number_array(schema, span))
    }

    /// Module function schemas by name: the lowered `FunctionDecl` once its
    /// body is lowered, else the predeclared annotation facts (recursive and
    /// forward references). `None` for generic functions and unknown names.
    /// Imported functions are resolved separately by callers.
    fn module_function_schemas(
        &self,
        name: &str,
    ) -> Option<(Vec<Option<TypeSchema>>, Option<TypeSchema>)> {
        let (index, _) = self.function_indices.get(name)?;
        if let Some(declaration) = self.functions.iter().find(|decl| decl.index == *index) {
            if declaration.type_params.is_empty() {
                return Some((
                    declaration.arg_schemas.clone(),
                    declaration.return_schema.clone(),
                ));
            }
            return None;
        }
        self.predeclared_signatures.get(name).cloned()
    }

    /// Argument schemas for a module function table index: the lowered
    /// declaration once present, else the predeclared annotation facts.
    /// Empty when unknown, matching the historical fallback.
    fn module_function_arg_schemas(&self, name: &str, index: u16) -> Vec<Option<TypeSchema>> {
        if let Some(declaration) = self.functions.iter().find(|decl| decl.index == index) {
            return declaration.arg_schemas.clone();
        }
        self.predeclared_signatures
            .get(name)
            .map(|(args, _)| args.clone())
            .unwrap_or_default()
    }

    fn predeclare_types(&mut self, module: &ast::Module) -> Result<(), ParseError> {
        let declarations = module
            .body
            .iter()
            .filter_map(type_declaration_item)
            .collect::<Vec<_>>();
        let mut names = HashSet::new();
        for declaration in &declarations {
            let (name, ident_span, type_params) = match declaration {
                TypeDeclaration::Interface(interface) => (
                    interface.id.sym.to_string(),
                    interface.id.span,
                    type_parameter_names(
                        interface.type_params.as_deref(),
                        self.base,
                        self.source_id,
                    )?,
                ),
                TypeDeclaration::Alias(alias) => (
                    alias.id.sym.to_string(),
                    alias.id.span,
                    type_parameter_names(alias.type_params.as_deref(), self.base, self.source_id)?,
                ),
            };
            if !names.insert(name.clone())
                || self.struct_schemas.contains_key(&name)
                || self.named_types.contains_key(&name)
            {
                return Err(ParseError::at_span(
                    byte_span(ident_span, self.base, self.source_id),
                    format!("duplicate TypeScript type '{name}'"),
                ));
            }
            match declaration {
                TypeDeclaration::Interface(_) => {
                    let generic_args = type_params
                        .iter()
                        .cloned()
                        .map(TypeSchema::GenericParam)
                        .collect();
                    self.named_types
                        .insert(name.clone(), TypeSchema::Named(name.clone(), generic_args));
                    let declaration = StructDecl {
                        name: name.clone(),
                        type_params,
                        body_schema: TypeSchema::Unknown,
                        origin: StructDeclOrigin::Guest,
                    };
                    self.named_type_declarations
                        .insert(name.clone(), declaration.clone());
                    self.struct_schemas.insert(name, declaration);
                }
                TypeDeclaration::Alias(_) => {
                    self.named_types.insert(name.clone(), TypeSchema::Unknown);
                    self.type_alias_schemas
                        .insert(name, (type_params, TypeSchema::Unknown));
                }
            }
        }

        for declaration in declarations {
            let (name, ident_span, declaration_span, type_params, body_schema, is_alias) =
                match declaration {
                    TypeDeclaration::Interface(interface) => {
                        if !interface.extends.is_empty() {
                            return Err(unsupported(
                                interface.span,
                                self.base,
                                self.source_id,
                                "interface inheritance",
                            ));
                        }
                        let type_params = type_parameter_names(
                            interface.type_params.as_deref(),
                            self.base,
                            self.source_id,
                        )?;
                        let previous_params = std::mem::replace(
                            &mut self.generic_type_params,
                            type_params.iter().cloned().collect(),
                        );
                        let fields = self.lower_type_members(&interface.body.body)?;
                        self.generic_type_params = previous_params;
                        (
                            interface.id.sym.to_string(),
                            interface.id.span,
                            interface.span,
                            type_params,
                            TypeSchema::Object(fields),
                            false,
                        )
                    }
                    TypeDeclaration::Alias(alias) => {
                        let type_params = type_parameter_names(
                            alias.type_params.as_deref(),
                            self.base,
                            self.source_id,
                        )?;
                        let previous_params = std::mem::replace(
                            &mut self.generic_type_params,
                            type_params.iter().cloned().collect(),
                        );
                        let body = self.lower_type(&alias.type_ann);
                        self.generic_type_params = previous_params;
                        (
                            alias.id.sym.to_string(),
                            alias.id.span,
                            alias.span,
                            type_params,
                            body?,
                            true,
                        )
                    }
                };
            let declaration_span = byte_span(declaration_span, self.base, self.source_id);
            let declaration = StructDecl {
                name: name.clone(),
                type_params: type_params.clone(),
                body_schema: body_schema.clone(),
                origin: StructDeclOrigin::Guest,
            };
            if is_alias {
                self.type_alias_schemas
                    .insert(name.clone(), (type_params, body_schema.clone()));
                self.named_types.insert(name.clone(), body_schema);
                self.named_type_declarations.remove(&name);
                self.struct_schemas.insert(name.clone(), declaration);
            } else {
                self.named_type_declarations
                    .insert(name.clone(), declaration.clone());
                self.struct_schemas.insert(name.clone(), declaration);
                self.type_provenance.push(GuestTypeProvenance {
                    canonical_name: name.clone(),
                    source_name: name.clone(),
                    source_id: self.source_id,
                    declaration_span: Some(declaration_span),
                });
            }
            let id = self.alloc_node_id();
            self.struct_decl_sites.push(StructDeclSite {
                id,
                ident_span: byte_span(ident_span, self.base, self.source_id),
                decl_span: declaration_span,
                name,
                scope_id: 0,
            });
        }
        Ok(())
    }

    fn lower_type_members(
        &self,
        members: &[ast::TsTypeElement],
    ) -> Result<HashMap<String, TypeSchema>, ParseError> {
        let mut fields = HashMap::new();
        for element in members {
            let ast::TsTypeElement::TsPropertySignature(property) = element else {
                return Err(unsupported(
                    element.span(),
                    self.base,
                    self.source_id,
                    "object type member",
                ));
            };
            if property.computed || property.readonly {
                return Err(unsupported(
                    property.span,
                    self.base,
                    self.source_id,
                    "computed or readonly object member",
                ));
            }
            let key = match &*property.key {
                ast::Expr::Ident(ident) => ident.sym.to_string(),
                ast::Expr::Lit(ast::Lit::Str(value)) => value.value.to_string_lossy().into_owned(),
                other => {
                    return Err(unsupported(
                        other.span(),
                        self.base,
                        self.source_id,
                        "object property name",
                    ));
                }
            };
            let Some(annotation) = property.type_ann.as_ref() else {
                return Err(ParseError::at_span(
                    byte_span(property.span, self.base, self.source_id),
                    format!("TypeScript noImplicitAny: property '{key}' requires a type"),
                ));
            };
            let mut schema = self.lower_type(&annotation.type_ann)?;
            if property.optional {
                schema = TypeSchema::Optional(Box::new(schema));
            }
            if fields.insert(key.clone(), schema).is_some() {
                return Err(ParseError::at_span(
                    byte_span(property.span, self.base, self.source_id),
                    format!("duplicate TypeScript property '{key}'"),
                ));
            }
        }
        Ok(fields)
    }

    fn declared_host_type(
        &self,
        ty: &ast::TsType,
        aliases: &HashMap<String, &ast::TsTypeAliasDecl>,
    ) -> Result<DeclaredHostType, ParseError> {
        let schema = self.lower_type(ty)?;
        let mut guest_types = DeclaredGuestTypeNames::new(&self.named_type_declarations, aliases);
        guest_types.collect(ty);
        Ok(DeclaredHostType {
            schema,
            guest_type_names: guest_types.names,
        })
    }

    fn schema_contains_named_type(schema: &TypeSchema, names: &HashSet<String>) -> bool {
        match schema {
            TypeSchema::Optional(inner) | TypeSchema::Array(inner) | TypeSchema::Map(inner) => {
                Self::schema_contains_named_type(inner, names)
            }
            TypeSchema::Named(name, arguments) => {
                names.contains(name)
                    || arguments
                        .iter()
                        .any(|argument| Self::schema_contains_named_type(argument, names))
            }
            TypeSchema::ArrayTuple(items) => items
                .iter()
                .any(|item| Self::schema_contains_named_type(item, names)),
            TypeSchema::ArrayTupleRest { prefix, rest } => {
                prefix
                    .iter()
                    .any(|item| Self::schema_contains_named_type(item, names))
                    || Self::schema_contains_named_type(rest, names)
            }
            TypeSchema::Object(fields) => fields
                .values()
                .any(|field| Self::schema_contains_named_type(field, names)),
            TypeSchema::Callable { params, result } => {
                params
                    .iter()
                    .any(|param| Self::schema_contains_named_type(param, names))
                    || Self::schema_contains_named_type(result, names)
            }
            TypeSchema::Unknown
            | TypeSchema::Null
            | TypeSchema::Int
            | TypeSchema::Float
            | TypeSchema::Number
            | TypeSchema::Bool
            | TypeSchema::String
            | TypeSchema::Bytes
            | TypeSchema::Resource(_)
            | TypeSchema::GenericParam(_) => false,
        }
    }

    fn declaration_type_matches_catalog(
        declaration: &DeclaredHostType,
        catalog_type: &vm::HostTypeSchema,
    ) -> bool {
        let catalog_schema = Self::host_type_schema(catalog_type);
        declaration.schema == catalog_schema
            && !Self::schema_contains_named_type(&catalog_schema, &declaration.guest_type_names)
    }

    fn declared_host_signature(
        &self,
        function: &ast::FnDecl,
        aliases: &HashMap<String, &ast::TsTypeAliasDecl>,
    ) -> Result<(Vec<DeclaredHostType>, DeclaredHostType), ParseError> {
        if !function.declare || function.function.body.is_some() {
            return Err(unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "host declaration body",
            ));
        }
        if function
            .function
            .type_params
            .as_ref()
            .is_some_and(|params| !params.params.is_empty())
        {
            return Err(unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "generic host declaration",
            ));
        }
        let mut parameters = Vec::new();
        for param in &function.function.params {
            let ast::Pat::Ident(binding) = &param.pat else {
                return Err(unsupported(
                    param.span(),
                    self.base,
                    self.source_id,
                    "host declaration parameter pattern",
                ));
            };
            let Some(annotation) = binding.type_ann.as_ref() else {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!(
                        "TypeScript noImplicitAny: host declaration parameter '{}' requires a type",
                        binding.id.sym
                    ),
                ));
            };
            parameters.push(self.declared_host_type(&annotation.type_ann, aliases)?);
        }
        let Some(return_type) = function.function.return_type.as_ref() else {
            return Err(ParseError::at_span(
                byte_span(function.ident.span, self.base, self.source_id),
                format!(
                    "TypeScript host declaration '{}' requires a return type",
                    function.ident.sym
                ),
            ));
        };
        Ok((
            parameters,
            self.declared_host_type(&return_type.type_ann, aliases)?,
        ))
    }

    fn predeclare_functions(&mut self, module: &ast::Module) -> Result<(), ParseError> {
        let type_aliases = module
            .body
            .iter()
            .filter_map(type_declaration_item)
            .filter_map(|declaration| match declaration {
                TypeDeclaration::Alias(alias) => Some((alias.id.sym.to_string(), alias)),
                TypeDeclaration::Interface(_) => None,
            })
            .collect::<HashMap<_, _>>();
        let mut declarations = HashMap::<String, Vec<(Vec<TypeSchema>, TypeSchema, Span)>>::new();
        for item in &module.body {
            let Some((function, exported)) = function_item(item) else {
                continue;
            };
            let name = function.ident.sym.to_string();
            if function.declare {
                let declaration_span = byte_span(function.function.span, self.base, self.source_id);
                if exported {
                    return Err(unsupported(
                        function.ident.span,
                        self.base,
                        self.source_id,
                        "exported host declaration",
                    ));
                }
                if self.function_indices.contains_key(&name)
                    || self.imported_functions.contains_key(&name)
                {
                    return Err(ParseError::at_span(
                        byte_span(function.ident.span, self.base, self.source_id),
                        format!("host declaration '{name}' conflicts with another function"),
                    ));
                }
                let Some(candidates) = self.declared_host_functions.get(&name) else {
                    return Err(ParseError::at_span(
                        byte_span(function.ident.span, self.base, self.source_id),
                        format!("host catalog does not declare '{name}'"),
                    ));
                };
                let signature = self.declared_host_signature(function, &type_aliases)?;
                if !candidates.iter().any(|candidate| {
                    candidate.params.len() == signature.0.len()
                        && candidate
                            .params
                            .iter()
                            .zip(&signature.0)
                            .all(|(parameter, declared)| {
                                Self::declaration_type_matches_catalog(declared, &parameter.ty)
                            })
                        && Self::declaration_type_matches_catalog(
                            &signature.1,
                            &candidate.return_type,
                        )
                }) {
                    return Err(ParseError::at_span(
                        declaration_span,
                        format!("host declaration '{name}' does not match its catalog signature"),
                    ));
                }
                let parameter_schemas = signature
                    .0
                    .into_iter()
                    .map(|parameter| parameter.schema)
                    .collect();
                declarations.entry(name).or_default().push((
                    parameter_schemas,
                    signature.1.schema,
                    declaration_span,
                ));
                continue;
            }
            let arity = u8::try_from(function.function.params.len()).map_err(|_| {
                unsupported(
                    function.function.span,
                    self.base,
                    self.source_id,
                    "function arity",
                )
            })?;
            self.function_parameters.insert(
                name.clone(),
                function
                    .function
                    .params
                    .iter()
                    .map(|parameter| parameter.pat.clone())
                    .collect(),
            );
            if self.function_indices.contains_key(&name)
                || self.host_function_indices.contains_key(&name)
                || self.imported_functions.contains_key(&name)
            {
                return Err(ParseError::at_span(
                    byte_span(function.ident.span, self.base, self.source_id),
                    format!("duplicate TypeScript function '{name}'"),
                ));
            }
            let index = self.next_function_index;
            self.next_function_index =
                self.next_function_index.checked_add(1).ok_or_else(|| {
                    unsupported(
                        function.ident.span,
                        self.base,
                        self.source_id,
                        "function table size",
                    )
                })?;
            if function
                .function
                .type_params
                .as_ref()
                .is_some_and(|params| !params.params.is_empty())
            {
                self.generic_function_indices.insert(index);
            }
            self.function_indices.insert(name.clone(), (index, arity));
            // Non-generic functions publish their declared (annotation-only)
            // facts now, so recursive and forward references lower against
            // the same declared schemas as later calls. Inferred-only
            // returns stay `None` until the body is lowered.
            if function
                .function
                .type_params
                .as_ref()
                .is_none_or(|params| params.params.is_empty())
            {
                let mut arg_schemas = Vec::with_capacity(function.function.params.len());
                for param in &function.function.params {
                    arg_schemas.push(parameter_binding(&param.pat).and_then(|(ident, _)| {
                        ident.type_ann.as_ref().and_then(|annotation| {
                            self.predeclared_annotation(&annotation.type_ann, ident.id.span)
                        })
                    }));
                }
                let return_schema = function
                    .function
                    .return_type
                    .as_ref()
                    .and_then(|annotation| {
                        self.predeclared_annotation(&annotation.type_ann, annotation.span)
                    });
                self.predeclared_signatures
                    .insert(name.clone(), (arg_schemas, return_schema));
            }
        }
        for item in &module.body {
            let Some((declaration, exported)) = variable_declaration_item(item) else {
                continue;
            };
            for declarator in &declaration.decls {
                let (ast::Pat::Ident(binding), Some(initializer)) =
                    (&declarator.name, &declarator.init)
                else {
                    continue;
                };
                let ast::Expr::Arrow(arrow) = initializer.as_ref() else {
                    continue;
                };
                let explicitly_typed = exported
                    || arrow.return_type.is_some()
                    || arrow.params.iter().any(|parameter| {
                        matches!(parameter, ast::Pat::Ident(binding) if binding.type_ann.is_some())
                    });
                if !explicitly_typed {
                    continue;
                }
                let name = binding.id.sym.to_string();
                if self.function_indices.contains_key(&name)
                    || self.host_function_indices.contains_key(&name)
                    || self.imported_functions.contains_key(&name)
                {
                    return Err(ParseError::at_span(
                        byte_span(binding.id.span, self.base, self.source_id),
                        format!("duplicate TypeScript function '{name}'"),
                    ));
                }
                let arity = u8::try_from(arrow.params.len()).map_err(|_| {
                    unsupported(arrow.span, self.base, self.source_id, "function arity")
                })?;
                let index = self.next_function_index;
                self.next_function_index =
                    self.next_function_index.checked_add(1).ok_or_else(|| {
                        unsupported(
                            binding.id.span,
                            self.base,
                            self.source_id,
                            "function table size",
                        )
                    })?;
                if arrow
                    .type_params
                    .as_ref()
                    .is_some_and(|params| !params.params.is_empty())
                {
                    self.generic_function_indices.insert(index);
                }
                self.function_parameters
                    .insert(name.clone(), arrow.params.clone());
                self.function_indices.insert(name.clone(), (index, arity));
                // Same declared-fact publication as function declarations:
                // non-generic explicitly typed arrows serve recursive and
                // forward references from their annotations.
                if arrow
                    .type_params
                    .as_ref()
                    .is_none_or(|params| params.params.is_empty())
                {
                    let mut arg_schemas = Vec::with_capacity(arrow.params.len());
                    for pat in &arrow.params {
                        arg_schemas.push(parameter_binding(pat).and_then(|(ident, _)| {
                            ident.type_ann.as_ref().and_then(|annotation| {
                                self.predeclared_annotation(&annotation.type_ann, ident.id.span)
                            })
                        }));
                    }
                    let return_schema = arrow.return_type.as_ref().and_then(|annotation| {
                        self.predeclared_annotation(&annotation.type_ann, annotation.span)
                    });
                    self.predeclared_signatures
                        .insert(name.clone(), (arg_schemas, return_schema));
                }
            }
        }
        for (name, signatures) in declarations {
            let candidates = self
                .declared_host_functions
                .get(&name)
                .expect("host declarations were resolved before lowering");
            for candidate in candidates {
                let candidate_parameters = candidate
                    .params
                    .iter()
                    .map(|parameter| Self::host_type_schema(&parameter.ty))
                    .collect::<Vec<_>>();
                let candidate_result = Self::host_type_schema(&candidate.return_type);
                if !signatures.iter().any(|(parameters, result, _)| {
                    *parameters == candidate_parameters && *result == candidate_result
                }) {
                    return Err(ParseError::at_span(
                        signatures[0].2,
                        format!("host declaration '{name}' omits a catalog overload"),
                    ));
                }
            }
        }
        Ok(())
    }

    fn lower(mut self, module: ast::Module) -> Result<FrontendIr, ParseError> {
        self.predeclare_types(&module)?;
        self.predeclare_functions(&module)?;
        let mut stmts = Vec::new();
        let total = module.body.len();
        let prelude_count = self.ref_prelude_items.min(total);
        for (position, item) in module.body.into_iter().enumerate() {
            // Hygienic provenance: leading prelude items keep plain array
            // semantics; user items map logical number[] to the carrier.
            self.in_ref_prelude = self.ref_mode && position < prelude_count;
            match item {
                ast::ModuleItem::Stmt(ast::Stmt::Decl(
                    ast::Decl::TsInterface(_) | ast::Decl::TsTypeAlias(_),
                )) => continue,
                ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::Fn(function)))
                    if function.declare =>
                {
                    continue;
                }
                ast::ModuleItem::Stmt(stmt) => match stmt {
                    ast::Stmt::Decl(ast::Decl::Fn(function)) => {
                        self.lower_function(&function, false, &mut stmts)?;
                    }
                    other => self.lower_stmt(other, &mut stmts)?,
                },
                ast::ModuleItem::ModuleDecl(ast::ModuleDecl::Import(_)) => continue,
                ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDecl(export)) => {
                    match export.decl {
                        ast::Decl::Fn(function) => {
                            self.lower_function(&function, true, &mut stmts)?
                        }
                        ast::Decl::TsInterface(_) | ast::Decl::TsTypeAlias(_) => continue,
                        ast::Decl::Var(declaration) => {
                            self.lower_var_decl(*declaration, true, &mut stmts)?
                        }
                        other => {
                            return Err(unsupported(
                                other.span(),
                                self.base,
                                self.source_id,
                                "export declaration",
                            ));
                        }
                    }
                }
                ast::ModuleItem::ModuleDecl(other) => {
                    return Err(unsupported(
                        other.span(),
                        self.base,
                        self.source_id,
                        "module declaration",
                    ));
                }
            }
        }
        // Callee-owned default thunks/carriers/getters for reference-array
        // mode (plain mode untouched). Must run before root collection so
        // synthetic top-level FuncDecls join the same IR.
        self.generate_default_callables(&mut stmts)?;
        let root_declarations = self
            .local_decls
            .iter()
            .filter(|site| site.scope_id == 0)
            .map(|site| site.slot)
            .collect();
        let root_functions = self
            .functions
            .iter()
            .map(|function| function.index)
            .collect();
        let mut parsed = ParsedSemanticIndex::default();
        parsed.scopes.push(ParsedLexicalScope {
            id: 0,
            parent: None,
            range: Span::new(self.source_id, 0, self.source.len()),
            declarations: root_declarations,
            functions: root_functions,
        });
        parsed.scopes.extend(self.function_scopes);
        parsed.local_decls = self.local_decls;
        parsed.local_refs = self.local_refs;
        parsed.func_decls = self.function_sites;
        parsed.struct_decls = self.struct_decl_sites;
        parsed.type_provenance = self.type_provenance;
        parsed.call_sites = self.call_sites;
        parsed.stmt_spans = self.stmt_spans;
        parsed.next_node_id = self.next_node_id;
        parsed.next_scope_id = self.next_scope_id;
        Ok(FrontendIr {
            stmts,
            locals: usize::from(self.next_slot),
            local_bindings: self.local_bindings,
            struct_schemas: self.struct_schemas,
            unknown_type_spans: Vec::new(),
            functions: self.functions,
            function_impls: self.function_impls,
            stmt_sources: Vec::new(),
            function_sources: HashMap::new(),
            use_declarations: Vec::new(),
            implicit_extern_names: Vec::new(),
            host_api_metadata: self.host_api_metadata,
            semantic_index: None,
            parsed_semantic_index: Some(parsed),
            catalog_visibility: Some(self.catalog_visibility),
            lexer_tokens: Vec::new(),
        })
    }

    /// True when an ordinary body needs the early-return transform: any
    /// `return` outside the single tail position. Nested closures own their
    /// returns and never trigger this. Bodies without early returns keep
    /// the exact historical lowering.
    fn body_has_early_return(stmts: &[ast::Stmt]) -> bool {
        for (position, stmt) in stmts.iter().enumerate() {
            if Self::stmt_has_non_tail_return(stmt, position + 1 == stmts.len()) {
                return true;
            }
        }
        false
    }

    fn stmt_has_non_tail_return(stmt: &ast::Stmt, is_tail: bool) -> bool {
        match stmt {
            ast::Stmt::Return(_) => !is_tail,
            ast::Stmt::Block(block) => block
                .stmts
                .iter()
                .any(|stmt| Self::stmt_has_non_tail_return(stmt, false)),
            ast::Stmt::If(if_stmt) => {
                Self::stmt_has_non_tail_return(&if_stmt.cons, false)
                    || if_stmt
                        .alt
                        .as_ref()
                        .is_some_and(|alt| Self::stmt_has_non_tail_return(alt, false))
            }
            // Returns inside loops (and any other unhandled position) still
            // count as non-tail so the transform runs and rejects
            // loop-carried returns precisely instead of silently keeping the
            // legacy rejection.
            ast::Stmt::While(while_stmt) => Self::stmt_contains_return(&while_stmt.body),
            ast::Stmt::For(for_stmt) => Self::stmt_contains_return(&for_stmt.body),
            _ => false,
        }
    }

    fn stmt_contains_return(stmt: &ast::Stmt) -> bool {
        match stmt {
            ast::Stmt::Return(_) => true,
            ast::Stmt::Block(block) => block.stmts.iter().any(Self::stmt_contains_return),
            ast::Stmt::If(if_stmt) => {
                Self::stmt_contains_return(&if_stmt.cons)
                    || if_stmt
                        .alt
                        .as_ref()
                        .is_some_and(|alt| Self::stmt_contains_return(alt))
            }
            ast::Stmt::While(while_stmt) => Self::stmt_contains_return(&while_stmt.body),
            ast::Stmt::For(for_stmt) => Self::stmt_contains_return(&for_stmt.body),
            _ => false,
        }
    }

    /// Lower an ordinary body containing early returns. The first early
    /// return allocates a hidden done flag (plus a hidden result cell when
    /// any return carries a value); every later top-level statement runs
    /// guarded by the flag, and the body evaluates to the result cell when
    /// set. Evaluation order, once-only semantics and error behavior match
    /// straight-line lowering exactly; loop-carried returns stay precisely
    /// rejected.
    fn lower_body_with_early_returns(
        &mut self,
        stmts: Vec<ast::Stmt>,
        body_span: swc_common::Span,
        declared_return: Option<TypeSchema>,
    ) -> Result<(Vec<Stmt>, Expr, u32), ParseError> {
        let mut valued_schemas: Vec<Option<TypeSchema>> = Vec::new();
        let mut bare_count = 0usize;
        let mut last_return_span: Option<swc_common::Span> = None;
        Self::collect_body_returns(
            self,
            &stmts,
            false,
            &mut valued_schemas,
            &mut bare_count,
            &mut last_return_span,
        )?;
        let has_valued = !valued_schemas.is_empty();
        if has_valued && bare_count > 0 {
            return Err(ParseError::at_span(
                byte_span(body_span, self.base, self.source_id),
                "mixed bare and valued returns in function body".to_owned(),
            ));
        }
        let mut joined: Option<TypeSchema> = None;
        for schema in valued_schemas {
            match (joined.take(), schema) {
                (None, schema) => joined = schema,
                (Some(previous), Some(schema)) if previous == schema => {
                    joined = Some(schema);
                }
                _ => joined = Some(TypeSchema::Number),
            }
        }
        let body_site = byte_span(body_span, self.base, self.source_id);
        let body_line = line_at(self.source, body_site.lo);
        let done_slot = self.alloc_hidden_local(body_span)?;
        let mut prefix = vec![Stmt::Let {
            index: done_slot,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(false),
            line: body_line,
        }];
        let ret_slot = if has_valued {
            let schema = declared_return.or(joined).unwrap_or(TypeSchema::Number);
            let slot = self.alloc_hidden_local(body_span)?;
            prefix.push(Stmt::Let {
                index: slot,
                declared_schema: Some(schema.clone()),
                expr: match schema {
                    TypeSchema::Float => Expr::Float(0.0),
                    TypeSchema::Int => Expr::Int(0),
                    TypeSchema::Bool => Expr::Bool(false),
                    TypeSchema::Null => Expr::Null,
                    _ => Expr::Int(0),
                },
                line: body_line,
            });
            Some(slot)
        } else {
            None
        };
        let mut fix = EarlyReturnFix {
            done: done_slot,
            ret: ret_slot,
            valued: has_valued,
        };
        let mut lowered = Vec::new();
        let tail_is_valued_return = matches!(
            stmts.last(),
            Some(ast::Stmt::Return(ret)) if ret.arg.is_some()
        );
        self.lower_seq_with_returns(stmts, &mut fix, &mut lowered)?;
        prefix.extend(lowered);
        // A valued tail return always executes when reached, so no
        // fallthrough value is possible and the cell reads directly;
        // otherwise an unset flag honestly yields Null like before.
        let line = last_return_span
            .map(|span| {
                let site = byte_span(span, self.base, self.source_id);
                line_at(self.source, site.lo)
            })
            .unwrap_or(body_line);
        let expr = match ret_slot {
            Some(slot) if tail_is_valued_return => Expr::Var(slot),
            Some(slot) => Expr::IfElse {
                condition: Box::new(Expr::Var(done_slot)),
                then_expr: Box::new(Expr::Var(slot)),
                else_expr: Box::new(Expr::Null),
            },
            None => Expr::Null,
        };
        Ok((prefix, expr, line))
    }

    fn collect_body_returns(
        lowerer: &mut Lowerer<'_>,
        stmts: &[ast::Stmt],
        in_loop: bool,
        valued_schemas: &mut Vec<Option<TypeSchema>>,
        bare_count: &mut usize,
        last_span: &mut Option<swc_common::Span>,
    ) -> Result<(), ParseError> {
        for stmt in stmts {
            match stmt {
                ast::Stmt::Return(ret) => {
                    if in_loop {
                        return Err(unsupported(
                            ret.span,
                            lowerer.base,
                            lowerer.source_id,
                            "return inside a loop in ordinary function bodies",
                        ));
                    }
                    *last_span = Some(ret.span);
                    match &ret.arg {
                        Some(value) => {
                            valued_schemas.push(lowerer.expression_schema(value));
                        }
                        None => *bare_count += 1,
                    }
                }
                ast::Stmt::Block(block) => Self::collect_body_returns(
                    lowerer,
                    &block.stmts,
                    in_loop,
                    valued_schemas,
                    bare_count,
                    last_span,
                )?,
                ast::Stmt::If(if_stmt) => {
                    Self::collect_body_returns(
                        lowerer,
                        std::slice::from_ref(if_stmt.cons.as_ref()),
                        in_loop,
                        valued_schemas,
                        bare_count,
                        last_span,
                    )?;
                    if let Some(alt) = &if_stmt.alt {
                        Self::collect_body_returns(
                            lowerer,
                            std::slice::from_ref(alt.as_ref()),
                            in_loop,
                            valued_schemas,
                            bare_count,
                            last_span,
                        )?;
                    }
                }
                ast::Stmt::While(while_stmt) => Self::collect_body_returns(
                    lowerer,
                    std::slice::from_ref(while_stmt.body.as_ref()),
                    true,
                    valued_schemas,
                    bare_count,
                    last_span,
                )?,
                ast::Stmt::For(for_stmt) => Self::collect_body_returns(
                    lowerer,
                    std::slice::from_ref(for_stmt.body.as_ref()),
                    true,
                    valued_schemas,
                    bare_count,
                    last_span,
                )?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Sequence ordinary statements with early-return guards. The list
    /// splits at the first return-containing statement: the prefix lowers
    /// exactly as before, the return statement itself becomes one atomic
    /// done-guarded unit, and the whole remainder runs under a single done
    /// guard (recursively transformed). Declarations therefore always
    /// dominate their uses within one guard branch, which the existing
    /// availability analysis accepts; a direct non-final `return` keeps the
    /// historical rejection. Blocks open lexical scopes like before; guard
    /// wrappers themselves never introduce scopes. Guards branch on the
    /// Bool cell directly to preserve native lowering.
    fn lower_seq_with_returns(
        &mut self,
        stmts: Vec<ast::Stmt>,
        fix: &mut EarlyReturnFix,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let mut split = stmts.len();
        for (index, stmt) in stmts.iter().enumerate() {
            if Self::stmt_has_non_tail_return(stmt, false) {
                split = index;
                break;
            }
        }
        let mut rest = stmts;
        let mut tail = rest.split_off(split);
        for stmt in rest {
            let mut buffer = Vec::new();
            self.with_consumed_stmt_values(|this| this.lower_stmt(stmt, &mut buffer))?;
            output.extend(buffer);
        }
        if tail.is_empty() {
            return Ok(());
        }
        let stmt = tail.remove(0);
        match stmt {
            ast::Stmt::Return(ret) => {
                // A direct non-final return keeps the historical rejection;
                // nested returns become atomic guarded units below.
                if !tail.is_empty() {
                    return Err(unsupported(
                        ret.span,
                        self.base,
                        self.source_id,
                        "non-final return",
                    ));
                }
                self.push_atomic_return(ret, fix, output)?;
            }
            ast::Stmt::If(if_stmt) => {
                let span = byte_span(if_stmt.span, self.base, self.source_id);
                let line = line_at(self.source, span.lo);
                let condition = self.lower_expr(*if_stmt.test)?;
                let mut then_branch = Vec::new();
                self.lower_seq_with_returns(vec![*if_stmt.cons], fix, &mut then_branch)?;
                let mut else_branch = Vec::new();
                if let Some(alt) = if_stmt.alt {
                    self.lower_seq_with_returns(vec![*alt], fix, &mut else_branch)?;
                }
                output.push(Stmt::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    line,
                });
                self.stmt_spans.push(StmtSpanSite { line, span });
            }
            ast::Stmt::Block(block) => {
                let block_span = block.span;
                let parent_scope = self.scope_id;
                let previous_locals = self.locals.clone();
                let previous_local_types = self.local_types.clone();
                let previous_mutable = self.mutable_locals.clone();
                let scope_id = self.next_scope_id;
                self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
                    ParseError::at_span(
                        byte_span(block_span, self.base, self.source_id),
                        "TypeScript lexical scope count exceeded".to_owned(),
                    )
                })?;
                self.scope_id = scope_id;
                let declaration_start = self.local_decls.len();
                let mut buffer = Vec::new();
                let result = self.lower_seq_with_returns(block.stmts, fix, &mut buffer);
                let declarations = self.local_decls[declaration_start..]
                    .iter()
                    .filter(|site| site.scope_id == scope_id)
                    .map(|site| site.slot)
                    .collect();
                self.function_scopes.push(ParsedLexicalScope {
                    id: scope_id,
                    parent: Some(parent_scope),
                    range: byte_span(block_span, self.base, self.source_id),
                    declarations,
                    functions: Vec::new(),
                });
                self.scope_id = parent_scope;
                self.locals = previous_locals;
                self.local_types = previous_local_types;
                self.mutable_locals = previous_mutable;
                result?;
                output.extend(buffer);
            }
            other => {
                // Loops were cleared of returns by collection; anything else
                // reaching the split position lowers historically (any return
                // inside still fails there exactly as before).
                let mut buffer = Vec::new();
                self.with_consumed_stmt_values(|this| this.lower_stmt(other, &mut buffer))?;
                output.extend(buffer);
            }
        }
        if !tail.is_empty() {
            let rest_span = tail
                .first()
                .map(|stmt| stmt.span())
                .expect("guarded remainder is non-empty");
            let mut rest_lowered = Vec::new();
            self.lower_seq_with_returns(tail, fix, &mut rest_lowered)?;
            let site = byte_span(rest_span, self.base, self.source_id);
            let line = line_at(self.source, site.lo);
            output.push(Stmt::IfElse {
                condition: Expr::Var(fix.done),
                then_branch: Vec::new(),
                else_branch: rest_lowered,
                line,
            });
            self.stmt_spans.push(StmtSpanSite { line, span: site });
        }
        Ok(())
    }

    /// Emit one return as a single atomic done-guarded unit: when the flag
    /// is unset the value cell and the flag update together; when set (an
    /// earlier return won) the whole unit is skipped, so later returns can
    /// never clobber earlier ones.
    fn push_atomic_return(
        &mut self,
        ret: ast::ReturnStmt,
        fix: &EarlyReturnFix,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let span = byte_span(ret.span, self.base, self.source_id);
        let line = line_at(self.source, span.lo);
        let mut unit = Vec::new();
        if fix.valued {
            let Some(value) = ret.arg else {
                return Err(ParseError::at_span(
                    span,
                    "bare return with valued returns in function body".to_owned(),
                ));
            };
            let lowered = self.lower_expr(*value)?;
            let slot = fix.ret.expect("valued mode allocates a result cell");
            unit.push(Stmt::Assign {
                kind: AssignmentKind::Set,
                index: slot,
                expr: lowered,
                line,
            });
        } else if ret.arg.is_some() {
            return Err(ParseError::at_span(
                span,
                "valued return with bare returns in function body".to_owned(),
            ));
        }
        unit.push(Stmt::Assign {
            kind: AssignmentKind::Set,
            index: fix.done,
            expr: Expr::Bool(true),
            line,
        });
        output.push(Stmt::IfElse {
            condition: Expr::Var(fix.done),
            then_branch: Vec::new(),
            else_branch: unit,
            line,
        });
        self.stmt_spans.push(StmtSpanSite { line, span });
        Ok(())
    }

    /// Inline pure module-constant captures for an isolated function body.
    ///
    /// Function bodies lower in isolation and cannot observe module bindings,
    /// but an explicitly-typed module arrow closing over module constants
    /// (like the upstream formula arrow over `sqrt5`/`phi`/`psi`) is still
    /// an ordinary exported function and must execute unchanged. For every
    /// body-referenced name that is otherwise unresolvable and denotes a
    /// previously lowered module `const` with a pure initializer, this
    /// materializes a function-local copy from the fully expanded
    /// initializer. Pure initializers recompute bit-identical values, so no
    /// observation can distinguish the copy; impure, cyclic, mutable,
    /// shadowed or later-declared bindings stay precisely rejected.
    fn inline_module_consts(
        &mut self,
        body: &ast::FunctionBody,
        arg_names: &[String],
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if !self.ref_mode {
            return Ok(());
        }
        let mut bound = HashSet::new();
        let mut used = Vec::new();
        for stmt in &body.stmts {
            Self::collect_body_idents(stmt, false, &mut bound, &mut used);
        }
        let mut needed = Vec::new();
        let mut seen = HashSet::new();
        for name in used {
            if bound.contains(&name) || arg_names.contains(&name) {
                continue;
            }
            if self.function_indices.contains_key(&name)
                || self.imported_functions.contains_key(&name)
                || self.host_function_indices.contains_key(&name)
            {
                continue;
            }
            if !self.module_const_inits.contains_key(&name) {
                continue;
            }
            if seen.insert(name.clone()) {
                needed.push(name);
            }
        }
        if needed.is_empty() {
            return Ok(());
        }
        let line = line_at(
            self.source,
            byte_span(body.span, self.base, self.source_id).lo,
        );
        for name in needed {
            let mut visiting = Vec::new();
            let expanded = self.expand_const_init(&name, &mut visiting)?;
            let schema = self
                .module_const_inits
                .get(&name)
                .and_then(|(_, schema)| schema.clone())
                .or_else(|| self.expression_schema(&expanded))
                .ok_or_else(|| {
                    ParseError::at_span(
                        byte_span(body.span, self.base, self.source_id),
                        format!("captured module constant '{name}' has no usable type"),
                    )
                })?;
            let slot = self.alloc_hidden_local(body.span)?;
            let lowered = self.lower_expr(expanded)?;
            output.push(Stmt::Let {
                index: slot,
                declared_schema: Some(schema.clone()),
                expr: lowered,
                line,
            });
            self.locals.insert(name.clone(), slot);
            self.local_types.insert(name, schema);
        }
        Ok(())
    }

    /// Collect identifier uses and body-local bindings in one walk. Bindings
    /// inside nested closures never count as body bindings (they cannot
    /// shadow the module scope from here); uses count everywhere so nested
    /// closures capturing through the function scope keep working.
    fn collect_body_idents(
        stmt: &ast::Stmt,
        in_closure: bool,
        bound: &mut HashSet<String>,
        used: &mut Vec<String>,
    ) {
        match stmt {
            ast::Stmt::Expr(expr) => Self::collect_expr_idents(&expr.expr, in_closure, bound, used),
            ast::Stmt::Decl(ast::Decl::Var(decl)) => {
                for declarator in &decl.decls {
                    if let ast::Pat::Ident(binding) = &declarator.name
                        && !in_closure
                    {
                        bound.insert(binding.id.sym.to_string());
                    }
                    if let Some(init) = &declarator.init {
                        Self::collect_expr_idents(init, in_closure, bound, used);
                    }
                }
            }
            ast::Stmt::Decl(_) => {}
            ast::Stmt::Block(block) => {
                for stmt in &block.stmts {
                    Self::collect_body_idents(stmt, in_closure, bound, used);
                }
            }
            ast::Stmt::If(if_stmt) => {
                Self::collect_expr_idents(&if_stmt.test, in_closure, bound, used);
                Self::collect_body_idents(&if_stmt.cons, in_closure, bound, used);
                if let Some(alt) = &if_stmt.alt {
                    Self::collect_body_idents(alt, in_closure, bound, used);
                }
            }
            ast::Stmt::While(while_stmt) => {
                Self::collect_expr_idents(&while_stmt.test, in_closure, bound, used);
                Self::collect_body_idents(&while_stmt.body, in_closure, bound, used);
            }
            ast::Stmt::For(for_stmt) => {
                if let Some(init) = &for_stmt.init {
                    match init {
                        ast::VarDeclOrExpr::VarDecl(decl) => {
                            for declarator in &decl.decls {
                                if let ast::Pat::Ident(binding) = &declarator.name
                                    && !in_closure
                                {
                                    bound.insert(binding.id.sym.to_string());
                                }
                                if let Some(init) = &declarator.init {
                                    Self::collect_expr_idents(init, in_closure, bound, used);
                                }
                            }
                        }
                        ast::VarDeclOrExpr::Expr(expr) => {
                            Self::collect_expr_idents(expr, in_closure, bound, used);
                        }
                    }
                }
                if let Some(test) = &for_stmt.test {
                    Self::collect_expr_idents(test, in_closure, bound, used);
                }
                if let Some(update) = &for_stmt.update {
                    Self::collect_expr_idents(update, in_closure, bound, used);
                }
                Self::collect_body_idents(&for_stmt.body, in_closure, bound, used);
            }
            ast::Stmt::Return(ret) => {
                if let Some(arg) = &ret.arg {
                    Self::collect_expr_idents(arg, in_closure, bound, used);
                }
            }
            ast::Stmt::Throw(throw) => {
                Self::collect_expr_idents(&throw.arg, in_closure, bound, used);
            }
            _ => {}
        }
    }

    /// Threaded for the closure-boundary flip; only read when collecting
    /// body-level bindings, never inside pure expressions.
    #[allow(clippy::only_used_in_recursion)]
    fn collect_expr_idents(
        expr: &ast::Expr,
        in_closure: bool,
        bound: &mut HashSet<String>,
        used: &mut Vec<String>,
    ) {
        match expr {
            ast::Expr::Ident(ident) => used.push(ident.sym.to_string()),
            ast::Expr::Paren(paren) => {
                Self::collect_expr_idents(&paren.expr, in_closure, bound, used);
            }
            ast::Expr::Array(array) => {
                for element in array.elems.iter().flatten() {
                    if element.spread.is_none() {
                        Self::collect_expr_idents(&element.expr, in_closure, bound, used);
                    }
                }
            }
            ast::Expr::Object(object) => {
                for property in &object.props {
                    if let ast::PropOrSpread::Prop(property) = property {
                        match property.as_ref() {
                            ast::Prop::KeyValue(property) => {
                                Self::collect_expr_idents(&property.value, in_closure, bound, used)
                            }
                            ast::Prop::Shorthand(ident) => {
                                used.push(ident.sym.to_string());
                            }
                            _ => {}
                        }
                    }
                }
            }
            ast::Expr::Unary(unary) => {
                Self::collect_expr_idents(&unary.arg, in_closure, bound, used);
            }
            ast::Expr::Update(update) => {
                Self::collect_expr_idents(&update.arg, in_closure, bound, used);
            }
            ast::Expr::Bin(binary) => {
                Self::collect_expr_idents(&binary.left, in_closure, bound, used);
                Self::collect_expr_idents(&binary.right, in_closure, bound, used);
            }
            ast::Expr::Assign(assign) => {
                Self::collect_expr_idents(&assign.right, in_closure, bound, used);
            }
            ast::Expr::Member(member) => {
                Self::collect_expr_idents(&member.obj, in_closure, bound, used);
                if let ast::MemberProp::Computed(property) = &member.prop {
                    Self::collect_expr_idents(&property.expr, in_closure, bound, used);
                }
            }
            ast::Expr::Cond(cond) => {
                Self::collect_expr_idents(&cond.test, in_closure, bound, used);
                Self::collect_expr_idents(&cond.cons, in_closure, bound, used);
                Self::collect_expr_idents(&cond.alt, in_closure, bound, used);
            }
            ast::Expr::Call(call) => {
                if let ast::Callee::Expr(callee) = &call.callee {
                    Self::collect_expr_idents(callee, in_closure, bound, used);
                }
                for arg in &call.args {
                    if arg.spread.is_none() {
                        Self::collect_expr_idents(&arg.expr, in_closure, bound, used);
                    }
                }
            }
            ast::Expr::New(new_expr) => {
                Self::collect_expr_idents(&new_expr.callee, in_closure, bound, used);
                if let Some(args) = &new_expr.args {
                    for arg in args {
                        if arg.spread.is_none() {
                            Self::collect_expr_idents(&arg.expr, in_closure, bound, used);
                        }
                    }
                }
            }
            ast::Expr::Seq(seq) => {
                for expr in &seq.exprs {
                    Self::collect_expr_idents(expr, in_closure, bound, used);
                }
            }
            ast::Expr::Arrow(arrow) => match arrow.body.as_ref() {
                ast::ArrowFunctionBody::Expr(body) => {
                    Self::collect_expr_idents(body, true, bound, used);
                }
                ast::ArrowFunctionBody::FunctionBody(block) => {
                    for stmt in &block.stmts {
                        Self::collect_body_idents(stmt, true, bound, used);
                    }
                }
            },
            ast::Expr::Yield(yield_expr) => {
                if let Some(arg) = &yield_expr.arg {
                    Self::collect_expr_idents(arg, in_closure, bound, used);
                }
            }
            _ => {}
        }
    }

    /// Fully expand one recorded module constant into a self-contained pure
    /// expression, transitively inlining referenced constants. Anything
    /// impure, cyclic or non-constant fails precisely.
    fn expand_const_init(
        &self,
        name: &str,
        visiting: &mut Vec<String>,
    ) -> Result<ast::Expr, ParseError> {
        if visiting.iter().any(|entry| entry == name) {
            return Err(ParseError::new(format!(
                "cyclic module constant capture '{name}'"
            )));
        }
        let init = self
            .module_const_inits
            .get(name)
            .map(|(init, _)| init.clone())
            .ok_or_else(|| {
                ParseError::new(format!(
                    "captured module binding '{name}' is not a recorded module constant"
                ))
            })?;
        visiting.push(name.to_owned());
        let result = self.expand_pure_expr(&init, visiting);
        visiting.pop();
        result
    }

    fn expand_pure_expr(
        &self,
        expr: &ast::Expr,
        visiting: &mut Vec<String>,
    ) -> Result<ast::Expr, ParseError> {
        match expr {
            ast::Expr::Lit(_) => Ok(expr.clone()),
            ast::Expr::Paren(paren) => {
                let mut expanded = paren.clone();
                expanded.expr = Box::new(self.expand_pure_expr(&paren.expr, visiting)?);
                Ok(ast::Expr::Paren(expanded))
            }
            ast::Expr::Unary(unary)
                if matches!(unary.op, ast::UnaryOp::Minus | ast::UnaryOp::Bang) =>
            {
                let mut expanded = unary.clone();
                expanded.arg = Box::new(self.expand_pure_expr(&unary.arg, visiting)?);
                Ok(ast::Expr::Unary(expanded))
            }
            ast::Expr::Bin(binary)
                if matches!(
                    binary.op,
                    ast::BinaryOp::Add
                        | ast::BinaryOp::Sub
                        | ast::BinaryOp::Mul
                        | ast::BinaryOp::Div
                        | ast::BinaryOp::Mod
                        | ast::BinaryOp::Exp
                        | ast::BinaryOp::EqEq
                        | ast::BinaryOp::NotEq
                        | ast::BinaryOp::EqEqEq
                        | ast::BinaryOp::NotEqEq
                        | ast::BinaryOp::Lt
                        | ast::BinaryOp::Gt
                        | ast::BinaryOp::LtEq
                        | ast::BinaryOp::GtEq
                        | ast::BinaryOp::LogicalAnd
                        | ast::BinaryOp::LogicalOr
                ) =>
            {
                let mut expanded = binary.clone();
                expanded.left = Box::new(self.expand_pure_expr(&binary.left, visiting)?);
                expanded.right = Box::new(self.expand_pure_expr(&binary.right, visiting)?);
                Ok(ast::Expr::Bin(expanded))
            }
            ast::Expr::Cond(cond) => {
                let mut expanded = cond.clone();
                expanded.test = Box::new(self.expand_pure_expr(&cond.test, visiting)?);
                expanded.cons = Box::new(self.expand_pure_expr(&cond.cons, visiting)?);
                expanded.alt = Box::new(self.expand_pure_expr(&cond.alt, visiting)?);
                Ok(ast::Expr::Cond(expanded))
            }
            ast::Expr::Call(call) => {
                let callee = match &call.callee {
                    ast::Callee::Expr(callee) => match callee.as_ref() {
                        ast::Expr::Ident(ident) => ident.sym.to_string(),
                        ast::Expr::Member(member) => match (member.obj.as_ref(), &member.prop) {
                            (ast::Expr::Ident(namespace), ast::MemberProp::Ident(property)) => {
                                format!("{}::{}", namespace.sym, property.sym)
                            }
                            _ => String::new(),
                        },
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                if ![
                    "Math::floor",
                    "Math::sqrt",
                    "Math::round",
                    "isNaN",
                    "Number::isNaN",
                    "Number::isInteger",
                ]
                .contains(&callee.as_str())
                {
                    return Err(ParseError::new(format!(
                        "captured module constant requires a pure initializer; call to '{callee}' is not allowed"
                    )));
                }
                if call.type_args.is_some() {
                    return Err(ParseError::new(
                        "captured module constant requires a pure initializer; type arguments are not allowed"
                            .to_owned(),
                    ));
                }
                let mut expanded = call.clone();
                let mut args = Vec::with_capacity(call.args.len());
                for arg in &call.args {
                    if arg.spread.is_some() {
                        return Err(ParseError::new(
                            "captured module constant requires a pure initializer; spread arguments are not allowed"
                                .to_owned(),
                        ));
                    }
                    let mut expanded_arg = arg.clone();
                    expanded_arg.expr = Box::new(self.expand_pure_expr(&arg.expr, visiting)?);
                    args.push(expanded_arg);
                }
                expanded.args = args;
                Ok(ast::Expr::Call(expanded))
            }
            ast::Expr::Ident(ident) => {
                let target = ident.sym.to_string();
                self.expand_const_init(&target, visiting)
            }
            _ => Err(ParseError::new(
                "captured module constant requires a pure initializer".to_owned(),
            )),
        }
    }

    fn lower_function(
        &mut self,
        function: &ast::FnDecl,
        exported: bool,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let name = function.ident.sym.to_string();
        let (index, arity) = *self.function_indices.get(&name).ok_or_else(|| {
            unsupported(
                function.ident.span,
                self.base,
                self.source_id,
                "function declaration",
            )
        })?;
        if self.locals.contains_key(&name) {
            return Err(ParseError::at_span(
                byte_span(function.ident.span, self.base, self.source_id),
                format!("function '{name}' conflicts with a local binding"),
            ));
        }
        let Some(body) = function.function.body.as_ref() else {
            return Err(unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "ambient function declaration",
            ));
        };
        let type_params = function
            .function
            .type_params
            .as_ref()
            .map(|params| {
                params
                    .params
                    .iter()
                    .map(|param| {
                        if param.constraint.is_some()
                            || param.default.is_some()
                            || param.is_in
                            || param.is_out
                            || param.is_const
                        {
                            return Err(unsupported(
                                param.span,
                                self.base,
                                self.source_id,
                                "generic constraints, defaults, variance, or const modifiers",
                            ));
                        }
                        Ok(param.name.sym.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let mut function_type_params = HashSet::new();
        for param in &type_params {
            if !function_type_params.insert(param.clone()) {
                return Err(ParseError::new(format!(
                    "duplicate TypeScript generic parameter '{param}'"
                )));
            }
        }
        let previous_generic_type_params =
            std::mem::replace(&mut self.generic_type_params, function_type_params);
        if function.function.is_async || function.function.is_generator {
            // Reference-array mode lowers lazy numeric generators through
            // the private continuation module; ordinary compilation and all
            // async functions keep the historical refusal.
            if !self.ref_mode || function.function.is_async {
                return Err(unsupported(
                    function.function.span,
                    self.base,
                    self.source_id,
                    "async or generator function",
                ));
            }
            return self.lower_generator_function(function, exported, output);
        }

        let mut arg_names = Vec::new();
        let mut arg_schemas = Vec::new();
        let previous_locals = std::mem::take(&mut self.locals);
        let previous_local_types = std::mem::take(&mut self.local_types);
        let previous_mutable_locals = std::mem::take(&mut self.mutable_locals);
        let previous_scope = self.scope_id;
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "lexical scope count",
            )
        })?;
        self.scope_id = scope_id;
        let previous_bindings_len = self.local_bindings.len();
        let declaration_start = self.local_decls.len();
        let mut param_slots = Vec::new();
        let mut default_seen = false;

        for (order, param) in function.function.params.iter().enumerate() {
            let Some((binding, default)) = parameter_binding(&param.pat) else {
                return Err(unsupported(
                    param.span(),
                    self.base,
                    self.source_id,
                    "function parameter pattern",
                ));
            };
            if default.is_some() {
                default_seen = true;
            } else if default_seen {
                return Err(unsupported(
                    param.span(),
                    self.base,
                    self.source_id,
                    "required parameter after a default parameter",
                ));
            }
            let param_name = binding.id.sym.to_string();
            let Some(annotation) = binding.type_ann.as_ref() else {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!("TypeScript noImplicitAny: parameter '{param_name}' requires a type"),
                ));
            };
            if self.locals.contains_key(&param_name) {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!("duplicate TypeScript parameter '{param_name}'"),
                ));
            }
            let schema = self.lower_type(&annotation.type_ann)?;
            let schema = self.ref_map_number_array(schema, binding.id.span);
            let slot = self.next_slot;
            self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                unsupported(
                    binding.id.span,
                    self.base,
                    self.source_id,
                    "function local slot limit",
                )
            })?;
            self.locals.insert(param_name.clone(), slot);
            self.local_types.insert(param_name.clone(), schema.clone());
            self.mutable_locals.insert(slot);
            self.local_bindings.push((param_name.clone(), slot));
            let ident_span = byte_span(binding.id.span, self.base, self.source_id);
            let node_id = self.alloc_node_id();
            self.local_decls.push(LocalDeclSite {
                id: node_id,
                ident_span,
                stmt_span: byte_span(param.span(), self.base, self.source_id),
                slot,
                name: param_name.clone(),
                scope_id,
                decl_order: u32::try_from(order).unwrap_or(u32::MAX),
            });
            param_slots.push(slot);
            arg_names.push(param_name);
            arg_schemas.push(Some(schema));
        }

        let mut return_schema = function
            .function
            .return_type
            .as_ref()
            .map(|annotation| {
                self.lower_type(&annotation.type_ann)
                    .map(|schema| self.ref_map_number_array(schema, annotation.span))
            })
            .transpose()?;
        // Reference-array mode: an un-annotated function whose body
        // returns a carrier (like the upstream QuickSort shape) records
        // the physical carrier as its return schema so callers alias the
        // same owner. A numeric return records Number so callers can use
        // the existing floating projection for `+` (e.g. partition's index).
        // Other shapes keep no declared return. Plain mode never has
        // carriers, so this changes nothing there.
        if return_schema.is_none()
            && self.ref_mode
            && let Some(ast::Stmt::Return(tail)) = body.stmts.last()
            && let Some(returned) = tail.arg.as_ref()
        {
            if self
                .expression_schema(returned)
                .is_some_and(super::array_lowering::is_carrier_schema)
            {
                return_schema = Some(super::array_lowering::carrier_schema());
            } else if self
                .expression_schema(returned)
                .as_ref()
                .is_some_and(Self::is_numeric_schema)
            {
                return_schema = Some(TypeSchema::Number);
            }
        }
        let mut body_stmts = Vec::new();
        // Pure module-constant capture: materialize function-locals for
        // otherwise unresolvable pure module consts before body lowering.
        // Bodies without such references emit nothing here.
        self.inline_module_consts(body, &arg_names, &mut body_stmts)?;
        let mut body_expr = Expr::Null;
        let mut body_expr_line = line_at(
            self.source,
            byte_span(body.span, self.base, self.source_id).lo,
        );
        // Bodies with early returns lower through the guarded transform in
        // the explicit mode; every other body keeps the exact historical
        // path in both modes.
        if self.ref_mode && Self::body_has_early_return(&body.stmts) {
            let (stmts, expr, line) = self.lower_body_with_early_returns(
                body.stmts.clone(),
                body.span,
                return_schema.clone(),
            )?;
            // The prologue (module-constant captures) stays first.
            body_stmts.extend(stmts);
            body_expr = expr;
            body_expr_line = line;
        } else {
            let mut returned = false;
            for (position, statement) in body.stmts.iter().enumerate() {
                if let ast::Stmt::Return(ret) = statement {
                    if returned || position + 1 != body.stmts.len() {
                        return Err(unsupported(
                            ret.span,
                            self.base,
                            self.source_id,
                            "non-final return",
                        ));
                    }
                    body_expr_line = line_at(
                        self.source,
                        byte_span(ret.span, self.base, self.source_id).lo,
                    );
                    body_expr = match ret.arg.as_ref() {
                        Some(value) => self.lower_expr((**value).clone())?,
                        None => Expr::Null,
                    };
                    returned = true;
                } else {
                    if returned {
                        return Err(unsupported(
                            statement.span(),
                            self.base,
                            self.source_id,
                            "statement after return",
                        ));
                    }
                    self.with_consumed_stmt_values(|this| {
                        this.lower_stmt(statement.clone(), &mut body_stmts)
                    })?;
                }
            }
        }

        // Post-body numeric/carrier return inference for un-annotated
        // functions whose returned local is declared in the body (e.g.
        // partition returning `i`). Pre-body inference only sees params;
        // after lowering, body locals are known. Keeps logical Number for
        // callers' floating projection without inventing unboxing facts.
        if return_schema.is_none()
            && self.ref_mode
            && let Some(ast::Stmt::Return(tail)) = body.stmts.last()
            && let Some(returned) = tail.arg.as_ref()
        {
            if self
                .expression_schema(returned)
                .is_some_and(super::array_lowering::is_carrier_schema)
            {
                return_schema = Some(super::array_lowering::carrier_schema());
            } else if self
                .expression_schema(returned)
                .as_ref()
                .is_some_and(Self::is_numeric_schema)
            {
                return_schema = Some(TypeSchema::Number);
            }
        }
        let scope_declarations = self.local_decls[declaration_start..]
            .iter()
            .filter(|site| site.scope_id == scope_id)
            .map(|site| site.slot)
            .collect();
        let function_id = self.alloc_node_id();
        let function_order = u32::try_from(self.function_sites.len()).unwrap_or(u32::MAX);
        self.generic_type_params = previous_generic_type_params;
        self.function_sites.push(FunctionDeclSite {
            id: function_id,
            ident_span: byte_span(function.ident.span, self.base, self.source_id),
            function_index: index,
            name: name.clone(),
            scope_id: 0,
            decl_order: function_order,
        });
        self.functions.push(FunctionDecl {
            name: name.clone(),
            arity,
            index,
            args: arg_names.clone(),
            arg_schemas,
            return_schema: return_schema.clone(),
            type_params: type_params.clone(),
            exported,
            return_type: return_schema
                .as_ref()
                .map(schema_value_type)
                .unwrap_or(vm::ValueType::Unknown),
            symbol: None,
        });
        self.function_impls.insert(
            index,
            FunctionImpl {
                param_slots,
                capture_copies: Vec::new(),
                body_stmts,
                body_expr,
                body_expr_line,
            },
        );
        self.local_bindings.truncate(previous_bindings_len);
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable_locals;
        self.scope_id = previous_scope;
        self.functions.sort_by_key(|declaration| declaration.index);
        output.push(Stmt::FuncDecl {
            name,
            index,
            arity,
            args: arg_names,
            exported,
            has_impl: true,
            line: line_at(
                self.source,
                byte_span(function.ident.span, self.base, self.source_id).lo,
            ),
        });
        self.stmt_spans.push(StmtSpanSite {
            line: line_at(
                self.source,
                byte_span(function.function.span, self.base, self.source_id).lo,
            ),
            span: byte_span(function.function.span, self.base, self.source_id),
        });
        self.next_scope_id = self.next_scope_id.max(scope_id + 1);
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(0),
            range: byte_span(function.function.span, self.base, self.source_id),
            declarations: scope_declarations,
            functions: Vec::new(),
        });
        Ok(())
    }

    /// Generates callee-owned default-callable thunks, carriers and getters
    /// for reference-array mode. Called once after all user functions are
    /// lowered, before building the final IR. Only in `ref_mode`; plain mode
    /// is untouched. Thunks evaluate only omitted defaults left-to-right in
    /// the callee scope then invoke one full implementation (no body
    /// duplication). Carriers hold truthful callable fields for the full
    /// arity and each accepted supplied arity; getters return carriers.
    /// Hygienic collisions fail precisely as duplicates.
    fn generate_default_callables(&mut self, output: &mut Vec<Stmt>) -> Result<(), ParseError> {
        // The producer and import scanner must use the same explicit mode.
        // Plain compilation must not inspect unused default initializers or
        // materialize new callable identities for exported functions.
        if !self.ref_mode {
            return Ok(());
        }
        // Snapshot candidates to avoid borrowing issues. Only top-level user
        // functions with defaults (non-generic, non-synthetic, non-prelude).
        // Prelude helpers use `__compat_ref_` and have no defaults; our own
        // synthetic uses `__compat_default*` / `__CompatDefaultCarrier_*`.
        let candidates: Vec<(String, u16, u8)> = self
            .function_indices
            .iter()
            .filter_map(|(name, (index, arity))| {
                if name.starts_with("__compat_ref_")
                    || name.starts_with("__compat_default_thunk_")
                    || name.starts_with("__compat_default_get_")
                    || name.starts_with("__CompatDefaultCarrier_")
                {
                    return None;
                }
                if super::default_lowering::is_synthetic_default_name(name) {
                    return None;
                }
                if self.generic_function_indices.contains(index) {
                    return None;
                }
                let params = self.function_parameters.get(name)?;
                let has_default = params
                    .iter()
                    .any(|p| parameter_binding(p).is_some_and(|(_, d)| d.is_some()));
                if !has_default {
                    // Still generate carrier/getter with only full for
                    // import safety? For now, only functions with defaults
                    // need carriers (getters for all would bloat). Import
                    // synthesis requests getters only for value imports; if
                    // dependency function has no defaults, requesting getter
                    // would fail. To keep imports safe, we generate getters
                    // for all user functions (even without defaults) below.
                    // Mark all user functions as candidates; thunks only for
                    // those with defaults.
                    return Some((name.clone(), *index, *arity));
                }
                Some((name.clone(), *index, *arity))
            })
            .collect();
        // Deterministic order by function index.
        let mut candidates = candidates;
        candidates.sort_by_key(|(_, index, _)| *index);
        for (name, full_index, full_arity) in candidates {
            // Skip prelude helpers that slipped through (no defaults, but
            // also not user? They have no defaults, but we still generate
            // getters for all? To avoid bloat for prelude (14 helpers),
            // skip our own prelude prefix here; user functions never use it
            // (collision would have already failed as duplicate).
            if name.starts_with("__compat_ref_") {
                continue;
            }
            // Only exported user functions need carriers/getters (importable).
            // Non-exported (e.g. test `main`, private `choosePivot`) never
            // appear in imports; generating getters for them would needlessly
            // materialise callable slots (self) and break ordinary
            // no-self guarantees. Thunks/carriers/getters for exported
            // without defaults still exist (full only) so synthetic
            // getter/carrier imports always resolve.
            let exported = self
                .functions
                .iter()
                .find(|d| d.index == full_index)
                .is_some_and(|d| d.exported);
            if !exported {
                continue;
            }
            self.generate_default_for_one(&name, full_index, full_arity, output)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn generate_default_for_one(
        &mut self,
        full_name: &str,
        full_index: u16,
        full_arity: u8,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        // Full declaration for schemas/return/exported/span.
        let full_decl = self
            .functions
            .iter()
            .find(|d| d.index == full_index)
            .cloned()
            .ok_or_else(|| ParseError::new("default callable declaration is missing"))?;
        let full_site = self
            .function_sites
            .iter()
            .find(|s| s.function_index == full_index)
            .cloned();
        let ident_span = full_site
            .as_ref()
            .map(|s| s.ident_span)
            .unwrap_or(Span::new(self.source_id, 0, self.source.len()));
        let func_span = full_site
            .as_ref()
            .map(|s| {
                // function_sites stores ident_span + function_index; use ident
                // span for synthetic lines (points at original).
                s.ident_span
            })
            .unwrap_or(ident_span);
        let line = line_at(self.source, ident_span.lo);
        let params = self
            .function_parameters
            .get(full_name)
            .cloned()
            .unwrap_or_default();
        // First default position (N if none).
        let first_default = params
            .iter()
            .position(|p| parameter_binding(p).is_some_and(|(_, d)| d.is_some()))
            .unwrap_or(params.len());
        let total = usize::from(full_arity);
        // Validate trailing-default shape (lower_function already enforces
        // for user functions; synthetic never has defaults).
        // Accepted supplied arities: first_default..total (total is full, no thunk).
        let mut thunk_indices: Vec<(u8, u16, String)> = Vec::new();
        // Save outer top-level bindings (module scope) to restore after;
        // thunks are top-level like full (empty outer, no captures).
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_types = std::mem::take(&mut self.local_types);
        let saved_mut = std::mem::take(&mut self.mutable_locals);
        let saved_bindings_len = self.local_bindings.len();
        let saved_scope = self.scope_id;
        // Generate thunks for each omitted arity.
        if first_default < total {
            for supplied in first_default..total {
                let supplied_u8 = u8::try_from(supplied).map_err(|_| {
                    ParseError::at_span(ident_span, "default callable arity exceeds VM limit")
                })?;
                let thunk_name = super::default_lowering::thunk_name(full_name, supplied_u8);
                // Collision refusal (precise duplicates like any other).
                if self.function_indices.contains_key(&thunk_name)
                    || self.host_function_indices.contains_key(&thunk_name)
                    || self.imported_functions.contains_key(&thunk_name)
                {
                    return Err(ParseError::at_span(
                        ident_span,
                        format!("duplicate TypeScript function '{thunk_name}'"),
                    ));
                }
                // Allocate thunk index.
                let thunk_index = self.next_function_index;
                self.next_function_index = self
                    .next_function_index
                    .checked_add(1)
                    .ok_or_else(|| ParseError::at_span(ident_span, "function table size"))?;
                self.function_indices
                    .insert(thunk_name.clone(), (thunk_index, supplied_u8));
                // New scope for thunk.
                let scope_id = self.next_scope_id;
                self.next_scope_id = self
                    .next_scope_id
                    .checked_add(1)
                    .ok_or_else(|| ParseError::at_span(ident_span, "lexical scope count"))?;
                self.scope_id = scope_id;
                // Clear locals for thunk (top-level, empty outer).
                self.locals.clear();
                self.local_types.clear();
                self.mutable_locals.clear();
                let decl_start = self.local_decls.len();
                let mut param_slots: Vec<u16> = Vec::with_capacity(supplied);
                let mut arg_names: Vec<String> = Vec::with_capacity(supplied);
                let mut arg_schemas: Vec<Option<TypeSchema>> = Vec::with_capacity(supplied);
                // First `supplied` params (strip defaults, keep names/schemas).
                for (order, idx) in (0..supplied).enumerate() {
                    let (pname, pschema) = {
                        let decl_arg = full_decl
                            .args
                            .get(idx)
                            .cloned()
                            .unwrap_or_else(|| format!("arg{idx}"));
                        let decl_schema = full_decl.arg_schemas.get(idx).cloned().unwrap_or(None);
                        (decl_arg, decl_schema)
                    };
                    // Original param Pat for span (if available) for diagnostics.
                    let _ = order;
                    let slot = self.next_slot;
                    self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                        ParseError::at_span(ident_span, "function local slot limit")
                    })?;
                    self.locals.insert(pname.clone(), slot);
                    if let Some(schema) = pschema.clone() {
                        self.local_types.insert(pname.clone(), schema);
                    }
                    self.mutable_locals.insert(slot);
                    self.local_bindings.push((pname.clone(), slot));
                    let node_id = self.alloc_node_id();
                    self.local_decls.push(LocalDeclSite {
                        id: node_id,
                        ident_span,
                        stmt_span: ident_span,
                        slot,
                        name: pname.clone(),
                        scope_id,
                        decl_order: u32::try_from(order).unwrap_or(u32::MAX),
                    });
                    param_slots.push(slot);
                    arg_names.push(pname);
                    arg_schemas.push(pschema);
                }
                // Omitted defaults (supplied..total) left-to-right in callee scope.
                let mut body_stmts: Vec<Stmt> = Vec::new();
                // Map all param names (supplied + omitted) for later defaults.
                // Supplied already in locals; now process omitted.
                let mut all_slots: Vec<u16> = param_slots.clone();
                for idx in supplied..total {
                    // Original param name/schema.
                    let (pname, pschema) = {
                        let n = full_decl
                            .args
                            .get(idx)
                            .cloned()
                            .unwrap_or_else(|| format!("arg{idx}"));
                        let s = full_decl.arg_schemas.get(idx).cloned().unwrap_or(None);
                        (n, s)
                    };
                    // Default AST from original Pat.
                    let default_expr: Option<ast::Expr> = params
                        .get(idx)
                        .and_then(|pat| parameter_binding(pat).and_then(|(_, d)| d.cloned()));
                    let Some(default_ast) = default_expr else {
                        return Err(ParseError::at_span(
                            ident_span,
                            "defaulted TypeScript call omits a required parameter",
                        ));
                    };
                    // Predicate before lowering (statically justified).
                    let needs_proj = matches!(pschema, Some(TypeSchema::Number))
                        && self
                            .expression_schema(&default_ast)
                            .as_ref()
                            .is_some_and(Self::is_numeric_schema);
                    let lowered = self.lower_expr(default_ast)?;
                    let lowered = if needs_proj && self.ref_mode && !self.in_ref_prelude {
                        Self::project_numeric_call_arg(lowered)
                    } else {
                        lowered
                    };
                    let slot = self.next_slot;
                    self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                        ParseError::at_span(ident_span, "function local slot limit")
                    })?;
                    // Hidden Lets for omitted do not get local_decls (like inline).
                    body_stmts.push(Stmt::Let {
                        index: slot,
                        declared_schema: pschema.clone(),
                        expr: lowered,
                        line,
                    });
                    self.locals.insert(pname.clone(), slot);
                    if let Some(schema) = pschema.clone() {
                        self.local_types.insert(pname.clone(), schema);
                    }
                    // Omitted slots are mutable? Not needed, but keep consistent.
                    self.mutable_locals.insert(slot);
                    all_slots.push(slot);
                    let _ = pschema;
                }
                // Inner call to full with all N args (Vars).
                let call_id = self.alloc_node_id();
                let call_args: Vec<Expr> = all_slots.into_iter().map(Expr::Var).collect();
                let site_span = ident_span;
                self.call_sites.push(ParsedCallSite {
                    id: call_id,
                    callee_span: site_span,
                    expr_span: site_span,
                    target: ParsedCallTarget::Function(full_index),
                    name: full_name.to_owned(),
                    scope_id,
                    is_namespace_call: false,
                });
                let body_expr = Expr::Call(full_index, Vec::new(), call_args, None, Some(call_id));
                // FunctionDecl for thunk (exported iff full exported, to avoid broadening).
                let return_schema = full_decl.return_schema.clone();
                let return_type = return_schema
                    .as_ref()
                    .map(schema_value_type)
                    .unwrap_or(vm::ValueType::Unknown);
                let thunk_order = u32::try_from(self.function_sites.len()).unwrap_or(u32::MAX);
                let thunk_site_id = self.alloc_node_id();
                self.function_sites.push(FunctionDeclSite {
                    id: thunk_site_id,
                    ident_span,
                    function_index: thunk_index,
                    name: thunk_name.clone(),
                    scope_id: 0,
                    decl_order: thunk_order,
                });
                self.functions.push(FunctionDecl {
                    name: thunk_name.clone(),
                    arity: supplied_u8,
                    index: thunk_index,
                    args: arg_names.clone(),
                    arg_schemas: arg_schemas.clone(),
                    return_schema: return_schema.clone(),
                    type_params: Vec::new(),
                    exported: full_decl.exported,
                    return_type,
                    symbol: None,
                });
                self.function_impls.insert(
                    thunk_index,
                    FunctionImpl {
                        param_slots: param_slots.clone(),
                        capture_copies: Vec::new(),
                        body_stmts,
                        body_expr,
                        body_expr_line: line,
                    },
                );
                let scope_decls = self.local_decls[decl_start..]
                    .iter()
                    .filter(|s| s.scope_id == scope_id)
                    .map(|s| s.slot)
                    .collect();
                self.function_scopes.push(ParsedLexicalScope {
                    id: scope_id,
                    parent: Some(0),
                    range: func_span,
                    declarations: scope_decls,
                    functions: Vec::new(),
                });
                self.next_scope_id = self.next_scope_id.max(scope_id + 1);
                output.push(Stmt::FuncDecl {
                    name: thunk_name.clone(),
                    index: thunk_index,
                    arity: supplied_u8,
                    args: arg_names,
                    exported: full_decl.exported,
                    has_impl: true,
                    line,
                });
                self.stmt_spans.push(StmtSpanSite {
                    line,
                    span: func_span,
                });
                self.functions.sort_by_key(|d| d.index);
                thunk_indices.push((supplied_u8, thunk_index, thunk_name));
                // Clear thunk locals for next thunk (keep outer saved for later restore after all).
                self.locals.clear();
                self.local_types.clear();
                self.mutable_locals.clear();
                // Truncate bindings pushed for this thunk's params (keep outer saved len).
                self.local_bindings.truncate(saved_bindings_len);
                // local_decls for thunk params remain (needed for scopes); omitted Lets have none.
            }
        }
        // Restore outer top-level bindings.
        self.locals = saved_locals;
        self.local_types = saved_types;
        self.mutable_locals = saved_mut;
        self.scope_id = saved_scope;
        // Carrier + getter (even for functions without defaults, for import safety).
        // Carrier fields: full + each thunk.
        let carrier_name = super::default_lowering::carrier_name(full_name);
        if self.struct_schemas.contains_key(&carrier_name)
            || self.named_types.contains_key(&carrier_name)
            || self.function_indices.contains_key(&carrier_name)
        {
            return Err(ParseError::at_span(
                ident_span,
                format!("duplicate TypeScript type '{carrier_name}'"),
            ));
        }
        let getter_name = super::default_lowering::getter_name(full_name);
        if self.function_indices.contains_key(&getter_name)
            || self.host_function_indices.contains_key(&getter_name)
            || self.imported_functions.contains_key(&getter_name)
        {
            return Err(ParseError::at_span(
                ident_span,
                format!("duplicate TypeScript function '{getter_name}'"),
            ));
        }
        for (_, _, thunk_name) in &thunk_indices {
            if !self.function_indices.contains_key(thunk_name) {
                return Err(ParseError::new("thunk declaration is missing"));
            }
        }
        // Build carrier Callable schemas (physical, truthful).
        let full_callable = TypeSchema::Callable {
            params: full_decl
                .arg_schemas
                .iter()
                .map(|s| s.clone().unwrap_or(TypeSchema::Unknown))
                .collect(),
            result: Box::new(
                full_decl
                    .return_schema
                    .clone()
                    .unwrap_or(TypeSchema::Unknown),
            ),
        };
        let mut fields: HashMap<String, TypeSchema> = HashMap::new();
        fields.insert(
            super::default_lowering::FIELD_FULL.to_owned(),
            full_callable,
        );
        for (supplied, thunk_index, _) in &thunk_indices {
            let thunk_decl = self
                .functions
                .iter()
                .find(|d| d.index == *thunk_index)
                .cloned()
                .ok_or_else(|| ParseError::new("thunk declaration is missing"))?;
            let callable = TypeSchema::Callable {
                params: thunk_decl
                    .arg_schemas
                    .iter()
                    .map(|s| s.clone().unwrap_or(TypeSchema::Unknown))
                    .collect(),
                result: Box::new(
                    thunk_decl
                        .return_schema
                        .clone()
                        .unwrap_or(TypeSchema::Unknown),
                ),
            };
            fields.insert(super::default_lowering::arity_field(*supplied), callable);
        }
        let carrier_decl = StructDecl {
            name: carrier_name.clone(),
            type_params: Vec::new(),
            body_schema: TypeSchema::Object(fields),
            origin: StructDeclOrigin::Guest,
        };
        self.struct_schemas
            .insert(carrier_name.clone(), carrier_decl.clone());
        self.named_types.insert(
            carrier_name.clone(),
            TypeSchema::Named(carrier_name.clone(), Vec::new()),
        );
        self.named_type_declarations
            .insert(carrier_name.clone(), carrier_decl.clone());
        let struct_site_id = self.alloc_node_id();
        self.struct_decl_sites.push(StructDeclSite {
            id: struct_site_id,
            ident_span,
            decl_span: func_span,
            name: carrier_name.clone(),
            scope_id: 0,
        });
        self.type_provenance.push(GuestTypeProvenance {
            canonical_name: carrier_name.clone(),
            source_name: full_name.to_owned(),
            source_id: self.source_id,
            declaration_span: Some(ident_span),
        });
        // Getter (0 args -> carrier). Exported iff full exported.
        let getter_index = self.next_function_index;
        self.next_function_index = self
            .next_function_index
            .checked_add(1)
            .ok_or_else(|| ParseError::at_span(ident_span, "function table size"))?;
        self.function_indices
            .insert(getter_name.clone(), (getter_index, 0));
        let getter_scope = self.next_scope_id;
        self.next_scope_id = self
            .next_scope_id
            .checked_add(1)
            .ok_or_else(|| ParseError::at_span(ident_span, "lexical scope count"))?;
        // Getter body: `{ full: Full, arityK: ThunkK, ... }` as Map.
        // Build via MapNew + Sets with FunctionRefs (existing IR, no new opcodes).
        // FunctionRefs for full/thunks (local functions, not imported).
        let mut map_expr = Self::builtin_call(vm::BuiltinFunction::MapNew, Vec::new());
        // Deterministic field order: full first, then arity ascending.
        let mut ordered: Vec<(String, u16)> = Vec::new();
        ordered.push((super::default_lowering::FIELD_FULL.to_owned(), full_index));
        let mut sorted_thunks = thunk_indices.clone();
        sorted_thunks.sort_by_key(|(supplied, _, _)| *supplied);
        for (supplied, thunk_index, _) in &sorted_thunks {
            ordered.push((
                super::default_lowering::arity_field(*supplied),
                *thunk_index,
            ));
        }
        for (field, target_index) in ordered {
            let value = Expr::FunctionRef(target_index, Vec::new());
            map_expr = Self::builtin_call(
                vm::BuiltinFunction::Set,
                vec![map_expr, Expr::String(field), value],
            );
        }
        let carrier_schema = TypeSchema::Named(carrier_name.clone(), Vec::new());
        let getter_return = Some(carrier_schema.clone());
        let getter_order = u32::try_from(self.function_sites.len()).unwrap_or(u32::MAX);
        let getter_site_id = self.alloc_node_id();
        self.function_sites.push(FunctionDeclSite {
            id: getter_site_id,
            ident_span,
            function_index: getter_index,
            name: getter_name.clone(),
            scope_id: 0,
            decl_order: getter_order,
        });
        self.functions.push(FunctionDecl {
            name: getter_name.clone(),
            arity: 0,
            index: getter_index,
            args: Vec::new(),
            arg_schemas: Vec::new(),
            return_schema: getter_return.clone(),
            type_params: Vec::new(),
            exported: full_decl.exported,
            return_type: vm::ValueType::Map,
            symbol: None,
        });
        self.function_impls.insert(
            getter_index,
            FunctionImpl {
                param_slots: Vec::new(),
                capture_copies: Vec::new(),
                body_stmts: Vec::new(),
                body_expr: map_expr,
                body_expr_line: line,
            },
        );
        self.function_scopes.push(ParsedLexicalScope {
            id: getter_scope,
            parent: Some(0),
            range: func_span,
            declarations: Vec::new(),
            functions: Vec::new(),
        });
        self.next_scope_id = self.next_scope_id.max(getter_scope + 1);
        output.push(Stmt::FuncDecl {
            name: getter_name,
            index: getter_index,
            arity: 0,
            args: Vec::new(),
            exported: full_decl.exported,
            has_impl: true,
            line,
        });
        self.stmt_spans.push(StmtSpanSite {
            line,
            span: func_span,
        });
        self.functions.sort_by_key(|d| d.index);
        Ok(())
    }
    fn lower_defaulted_function_call(
        &mut self,
        function_name: &str,
        function_index: u16,
        type_args: Vec<TypeSchema>,
        provided_args: Vec<Expr>,
        call_id: SemanticNodeId,
    ) -> Result<Expr, ParseError> {
        if self.generic_function_indices.contains(&function_index) {
            return Err(ParseError::new(
                "default parameters on generic TypeScript functions are unsupported",
            ));
        }
        let parameters = self
            .function_parameters
            .get(function_name)
            .cloned()
            .ok_or_else(|| ParseError::new("default parameter declaration metadata is missing"))?;
        let mut parameter_data = Vec::with_capacity(parameters.len());
        let mut default_seen = false;
        for parameter in &parameters {
            let Some((binding, default)) = parameter_binding(parameter) else {
                return Err(unsupported(
                    parameter.span(),
                    self.base,
                    self.source_id,
                    "default parameter pattern",
                ));
            };
            let Some(annotation) = binding.type_ann.as_ref() else {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!(
                        "TypeScript noImplicitAny: parameter '{}' requires a type",
                        binding.id.sym
                    ),
                ));
            };
            let schema = self.lower_type(&annotation.type_ann)?;
            let schema = self.ref_map_number_array(schema, binding.id.span);
            if default.is_some() {
                default_seen = true;
            } else if default_seen {
                return Err(unsupported(
                    parameter.span(),
                    self.base,
                    self.source_id,
                    "required parameter after a default parameter",
                ));
            }
            parameter_data.push((
                binding.id.sym.to_string(),
                binding.id.span,
                parameter.span(),
                schema,
                default.cloned(),
            ));
        }
        if provided_args.len() >= parameter_data.len()
            || parameter_data[provided_args.len()..]
                .iter()
                .any(|(_, _, _, _, default)| default.is_none())
        {
            return Err(ParseError::new(
                "defaulted TypeScript call does not omit only defaulted parameters",
            ));
        }

        let parent_scope = self.scope_id;
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            unsupported(
                parameters[0].span(),
                self.base,
                self.source_id,
                "lexical scope count",
            )
        })?;
        let previous_locals = std::mem::take(&mut self.locals);
        let previous_local_types = std::mem::take(&mut self.local_types);
        let previous_mutable_locals = std::mem::take(&mut self.mutable_locals);
        let previous_scope = self.scope_id;
        self.scope_id = scope_id;
        let previous_capture_depth = self.closure_captures.len();
        self.closure_captures.push(ClosureCaptureContext {
            local_slots: HashSet::new(),
            captures: HashMap::new(),
            capture_copies: Vec::new(),
        });
        let lowered = (|| -> Result<LoweredDefaultArguments, ParseError> {
            let mut closure_params = Vec::with_capacity(provided_args.len());
            let mut default_statements = Vec::new();
            let mut call_args = Vec::with_capacity(parameter_data.len());
            for (index, (name, ident_span, parameter_span, schema, default)) in
                parameter_data.iter().enumerate()
            {
                let slot = self.next_slot;
                self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                    ParseError::at_span(
                        byte_span(*ident_span, self.base, self.source_id),
                        "TypeScript closure local slot limit exceeded",
                    )
                })?;
                self.closure_captures
                    .last_mut()
                    .expect("default call closure context remains active")
                    .local_slots
                    .insert(slot);
                if index < provided_args.len() {
                    closure_params.push(slot);
                } else {
                    let expression = default.as_ref().ok_or_else(|| {
                        ParseError::at_span(
                            byte_span(*ident_span, self.base, self.source_id),
                            "missing argument has no default initializer",
                        )
                    })?;
                    // Numeric projection predicate from the guest AST before
                    // lowering; the wrap applies after lowering so the
                    // default still evaluates exactly once, in order, in the
                    // callee scope.
                    let needs_number_projection = self.ref_mode
                        && !self.in_ref_prelude
                        && matches!(schema, TypeSchema::Number)
                        && self
                            .expression_schema(expression)
                            .as_ref()
                            .is_some_and(Self::is_numeric_schema);
                    let expression = self.lower_expr(expression.clone())?;
                    let expression = if needs_number_projection {
                        Self::project_numeric_call_arg(expression)
                    } else {
                        expression
                    };
                    default_statements.push(Stmt::Let {
                        index: slot,
                        declared_schema: Some(schema.clone()),
                        expr: expression,
                        line: line_at(
                            self.source,
                            byte_span(*parameter_span, self.base, self.source_id).lo,
                        ),
                    });
                }
                self.locals.insert(name.clone(), slot);
                self.local_types.insert(name.clone(), schema.clone());
                call_args.push(Expr::Var(slot));
            }
            Ok((closure_params, default_statements, call_args))
        })();
        let closure_context = self
            .closure_captures
            .pop()
            .expect("default call closure context remains active");
        debug_assert_eq!(self.closure_captures.len(), previous_capture_depth);
        let declarations = Vec::new();
        let span = parameters
            .iter()
            .map(|parameter| byte_span(parameter.span(), self.base, self.source_id))
            .fold(None, |range: Option<Span>, current| {
                Some(match range {
                    Some(range) => Span::new(
                        self.source_id,
                        range.lo.min(current.lo),
                        range.hi.max(current.hi),
                    ),
                    None => current,
                })
            })
            .expect("defaulted function parameter spans are non-empty");
        self.scope_id = previous_scope;
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable_locals;
        self.next_scope_id = self.next_scope_id.max(scope_id + 1);
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(parent_scope),
            range: span,
            declarations,
            functions: Vec::new(),
        });
        let (param_slots, default_statements, call_args) = lowered?;
        let body = Expr::Block {
            stmts: default_statements,
            expr: Box::new(Expr::Call(
                function_index,
                type_args,
                call_args,
                None,
                Some(call_id),
            )),
        };
        Ok(Expr::ClosureCall(
            vm::compiler::ir::ClosureExpr {
                param_slots,
                capture_copies: closure_context.capture_copies,
                body: Box::new(body),
            },
            provided_args,
        ))
    }

    fn lower_var_decl(
        &mut self,
        decl: ast::VarDecl,
        exported: bool,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if decl.kind == ast::VarDeclKind::Var {
            return Err(unsupported(
                decl.span,
                self.base,
                self.source_id,
                "var declaration",
            ));
        }
        let mutable = decl.kind == ast::VarDeclKind::Let;
        for declarator in decl.decls {
            if self.scope_id == 0
                && let (ast::Pat::Ident(binding), Some(initializer)) =
                    (&declarator.name, &declarator.init)
                && let ast::Expr::Arrow(arrow) = initializer.as_ref()
                && self.function_indices.contains_key(binding.id.sym.as_ref())
            {
                let function = arrow_function_decl(binding.id.clone(), arrow.clone());
                self.lower_function(&function, exported, output)?;
                continue;
            }
            let ast::Pat::Ident(binding) = declarator.name else {
                return Err(unsupported(
                    declarator.name.span(),
                    self.base,
                    self.source_id,
                    "binding pattern",
                ));
            };
            let name = binding.id.sym.to_string();
            let duplicate = self
                .local_decls
                .iter()
                .any(|site| site.scope_id == self.scope_id && site.name == name);
            let root_conflict = self.scope_id == 0
                && (self.host_function_indices.contains_key(&name)
                    || self.imported_functions.contains_key(&name)
                    || self.function_indices.contains_key(&name));
            if duplicate || root_conflict {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!("duplicate TypeScript binding '{name}'"),
                ));
            }
            let Some(init) = declarator.init else {
                return Err(unsupported(
                    declarator.span,
                    self.base,
                    self.source_id,
                    "uninitialized declaration",
                ));
            };
            let declared_schema = binding
                .type_ann
                .as_ref()
                .map(|ann| self.lower_type(&ann.type_ann))
                .transpose()?;
            let inferred_schema = self.expression_schema(&init);
            let generic_function_value = match init.as_ref() {
                ast::Expr::Ident(ident) => {
                    let name = ident.sym.to_string();
                    let generic = !self.locals.contains_key(&name)
                        && (self.function_indices.get(&name).is_some_and(|(index, _)| {
                            self.generic_function_indices.contains(index)
                        }) || self
                            .imported_functions
                            .get(&name)
                            .is_some_and(|(_, declaration)| !declaration.type_params.is_empty()));
                    generic.then_some((name, ident.span))
                }
                ast::Expr::Paren(paren) => match paren.expr.as_ref() {
                    ast::Expr::Ident(ident) => {
                        let name = ident.sym.to_string();
                        let generic = !self.locals.contains_key(&name)
                            && (self.function_indices.get(&name).is_some_and(|(index, _)| {
                                self.generic_function_indices.contains(index)
                            }) || self.imported_functions.get(&name).is_some_and(
                                |(_, declaration)| !declaration.type_params.is_empty(),
                            ));
                        generic.then_some((name, ident.span))
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some((generic_name, span)) = generic_function_value {
                let has_concrete_context =
                    matches!(declared_schema.as_ref(), Some(TypeSchema::Callable { .. }))
                        && declared_schema
                            .as_ref()
                            .is_some_and(|schema| generic_params_in_schema(schema).is_empty());
                if !has_concrete_context {
                    return Err(ParseError::at_span(
                        byte_span(span, self.base, self.source_id),
                        format!(
                            "generic function value '{generic_name}' requires explicit type arguments or an unambiguous callable context"
                        ),
                    ));
                }
            }
            let local_type = declared_schema.clone().or(inferred_schema.clone());
            let callable_origin = (!mutable)
                .then(|| self.immutable_guest_callable_origin(&init))
                .flatten();
            // Reference-array mode: a user-region logical `number[]`
            // binding takes the physical carrier, and a plain staging
            // array initializer converts through the owning factory. An
            // already-carrier initializer is a plain alias share.
            let ref_wrap_span = if self.ref_is_user_span(binding.id.span)
                && local_type.as_ref().is_some_and(|schema| {
                    super::array_lowering::is_logical_number_array(schema.clone())
                })
                && !inferred_schema
                    .as_ref()
                    .is_some_and(|schema| super::array_lowering::is_carrier_schema(schema.clone()))
            {
                Some(init.span())
            } else {
                None
            };
            let local_type =
                local_type.map(|schema| self.ref_map_number_array(schema, binding.id.span));
            // Reserve the binding while lowering an arrow so recursive references resolve.
            let is_arrow_initializer = matches!(init.as_ref(), ast::Expr::Arrow(_));
            // Module-level `const` initializers are recorded for pure-constant
            // capture by isolated function bodies (source order enforced by
            // lowering sequence; purity and shadowing enforced at use).
            if self.scope_id == 0 && !mutable && !self.in_ref_prelude {
                self.module_const_inits
                    .insert(name.clone(), (init.as_ref().clone(), local_type.clone()));
            }
            let slot = if is_arrow_initializer {
                let slot = self.next_slot;
                self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                    ParseError::at_span(
                        byte_span(binding.id.span, self.base, self.source_id),
                        "TypeScript local slot limit exceeded",
                    )
                })?;
                self.own_closure_local(slot);
                self.locals.insert(name.clone(), slot);
                if let Some(schema) = &local_type {
                    self.local_types.insert(name.clone(), schema.clone());
                } else {
                    self.local_types.remove(&name);
                }
                if mutable {
                    self.mutable_locals.insert(slot);
                }
                Some(slot)
            } else {
                None
            };
            let expr = self.lower_expr(*init)?;
            let expr = match ref_wrap_span {
                Some(span) => {
                    self.syn_call(super::array_lowering::HELPER_MAKE, vec![expr], span)?
                }
                None => expr,
            };
            let slot = match slot {
                Some(slot) => slot,
                None => {
                    let slot = self.next_slot;
                    self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                        ParseError::at_span(
                            byte_span(binding.id.span, self.base, self.source_id),
                            "TypeScript local slot limit exceeded",
                        )
                    })?;
                    slot
                }
            };
            self.own_closure_local(slot);
            self.locals.insert(name.clone(), slot);
            if let Some(origin) = callable_origin {
                self.const_callable_origins.insert(slot, origin);
            }
            if let Some(schema) = &local_type {
                self.local_types.insert(name.clone(), schema.clone());
            }
            if mutable {
                self.mutable_locals.insert(slot);
            }
            self.local_bindings.push((name.clone(), slot));
            let ident_span = byte_span(binding.id.span, self.base, self.source_id);
            let decl_span = byte_span(decl.span, self.base, self.source_id);
            let line = line_at(self.source, decl_span.lo);
            let node_id = self.alloc_node_id();
            let decl_order = u32::try_from(
                self.local_decls
                    .iter()
                    .filter(|site| site.scope_id == self.scope_id)
                    .count(),
            )
            .unwrap_or(u32::MAX);
            self.local_decls.push(LocalDeclSite {
                id: node_id,
                ident_span,
                stmt_span: decl_span,
                slot,
                name,
                scope_id: self.scope_id,
                decl_order,
            });
            output.push(Stmt::Let {
                index: slot,
                declared_schema: local_type,
                expr,
                line,
            });
            self.stmt_spans.push(StmtSpanSite {
                line,
                span: decl_span,
            });
        }
        Ok(())
    }

    fn lower_block(&mut self, block: ast::BlockStmt) -> Result<Vec<Stmt>, ParseError> {
        self.lower_block_contents(block.span, block.stmts)
    }

    fn lower_block_contents(
        &mut self,
        block_span: swc_common::Span,
        block_stmts: Vec<ast::Stmt>,
    ) -> Result<Vec<Stmt>, ParseError> {
        let parent_scope = self.scope_id;
        let previous_locals = self.locals.clone();
        let previous_local_types = self.local_types.clone();
        let previous_mutable = self.mutable_locals.clone();
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            ParseError::at_span(
                byte_span(block_span, self.base, self.source_id),
                "TypeScript lexical scope count exceeded",
            )
        })?;
        self.scope_id = scope_id;
        let declaration_start = self.local_decls.len();
        let result = (|| {
            let mut statements = Vec::new();
            for statement in block_stmts {
                self.lower_stmt(statement, &mut statements)?;
            }
            Ok(statements)
        })();
        let declarations = self.local_decls[declaration_start..]
            .iter()
            .filter(|site| site.scope_id == scope_id)
            .map(|site| site.slot)
            .collect();
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(parent_scope),
            range: byte_span(block_span, self.base, self.source_id),
            declarations,
            functions: Vec::new(),
        });
        self.scope_id = parent_scope;
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable;
        result
    }

    pub(crate) fn lower_assignment_statement(
        &mut self,
        expr: ast::Expr,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let span = expr.span();
        let site = byte_span(span, self.base, self.source_id);
        let line = line_at(self.source, site.lo);
        match expr {
            ast::Expr::Assign(assignment) => {
                let target = match assignment.left {
                    ast::AssignTarget::Simple(target) => target,
                    ast::AssignTarget::Pat(pattern) => {
                        return self.lower_destructuring_assignment(
                            pattern,
                            *assignment.right,
                            assignment.op,
                            assignment.span,
                            line,
                            output,
                        );
                    }
                };
                let ast::SimpleAssignTarget::Ident(binding) = target else {
                    if let ast::SimpleAssignTarget::Member(member) = target {
                        return self.lower_array_index_assignment(
                            member,
                            assignment.op,
                            *assignment.right,
                            line,
                            output,
                        );
                    }
                    return Err(unsupported(
                        assignment.span,
                        self.base,
                        self.source_id,
                        "non-local assignment target",
                    ));
                };
                let name = binding.id.sym.to_string();
                let Some(source_slot) = self.locals.get(&name).copied() else {
                    return Err(ParseError::at_span(
                        byte_span(binding.id.span, self.base, self.source_id),
                        format!("unresolved TypeScript assignment target '{name}'"),
                    ));
                };
                if !self.mutable_locals.contains(&source_slot) {
                    return Err(ParseError::at_span(
                        byte_span(binding.id.span, self.base, self.source_id),
                        format!("cannot assign to const binding '{name}'"),
                    ));
                }
                let slot = self.capture_slot(source_slot, &name, binding.id.span)?;
                // Explicit reference-array mode: `+=` on a Number/Float local
                // lowers to Set with the floating projection (native Float
                // Add exists); pure Int keeps the Add kind. `--`/Sub stays.
                let target_is_int_add = self
                    .local_types
                    .get(&name)
                    .is_some_and(|schema| matches!(schema, TypeSchema::Int));
                let rhs_is_int_add = self
                    .expression_schema(&assignment.right)
                    .is_some_and(|schema| matches!(schema, TypeSchema::Int));
                let use_float_addassign = self.ref_mode
                    && assignment.op == ast::AssignOp::AddAssign
                    && !(target_is_int_add && rhs_is_int_add)
                    && self
                        .local_types
                        .get(&name)
                        .is_some_and(Self::is_numeric_schema)
                    && self
                        .expression_schema(&assignment.right)
                        .as_ref()
                        .is_some_and(Self::is_numeric_schema);
                let kind = match assignment.op {
                    ast::AssignOp::Assign => AssignmentKind::Set,
                    ast::AssignOp::AddAssign if use_float_addassign => AssignmentKind::Set,
                    ast::AssignOp::AddAssign => AssignmentKind::Add,
                    _ => {
                        return Err(unsupported(
                            assignment.span,
                            self.base,
                            self.source_id,
                            "compound assignment operator",
                        ));
                    }
                };
                let rhs_needs_factory = self
                    .expression_schema(&assignment.right)
                    .is_some_and(super::array_lowering::is_logical_number_array);
                let value = self.lower_expr(*assignment.right)?;
                // Rebinding a reference-array variable to a plain staging
                // array converts through the owning factory; carrier to
                // carrier assignment stays a plain rebinding share.
                let value = if rhs_needs_factory
                    && self.local_types.get(&name).is_some_and(|schema| {
                        super::array_lowering::is_carrier_schema(schema.clone())
                    }) {
                    self.syn_call(
                        super::array_lowering::HELPER_MAKE,
                        vec![value],
                        assignment.span,
                    )?
                } else {
                    value
                };
                // Floating `+=`: `x += rhs` becomes `x = float_add(x, rhs)`
                // with once-only semantics (local target evaluates once).
                let value = if use_float_addassign {
                    Self::lower_numeric_add(Expr::Var(slot), value)
                } else {
                    value
                };
                output.push(Stmt::Assign {
                    kind,
                    index: slot,
                    expr: value,
                    line,
                });
            }
            ast::Expr::Update(update) => {
                let update_span = update.span;
                let update_op = update.op;
                match *update.arg {
                    ast::Expr::Member(member)
                        if self
                            .expression_schema(&member.obj)
                            .is_some_and(super::array_lowering::is_carrier_schema) =>
                    {
                        // Indexed element update on a reference-array
                        // carrier: evaluate the receiver and index exactly
                        // once, then read-modify-write through the owning
                        // dispatcher. The statement value is discarded.
                        let ast::MemberProp::Computed(property) = member.prop else {
                            return Err(unsupported(
                                member.span,
                                self.base,
                                self.source_id,
                                "non-indexed update target",
                            ));
                        };
                        let container = self.lower_expr(*member.obj)?;
                        let index = self.lower_expr(*property.expr)?;
                        let carrier_slot = self.alloc_hidden_local(update_span)?;
                        output.push(Stmt::Let {
                            index: carrier_slot,
                            declared_schema: Some(super::array_lowering::carrier_schema()),
                            expr: container,
                            line,
                        });
                        let index_slot = self.alloc_hidden_local(update_span)?;
                        output.push(Stmt::Let {
                            index: index_slot,
                            declared_schema: Some(TypeSchema::Number),
                            expr: index,
                            line,
                        });
                        let previous = self.syn_call(
                            super::array_lowering::HELPER_READ,
                            vec![Expr::Var(carrier_slot), Expr::Var(index_slot)],
                            update_span,
                        )?;
                        let previous_slot = self.alloc_hidden_local(update_span)?;
                        output.push(Stmt::Let {
                            index: previous_slot,
                            declared_schema: Some(TypeSchema::Number),
                            expr: previous,
                            line,
                        });
                        // Carrier elements are Number: `++` uses the floating
                        // projection in this mode, `--` keeps Sub.
                        let updated = if update_op == ast::UpdateOp::MinusMinus {
                            Expr::Sub(Box::new(Expr::Var(previous_slot)), Box::new(Expr::Int(1)))
                        } else if self.ref_mode {
                            Self::lower_numeric_add(Expr::Var(previous_slot), Expr::Int(1))
                        } else {
                            Expr::Add(Box::new(Expr::Var(previous_slot)), Box::new(Expr::Int(1)))
                        };
                        let written = self.syn_call(
                            super::array_lowering::HELPER_WRITE,
                            vec![Expr::Var(carrier_slot), Expr::Var(index_slot), updated],
                            update_span,
                        )?;
                        self.push_expr_stmt(
                            written,
                            Some(TypeSchema::Number),
                            update_span,
                            line,
                            output,
                        )?;
                    }
                    ast::Expr::Member(_) => {
                        return Err(unsupported(
                            update_span,
                            self.base,
                            self.source_id,
                            "non-local increment target",
                        ));
                    }
                    ast::Expr::Ident(ident) => {
                        let name = ident.sym.to_string();
                        let Some(slot) = self.locals.get(&name).copied() else {
                            return Err(ParseError::at_span(
                                byte_span(ident.span, self.base, self.source_id),
                                format!("unresolved TypeScript increment target '{name}'"),
                            ));
                        };
                        if !self.mutable_locals.contains(&slot) {
                            return Err(ParseError::at_span(
                                byte_span(ident.span, self.base, self.source_id),
                                format!("cannot increment const binding '{name}'"),
                            ));
                        }
                        // Plain read-modify-write assignment: the dedicated
                        // increment kind does not advance loop-nested slots
                        // on this Core version (the loop spins forever),
                        // while read-modify-write assignment is the proven
                        // path (native 2048-iteration loops, factorial).
                        // Semantically identical for numbers.
                        if update_op == ast::UpdateOp::MinusMinus {
                            output.push(Stmt::Assign {
                                kind: AssignmentKind::Set,
                                index: slot,
                                expr: Expr::Sub(Box::new(Expr::Var(slot)), Box::new(Expr::Int(1))),
                                line,
                            });
                        } else {
                            let target_is_int = self
                                .local_types
                                .get(&name)
                                .is_some_and(|schema| matches!(schema, TypeSchema::Int));
                            let expr = if self.ref_mode && !target_is_int {
                                Self::lower_numeric_add(Expr::Var(slot), Expr::Int(1))
                            } else {
                                Expr::Add(Box::new(Expr::Var(slot)), Box::new(Expr::Int(1)))
                            };
                            output.push(Stmt::Assign {
                                kind: AssignmentKind::Set,
                                index: slot,
                                expr,
                                line,
                            });
                        }
                    }
                    _ => {
                        return Err(unsupported(
                            update_span,
                            self.base,
                            self.source_id,
                            "non-local increment target",
                        ));
                    }
                }
            }
            other => {
                let span = other.span();
                let declared = self.expression_schema(&other);
                let lowered = self.lower_expr(other)?;
                self.push_expr_stmt(lowered, declared, span, line, output)?;
            }
        }
        self.stmt_spans.push(StmtSpanSite { line, span: site });
        Ok(())
    }

    fn lower_array_index_assignment(
        &mut self,
        member: ast::MemberExpr,
        op: ast::AssignOp,
        right: ast::Expr,
        line: u32,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let ast::MemberProp::Computed(property) = member.prop else {
            return Err(unsupported(
                member.span,
                self.base,
                self.source_id,
                "non-indexed assignment target",
            ));
        };
        let array_schema = self.expression_schema(&member.obj);
        // Reference-array carriers lower indexed writes through the owning
        // dispatcher; the plain Core-array path below never sees one.
        if array_schema
            .as_ref()
            .is_some_and(|schema| super::array_lowering::is_carrier_schema(schema.clone()))
        {
            if op != ast::AssignOp::Assign {
                return Err(unsupported(
                    member.span,
                    self.base,
                    self.source_id,
                    "compound indexed assignment operator",
                ));
            }
            let container = self.lower_expr(*member.obj)?;
            let index = self.lower_expr(*property.expr)?;
            let value = self.lower_expr(right)?;
            let written = self.syn_call(
                super::array_lowering::HELPER_WRITE,
                vec![container, index, value],
                member.span,
            )?;
            self.push_expr_stmt(written, Some(TypeSchema::Number), member.span, line, output)?;
            return Ok(());
        }
        if !array_schema.as_ref().is_some_and(|schema| {
            matches!(
                schema,
                TypeSchema::Array(_)
                    | TypeSchema::ArrayTuple(_)
                    | TypeSchema::ArrayTupleRest { .. }
            )
        }) {
            return Err(unsupported(
                member.span,
                self.base,
                self.source_id,
                "indexed assignment to a non-array expression",
            ));
        }
        let ast::Expr::Ident(binding) = *member.obj else {
            return Err(unsupported(
                member.span,
                self.base,
                self.source_id,
                "indexed assignment through a non-local array expression",
            ));
        };
        let name = binding.sym.to_string();
        let Some(source_slot) = self.locals.get(&name).copied() else {
            return Err(ParseError::at_span(
                byte_span(binding.span, self.base, self.source_id),
                format!("unresolved TypeScript indexed assignment target '{name}'"),
            ));
        };
        let slot = self.capture_slot(source_slot, &name, binding.span)?;
        if op != ast::AssignOp::Assign {
            return Err(unsupported(
                member.span,
                self.base,
                self.source_id,
                "compound indexed assignment operator",
            ));
        }
        let index = self.lower_expr(*property.expr)?;
        let value = self.lower_expr(right)?;
        let updated = Self::builtin_call(
            vm::BuiltinFunction::Set,
            vec![Expr::Var(slot), index, value],
        );
        output.push(Stmt::Assign {
            kind: AssignmentKind::Set,
            index: slot,
            expr: updated,
            line,
        });
        Ok(())
    }

    fn lower_for(
        &mut self,
        statement: ast::ForStmt,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let parent_scope = self.scope_id;
        let previous_locals = self.locals.clone();
        let previous_local_types = self.local_types.clone();
        let previous_mutable = self.mutable_locals.clone();
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            ParseError::at_span(
                byte_span(statement.span, self.base, self.source_id),
                "TypeScript lexical scope count exceeded",
            )
        })?;
        self.scope_id = scope_id;
        let declaration_start = self.local_decls.len();
        let loop_span = byte_span(statement.span, self.base, self.source_id);
        let line = line_at(self.source, loop_span.lo);
        let lowered = (|| -> Result<(Stmt, Expr, Stmt, Vec<Stmt>), ParseError> {
            let init = match statement.init {
                None => Stmt::Noop { line },
                Some(ast::VarDeclOrExpr::VarDecl(declaration)) => {
                    let mut lowered = Vec::new();
                    self.lower_var_decl(*declaration, false, &mut lowered)?;
                    if lowered.len() != 1 {
                        return Err(unsupported(
                            statement.span,
                            self.base,
                            self.source_id,
                            "multiple for-loop initializers",
                        ));
                    }
                    lowered.pop().expect("one initializer was checked")
                }
                Some(ast::VarDeclOrExpr::Expr(expression)) => {
                    let mut lowered = Vec::new();
                    self.with_consumed_stmt_values(|this| {
                        this.lower_assignment_statement(*expression, &mut lowered)
                    })?;
                    lowered
                        .pop()
                        .expect("expression initializer emits one statement")
                }
            };
            let condition = match statement.test {
                Some(test) => self.lower_expr(*test)?,
                None => Expr::Bool(true),
            };
            let post = match statement.update {
                Some(update) => {
                    let mut lowered = Vec::new();
                    self.with_consumed_stmt_values(|this| {
                        this.lower_assignment_statement(*update, &mut lowered)
                    })?;
                    lowered
                        .pop()
                        .expect("update expression emits one statement")
                }
                None => Stmt::Noop { line },
            };
            let mut body = Vec::new();
            self.with_consumed_stmt_values(|this| this.lower_stmt(*statement.body, &mut body))?;
            Ok((init, condition, post, body))
        })();
        let declarations = self.local_decls[declaration_start..]
            .iter()
            .filter(|site| site.scope_id == scope_id)
            .map(|site| site.slot)
            .collect();
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(parent_scope),
            range: loop_span,
            declarations,
            functions: Vec::new(),
        });
        self.scope_id = parent_scope;
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable;
        let (init, condition, post, body) = lowered?;
        output.push(Stmt::For {
            init: Box::new(init),
            condition,
            post: Box::new(post),
            body,
            line,
        });
        self.stmt_spans.push(StmtSpanSite {
            line,
            span: loop_span,
        });
        Ok(())
    }

    pub(crate) fn lower_throw(
        &mut self,
        statement: ast::ThrowStmt,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let statement_span = byte_span(statement.span, self.base, self.source_id);
        let line = line_at(self.source, statement_span.lo);
        let ast::Expr::New(new_expression) = *statement.arg else {
            return Err(unsupported(
                statement.span,
                self.base,
                self.source_id,
                "throw expression",
            ));
        };
        let ast::Expr::Ident(error_constructor) = new_expression.callee.as_ref() else {
            return Err(unsupported(
                new_expression.span,
                self.base,
                self.source_id,
                "throw constructor",
            ));
        };
        if error_constructor.sym != "Error" {
            return Err(unsupported(
                new_expression.span,
                self.base,
                self.source_id,
                "throw constructor",
            ));
        }
        let constructor_span = byte_span(error_constructor.span, self.base, self.source_id);
        if self.locals.contains_key("Error")
            || self.function_indices.contains_key("Error")
            || self.imported_functions.contains_key("Error")
            || self.host_function_indices.contains_key("Error")
        {
            return Err(ParseError::at_span(
                constructor_span,
                "TypeScript throw cannot use a shadowed Error binding",
            ));
        }
        if new_expression.type_args.is_some() {
            return Err(unsupported(
                new_expression.span,
                self.base,
                self.source_id,
                "Error constructor type arguments",
            ));
        }
        let Some(mut arguments) = new_expression.args else {
            return Err(ParseError::at_span(
                byte_span(new_expression.span, self.base, self.source_id),
                "TypeScript throw new Error requires one string-typed message",
            ));
        };
        if arguments.len() != 1 {
            return Err(ParseError::at_span(
                byte_span(new_expression.span, self.base, self.source_id),
                "TypeScript throw new Error requires one string-typed message",
            ));
        }
        let argument = arguments.remove(0);
        if argument.spread.is_some()
            || !matches!(
                self.expression_schema(&argument.expr),
                Some(TypeSchema::String)
            )
        {
            return Err(ParseError::at_span(
                byte_span(argument.span(), self.base, self.source_id),
                "TypeScript throw new Error requires one string-typed message",
            ));
        }
        let Some(index) = self
            .host_function_indices
            .get(crate::runtime::scalar_error::THROW_ERROR_BINDING)
            .and_then(|by_arity| by_arity.get(&1))
            .copied()
        else {
            return Err(ParseError::at_span(
                constructor_span,
                "TypeScript throw new Error requires the exact typescript::throw_error(message: string): null host catalog entry",
            ));
        };
        let message = self.lower_expr(*argument.expr)?;
        let id = self.alloc_node_id();
        let expression_span = byte_span(new_expression.span, self.base, self.source_id);
        self.call_sites.push(ParsedCallSite {
            id,
            callee_span: constructor_span,
            expr_span: expression_span,
            target: ParsedCallTarget::Function(index),
            name: crate::runtime::scalar_error::THROW_ERROR_FUNCTION.to_owned(),
            scope_id: self.scope_id,
            is_namespace_call: false,
        });
        // The host adapter always errors, but static control-flow analysis must
        // also balance its declared Null result on the non-error successor.
        let result_slot = self.alloc_hidden_local(new_expression.span)?;
        output.push(Stmt::Let {
            index: result_slot,
            declared_schema: Some(TypeSchema::Null),
            expr: Expr::Call(index, Vec::new(), vec![message], None, Some(id)),
            line,
        });
        self.stmt_spans.push(StmtSpanSite {
            line,
            span: statement_span,
        });
        Ok(())
    }

    fn lower_stmt(&mut self, stmt: ast::Stmt, output: &mut Vec<Stmt>) -> Result<(), ParseError> {
        match stmt {
            ast::Stmt::Decl(ast::Decl::Var(decl)) => self.lower_var_decl(*decl, false, output)?,
            ast::Stmt::Throw(statement) => self.lower_throw(statement, output)?,
            ast::Stmt::Expr(expr) => {
                self.lower_assignment_statement(*expr.expr, output)?;
            }
            ast::Stmt::Block(block) => {
                output.extend(self.lower_block(block)?);
            }
            ast::Stmt::If(if_statement) => {
                let condition = self.lower_expr(*if_statement.test)?;
                let mut then_branch = Vec::new();
                self.with_consumed_stmt_values(|this| {
                    this.lower_stmt(*if_statement.cons, &mut then_branch)
                })?;
                let mut else_branch = Vec::new();
                if let Some(else_statement) = if_statement.alt {
                    self.with_consumed_stmt_values(|this| {
                        this.lower_stmt(*else_statement, &mut else_branch)
                    })?;
                }
                let span = byte_span(if_statement.span, self.base, self.source_id);
                let line = line_at(self.source, span.lo);
                output.push(Stmt::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    line,
                });
                self.stmt_spans.push(StmtSpanSite { line, span });
            }
            ast::Stmt::For(for_statement) => self.lower_for(for_statement, output)?,
            ast::Stmt::While(while_statement) => {
                let condition = self.lower_expr(*while_statement.test)?;
                let mut body = Vec::new();
                self.with_consumed_stmt_values(|this| {
                    this.lower_stmt(*while_statement.body, &mut body)
                })?;
                let span = byte_span(while_statement.span, self.base, self.source_id);
                let line = line_at(self.source, span.lo);
                output.push(Stmt::While {
                    condition,
                    body,
                    line,
                });
                self.stmt_spans.push(StmtSpanSite { line, span });
            }
            ast::Stmt::Break(statement) => {
                let span = byte_span(statement.span, self.base, self.source_id);
                let line = line_at(self.source, span.lo);
                output.push(Stmt::Break { line });
                self.stmt_spans.push(StmtSpanSite { line, span });
            }
            ast::Stmt::Continue(statement) => {
                let span = byte_span(statement.span, self.base, self.source_id);
                let line = line_at(self.source, span.lo);
                output.push(Stmt::Continue { line });
                self.stmt_spans.push(StmtSpanSite { line, span });
            }
            ast::Stmt::Empty(_) => {}
            other => {
                return Err(unsupported(
                    other.span(),
                    self.base,
                    self.source_id,
                    "statement",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn own_closure_local(&mut self, slot: u16) {
        if let Some(context) = self.closure_captures.last_mut() {
            context.local_slots.insert(slot);
        }
    }

    pub(crate) fn capture_slot(
        &mut self,
        source_slot: u16,
        name: &str,
        span: swc_common::Span,
    ) -> Result<u16, ParseError> {
        let mut slot = source_slot;
        // Slots are globally unique before Core frame compaction. A binding (including
        // an already remapped capture) belongs to its innermost declaring context;
        // only closures inside that context need to capture it.
        let capture_start = self
            .closure_captures
            .iter()
            .rposition(|context| context.local_slots.contains(&source_slot))
            .map_or(0, |index| index + 1);
        for index in capture_start..self.closure_captures.len() {
            if let Some(captured) = self.closure_captures[index].captures.get(&slot).copied() {
                slot = captured;
                continue;
            }
            let capture_slot = self.next_slot;
            self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                ParseError::at_span(
                    byte_span(span, self.base, self.source_id),
                    "TypeScript closure capture slot limit exceeded",
                )
            })?;
            let context = &mut self.closure_captures[index];
            context.captures.insert(slot, capture_slot);
            context.capture_copies.push((slot, capture_slot));
            context.local_slots.insert(capture_slot);
            if let Some(origin) = self.const_callable_origins.get(&slot).cloned() {
                self.const_callable_origins.insert(capture_slot, origin);
            }
            if self.mutable_locals.contains(&slot) {
                self.mutable_locals.insert(capture_slot);
            }
            slot = capture_slot;
        }
        if !self.closure_captures.is_empty() {
            self.locals.insert(name.to_owned(), slot);
        }
        Ok(slot)
    }

    fn lower_arrow_body(&mut self, block: ast::FunctionBody) -> Result<Expr, ParseError> {
        let block_span = block.span;
        let mut statements = block.stmts;
        // Bodies with early returns lower through the guarded transform
        // (tail included) in the explicit mode; all other bodies keep the
        // exact historical path in both modes.
        let early = self.ref_mode && Self::body_has_early_return(&statements);
        let return_value = if early {
            None
        } else {
            match statements.pop() {
                Some(ast::Stmt::Return(return_statement)) => {
                    return_statement.arg.map(|value| *value)
                }
                Some(statement) => {
                    statements.push(statement);
                    None
                }
                None => None,
            }
        };
        let parent_scope = self.scope_id;
        let previous_locals = self.locals.clone();
        let previous_local_types = self.local_types.clone();
        let previous_mutable = self.mutable_locals.clone();
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            ParseError::at_span(
                byte_span(block_span, self.base, self.source_id),
                "TypeScript lexical scope count exceeded",
            )
        })?;
        self.scope_id = scope_id;
        let declaration_start = self.local_decls.len();
        let result = (|| {
            if early {
                let (stmts, expr, _) =
                    self.lower_body_with_early_returns(statements, block_span, None)?;
                Ok(Expr::Block {
                    stmts,
                    expr: Box::new(expr),
                })
            } else {
                let mut lowered_statements = Vec::new();
                for statement in statements {
                    self.with_consumed_stmt_values(|this| {
                        this.lower_stmt(statement, &mut lowered_statements)
                    })?;
                }
                let expr = match return_value {
                    Some(value) => self.lower_expr(value)?,
                    None => Expr::Null,
                };
                Ok(Expr::Block {
                    stmts: lowered_statements,
                    expr: Box::new(expr),
                })
            }
        })();
        let declarations = self.local_decls[declaration_start..]
            .iter()
            .filter(|site| site.scope_id == scope_id)
            .map(|site| site.slot)
            .collect();
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(parent_scope),
            range: byte_span(block_span, self.base, self.source_id),
            declarations,
            functions: Vec::new(),
        });
        self.scope_id = parent_scope;
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable;
        result
    }

    fn lower_arrow(&mut self, arrow: ast::ArrowExpr) -> Result<Expr, ParseError> {
        if arrow.is_async || arrow.type_params.is_some() {
            return Err(unsupported(
                arrow.span,
                self.base,
                self.source_id,
                "async or generic arrow function",
            ));
        }
        let parent_scope = self.scope_id;
        let previous_locals = self.locals.clone();
        let previous_local_types = self.local_types.clone();
        let previous_mutable = self.mutable_locals.clone();
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            ParseError::at_span(
                byte_span(arrow.span, self.base, self.source_id),
                "TypeScript lexical scope count exceeded",
            )
        })?;
        self.scope_id = scope_id;
        let declaration_start = self.local_decls.len();
        let mut param_slots = Vec::new();
        let mut local_slots = HashSet::new();
        let mut seen = HashSet::new();
        for (order, param) in arrow.params.iter().enumerate() {
            let Some((binding, default)) = parameter_binding(param) else {
                return Err(unsupported(
                    param.span(),
                    self.base,
                    self.source_id,
                    "arrow parameter pattern",
                ));
            };
            if default.is_some() {
                return Err(unsupported(
                    param.span(),
                    self.base,
                    self.source_id,
                    "default parameter on nested arrow function",
                ));
            }
            let name = binding.id.sym.to_string();
            if !seen.insert(name.clone()) {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!("duplicate TypeScript arrow parameter '{name}'"),
                ));
            }
            let schema = binding
                .type_ann
                .as_ref()
                .map(|annotation| {
                    self.lower_type(&annotation.type_ann)
                        .map(|schema| self.ref_map_number_array(schema, binding.id.span))
                })
                .transpose()?;
            let slot = self.next_slot;
            self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    "TypeScript closure local slot limit exceeded",
                )
            })?;
            self.locals.insert(name.clone(), slot);
            if let Some(schema) = schema {
                self.local_types.insert(name.clone(), schema);
            } else {
                self.local_types.remove(&name);
            }
            local_slots.insert(slot);
            param_slots.push(slot);
            let ident_span = byte_span(binding.id.span, self.base, self.source_id);
            let node_id = self.alloc_node_id();
            self.local_decls.push(LocalDeclSite {
                id: node_id,
                ident_span,
                stmt_span: byte_span(param.span(), self.base, self.source_id),
                slot,
                name,
                scope_id,
                decl_order: u32::try_from(order).unwrap_or(u32::MAX),
            });
        }
        self.closure_captures.push(ClosureCaptureContext {
            local_slots,
            captures: HashMap::new(),
            capture_copies: Vec::new(),
        });
        let body_result = match *arrow.body {
            ast::ArrowFunctionBody::Expr(expression) => self.lower_expr(*expression),
            ast::ArrowFunctionBody::FunctionBody(block) => self.lower_arrow_body(block),
        };
        let capture_context = self
            .closure_captures
            .pop()
            .expect("the current arrow closure context was pushed");
        let declarations = self.local_decls[declaration_start..]
            .iter()
            .filter(|site| site.scope_id == scope_id)
            .map(|site| site.slot)
            .collect();
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(parent_scope),
            range: byte_span(arrow.span, self.base, self.source_id),
            declarations,
            functions: Vec::new(),
        });
        self.scope_id = parent_scope;
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable;
        let body = body_result?;
        Ok(Expr::Closure(vm::compiler::ir::ClosureExpr {
            param_slots,
            capture_copies: capture_context.capture_copies,
            body: Box::new(body),
        }))
    }

    fn schema_field(&self, schema: &TypeSchema, property: &str) -> Option<TypeSchema> {
        fn find_field(
            schema: &TypeSchema,
            property: &str,
            struct_schemas: &HashMap<String, StructDecl>,
            named_type_declarations: &HashMap<String, StructDecl>,
            named_types: &HashMap<String, TypeSchema>,
            aliases: &HashMap<String, (Vec<String>, TypeSchema)>,
            depth: usize,
        ) -> Option<TypeSchema> {
            if depth > 32 {
                return None;
            }
            match schema {
                TypeSchema::Array(_)
                | TypeSchema::ArrayTuple(_)
                | TypeSchema::ArrayTupleRest { .. }
                    if property == "length" =>
                {
                    Some(TypeSchema::Number)
                }
                TypeSchema::Object(fields) => fields.get(property).cloned(),
                TypeSchema::Optional(_) => None,
                TypeSchema::Named(name, arguments) => {
                    let declaration = struct_schemas
                        .get(name)
                        .or_else(|| named_type_declarations.get(name))
                        .or_else(|| {
                            named_types
                                .iter()
                                .find_map(|(source_name, alias)| match alias {
                                    TypeSchema::Named(canonical, _) if canonical == name => {
                                        named_type_declarations.get(source_name)
                                    }
                                    _ => None,
                                })
                        });
                    if let Some(declaration) = declaration {
                        if declaration.type_params.len() != arguments.len() {
                            return None;
                        }
                        let bindings = declaration
                            .type_params
                            .iter()
                            .cloned()
                            .zip(arguments.iter().cloned())
                            .collect::<HashMap<_, _>>();
                        let body = substitute_type_schema(&declaration.body_schema, &bindings);
                        return find_field(
                            &body,
                            property,
                            struct_schemas,
                            named_type_declarations,
                            named_types,
                            aliases,
                            depth + 1,
                        );
                    }
                    if let Some((parameters, body)) = aliases.get(name) {
                        if parameters.len() != arguments.len() {
                            return None;
                        }
                        let bindings = parameters
                            .iter()
                            .cloned()
                            .zip(arguments.iter().cloned())
                            .collect::<HashMap<_, _>>();
                        let body = substitute_type_schema(body, &bindings);
                        return find_field(
                            &body,
                            property,
                            struct_schemas,
                            named_type_declarations,
                            named_types,
                            aliases,
                            depth + 1,
                        );
                    }
                    let expanded = named_types.get(name)?;
                    if expanded == schema {
                        return None;
                    }
                    find_field(
                        expanded,
                        property,
                        struct_schemas,
                        named_type_declarations,
                        named_types,
                        aliases,
                        depth + 1,
                    )
                }
                _ => None,
            }
        }

        find_field(
            schema,
            property,
            &self.struct_schemas,
            &self.named_type_declarations,
            &self.named_types,
            &self.type_alias_schemas,
            0,
        )
    }

    pub(crate) fn expression_schema(&self, expression: &ast::Expr) -> Option<TypeSchema> {
        match expression {
            ast::Expr::Lit(ast::Lit::Num(value)) => Some(Self::numeric_literal_schema(value.value)),
            ast::Expr::Lit(ast::Lit::Str(_)) => Some(TypeSchema::String),
            ast::Expr::Lit(ast::Lit::Bool(_)) => Some(TypeSchema::Bool),
            ast::Expr::Lit(ast::Lit::Null(_)) => Some(TypeSchema::Null),
            ast::Expr::Ident(ident) => {
                let name = ident.sym.as_ref();
                if let Some(schema) = self.local_types.get(name) {
                    return Some(schema.clone());
                }
                if !self.ref_mode
                    || self.in_ref_prelude
                    || self.locals.contains_key(name)
                    || self.host_function_indices.contains_key(name)
                {
                    return None;
                }
                // Module functions resolve through the lowered declaration
                // once present, else the predeclared annotation facts
                // (recursive and forward references lower against the same
                // declared schemas as later calls). Generics stay unresolved.
                if self.function_indices.contains_key(name) {
                    let (arg_schemas, return_schema) = self.module_function_schemas(name)?;
                    return Some(TypeSchema::Callable {
                        params: arg_schemas.into_iter().collect::<Option<Vec<_>>>()?,
                        result: Box::new(return_schema?),
                    });
                }
                let (_, declaration) = self.imported_functions.get(name)?;
                if !declaration.type_params.is_empty() {
                    return None;
                }
                Some(TypeSchema::Callable {
                    params: declaration
                        .arg_schemas
                        .iter()
                        .cloned()
                        .collect::<Option<Vec<_>>>()?,
                    result: Box::new(declaration.return_schema.clone()?),
                })
            }
            ast::Expr::Array(array) => {
                // Reference-array mode only: infer a staging array so
                // `number[]` bindings convert through the factory. Plain
                // mode keeps the historical `None`. Indexed carrier reads
                // count as numeric elements (strict reads yield numbers);
                // anything else keeps `None`.
                if !self.ref_mode || array.elems.is_empty() {
                    return None;
                }
                let mut numeric = true;
                for element in &array.elems {
                    let Some(element) = element else {
                        return None;
                    };
                    if element.spread.is_some() {
                        return None;
                    }
                    let element_numeric = self
                        .expression_schema(&element.expr)
                        .is_some_and(|schema| Self::is_numeric_schema(&schema))
                        || self.ref_index_access(&element.expr).is_some();
                    if !element_numeric {
                        numeric = false;
                        break;
                    }
                }
                numeric.then(|| TypeSchema::Array(Box::new(TypeSchema::Number)))
            }
            ast::Expr::Update(update) => {
                if !self.ref_mode {
                    return None;
                }
                // An indexed carrier read yields a number; a numeric local
                // keeps its own schema so Int bindings stay Int.
                match update.arg.as_ref() {
                    ast::Expr::Ident(_) => self
                        .expression_schema(&update.arg)
                        .filter(Self::is_numeric_schema),
                    target if self.ref_index_access(target).is_some() => Some(TypeSchema::Number),
                    _ => None,
                }
            }
            ast::Expr::Paren(paren) => self.expression_schema(&paren.expr),
            ast::Expr::Unary(unary) => match unary.op {
                ast::UnaryOp::Bang => Some(TypeSchema::Bool),
                ast::UnaryOp::Minus => parsed_numeric_literal(expression)
                    .map(Self::numeric_literal_schema)
                    .or_else(|| {
                        self.expression_schema(&unary.arg)
                            .filter(Self::is_numeric_schema)
                            .map(|_| TypeSchema::Number)
                    }),
                _ => None,
            },
            ast::Expr::Bin(binary) => {
                let left = self.expression_schema(&binary.left);
                let right = self.expression_schema(&binary.right);
                match binary.op {
                    ast::BinaryOp::Add
                        if left
                            .as_ref()
                            .is_some_and(|schema| matches!(schema, TypeSchema::String))
                            || right
                                .as_ref()
                                .is_some_and(|schema| matches!(schema, TypeSchema::String)) =>
                    {
                        Some(TypeSchema::String)
                    }
                    ast::BinaryOp::Add
                    | ast::BinaryOp::Sub
                    | ast::BinaryOp::Mul
                    | ast::BinaryOp::Div
                    | ast::BinaryOp::Mod
                        if self.in_gen_body
                            && left
                                .as_ref()
                                .is_some_and(|schema| matches!(schema, TypeSchema::Int))
                            && right
                                .as_ref()
                                .is_some_and(|schema| matches!(schema, TypeSchema::Int)) =>
                    {
                        // Generator bodies track integer precision: the
                        // backend keeps pure integer arithmetic exact, so the
                        // schema agrees. Other contexts keep `number`.
                        Some(TypeSchema::Int)
                    }
                    ast::BinaryOp::Add
                    | ast::BinaryOp::Sub
                    | ast::BinaryOp::Mul
                    | ast::BinaryOp::Div
                    | ast::BinaryOp::Mod
                        if left.as_ref().is_some_and(Self::is_numeric_schema)
                            && right.as_ref().is_some_and(Self::is_numeric_schema) =>
                    {
                        Some(TypeSchema::Number)
                    }
                    // Explicit reference-array mode only: statically proven
                    // numeric `**` (the lowered Core power builtin returns a
                    // float, a numeric). Ordinary mode keeps `None`.
                    ast::BinaryOp::Exp
                        if self.ref_mode
                            && left.as_ref().is_some_and(Self::is_numeric_schema)
                            && right.as_ref().is_some_and(Self::is_numeric_schema) =>
                    {
                        Some(TypeSchema::Number)
                    }
                    ast::BinaryOp::EqEq
                    | ast::BinaryOp::EqEqEq
                    | ast::BinaryOp::NotEq
                    | ast::BinaryOp::NotEqEq
                    | ast::BinaryOp::Lt
                    | ast::BinaryOp::Gt => Some(TypeSchema::Bool),
                    // Explicit reference-array mode only: statically proven
                    // numeric `<=`/`>=` (lowered without new IR below).
                    // Ordinary mode keeps `None`.
                    ast::BinaryOp::LtEq | ast::BinaryOp::GtEq
                        if self.ref_mode
                            && left.as_ref().is_some_and(Self::is_numeric_schema)
                            && right.as_ref().is_some_and(Self::is_numeric_schema) =>
                    {
                        Some(TypeSchema::Bool)
                    }
                    _ => None,
                }
            }
            ast::Expr::Cond(conditional) => {
                let consequent = self.expression_schema(&conditional.cons)?;
                let alternate = self.expression_schema(&conditional.alt)?;
                if consequent == alternate {
                    Some(consequent)
                } else if Self::is_numeric_schema(&consequent)
                    && Self::is_numeric_schema(&alternate)
                {
                    Some(TypeSchema::Number)
                } else {
                    None
                }
            }
            ast::Expr::Arrow(arrow) => {
                let mut params = Vec::with_capacity(arrow.params.len());
                for parameter in &arrow.params {
                    let (binding, default) = parameter_binding(parameter)?;
                    if default.is_some() {
                        return None;
                    }
                    let annotation = binding.type_ann.as_ref()?;
                    params.push(self.ref_map_number_array(
                        self.lower_type(&annotation.type_ann).ok()?,
                        binding.id.span,
                    ));
                }
                let result = if let Some(return_type) = arrow.return_type.as_ref() {
                    self.ref_map_number_array(
                        self.lower_type(&return_type.type_ann).ok()?,
                        arrow.span,
                    )
                } else if self.ref_mode && !self.in_ref_prelude && arrow.params.is_empty() {
                    // No parameter bindings can shadow the current lexical
                    // environment: an expression body has its actual schema.
                    match arrow.body.as_ref() {
                        ast::ArrowFunctionBody::Expr(body) => self.expression_schema(body)?,
                        _ => return None,
                    }
                } else {
                    return None;
                };
                Some(TypeSchema::Callable {
                    params,
                    result: Box::new(result),
                })
            }
            ast::Expr::Member(member) => {
                // A strict indexed carrier read yields a number. Routing it
                // through numeric strict equality (instead of the generic
                // object comparison) keeps both the interpreter and the
                // native backend on the statically typed path. Plain arrays
                // keep the historical `None`.
                if matches!(member.prop, ast::MemberProp::Computed(_))
                    && self.ref_carrier_of(&member.obj)
                {
                    return Some(TypeSchema::Number);
                }
                let ast::MemberProp::Ident(property) = &member.prop else {
                    return None;
                };
                let object = self.expression_schema(&member.obj)?;
                self.schema_field(&object, property.sym.as_ref())
            }
            ast::Expr::Call(call) => self.call_result_schema(call),
            _ => None,
        }
    }

    pub(crate) fn is_numeric_schema(schema: &TypeSchema) -> bool {
        matches!(
            schema,
            TypeSchema::Int | TypeSchema::Float | TypeSchema::Number
        )
    }

    fn numeric_literal_schema(value: f64) -> TypeSchema {
        match numeric_literal(value) {
            Expr::Int(_) => TypeSchema::Int,
            _ => TypeSchema::Float,
        }
    }

    fn host_return_schema(&self, index: u16) -> Option<TypeSchema> {
        let candidates = self.host_api_metadata.as_ref()?.candidates(index)?;
        let mut candidates = candidates.iter();
        let first = Self::host_type_schema(&candidates.next()?.return_type);
        candidates
            .all(|candidate| Self::host_type_schema(&candidate.return_type) == first)
            .then_some(first)
    }

    fn host_type_schema(schema: &vm::HostTypeSchema) -> TypeSchema {
        match schema {
            vm::HostTypeSchema::Unknown => TypeSchema::Unknown,
            vm::HostTypeSchema::Null => TypeSchema::Null,
            vm::HostTypeSchema::Int => TypeSchema::Int,
            vm::HostTypeSchema::Float => TypeSchema::Float,
            vm::HostTypeSchema::Number => TypeSchema::Number,
            vm::HostTypeSchema::Bool => TypeSchema::Bool,
            vm::HostTypeSchema::String => TypeSchema::String,
            vm::HostTypeSchema::Bytes => TypeSchema::Bytes,
            vm::HostTypeSchema::Array(inner) => {
                TypeSchema::Array(Box::new(Self::host_type_schema(inner)))
            }
            vm::HostTypeSchema::Map(inner) => {
                TypeSchema::Map(Box::new(Self::host_type_schema(inner)))
            }
            vm::HostTypeSchema::Optional(inner) => {
                TypeSchema::Optional(Box::new(Self::host_type_schema(inner)))
            }
            vm::HostTypeSchema::Callable { params, result } => TypeSchema::Callable {
                params: params.iter().map(Self::host_type_schema).collect(),
                result: Box::new(Self::host_type_schema(result)),
            },
            vm::HostTypeSchema::Resource(key) => TypeSchema::Resource(key.clone()),
            vm::HostTypeSchema::Named { name, .. } => TypeSchema::Named(name.clone(), Vec::new()),
        }
    }

    fn standard_call_result_schema(callee: &str) -> Option<TypeSchema> {
        match callee {
            "Math::floor" | "Math::sqrt" | "Math::round" => Some(TypeSchema::Number),
            "Number::isInteger" | "Number::isNaN" | "isNaN" => Some(TypeSchema::Bool),
            _ => None,
        }
    }

    pub(crate) fn call_result_schema(&self, call: &ast::CallExpr) -> Option<TypeSchema> {
        // Generator protocol: a `.next()` call on a locally known iterator
        // carries the private result nominal, so `.value`/`.done` member
        // reads resolve to their honest field schemas.
        if let ast::Callee::Expr(callee) = &call.callee
            && let ast::Expr::Member(member) = callee.as_ref()
            && let ast::MemberProp::Ident(property) = &member.prop
            && property.sym.as_ref() == "next"
            && let Some(info) = self.generator_iterator_of(&member.obj)
        {
            return Some(info.result_schema());
        }
        let callee = match &call.callee {
            ast::Callee::Expr(callee) => match callee.as_ref() {
                ast::Expr::Ident(ident) => ident.sym.to_string(),
                ast::Expr::Member(member) => {
                    let ast::Expr::Ident(namespace) = member.obj.as_ref() else {
                        return None;
                    };
                    let ast::MemberProp::Ident(property) = &member.prop else {
                        return None;
                    };
                    format!("{}::{}", namespace.sym, property.sym)
                }
                _ => return None,
            },
            _ => return None,
        };
        if self.locals.contains_key(&callee) {
            return match self.local_types.get(&callee) {
                Some(TypeSchema::Callable { result, .. }) => Some((**result).clone()),
                _ => None,
            };
        }
        if self.function_indices.contains_key(&callee) {
            // Lowered declaration once the body is lowered, else the
            // predeclared annotation facts (recursive and forward calls
            // observe the same declared return as later calls).
            let (_, return_schema) = self.module_function_schemas(&callee)?;
            return return_schema;
        }
        let declaration = if let Some((_, declaration)) = self.imported_functions.get(&callee) {
            Some(declaration)
        } else {
            let arity = u8::try_from(call.args.len()).ok()?;
            if let Some(index) = self
                .host_function_indices
                .get(&callee)
                .and_then(|overloads| overloads.get(&arity))
            {
                return self.host_return_schema(*index);
            }
            return Self::standard_call_result_schema(&callee);
        }?;
        if declaration.type_params.is_empty() {
            declaration.return_schema.clone()
        } else {
            None
        }
    }

    /// True for declaration sites in the user region. Prelude helpers are
    /// parsed separately and merged as hygienic AST items; they keep plain
    /// array semantics while user bindings map to the carrier.
    fn ref_is_user_span(&self, _span: swc_common::Span) -> bool {
        self.ref_mode && !self.in_ref_prelude
    }

    /// Flag-based user-region check used by newly merged code paths.
    #[allow(dead_code)]
    fn ref_is_user(&self) -> bool {
        self.ref_mode && !self.in_ref_prelude
    }

    /// Maps a logical `number[]` binding schema to the physical carrier in
    /// the user region. Every other schema (including prelude-region
    /// staging arrays) passes through unchanged.
    pub(crate) fn ref_map_number_array(
        &self,
        schema: TypeSchema,
        site: swc_common::Span,
    ) -> TypeSchema {
        if self.ref_is_user_span(site)
            && super::array_lowering::is_logical_number_array(schema.clone())
        {
            super::array_lowering::carrier_schema()
        } else {
            schema
        }
    }

    /// True when `expr` denotes a reference-array carrier: its schema is
    /// exactly the private carrier record. Callable shapes, global slots
    /// and names never qualify.
    fn ref_carrier_of(&self, expr: &ast::Expr) -> bool {
        self.expression_schema(expr)
            .is_some_and(super::array_lowering::is_carrier_schema)
    }

    /// Decomposes `carrier[index]` over a reference-array carrier.
    fn ref_index_access<'e>(&self, expr: &'e ast::Expr) -> Option<(&'e ast::Expr, &'e ast::Expr)> {
        let ast::Expr::Member(member) = expr else {
            return None;
        };
        if !matches!(member.prop, ast::MemberProp::Computed(_)) {
            return None;
        }
        if !self.ref_carrier_of(&member.obj) {
            return None;
        }
        let ast::MemberProp::Computed(index) = &member.prop else {
            return None;
        };
        Some((member.obj.as_ref(), index.expr.as_ref()))
    }

    /// Lowers an ordinary guest call into a reference-array prelude helper.
    /// Generated operations never synthesize direct host calls; capture
    /// inference, host-call wrapping and schema checking reuse the exact
    /// existing call lowering path through the helper bodies.
    fn syn_call(
        &mut self,
        helper: &str,
        args: Vec<Expr>,
        span: swc_common::Span,
    ) -> Result<Expr, ParseError> {
        let (index, _) = self.function_indices.get(helper).copied().ok_or_else(|| {
            ParseError::at_span(
                byte_span(span, self.base, self.source_id),
                format!("reference-array helper '{helper}' is not available"),
            )
        })?;
        let id = self.alloc_node_id();
        let site = byte_span(span, self.base, self.source_id);
        self.call_sites.push(ParsedCallSite {
            id,
            callee_span: site,
            expr_span: site,
            target: ParsedCallTarget::Function(index),
            name: helper.to_owned(),
            scope_id: self.scope_id,
            is_namespace_call: false,
        });
        Ok(Expr::Call(index, Vec::new(), args, None, Some(id)))
    }

    /// Hoists an already-lowered receiver, index or snapshot expression into
    /// a hidden local appended to `output`, returning its slot. Call sites
    /// evaluate their arguments exactly once, so hoisting the receiver and
    /// index first gives correct once-only evaluation for guarded
    /// comparisons, updates and destructuring swaps.
    fn hoist_hidden(
        &mut self,
        value_ast: &ast::Expr,
        output: &mut Vec<Stmt>,
        line: u32,
        span: swc_common::Span,
    ) -> Result<u16, ParseError> {
        let declared = self
            .expression_schema(value_ast)
            .or(Some(TypeSchema::Number));
        let value = self.lower_expr(value_ast.clone())?;
        let slot = self.alloc_hidden_local(span)?;
        output.push(Stmt::Let {
            index: slot,
            declared_schema: declared,
            expr: value,
            line,
        });
        Ok(slot)
    }

    /// Lowers `[carrier[i], carrier[j]] = [x, y]` over reference-array
    /// carriers. Every right-hand side element evaluates left to right
    /// into a snapshot first; then each left-hand receiver and index
    /// evaluates exactly once at its assignment point, left to right.
    /// Observable order matches an independent Node execution.
    fn lower_destructuring_assignment(
        &mut self,
        pattern: ast::AssignTargetPat,
        right: ast::Expr,
        op: ast::AssignOp,
        span: swc_common::Span,
        line: u32,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if op != ast::AssignOp::Assign {
            return Err(unsupported(
                span,
                self.base,
                self.source_id,
                "compound destructuring assignment",
            ));
        }
        let ast::AssignTargetPat::Array(elements) = pattern else {
            return Err(unsupported(
                span,
                self.base,
                self.source_id,
                "non-array destructuring target",
            ));
        };
        let ast::Expr::Array(sources) = right else {
            return Err(unsupported(
                span,
                self.base,
                self.source_id,
                "non-array destructuring source",
            ));
        };
        if sources.elems.len() != elements.elems.len() {
            return Err(unsupported(
                span,
                self.base,
                self.source_id,
                "destructuring arity",
            ));
        }
        let mut targets = Vec::with_capacity(elements.elems.len());
        for element in &elements.elems {
            let Some(element) = element else {
                return Err(unsupported(
                    span,
                    self.base,
                    self.source_id,
                    "destructuring hole",
                ));
            };
            let ast::Pat::Expr(target) = element else {
                return Err(unsupported(
                    span,
                    self.base,
                    self.source_id,
                    "destructuring target",
                ));
            };
            let Some((receiver, index)) = self.ref_index_access(target) else {
                return Err(unsupported(
                    span,
                    self.base,
                    self.source_id,
                    "non-indexed destructuring target",
                ));
            };
            targets.push((receiver, index));
        }
        let mut snapshots = Vec::with_capacity(sources.elems.len());
        for element in sources.elems {
            let Some(element) = element else {
                return Err(unsupported(
                    span,
                    self.base,
                    self.source_id,
                    "sparse destructuring source",
                ));
            };
            if element.spread.is_some() {
                return Err(unsupported(
                    span,
                    self.base,
                    self.source_id,
                    "spread destructuring source",
                ));
            }
            snapshots.push(Expr::Var(self.hoist_hidden(
                &element.expr,
                output,
                line,
                span,
            )?));
        }
        for ((receiver, index), snapshot) in targets.into_iter().zip(snapshots) {
            let carrier = Expr::Var(self.hoist_hidden(receiver, output, line, span)?);
            let position = Expr::Var(self.hoist_hidden(index, output, line, span)?);
            let written = self.syn_call(
                super::array_lowering::HELPER_WRITE,
                vec![carrier, position, snapshot],
                span,
            )?;
            self.push_expr_stmt(written, Some(TypeSchema::Number), span, line, output)?;
        }
        Ok(())
    }

    /// Explicit reference-array mode only: statically proven numeric
    /// `<=`/`>=` as once-only hoisted `<`/`>`-or-typed-`===`, reusing only
    /// existing IR. Guards stay in the Bool domain for native lowering.
    /// Carrier-indexed operands stay precisely rejected (presence-aware
    /// non-strict comparison has no honest lowering here); ordinary mode
    /// and non-numeric operands fall through to the historical rejection.
    fn lower_non_strict_comparison(
        &mut self,
        value: &ast::BinExpr,
    ) -> Result<Option<Expr>, ParseError> {
        if !self.ref_mode {
            return Ok(None);
        }
        let strict = match value.op {
            ast::BinaryOp::LtEq => true,
            ast::BinaryOp::GtEq => false,
            _ => return Ok(None),
        };
        let left_schema = self.expression_schema(&value.left);
        let right_schema = self.expression_schema(&value.right);
        if !left_schema.as_ref().is_some_and(Self::is_numeric_schema)
            || !right_schema.as_ref().is_some_and(Self::is_numeric_schema)
        {
            return Ok(None);
        }
        if self.ref_index_access(&value.left).is_some()
            || self.ref_index_access(&value.right).is_some()
        {
            return Err(unsupported(
                value.span,
                self.base,
                self.source_id,
                "guarded non-strict comparison over reference arrays",
            ));
        }
        let site = byte_span(value.span, self.base, self.source_id);
        let line = line_at(self.source, site.lo);
        let left = self.lower_expr((*value.left).clone())?;
        let right = self.lower_expr((*value.right).clone())?;
        let left_slot = self.alloc_hidden_local(value.span)?;
        let right_slot = self.alloc_hidden_local(value.span)?;
        let ordering = if strict {
            Expr::Lt(
                Box::new(Expr::Var(left_slot)),
                Box::new(Expr::Var(right_slot)),
            )
        } else {
            Expr::Gt(
                Box::new(Expr::Var(left_slot)),
                Box::new(Expr::Var(right_slot)),
            )
        };
        let equality =
            Self::lower_numeric_strict_equality(Expr::Var(left_slot), Expr::Var(right_slot));
        Ok(Some(Expr::Block {
            stmts: vec![
                Stmt::Let {
                    index: left_slot,
                    declared_schema: left_schema,
                    expr: left,
                    line,
                },
                Stmt::Let {
                    index: right_slot,
                    declared_schema: right_schema,
                    expr: right,
                    line,
                },
            ],
            expr: Box::new(Expr::Or(Box::new(ordering), Box::new(equality))),
        }))
    }

    /// Lowers a guarded relational comparison with an absent-aware numeric
    /// element. A guarded comparison evaluates to false when an integral
    /// index is missing instead of presenting a fabricated element value;
    /// other missing-value contexts keep failing precisely. Returns `None`
    /// when neither operand is a carrier-indexed read, leaving the plain
    /// comparison path untouched.
    fn lower_guarded_comparison(
        &mut self,
        value: &ast::BinExpr,
    ) -> Result<Option<Expr>, ParseError> {
        let two_sided_helper = match value.op {
            ast::BinaryOp::Lt => Some((
                super::array_lowering::HELPER_LT,
                super::array_lowering::HELPER_GT,
                super::array_lowering::HELPER_LT2,
            )),
            ast::BinaryOp::Gt => Some((
                super::array_lowering::HELPER_GT,
                super::array_lowering::HELPER_LT,
                super::array_lowering::HELPER_GT2,
            )),
            _ => None,
        };
        let Some((one_sided, swapped, two_sided)) = two_sided_helper else {
            return Ok(None);
        };
        let left = self.ref_index_access(&value.left);
        let right = self.ref_index_access(&value.right);
        if left.is_none() && right.is_none() {
            return Ok(None);
        }
        let site = byte_span(value.span, self.base, self.source_id);
        let line = line_at(self.source, site.lo);
        let mut stmts = Vec::new();
        let guarded = match (left, right) {
            (Some((receiver, index)), Some((other_receiver, other_index))) => {
                // Two-sided guarded comparisons stay on one owning
                // dispatcher: the second index travels in the `value`
                // parameter. Cross-carrier element comparisons fail
                // precisely instead of silently dropping presence semantics.
                let same = match (receiver, other_receiver) {
                    (ast::Expr::Ident(first), ast::Expr::Ident(second)) => {
                        first.sym == second.sym
                            && self.locals.get(first.sym.as_ref())
                                == self.locals.get(second.sym.as_ref())
                    }
                    _ => false,
                };
                if !same {
                    return Err(unsupported(
                        value.span,
                        self.base,
                        self.source_id,
                        "cross-carrier guarded comparison",
                    ));
                }
                let carrier = Expr::Var(self.hoist_hidden(receiver, &mut stmts, line, value.span)?);
                let position = Expr::Var(self.hoist_hidden(index, &mut stmts, line, value.span)?);
                let other_position =
                    Expr::Var(self.hoist_hidden(other_index, &mut stmts, line, value.span)?);
                self.syn_call(
                    two_sided,
                    vec![carrier, position, other_position],
                    value.span,
                )?
            }
            (Some((receiver, index)), None) => {
                let carrier = Expr::Var(self.hoist_hidden(receiver, &mut stmts, line, value.span)?);
                let position = Expr::Var(self.hoist_hidden(index, &mut stmts, line, value.span)?);
                let other = self.lower_expr((*value.right).clone())?;
                self.syn_call(one_sided, vec![carrier, position, other], value.span)?
            }
            (None, Some((receiver, index))) => {
                // `x < carrier[i]` canonicalizes to `carrier[i] > x` so one
                // helper family covers both operand orders. The left side
                // still evaluates first, preserving JS order.
                let other = self.lower_expr((*value.left).clone())?;
                let carrier = Expr::Var(self.hoist_hidden(receiver, &mut stmts, line, value.span)?);
                let position = Expr::Var(self.hoist_hidden(index, &mut stmts, line, value.span)?);
                self.syn_call(swapped, vec![carrier, position, other], value.span)?
            }
            (None, None) => return Ok(None),
        };
        Ok(Some(Expr::Block {
            stmts,
            expr: Box::new(guarded),
        }))
    }

    /// Lowers prefix/postfix `++` and `--` in expression position with
    /// correct once-only evaluation: the target and, for indexed elements,
    /// the receiver and index each evaluate exactly once. Uses the
    /// existing `Expr::Block` with typed hidden locals; no IR or opcode
    /// changes and no hoisting across short-circuit boundaries.
    pub(crate) fn lower_update_expr(
        &mut self,
        update: ast::UpdateExpr,
    ) -> Result<Expr, ParseError> {
        let span = update.span;
        let site = byte_span(span, self.base, self.source_id);
        let line = line_at(self.source, site.lo);
        let decrement = update.op == ast::UpdateOp::MinusMinus;
        let bump_int = |previous: Expr| {
            if decrement {
                Expr::Sub(Box::new(previous), Box::new(Expr::Int(1)))
            } else {
                Expr::Add(Box::new(previous), Box::new(Expr::Int(1)))
            }
        };
        // Explicit reference-array mode floating `++`: feeds the existing
        // Float Add backend. `--` keeps Sub (tagged-Number Sub exists).
        let bump_float = |previous: Expr| {
            if decrement {
                Expr::Sub(Box::new(previous), Box::new(Expr::Int(1)))
            } else {
                Self::lower_numeric_add(previous, Expr::Int(1))
            }
        };
        match *update.arg {
            ast::Expr::Ident(ident) => {
                let name = ident.sym.to_string();
                let Some(source_slot) = self.locals.get(&name).copied() else {
                    return Err(ParseError::at_span(
                        byte_span(ident.span, self.base, self.source_id),
                        format!("unresolved TypeScript update target '{name}'"),
                    ));
                };
                if !self.mutable_locals.contains(&source_slot) {
                    return Err(ParseError::at_span(
                        byte_span(ident.span, self.base, self.source_id),
                        format!("cannot update const binding '{name}'"),
                    ));
                }
                // The hidden previous value keeps the target's own numeric
                // schema (Int stays Int) so bindings of the update result
                // keep their precise type.
                let target_schema = self
                    .expression_schema(&ast::Expr::Ident(ident.clone()))
                    .filter(Self::is_numeric_schema)
                    .or(Some(TypeSchema::Number));
                let slot = self.capture_slot(source_slot, &name, ident.span)?;
                let read = self.lower_expr(ast::Expr::Ident(ident))?;
                let previous_slot = self.alloc_hidden_local(span)?;
                // In explicit reference-array mode a Number/Float local
                // `++` uses the floating projection; pure Int keeps the
                // exact integer path (native Int Add exists).
                let use_float =
                    self.ref_mode && !decrement && !matches!(target_schema, Some(TypeSchema::Int));
                let updated = if use_float {
                    bump_float(Expr::Var(previous_slot))
                } else {
                    bump_int(Expr::Var(previous_slot))
                };
                let result = if use_float {
                    bump_float(Expr::Var(previous_slot))
                } else {
                    bump_int(Expr::Var(previous_slot))
                };
                Ok(Expr::Block {
                    stmts: vec![
                        Stmt::Let {
                            index: previous_slot,
                            declared_schema: target_schema,
                            expr: read,
                            line,
                        },
                        Stmt::Assign {
                            kind: AssignmentKind::Set,
                            index: slot,
                            expr: updated,
                            line,
                        },
                    ],
                    expr: Box::new(if update.prefix {
                        result
                    } else {
                        Expr::Var(previous_slot)
                    }),
                })
            }
            ast::Expr::Member(member)
                if self
                    .expression_schema(&member.obj)
                    .is_some_and(super::array_lowering::is_carrier_schema) =>
            {
                let ast::MemberProp::Computed(property) = member.prop else {
                    return Err(unsupported(
                        member.span,
                        self.base,
                        self.source_id,
                        "non-indexed update target",
                    ));
                };
                let mut stmts = Vec::new();
                let carrier_slot = self.hoist_hidden(&member.obj, &mut stmts, line, span)?;
                let position_slot = self.hoist_hidden(&property.expr, &mut stmts, line, span)?;
                let previous = self.syn_call(
                    super::array_lowering::HELPER_READ,
                    vec![Expr::Var(carrier_slot), Expr::Var(position_slot)],
                    span,
                )?;
                let previous_slot = self.alloc_hidden_local(span)?;
                stmts.push(Stmt::Let {
                    index: previous_slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: previous,
                    line,
                });
                // Indexed carrier elements are always Number: `++` uses the
                // floating projection in reference-array mode, `--` keeps Sub.
                let updated = if self.ref_mode && !decrement {
                    bump_float(Expr::Var(previous_slot))
                } else {
                    bump_int(Expr::Var(previous_slot))
                };
                let written = self.syn_call(
                    super::array_lowering::HELPER_WRITE,
                    vec![Expr::Var(carrier_slot), Expr::Var(position_slot), updated],
                    span,
                )?;
                stmts.push(Stmt::Expr {
                    expr: written,
                    line,
                });
                Ok(Expr::Block {
                    stmts,
                    expr: Box::new(if update.prefix {
                        if self.ref_mode && !decrement {
                            bump_float(Expr::Var(previous_slot))
                        } else {
                            bump_int(Expr::Var(previous_slot))
                        }
                    } else {
                        Expr::Var(previous_slot)
                    }),
                })
            }
            _ => Err(unsupported(
                span,
                self.base,
                self.source_id,
                "non-local update target",
            )),
        }
    }

    /// Runs `body` with expression-statement values bound to hidden
    /// locals (see `stmt_value_depth`).
    pub(crate) fn with_consumed_stmt_values<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        self.stmt_value_depth += 1;
        let result = body(self);
        self.stmt_value_depth -= 1;
        result
    }

    /// Pushes an expression statement: at top level it evaluates to the
    /// stack, while nested bodies bind the value to a hidden local so the
    /// native backend never sees a leftover stack value at a merge point.
    fn push_expr_stmt(
        &mut self,
        expr: Expr,
        declared: Option<TypeSchema>,
        span: swc_common::Span,
        line: u32,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if self.stmt_value_depth == 0 {
            output.push(Stmt::Expr { expr, line });
        } else {
            let slot = self.alloc_hidden_local(span)?;
            output.push(Stmt::Let {
                index: slot,
                declared_schema: declared,
                expr,
                line,
            });
        }
        Ok(())
    }

    pub(crate) fn alloc_hidden_local(&mut self, span: swc_common::Span) -> Result<u16, ParseError> {
        let slot = self.next_slot;
        self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
            unsupported(span, self.base, self.source_id, "function local slot limit")
        })?;
        self.own_closure_local(slot);
        Ok(slot)
    }

    fn lower_math_round(
        &mut self,
        argument: Expr,
        span: swc_common::Span,
    ) -> Result<Expr, ParseError> {
        let source_span = byte_span(span, self.base, self.source_id);
        let line = line_at(self.source, source_span.lo);
        let value_slot = self.alloc_hidden_local(span)?;
        let floor_slot = self.alloc_hidden_local(span)?;
        let rounded_slot = self.alloc_hidden_local(span)?;
        let value = || Expr::Var(value_slot);
        let floor = || Expr::Var(floor_slot);
        let rounded = || Expr::Var(rounded_slot);
        let floor_call = Self::builtin_call(vm::BuiltinFunction::MathFloor, vec![value()]);
        let is_negative_tie = Expr::Lt(
            Box::new(value()),
            Box::new(Expr::Add(Box::new(floor()), Box::new(Expr::Float(0.5)))),
        );
        let rounded_value = Expr::IfElse {
            condition: Box::new(Expr::Eq(Box::new(value()), Box::new(floor()))),
            then_expr: Box::new(value()),
            else_expr: Box::new(Expr::IfElse {
                condition: Box::new(is_negative_tie),
                then_expr: Box::new(floor()),
                else_expr: Box::new(Expr::Add(Box::new(floor()), Box::new(Expr::Float(1.0)))),
            }),
        };
        let negative_zero = Expr::IfElse {
            condition: Box::new(Expr::And(
                Box::new(Expr::Lt(Box::new(value()), Box::new(Expr::Float(0.0)))),
                Box::new(Expr::Eq(Box::new(rounded()), Box::new(Expr::Float(0.0)))),
            )),
            then_expr: Box::new(Expr::Float(-0.0)),
            else_expr: Box::new(rounded()),
        };
        Ok(Expr::Block {
            stmts: vec![
                Stmt::Let {
                    index: value_slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: Expr::Div(Box::new(argument), Box::new(Expr::Float(1.0))),
                    line,
                },
                Stmt::Let {
                    index: floor_slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: floor_call,
                    line,
                },
                Stmt::Let {
                    index: rounded_slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: rounded_value,
                    line,
                },
            ],
            expr: Box::new(negative_zero),
        })
    }

    fn lower_numeric_strict_equality(left: Expr, right: Expr) -> Expr {
        let as_number = |value| Expr::Div(Box::new(value), Box::new(Expr::Float(1.0)));
        Expr::Eq(Box::new(as_number(left)), Box::new(as_number(right)))
    }

    /// Projects a statically proven numeric `+` onto the existing checked
    /// floating path in explicit reference-array mode. Both operands are
    /// already lowered exactly once (left then right); wrapping each in the
    /// existing `Div(_, Float(1.0))` coercion feeds the existing `Float`
    /// `Add` backend without new IR/opcodes. Pure `Int + Int` keeps the
    /// exact integer path; string addition never reaches here.
    fn lower_numeric_add(left: Expr, right: Expr) -> Expr {
        let as_float = |value: Expr| Expr::Div(Box::new(value), Box::new(Expr::Float(1.0)));
        Expr::Add(Box::new(as_float(left)), Box::new(as_float(right)))
    }

    /// Projects a statically proven numeric call argument onto the existing
    /// checked floating domain at a guest-call boundary where the declared
    /// formal is `number`. The argument is already lowered exactly once;
    /// wrapping it in the existing `Div(_, Float(1.0))` coercion feeds the
    /// existing `Float` inference without new IR/opcodes. Strings, bools,
    /// carriers, resources and host parameters never reach here.
    fn project_numeric_call_arg(expr: Expr) -> Expr {
        Expr::Div(Box::new(expr), Box::new(Expr::Float(1.0)))
    }

    fn lower_member_get(&mut self, member: ast::MemberExpr) -> Result<Expr, ParseError> {
        let member_span = member.span;
        let object = member.obj;
        let property = match member.prop {
            ast::MemberProp::Ident(property) => property,
            ast::MemberProp::Computed(property) => {
                // Reference-array carriers lower indexed reads through the
                // owning dispatcher; the plain Core-array path below never
                // sees a carrier value.
                if self.ref_carrier_of(&object) {
                    let container = self.lower_expr(*object)?;
                    let index = self.lower_expr(*property.expr)?;
                    return self.syn_call(
                        super::array_lowering::HELPER_READ,
                        vec![container, index],
                        member_span,
                    );
                }
                let array_schema = self.expression_schema(&object);
                if !array_schema.as_ref().is_some_and(|schema| {
                    matches!(
                        schema,
                        TypeSchema::Array(_)
                            | TypeSchema::ArrayTuple(_)
                            | TypeSchema::ArrayTupleRest { .. }
                    )
                }) {
                    return Err(unsupported(
                        member_span,
                        self.base,
                        self.source_id,
                        "computed property access",
                    ));
                }
                let container = self.lower_expr(*object)?;
                let index = self.lower_expr(*property.expr)?;
                let id = self.alloc_node_id();
                self.call_sites.push(ParsedCallSite {
                    id,
                    callee_span: byte_span(property.span, self.base, self.source_id),
                    expr_span: byte_span(member_span, self.base, self.source_id),
                    target: ParsedCallTarget::Function(vm::BuiltinFunction::Get.call_index()),
                    name: "[]".to_owned(),
                    scope_id: self.scope_id,
                    is_namespace_call: false,
                });
                return Ok(Expr::Call(
                    vm::BuiltinFunction::Get.call_index(),
                    Vec::new(),
                    vec![container, index],
                    None,
                    Some(id),
                ));
            }
            _ => {
                return Err(unsupported(
                    member_span,
                    self.base,
                    self.source_id,
                    "computed property access",
                ));
            }
        };
        let property_name = property.sym.to_string();
        let receiver_schema = self.expression_schema(&object);
        // Generator nominals expose exactly their protocol fields. Any other
        // field access fails precisely instead of projecting a conflicting
        // record protocol; ordinary receivers are untouched.
        if let Some(TypeSchema::Named(nominal, args)) = receiver_schema.clone()
            && args.is_empty()
        {
            if self.gen_iterators.contains_key(&nominal) {
                if property_name != "next" {
                    return Err(ParseError::at_span(
                        byte_span(member_span, self.base, self.source_id),
                        format!(
                            "generator iterator has no field '{property_name}'; only next is supported"
                        ),
                    ));
                }
            } else if self.gen_results.contains_key(&nominal)
                && property_name != "value"
                && property_name != "done"
            {
                return Err(ParseError::at_span(
                    byte_span(member_span, self.base, self.source_id),
                    format!(
                        "generator result has no field '{property_name}'; only value and done are supported"
                    ),
                ));
            }
        }
        if property_name == "length" && self.ref_carrier_of(&object) {
            let container = self.lower_expr(*object)?;
            return self.syn_call(
                super::array_lowering::HELPER_LENGTH,
                vec![container],
                member_span,
            );
        }
        if property_name == "length"
            && receiver_schema.as_ref().is_some_and(|schema| {
                matches!(
                    schema,
                    TypeSchema::Array(_)
                        | TypeSchema::ArrayTuple(_)
                        | TypeSchema::ArrayTupleRest { .. }
                )
            })
        {
            let container = self.lower_expr(*object)?;
            let id = self.alloc_node_id();
            self.call_sites.push(ParsedCallSite {
                id,
                callee_span: byte_span(property.span, self.base, self.source_id),
                expr_span: byte_span(member_span, self.base, self.source_id),
                target: ParsedCallTarget::Function(vm::BuiltinFunction::Len.call_index()),
                name: property_name,
                scope_id: self.scope_id,
                is_namespace_call: false,
            });
            return Ok(Expr::Call(
                vm::BuiltinFunction::Len.call_index(),
                Vec::new(),
                vec![container],
                None,
                Some(id),
            ));
        }
        let optional_field = receiver_schema
            .as_ref()
            .and_then(|schema| self.schema_field(schema, &property_name))
            .is_some_and(|schema| matches!(schema, TypeSchema::Optional(_)));
        let direct_nonoptional_local_root = matches!(&*object, ast::Expr::Ident(_))
            && receiver_schema
                .as_ref()
                .is_some_and(|schema| !matches!(schema, TypeSchema::Optional(_)));
        let container = self.lower_expr(*object)?;
        let id = self.alloc_node_id();
        if optional_field {
            let field_projection =
                direct_nonoptional_local_root && matches!(&container, Expr::Var(_));
            let container = if field_projection {
                container
            } else {
                Expr::Borrow(Box::new(container))
            };
            let container_slot = self.alloc_hidden_local(member_span)?;
            let key_slot = self.alloc_hidden_local(member_span)?;
            self.call_sites.push(ParsedCallSite {
                id,
                callee_span: byte_span(property.span, self.base, self.source_id),
                expr_span: byte_span(member_span, self.base, self.source_id),
                target: ParsedCallTarget::Unresolved,
                name: property_name,
                scope_id: self.scope_id,
                is_namespace_call: false,
            });
            return Ok(Expr::OptionalGet {
                field_projection,
                container: Box::new(container),
                key: Box::new(Expr::String(property.sym.to_string())),
                container_slot,
                key_slot,
                semantic_id: Some(id),
            });
        }
        self.call_sites.push(ParsedCallSite {
            id,
            callee_span: byte_span(property.span, self.base, self.source_id),
            expr_span: byte_span(member_span, self.base, self.source_id),
            target: ParsedCallTarget::Function(vm::BuiltinFunction::Get.call_index()),
            name: property_name,
            scope_id: self.scope_id,
            is_namespace_call: false,
        });
        Ok(Expr::Call(
            vm::BuiltinFunction::Get.call_index(),
            Vec::new(),
            vec![container, Expr::String(property.sym.to_string())],
            None,
            Some(id),
        ))
    }

    pub(crate) fn builtin_call(builtin: vm::BuiltinFunction, args: Vec<Expr>) -> Expr {
        Expr::Call(builtin.call_index(), Vec::new(), args, None, None)
    }

    fn lower_array_literal(&mut self, array: ast::ArrayLit) -> Result<Expr, ParseError> {
        let mut result = Self::builtin_call(vm::BuiltinFunction::ArrayNew, Vec::new());
        for element in array.elems {
            let Some(element) = element else {
                return Err(unsupported(
                    array.span,
                    self.base,
                    self.source_id,
                    "sparse array literal",
                ));
            };
            if element.spread.is_some() {
                return Err(unsupported(
                    element.span(),
                    self.base,
                    self.source_id,
                    "array spread element",
                ));
            }
            let value = self.lower_expr(*element.expr)?;
            result = Self::builtin_call(vm::BuiltinFunction::ArrayPush, vec![result, value]);
        }
        Ok(result)
    }

    fn lower_object_literal(&mut self, object: ast::ObjectLit) -> Result<Expr, ParseError> {
        let mut result = Self::builtin_call(vm::BuiltinFunction::MapNew, Vec::new());
        let mut seen = HashSet::<String>::new();
        for property in object.props {
            let (name, value, span) = match property {
                ast::PropOrSpread::Spread(spread) => {
                    return Err(unsupported(
                        spread.span(),
                        self.base,
                        self.source_id,
                        "object spread property",
                    ));
                }
                ast::PropOrSpread::Prop(property) => match *property {
                    ast::Prop::KeyValue(property) => {
                        let property_span = property.span();
                        let name = match property.key {
                            ast::PropName::Ident(name) => name.sym.to_string(),
                            ast::PropName::Str(name) => name.value.to_string_lossy().into_owned(),
                            other => {
                                return Err(unsupported(
                                    other.span(),
                                    self.base,
                                    self.source_id,
                                    "computed or non-string object key",
                                ));
                            }
                        };
                        (name, *property.value, property_span)
                    }
                    ast::Prop::Shorthand(ident) => {
                        let name = ident.sym.to_string();
                        (name, ast::Expr::Ident(ident.clone()), ident.span)
                    }
                    other => {
                        return Err(unsupported(
                            other.span(),
                            self.base,
                            self.source_id,
                            "method or accessor object property",
                        ));
                    }
                },
            };
            if !seen.insert(name.clone()) {
                return Err(ParseError::at_span(
                    byte_span(span, self.base, self.source_id),
                    format!("duplicate TypeScript object property '{name}'"),
                ));
            }
            let value = self.lower_expr(value)?;
            result = Self::builtin_call(
                vm::BuiltinFunction::Set,
                vec![result, Expr::String(name), value],
            );
        }
        Ok(result)
    }

    pub(crate) fn lower_expr(&mut self, expr: ast::Expr) -> Result<Expr, ParseError> {
        match expr {
            ast::Expr::Lit(ast::Lit::Str(value)) => {
                Ok(Expr::String(value.value.to_string_lossy().into_owned()))
            }
            ast::Expr::Lit(ast::Lit::Num(value)) if value.value.is_finite() => {
                Ok(numeric_literal(value.value))
            }
            ast::Expr::Lit(ast::Lit::Bool(value)) => Ok(Expr::Bool(value.value)),
            ast::Expr::Lit(ast::Lit::Null(_)) => Ok(Expr::Null),
            ast::Expr::Array(array) => self.lower_array_literal(array),
            ast::Expr::Object(object) => self.lower_object_literal(object),
            ast::Expr::Ident(ident) => {
                let name = ident.sym.to_string();
                let Some(source_slot) = self.locals.get(&name).copied() else {
                    if let Some((symbol, _)) = self.imported_functions.get(&name) {
                        return Ok(Expr::ModuleFunctionRef(*symbol, Vec::new()));
                    }
                    if let Some((index, _)) = self.function_indices.get(&name) {
                        return Ok(Expr::FunctionRef(*index, Vec::new()));
                    }
                    if let Some(by_arity) = self.host_function_indices.get(&name) {
                        let mut indexes = by_arity.values().copied().collect::<Vec<_>>();
                        indexes.sort_unstable();
                        indexes.dedup();
                        if let [index] = indexes.as_slice() {
                            return Ok(Expr::FunctionRef(*index, Vec::new()));
                        }
                        return Err(ParseError::at_span(
                            byte_span(ident.span, self.base, self.source_id),
                            format!("overloaded host function '{name}' cannot be used as a value"),
                        ));
                    }
                    return Err(ParseError::at_span(
                        byte_span(ident.span, self.base, self.source_id),
                        format!("unresolved TypeScript value '{name}'"),
                    ));
                };
                let slot = self.capture_slot(source_slot, &name, ident.span)?;
                let is_shared_mutable_capture = self.mutable_locals.contains(&slot)
                    && self.closure_captures.iter().any(|context| {
                        context
                            .captures
                            .values()
                            .any(|captured_slot| *captured_slot == slot)
                    });
                let span = byte_span(ident.span, self.base, self.source_id);
                let node_id = self.alloc_node_id();
                self.local_refs.push(LocalRefSite {
                    id: node_id,
                    ident_span: span,
                    slot,
                    name,
                    scope_id: self.scope_id,
                });
                let expression = Expr::Var(slot);
                Ok(if is_shared_mutable_capture {
                    Expr::Borrow(Box::new(expression))
                } else {
                    expression
                })
            }
            ast::Expr::Call(call) => {
                let explicit_type_args = call
                    .type_args
                    .as_ref()
                    .map(|instantiation| {
                        instantiation
                            .params
                            .iter()
                            .map(|argument| self.lower_type(argument))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                let call_span = byte_span(call.span, self.base, self.source_id);
                let (callee, callee_span, is_namespace_call) = match call.callee {
                    ast::Callee::Expr(callee) => match *callee {
                        ast::Expr::Ident(ident) => (ident.sym.to_string(), ident.span, false),
                        ast::Expr::Member(member) => {
                            let member_span = member.span;
                            // Reference-array generator protocol: `it.next()`
                            // on a locally known iterator lowers through the
                            // captured `next` closure with once-only receiver
                            // evaluation. Any other method shape on an
                            // iterator or result record fails precisely;
                            // ordinary receivers keep the historical path.
                            if let ast::MemberProp::Ident(property) = &member.prop
                                && property.sym.as_ref() == "next"
                                && self.generator_iterator_of(&member.obj).is_some()
                            {
                                let receiver = member.obj;
                                let arguments = call.args;
                                let target_span = call.span;
                                return self.lower_generator_next_call(
                                    receiver,
                                    member_span,
                                    target_span,
                                    arguments,
                                );
                            }
                            if self.generator_iterator_of(&member.obj).is_some() {
                                return Err(ParseError::at_span(
                                    byte_span(member_span, self.base, self.source_id),
                                    "generator iterator methods other than next() are not supported".to_owned(),
                                ));
                            }
                            if self.generator_result_of(&member.obj).is_some() {
                                return Err(ParseError::at_span(
                                    byte_span(member_span, self.base, self.source_id),
                                    "generator result methods are not supported".to_owned(),
                                ));
                            }
                            let ast::Expr::Ident(object) = *member.obj else {
                                return Err(unsupported(
                                    member.span,
                                    self.base,
                                    self.source_id,
                                    "non-namespace call target",
                                ));
                            };
                            let namespace_name = object.sym.to_string();
                            if self.locals.contains_key(&namespace_name) {
                                return Err(ParseError::at_span(
                                    byte_span(member.span, self.base, self.source_id),
                                    format!(
                                        "TypeScript namespace '{namespace_name}' is shadowed by a local value"
                                    ),
                                ));
                            }
                            let ast::MemberProp::Ident(property) = member.prop else {
                                return Err(unsupported(
                                    member.span,
                                    self.base,
                                    self.source_id,
                                    "computed namespace call target",
                                ));
                            };
                            (
                                format!("{}::{}", object.sym, property.sym),
                                member.span,
                                true,
                            )
                        }
                        other => {
                            return Err(unsupported(
                                other.span(),
                                self.base,
                                self.source_id,
                                "call target",
                            ));
                        }
                    },
                    other => {
                        return Err(unsupported(
                            other.span(),
                            self.base,
                            self.source_id,
                            "call target",
                        ));
                    }
                };
                let source_local_slot = if is_namespace_call {
                    None
                } else {
                    self.locals.get(&callee).copied()
                };
                let source_callee = callee.clone();
                let alias_origin = source_local_slot
                    .and_then(|slot| self.const_callable_origins.get(&slot))
                    .filter(|origin| {
                        self.ref_mode
                            && !self.in_ref_prelude
                            && call.args.len() < usize::from(origin.arity())
                            && self.callable_origin_is_current(origin)
                    })
                    .cloned();
                let alias_slot = if alias_origin.is_some() {
                    Some(self.capture_slot(
                        source_local_slot.expect("alias origin has a source slot"),
                        &source_callee,
                        callee_span,
                    )?)
                } else {
                    None
                };
                let local_slot = if alias_origin.is_some() {
                    None
                } else {
                    source_local_slot
                };
                let callee = alias_origin
                    .as_ref()
                    .map_or(callee, |origin| origin.name().to_owned());
                // Reference-array mode resolves `Math.random()` through the
                // prelude randomness helper. Plain mode keeps the historical
                // rejection: no new global is introduced there.
                let callee = if self.ref_mode && callee == "Math::random" {
                    super::array_lowering::HELPER_RAND.to_owned()
                } else {
                    callee
                };
                let local_function = match alias_origin.as_ref() {
                    Some(ConstCallableOrigin::Local { index, arity, .. }) => Some((*index, *arity)),
                    Some(ConstCallableOrigin::Imported { .. }) => None,
                    None => local_slot
                        .is_none()
                        .then(|| self.function_indices.get(&callee).copied())
                        .flatten(),
                };
                let imported_function = match alias_origin.as_ref() {
                    Some(ConstCallableOrigin::Local { .. }) => None,
                    _ => local_slot
                        .is_none()
                        .then(|| self.imported_functions.get(&callee).cloned())
                        .flatten(),
                };
                let call_arity = u8::try_from(call.args.len()).ok();
                if call_arity.is_none() {
                    return Err(ParseError::at_span(
                        call_span,
                        "TypeScript function call exceeds the VM argument limit",
                    ));
                }
                let host_index = if local_slot.is_none() {
                    call_arity.and_then(|arity| {
                        self.host_function_indices
                            .get(&callee)
                            .and_then(|functions| functions.get(&arity))
                            .copied()
                    })
                } else {
                    None
                };
                let standard_builtin = if local_slot.is_none()
                    && local_function.is_none()
                    && imported_function.is_none()
                    && host_index.is_none()
                {
                    match callee.as_str() {
                        "Math::floor" => Some(vm::BuiltinFunction::MathFloor),
                        "Math::sqrt" => Some(vm::BuiltinFunction::MathSqrt),
                        "isNaN" => Some(vm::BuiltinFunction::MathIsNaN),
                        _ => None,
                    }
                } else {
                    None
                };
                let standard_round = local_slot.is_none()
                    && local_function.is_none()
                    && imported_function.is_none()
                    && host_index.is_none()
                    && callee == "Math::round";
                let standard_number_is_nan = local_slot.is_none()
                    && local_function.is_none()
                    && imported_function.is_none()
                    && host_index.is_none()
                    && callee == "Number::isNaN";
                let standard_global_is_nan =
                    standard_builtin == Some(vm::BuiltinFunction::MathIsNaN);
                let standard_integer = local_slot.is_none()
                    && local_function.is_none()
                    && imported_function.is_none()
                    && host_index.is_none()
                    && callee == "Number::isInteger";
                let standard_arity = (standard_builtin.is_some()
                    || standard_round
                    || standard_number_is_nan
                    || standard_integer)
                    .then_some(1);
                let standard_argument_span = call.args.first().map(|argument| argument.span());
                let host_passings = call_arity
                    .filter(|_| host_index.is_some())
                    .and_then(|arity| {
                        self.host_parameter_passings
                            .get(&callee)
                            .and_then(|by_arity| by_arity.get(&arity))
                    })
                    .cloned()
                    .unwrap_or_default();
                // Reference-array call boundary: a plain staging array
                // argument converts through the owning factory wherever the
                // callee parameter is physically the carrier. Carrier
                // arguments pass through untouched.
                let ref_carrier_params: Vec<bool> = if self.ref_mode {
                    if let Some((index, _)) = local_function {
                        // Lowered declaration once present, else the
                        // predeclared annotation facts (recursive and forward
                        // calls convert against the same formals as later
                        // calls).
                        self.module_function_arg_schemas(&callee, index)
                    } else if let Some((_, declaration)) = imported_function.as_ref() {
                        declaration.arg_schemas.clone()
                    } else if local_slot.is_some() {
                        match self.local_types.get(&callee).cloned() {
                            Some(TypeSchema::Callable { params, .. }) => {
                                params.into_iter().map(Some).collect()
                            }
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    }
                    .into_iter()
                    .map(|schema| schema.is_some_and(super::array_lowering::is_carrier_schema))
                    .collect()
                } else {
                    Vec::new()
                };
                // Reference-array numeric call boundary: where the declared
                // formal is exactly `number` and the actual is statically
                // proven numeric, project the already-once-lowered argument
                // through the existing floating coercion so recursive Int and
                // Float flows share one Float inference. Strings, bools,
                // carriers, resources, host and prelude helpers never qualify.
                // Ordinary mode is untouched.
                let ref_number_params: Vec<bool> = if self.ref_mode && !self.in_ref_prelude {
                    if let Some((index, _)) = local_function {
                        // Lowered declaration once present, else the
                        // predeclared annotation facts, so recursive Int and
                        // Float flows share one Float inference with later
                        // calls.
                        self.module_function_arg_schemas(&callee, index)
                    } else if let Some((_, declaration)) = imported_function.as_ref() {
                        declaration.arg_schemas.clone()
                    } else if let Some(slot) = local_slot {
                        // A bare local Callable(Number) schema alone proves no
                        // guest provenance: the slot can hold a host function
                        // value with a compatible typed annotation. Only a
                        // current immutable guest origin keeps the numeric
                        // computational-domain projection; otherwise the
                        // physical Number ABI passes through untouched so
                        // typed host aliases observe Int vs Float exactly
                        // like direct host calls.
                        match self.const_callable_origins.get(&slot).cloned() {
                            Some(origin) if self.callable_origin_is_current(&origin) => {
                                match origin {
                                    ConstCallableOrigin::Local { index, name, .. } => {
                                        self.module_function_arg_schemas(&name, index)
                                    }
                                    ConstCallableOrigin::Imported { name, .. } => self
                                        .imported_functions
                                        .get(&name)
                                        .map(|(_, declaration)| declaration.arg_schemas.clone())
                                        .unwrap_or_default(),
                                }
                            }
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    }
                    .into_iter()
                    .map(|schema| matches!(schema, Some(TypeSchema::Number)))
                    .collect()
                } else {
                    Vec::new()
                };
                let arity = local_function
                    .map(|(_, arity)| arity)
                    .or_else(|| {
                        imported_function
                            .as_ref()
                            .map(|(_, declaration)| declaration.arity)
                    })
                    .or_else(|| host_index.map(|_| call_arity.unwrap_or_default()))
                    .or(standard_arity);
                if local_slot.is_none() && arity.is_none() {
                    return Err(ParseError::at_span(
                        byte_span(callee_span, self.base, self.source_id),
                        format!("unresolved TypeScript function '{callee}'"),
                    ));
                }
                let defaulted_call = match (local_function, arity) {
                    (Some((_, function_arity)), Some(expected))
                        if function_arity == expected
                            && call.args.len() < usize::from(expected) =>
                    {
                        self.function_parameters
                            .get(&callee)
                            .is_some_and(|parameters| {
                                parameters.len() == usize::from(expected)
                                    && parameters
                                        .iter()
                                        .position(|parameter| {
                                            parameter_binding(parameter)
                                                .is_some_and(|(_, default)| default.is_some())
                                        })
                                        .is_some_and(|first_default| {
                                            call.args.len() >= first_default
                                                && parameters[first_default..].iter().all(
                                                    |parameter| {
                                                        parameter_binding(parameter).is_some_and(
                                                            |(_, default)| default.is_some(),
                                                        )
                                                    },
                                                )
                                        })
                            })
                    }
                    _ => false,
                };
                // Imported omitted defaults via callee-owned carrier (ref_mode only).
                // Uses actual import + type identities (getter + carrier field for
                // the supplied arity); no caller-local names, no filename reading,
                // no literal special-casing. Plain mode keeps the historical arity
                // error. If the getter/carrier/field is absent (function without
                // defaults or invalid arity omitting required), fall through to
                // the precise arity error below.
                let imported_defaulted_via_getter = if self.ref_mode
                    && !self.in_ref_prelude
                    && local_function.is_none()
                    && local_slot.is_none()
                    && host_index.is_none()
                    && standard_arity.is_none()
                    && !standard_round
                    && !standard_number_is_nan
                    && !standard_global_is_nan
                    && !standard_integer
                {
                    if let (Some((_, _decl)), Some(expected)) = (imported_function.as_ref(), arity)
                    {
                        let supplied = call.args.len();
                        if supplied < usize::from(expected) {
                            let supplied_u8 = u8::try_from(supplied).ok();
                            if let Some(supplied_u8) = supplied_u8 {
                                let getter_local = super::default_lowering::getter_name(&callee);
                                let carrier_local = super::default_lowering::carrier_name(&callee);
                                let getter_ok = self
                                    .imported_functions
                                    .get(&getter_local)
                                    .is_some_and(|(_, gdecl)| gdecl.arity == 0);
                                let field = super::default_lowering::arity_field(supplied_u8);
                                let field_ok = self
                                    .named_type_declarations
                                    .get(&carrier_local)
                                    .is_some_and(|sdecl| match &sdecl.body_schema {
                                        TypeSchema::Object(fields) => fields.contains_key(&field),
                                        _ => false,
                                    });
                                // Also require full decl has carrier? No, Number
                                // formals already covered by F1; here we only
                                // need getter+field existence (which exporter
                                // generates exactly for valid omitted arities).
                                getter_ok && field_ok
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                if let Some(arity) = arity
                    && call.args.len() != usize::from(arity)
                    && !defaulted_call
                    && !imported_defaulted_via_getter
                {
                    return Err(ParseError::at_span(
                        call_span,
                        format!(
                            "TypeScript function '{callee}' expects {arity} arguments but received {}",
                            call.args.len()
                        ),
                    ));
                }
                let standard_nan_argument_schema = if standard_number_is_nan
                    || standard_global_is_nan
                {
                    let argument = call.args.first().expect("standard predicate arity checked");
                    let argument_schema = self.expression_schema(&argument.expr);
                    let numeric = argument_schema
                        .as_ref()
                        .is_some_and(Self::is_numeric_schema);
                    if standard_global_is_nan && !numeric {
                        return Err(ParseError::at_span(
                            byte_span(argument.span(), self.base, self.source_id),
                            "global isNaN only accepts numeric inputs in this typed subset",
                        ));
                    }
                    if standard_number_is_nan
                        && !numeric
                        && !argument_schema.as_ref().is_some_and(|schema| {
                            matches!(
                                schema,
                                TypeSchema::String | TypeSchema::Bool | TypeSchema::Null
                            )
                        })
                    {
                        return Err(ParseError::at_span(
                            byte_span(argument.span(), self.base, self.source_id),
                            "Number.isNaN does not support non-scalar input in this typed subset",
                        ));
                    }
                    argument_schema
                } else {
                    None
                };
                if host_index.is_some() && !explicit_type_args.is_empty() {
                    return Err(unsupported(
                        call.span,
                        self.base,
                        self.source_id,
                        "generic host function type arguments",
                    ));
                }
                let guarded_alias = alias_origin.is_some();
                let mut alias_number_flags: Vec<bool> = Vec::new();
                let mut lowered_args: Vec<Expr> = Vec::new();
                for (index, argument) in call.args.into_iter().enumerate() {
                    let argument_span = argument.span();
                    if argument.spread.is_some() {
                        return Err(unsupported(
                            argument_span,
                            self.base,
                            self.source_id,
                            "spread call argument",
                        ));
                    }
                    let needs_factory = ref_carrier_params.get(index).copied().unwrap_or(false)
                        && (matches!(&*argument.expr, ast::Expr::Array(_))
                            || self
                                .expression_schema(&argument.expr)
                                .is_some_and(|schema| {
                                    super::array_lowering::is_logical_number_array(schema)
                                }));
                    // Numeric projection predicate is computed before
                    // lowering (statically justified from the guest AST);
                    // the wrap itself applies after lowering so the
                    // argument still evaluates exactly once, in order.
                    let needs_number_projection = !needs_factory
                        && ref_number_params.get(index).copied().unwrap_or(false)
                        && self
                            .expression_schema(&argument.expr)
                            .as_ref()
                            .is_some_and(Self::is_numeric_schema);
                    let expression = self.lower_expr(*argument.expr)?;
                    // Reference-array call boundary conversion: a plain
                    // staging array (including an empty literal) becomes
                    // an owned carrier wherever the callee parameter is
                    // physically the carrier. Carriers stay guest-physical
                    // and apply before staging (existing behavior); only
                    // the numeric domain projection is deferred for guarded
                    // aliases so a host fallback observes the original
                    // physical Number variant.
                    let expression = if needs_factory {
                        self.syn_call(
                            super::array_lowering::HELPER_MAKE,
                            vec![expression],
                            argument_span,
                        )?
                    } else {
                        expression
                    };
                    // Guarded aliases stage the once-evaluated (possibly
                    // carrier-converted) value; the guest numeric projection
                    // applies only to the taken guest branch below, so an
                    // actual-value fallback after a public slot replacement
                    // observes the original Number ABI.
                    let expression = if guarded_alias {
                        alias_number_flags.push(needs_number_projection);
                        expression
                    } else {
                        // Numeric computational-domain projection: a declared
                        // `number` formal with a statically numeric actual
                        // runs through the existing floating coercion so Int
                        // and Float call sites share one Float inference.
                        if needs_number_projection {
                            Self::project_numeric_call_arg(expression)
                        } else {
                            expression
                        }
                    };
                    let expression: Expr = match host_passings.get(index) {
                        Some(vm::HostParamPassing::Borrow) => match expression {
                            variable @ Expr::Var(_) => Expr::Borrow(Box::new(variable)),
                            Expr::Borrow(inner) if matches!(inner.as_ref(), Expr::Var(_)) => {
                                Expr::Borrow(inner)
                            }
                            _ => {
                                return Err(unsupported(
                                    argument_span,
                                    self.base,
                                    self.source_id,
                                    "borrowed host argument must be a local binding",
                                ));
                            }
                        },
                        Some(vm::HostParamPassing::BorrowMut) => match expression {
                            variable @ Expr::Var(_) => Expr::BorrowMut(Box::new(variable)),
                            Expr::Borrow(inner) if matches!(inner.as_ref(), Expr::Var(_)) => {
                                Expr::BorrowMut(inner)
                            }
                            _ => {
                                return Err(unsupported(
                                    argument_span,
                                    self.base,
                                    self.source_id,
                                    "mutable-borrow host argument must be a local binding",
                                ));
                            }
                        },
                        Some(vm::HostParamPassing::TakeOwned) => match expression {
                            Expr::Borrow(inner) if matches!(inner.as_ref(), Expr::Var(_)) => *inner,
                            other => other,
                        },
                        _ => expression,
                    };
                    lowered_args.push(expression);
                }
                let args = lowered_args;
                // Stage the actual callee first, then each provided argument,
                // before selecting a default adapter. Neither branch duplicates
                // guest evaluation or relies on a stale physical slot value.
                let mut alias_statements = Vec::new();
                let (args, alias_fallback, alias_condition) =
                    if let (Some(origin), Some(slot)) = (alias_origin.as_ref(), alias_slot) {
                        let line = line_at(self.source, call_span.lo);
                        let snapshot = self.alloc_hidden_local(call.span)?;
                        // The snapshot holds the actual runtime callable for the
                        // identity guard and fallback. It must stay
                        // schema-free so the fallback's original physical
                        // arguments are not statically forced through the
                        // guest numeric projection; the guest branch below
                        // carries its own converted types.
                        alias_statements.push(Stmt::Let {
                            index: snapshot,
                            declared_schema: None,
                            expr: Expr::Var(slot),
                            line,
                        });
                        let mut staged = Vec::with_capacity(args.len());
                        for argument in args {
                            let temporary = self.alloc_hidden_local(call.span)?;
                            alias_statements.push(Stmt::Let {
                                index: temporary,
                                declared_schema: None,
                                expr: argument,
                                line,
                            });
                            staged.push(Expr::Var(temporary));
                        }
                        // Guest numeric projection applies to the staged
                        // temporaries only, preserving single evaluation and
                        // left-to-right order while keeping the fallback's
                        // original physical Number arguments intact. Carriers
                        // were already converted before staging.
                        let mut then_args = Vec::with_capacity(staged.len());
                        for (position, staged_var) in staged.iter().enumerate() {
                            let mut converted = staged_var.clone();
                            if alias_number_flags.get(position).copied().unwrap_or(false) {
                                converted = Self::project_numeric_call_arg(converted);
                            }
                            then_args.push(converted);
                        }
                        let fallback_id = self.alloc_node_id();
                        self.call_sites.push(ParsedCallSite {
                            id: fallback_id,
                            callee_span: byte_span(callee_span, self.base, self.source_id),
                            expr_span: call_span,
                            target: ParsedCallTarget::Local(snapshot),
                            name: source_callee.clone(),
                            scope_id: self.scope_id,
                            is_namespace_call: false,
                        });
                        let fallback = Expr::LocalCall(
                            snapshot,
                            explicit_type_args.clone(),
                            staged.clone(),
                            Some(fallback_id),
                        );
                        let condition =
                            Expr::Eq(Box::new(Expr::Var(snapshot)), Box::new(origin.reference()));
                        (then_args, Some(fallback), Some(condition))
                    } else {
                        (args, None, None)
                    };
                let id = self.alloc_node_id();
                let (target, expression) = if let Some(slot) = local_slot {
                    (
                        ParsedCallTarget::Local(slot),
                        Expr::LocalCall(slot, explicit_type_args, args, Some(id)),
                    )
                } else if let Some((index, _)) = local_function {
                    let target = ParsedCallTarget::Function(index);
                    let expression = if defaulted_call {
                        self.lower_defaulted_function_call(
                            &callee,
                            index,
                            explicit_type_args,
                            args,
                            id,
                        )?
                    } else {
                        Expr::Call(index, explicit_type_args, args, None, Some(id))
                    };
                    (target, expression)
                } else if let Some((symbol, _)) = imported_function {
                    // Imported omitted defaults via callee-owned carrier (ref_mode only).
                    // `imported_defaulted_via_getter` was proven above via actual
                    // getter + carrier field identities for the supplied arity.
                    // Lower to: carrier = getter(); thunk = carrier.<arityK>;
                    // LocalCall(thunk, supplied...). Defaults run inside the
                    // callee thunk at call time, left-to-right, once-only, in
                    // callee scope; no body duplication, no global caches.
                    if imported_defaulted_via_getter {
                        let supplied_u8 = u8::try_from(args.len()).map_err(|_| {
                            ParseError::at_span(
                                call_span,
                                "TypeScript function call exceeds the VM argument limit",
                            )
                        })?;
                        let getter_local = super::default_lowering::getter_name(&callee);
                        let carrier_local = super::default_lowering::carrier_name(&callee);
                        let (getter_symbol, getter_decl) = self
                            .imported_functions
                            .get(&getter_local)
                            .cloned()
                            .ok_or_else(|| {
                                ParseError::new("default callable getter declaration is missing")
                            })?;
                        if getter_decl.arity != 0 {
                            return Err(ParseError::new(
                                "default callable getter must take zero arguments",
                            ));
                        }
                        let carrier_decl = self
                            .named_type_declarations
                            .get(&carrier_local)
                            .cloned()
                            .ok_or_else(|| {
                                ParseError::new("default callable carrier declaration is missing")
                            })?;
                        let field = super::default_lowering::arity_field(supplied_u8);
                        let thunk_callable = match &carrier_decl.body_schema {
                            TypeSchema::Object(fields) => fields.get(&field).cloned(),
                            _ => None,
                        }
                        .ok_or_else(|| {
                            ParseError::new(
                                "default callable carrier is missing the supplied arity",
                            )
                        })?;
                        // Carrier = getter().
                        let getter_id = self.alloc_node_id();
                        let getter_site_span = call_span;
                        self.call_sites.push(ParsedCallSite {
                            id: getter_id,
                            callee_span: byte_span(callee_span, self.base, self.source_id),
                            expr_span: getter_site_span,
                            target: ParsedCallTarget::Module(getter_symbol),
                            name: getter_local.clone(),
                            scope_id: self.scope_id,
                            is_namespace_call: false,
                        });
                        let getter_call = Expr::ModuleCall(
                            getter_symbol,
                            Vec::new(),
                            Vec::new(),
                            Some(getter_id),
                        );
                        let carrier_slot = self.alloc_hidden_local(call.span)?;
                        let carrier_schema = getter_decl
                            .return_schema
                            .clone()
                            .unwrap_or(TypeSchema::Named(carrier_local.clone(), Vec::new()));
                        let line = line_at(self.source, call_span.lo);
                        let mut block_stmts = vec![Stmt::Let {
                            index: carrier_slot,
                            declared_schema: Some(carrier_schema),
                            expr: getter_call,
                            line,
                        }];
                        // Thunk = carrier.<field> (truthful Callable field).
                        let get_id = self.alloc_node_id();
                        self.call_sites.push(ParsedCallSite {
                            id: get_id,
                            callee_span: byte_span(callee_span, self.base, self.source_id),
                            expr_span: call_span,
                            target: ParsedCallTarget::Function(
                                vm::BuiltinFunction::Get.call_index(),
                            ),
                            name: field.clone(),
                            scope_id: self.scope_id,
                            is_namespace_call: false,
                        });
                        let get_expr = Expr::Call(
                            vm::BuiltinFunction::Get.call_index(),
                            Vec::new(),
                            vec![Expr::Var(carrier_slot), Expr::String(field)],
                            None,
                            Some(get_id),
                        );
                        let thunk_slot = self.alloc_hidden_local(call.span)?;
                        block_stmts.push(Stmt::Let {
                            index: thunk_slot,
                            declared_schema: Some(thunk_callable),
                            expr: get_expr,
                            line,
                        });
                        // LocalCall(thunk, supplied...) with original ID to
                        // preserve provenance; supplied args already projected.
                        let target = ParsedCallTarget::Module(symbol);
                        let expression = Expr::Block {
                            stmts: block_stmts,
                            expr: Box::new(Expr::LocalCall(
                                thunk_slot,
                                explicit_type_args,
                                args,
                                Some(id),
                            )),
                        };
                        (target, expression)
                    } else {
                        (
                            ParsedCallTarget::Module(symbol),
                            Expr::ModuleCall(symbol, explicit_type_args, args, Some(id)),
                        )
                    }
                } else if standard_integer {
                    let argument = args
                        .into_iter()
                        .next()
                        .expect("Number.isInteger has one argument");
                    let span =
                        standard_argument_span.expect("Number.isInteger argument span was checked");
                    let source_span = byte_span(span, self.base, self.source_id);
                    let hidden_slot = self.alloc_hidden_local(span)?;
                    let hidden_value = Expr::Var(hidden_slot);
                    let finite = Expr::Call(
                        vm::BuiltinFunction::MathIsFinite.call_index(),
                        Vec::new(),
                        vec![hidden_value.clone()],
                        None,
                        Some(id),
                    );
                    let floored = Self::builtin_call(
                        vm::BuiltinFunction::MathFloor,
                        vec![hidden_value.clone()],
                    );
                    (
                        ParsedCallTarget::Function(vm::BuiltinFunction::MathIsFinite.call_index()),
                        Expr::Block {
                            stmts: vec![Stmt::Let {
                                index: hidden_slot,
                                declared_schema: Some(TypeSchema::Number),
                                expr: argument,
                                line: line_at(self.source, source_span.lo),
                            }],
                            expr: Box::new(Expr::And(
                                Box::new(finite),
                                Box::new(Expr::Eq(Box::new(hidden_value), Box::new(floored))),
                            )),
                        },
                    )
                } else if standard_number_is_nan {
                    let argument = args
                        .into_iter()
                        .next()
                        .expect("Number.isNaN has one argument");
                    let argument_schema = standard_nan_argument_schema
                        .expect("Number.isNaN argument type was checked");
                    let target =
                        ParsedCallTarget::Function(vm::BuiltinFunction::MathIsNaN.call_index());
                    let expression = if Self::is_numeric_schema(&argument_schema) {
                        Expr::Call(
                            vm::BuiltinFunction::MathIsNaN.call_index(),
                            Vec::new(),
                            vec![argument],
                            None,
                            Some(id),
                        )
                    } else {
                        let span =
                            standard_argument_span.expect("Number.isNaN argument span was checked");
                        let source_span = byte_span(span, self.base, self.source_id);
                        let hidden_slot = self.alloc_hidden_local(span)?;
                        Expr::Block {
                            stmts: vec![Stmt::Let {
                                index: hidden_slot,
                                declared_schema: Some(argument_schema),
                                expr: argument,
                                line: line_at(self.source, source_span.lo),
                            }],
                            expr: Box::new(Expr::Bool(false)),
                        }
                    };
                    (target, expression)
                } else if standard_round {
                    let argument = args
                        .into_iter()
                        .next()
                        .expect("Math.round has one argument");
                    let span = standard_argument_span.expect("Math.round argument span checked");
                    (
                        ParsedCallTarget::Function(vm::BuiltinFunction::MathRound.call_index()),
                        self.lower_math_round(argument, span)?,
                    )
                } else if let Some(builtin) = standard_builtin {
                    (
                        ParsedCallTarget::Function(builtin.call_index()),
                        Expr::Call(builtin.call_index(), Vec::new(), args, None, Some(id)),
                    )
                } else if let Some(index) = host_index {
                    (
                        ParsedCallTarget::Function(index),
                        Expr::Call(index, Vec::new(), args, None, Some(id)),
                    )
                } else {
                    unreachable!("resolved TypeScript call lost its binding")
                };
                self.call_sites.push(ParsedCallSite {
                    id,
                    callee_span: byte_span(callee_span, self.base, self.source_id),
                    expr_span: call_span,
                    target,
                    name: if alias_origin.is_some() {
                        source_callee
                    } else {
                        callee
                    },
                    scope_id: self.scope_id,
                    is_namespace_call,
                });
                match (alias_condition, alias_fallback) {
                    (Some(condition), Some(fallback)) => Ok(Expr::Block {
                        stmts: alias_statements,
                        expr: Box::new(Expr::IfElse {
                            condition: Box::new(condition),
                            then_expr: Box::new(expression),
                            else_expr: Box::new(fallback),
                        }),
                    }),
                    _ => Ok(expression),
                }
            }
            ast::Expr::Cond(conditional) => Ok(Expr::IfElse {
                condition: Box::new(self.lower_expr(*conditional.test)?),
                then_expr: Box::new(self.lower_expr(*conditional.cons)?),
                else_expr: Box::new(self.lower_expr(*conditional.alt)?),
            }),
            ast::Expr::Member(member) => self.lower_member_get(member),
            ast::Expr::Arrow(arrow) => self.lower_arrow(arrow),
            ast::Expr::Update(update) => self.lower_update_expr(update),
            ast::Expr::Paren(value) => self.lower_expr(*value.expr),
            ast::Expr::Unary(value) => {
                if value.op == ast::UnaryOp::Minus
                    && let Some(number) = parsed_numeric_literal(&value.arg)
                    && number.is_finite()
                {
                    return Ok(numeric_literal(-number));
                }
                let inner = Box::new(self.lower_expr(*value.arg)?);
                match value.op {
                    ast::UnaryOp::Minus => Ok(Expr::Neg(inner)),
                    ast::UnaryOp::Bang => Ok(Expr::Not(inner)),
                    _ => Err(unsupported(
                        value.span,
                        self.base,
                        self.source_id,
                        "unary operator",
                    )),
                }
            }
            ast::Expr::Bin(value) => {
                // Generator results compare by identity in JavaScript but
                // lower to structural records here: their equality operations
                // fail precisely instead of exposing conflicting semantics.
                // Iterator identity itself is exact and stays available.
                if matches!(
                    value.op,
                    ast::BinaryOp::EqEq
                        | ast::BinaryOp::NotEq
                        | ast::BinaryOp::EqEqEq
                        | ast::BinaryOp::NotEqEq
                ) && (self.generator_result_of(&value.left).is_some()
                    || self.generator_result_of(&value.right).is_some())
                {
                    return Err(ParseError::at_span(
                        byte_span(value.span, self.base, self.source_id),
                        "generator result identity comparison is not supported".to_owned(),
                    ));
                }
                let numeric_strict_equality =
                    matches!(value.op, ast::BinaryOp::EqEqEq | ast::BinaryOp::NotEqEq)
                        && self
                            .expression_schema(&value.left)
                            .is_some_and(|schema| Self::is_numeric_schema(&schema))
                        && self
                            .expression_schema(&value.right)
                            .is_some_and(|schema| Self::is_numeric_schema(&schema));
                // Presence-aware relational comparisons over reference-array
                // elements evaluate false on a missing integral index
                // instead of fabricating an element value.
                if matches!(value.op, ast::BinaryOp::Lt | ast::BinaryOp::Gt)
                    && let Some(guarded) = self.lower_guarded_comparison(&value)?
                {
                    return Ok(guarded);
                }
                // Explicit reference-array mode only: statically proven
                // numeric `<=`/`>=` without new IR (see helper).
                if matches!(value.op, ast::BinaryOp::LtEq | ast::BinaryOp::GtEq)
                    && let Some(lowered) = self.lower_non_strict_comparison(&value)?
                {
                    return Ok(lowered);
                }
                let left_schema_for_add = self.expression_schema(&value.left);
                let right_schema_for_add = self.expression_schema(&value.right);
                let left = Box::new(self.lower_expr(*value.left)?);
                let right = Box::new(self.lower_expr(*value.right)?);
                if numeric_strict_equality {
                    let equality = Self::lower_numeric_strict_equality(*left, *right);
                    return Ok(if value.op == ast::BinaryOp::NotEqEq {
                        Expr::Not(Box::new(equality))
                    } else {
                        equality
                    });
                }
                // Explicit reference-array mode: statically proven numeric
                // `+` (other than pure Int+Int and string `+`) runs through
                // the existing checked floating coercion so the existing
                // Float Add backend covers it. Ordinary mode is untouched.
                if value.op == ast::BinaryOp::Add
                    && self.ref_mode
                    && left_schema_for_add
                        .as_ref()
                        .is_some_and(Self::is_numeric_schema)
                    && right_schema_for_add
                        .as_ref()
                        .is_some_and(Self::is_numeric_schema)
                    && !matches!(left_schema_for_add, Some(TypeSchema::String))
                    && !matches!(right_schema_for_add, Some(TypeSchema::String))
                    && !(matches!(left_schema_for_add, Some(TypeSchema::Int))
                        && matches!(right_schema_for_add, Some(TypeSchema::Int)))
                {
                    return Ok(Self::lower_numeric_add(*left, *right));
                }
                match value.op {
                    ast::BinaryOp::Add => Ok(Expr::Add(left, right)),
                    ast::BinaryOp::Sub => Ok(Expr::Sub(left, right)),
                    ast::BinaryOp::Mul => Ok(Expr::Mul(left, right)),
                    ast::BinaryOp::Div => Ok(Expr::Div(left, right)),
                    ast::BinaryOp::Mod => Ok(Expr::Mod(left, right)),
                    // Explicit reference-array mode only: numeric `**`
                    // lowers to the existing Core floating power builtin.
                    // Ordinary mode keeps the historical rejection.
                    ast::BinaryOp::Exp
                        if self.ref_mode
                            && left_schema_for_add
                                .as_ref()
                                .is_some_and(Self::is_numeric_schema)
                            && right_schema_for_add
                                .as_ref()
                                .is_some_and(Self::is_numeric_schema) =>
                    {
                        Ok(Expr::Call(
                            vm::BuiltinFunction::MathPowF.call_index(),
                            Vec::new(),
                            vec![*left, *right],
                            None,
                            None,
                        ))
                    }
                    ast::BinaryOp::EqEq | ast::BinaryOp::EqEqEq => Ok(Expr::Eq(left, right)),
                    ast::BinaryOp::NotEq | ast::BinaryOp::NotEqEq => {
                        Ok(Expr::Not(Box::new(Expr::Eq(left, right))))
                    }
                    ast::BinaryOp::Lt => Ok(Expr::Lt(left, right)),
                    ast::BinaryOp::Gt => Ok(Expr::Gt(left, right)),
                    ast::BinaryOp::LogicalAnd => Ok(Expr::And(left, right)),
                    ast::BinaryOp::LogicalOr => Ok(Expr::Or(left, right)),
                    _ => Err(unsupported(
                        value.span,
                        self.base,
                        self.source_id,
                        "binary operator",
                    )),
                }
            }
            other => Err(unsupported(
                other.span(),
                self.base,
                self.source_id,
                "expression",
            )),
        }
    }

    pub(crate) fn lower_type(&self, ty: &ast::TsType) -> Result<TypeSchema, ParseError> {
        match ty {
            ast::TsType::TsKeywordType(keyword) => match keyword.kind {
                ast::TsKeywordTypeKind::TsStringKeyword => Ok(TypeSchema::String),
                ast::TsKeywordTypeKind::TsNumberKeyword => Ok(TypeSchema::Number),
                ast::TsKeywordTypeKind::TsBooleanKeyword => Ok(TypeSchema::Bool),
                ast::TsKeywordTypeKind::TsNullKeyword => Ok(TypeSchema::Null),
                ast::TsKeywordTypeKind::TsVoidKeyword => Ok(TypeSchema::Null),
                _ => Err(unsupported(
                    keyword.span,
                    self.base,
                    self.source_id,
                    "type annotation",
                )),
            },
            ast::TsType::TsArrayType(array) => Ok(TypeSchema::Array(Box::new(
                self.lower_type(&array.elem_type)?,
            ))),
            ast::TsType::TsTupleType(tuple) => {
                let mut prefix = Vec::new();
                let mut rest = None;
                for (index, element) in tuple.elem_types.iter().enumerate() {
                    match element.ty.as_ref() {
                        ast::TsType::TsRestType(rest_type)
                            if index + 1 == tuple.elem_types.len() && rest.is_none() =>
                        {
                            rest = Some(Box::new(self.lower_type(&rest_type.type_ann)?));
                        }
                        ast::TsType::TsRestType(_) => {
                            return Err(unsupported(
                                element.span,
                                self.base,
                                self.source_id,
                                "non-trailing tuple rest element",
                            ));
                        }
                        ast::TsType::TsOptionalType(_) => {
                            return Err(unsupported(
                                element.span,
                                self.base,
                                self.source_id,
                                "optional tuple element",
                            ));
                        }
                        _ty if rest.is_some() => {
                            return Err(unsupported(
                                element.span,
                                self.base,
                                self.source_id,
                                "tuple element after rest",
                            ));
                        }
                        ty => prefix.push(self.lower_type(ty)?),
                    }
                }
                if let Some(rest) = rest {
                    Ok(TypeSchema::ArrayTupleRest { prefix, rest })
                } else {
                    Ok(TypeSchema::ArrayTuple(prefix))
                }
            }
            ast::TsType::TsTypeLit(literal) => Ok(TypeSchema::Object(
                self.lower_type_members(&literal.members)?,
            )),
            ast::TsType::TsParenthesizedType(paren) => self.lower_type(&paren.type_ann),
            ast::TsType::TsFnOrConstructorType(ast::TsFnOrConstructorType::TsFnType(function)) => {
                if function.type_params.is_some() {
                    return Err(unsupported(
                        function.span,
                        self.base,
                        self.source_id,
                        "generic function type",
                    ));
                }
                let mut params = Vec::with_capacity(function.params.len());
                for parameter in &function.params {
                    let ast::TsFnParam::Ident(binding) = parameter else {
                        return Err(unsupported(
                            parameter.span(),
                            self.base,
                            self.source_id,
                            "rest or destructured function type parameter",
                        ));
                    };
                    let Some(annotation) = binding.type_ann.as_ref() else {
                        let name = binding.id.sym.to_string();
                        return Err(ParseError::at_span(
                            byte_span(binding.id.span, self.base, self.source_id),
                            format!("TypeScript noImplicitAny: parameter '{name}' requires a type"),
                        ));
                    };
                    params.push(self.lower_type(&annotation.type_ann)?);
                }
                Ok(TypeSchema::Callable {
                    params,
                    result: Box::new(self.lower_type(&function.type_ann.type_ann)?),
                })
            }
            ast::TsType::TsFnOrConstructorType(ast::TsFnOrConstructorType::TsConstructorType(
                constructor,
            )) => Err(unsupported(
                constructor.span,
                self.base,
                self.source_id,
                "constructor type",
            )),
            ast::TsType::TsTypeRef(reference) => {
                let name = entity_name(&reference.type_name);
                if matches!(
                    self.named_types.get(&name),
                    Some(TypeSchema::Named(marker, _)) if marker == "__rustscript_resource_type"
                ) {
                    let params = reference.type_params.as_ref().map(|params| &params.params);
                    let Some([argument]) = params.map(|params| params.as_slice()) else {
                        return Err(ParseError::at_span(
                            byte_span(reference.span, self.base, self.source_id),
                            "Resource type requires exactly one string-literal catalog key",
                        ));
                    };
                    let ast::TsType::TsLitType(literal) = argument.as_ref() else {
                        return Err(unsupported(
                            argument.span(),
                            self.base,
                            self.source_id,
                            "non-literal resource type key",
                        ));
                    };
                    let ast::TsLit::Str(key) = &literal.lit else {
                        return Err(unsupported(
                            literal.span,
                            self.base,
                            self.source_id,
                            "non-string resource type key",
                        ));
                    };
                    let key = key.value.to_string_lossy().into_owned();
                    if !self.resource_keys.contains(&key) {
                        return Err(ParseError::at_span(
                            byte_span(literal.span, self.base, self.source_id),
                            format!("resource type '{key}' is not declared in the host catalog"),
                        ));
                    }
                    return vm::ResourceTypeKey::new(key)
                        .map(TypeSchema::Resource)
                        .map_err(|error| {
                            ParseError::at_span(
                                byte_span(literal.span, self.base, self.source_id),
                                error.to_string(),
                            )
                        });
                }
                let args = reference
                    .type_params
                    .as_ref()
                    .map(|params| {
                        params
                            .params
                            .iter()
                            .map(|parameter| self.lower_type(parameter))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                if name == "Array" && args.len() == 1 {
                    return Ok(TypeSchema::Array(Box::new(args[0].clone())));
                }
                if self.generic_type_params.contains(&name) {
                    if !args.is_empty() {
                        return Err(unsupported(
                            reference.span,
                            self.base,
                            self.source_id,
                            "generic parameter instantiation",
                        ));
                    }
                    return Ok(TypeSchema::GenericParam(name));
                }
                if let Some((parameters, schema)) = self.type_alias_schemas.get(&name) {
                    if matches!(schema, TypeSchema::Unknown) {
                        return Err(ParseError::at_span(
                            byte_span(reference.span, self.base, self.source_id),
                            format!(
                                "cyclic or forward TypeScript type alias '{name}' cannot be resolved"
                            ),
                        ));
                    }
                    if parameters.len() != args.len() {
                        return Err(ParseError::at_span(
                            byte_span(reference.span, self.base, self.source_id),
                            format!(
                                "TypeScript type alias '{name}' expects {} type arguments but received {}",
                                parameters.len(),
                                args.len()
                            ),
                        ));
                    }
                    let bindings = parameters
                        .iter()
                        .cloned()
                        .zip(args)
                        .collect::<HashMap<_, _>>();
                    return Ok(substitute_type_schema(schema, &bindings));
                }
                if let Some(declaration) = self.named_type_declarations.get(&name) {
                    if declaration.type_params.len() != args.len() {
                        return Err(ParseError::at_span(
                            byte_span(reference.span, self.base, self.source_id),
                            format!(
                                "TypeScript type '{name}' expects {} type arguments but received {}",
                                declaration.type_params.len(),
                                args.len()
                            ),
                        ));
                    }
                    return Ok(TypeSchema::Named(declaration.name.clone(), args));
                }
                if let Some(schema) = self.named_types.get(&name) {
                    return match schema {
                        TypeSchema::Named(canonical, formals) => {
                            if formals.len() != args.len() {
                                return Err(ParseError::at_span(
                                    byte_span(reference.span, self.base, self.source_id),
                                    format!(
                                        "TypeScript type '{name}' expects {} type arguments but received {}",
                                        formals.len(),
                                        args.len()
                                    ),
                                ));
                            }
                            Ok(TypeSchema::Named(canonical.clone(), args))
                        }
                        _ if args.is_empty() => Ok(schema.clone()),
                        _ => Err(unsupported(
                            reference.span,
                            self.base,
                            self.source_id,
                            "type alias type arguments",
                        )),
                    };
                }
                Err(ParseError::at_span(
                    byte_span(reference.span, self.base, self.source_id),
                    format!("unknown TypeScript type '{name}'"),
                ))
            }
            ast::TsType::TsUnionOrIntersectionType(union) => match union {
                ast::TsUnionOrIntersectionType::TsUnionType(union) => {
                    let nulls = union
                        .types
                        .iter()
                        .filter(|member| matches!(&***member, ast::TsType::TsKeywordType(keyword) if keyword.kind == ast::TsKeywordTypeKind::TsNullKeyword))
                        .count();
                    if nulls != 1 || union.types.len() != 2 {
                        return Err(unsupported(
                            union.span,
                            self.base,
                            self.source_id,
                            "non-nullable union type",
                        ));
                    }
                    let member = union
                        .types
                        .iter()
                        .find(|member| !matches!(&***member, ast::TsType::TsKeywordType(keyword) if keyword.kind == ast::TsKeywordTypeKind::TsNullKeyword))
                        .ok_or_else(|| ParseError::at_span(
                            byte_span(union.span, self.base, self.source_id),
                            "nullable TypeScript union has no value type",
                        ))?;
                    Ok(TypeSchema::Optional(Box::new(self.lower_type(member)?)))
                }
                ast::TsUnionOrIntersectionType::TsIntersectionType(intersection) => {
                    Err(unsupported(
                        intersection.span,
                        self.base,
                        self.source_id,
                        "intersection type",
                    ))
                }
            },
            _ => Err(unsupported(
                ty.span(),
                self.base,
                self.source_id,
                "type annotation",
            )),
        }
    }
}

pub(crate) fn lower(
    source: &str,
    context: &SourceFrontendContext<'_>,
) -> Result<FrontendIr, ParseError> {
    lower_impl(source, context, false, 0, None)
}

/// Explicit reference-array compilation entry. The fixed private prelude
/// is parsed separately and merged as hygienic AST items (prelude first);
/// user source bytes and spans are never rewritten or concatenated, and
/// helper authority comes from real typed predeclare indices, never from
/// user-controlled names. Plain `lower` stays untouched.
pub(crate) fn lower_reference_array(
    source: &str,
    context: &SourceFrontendContext<'_>,
) -> Result<FrontendIr, ParseError> {
    let user = parse_module(source)?;
    let prelude = parse_module(super::array_lowering::REFERENCE_ARRAY_PRELUDE)?;
    lower_impl_merged(user, Some(prelude), source, context, true)
}

fn lower_impl(
    source: &str,
    context: &SourceFrontendContext<'_>,
    ref_mode: bool,
    _ref_user_len: usize,
    _extra: Option<ParsedModule>,
) -> Result<FrontendIr, ParseError> {
    debug_assert!(!ref_mode, "reference-array mode must use lower_impl_merged");
    let parsed = parse_module(source)?;
    lower_impl_merged(parsed, None, source, context, false)
}

fn lower_impl_merged(
    user: ParsedModule,
    prelude: Option<ParsedModule>,
    source: &str,
    context: &SourceFrontendContext<'_>,
    ref_mode: bool,
) -> Result<FrontendIr, ParseError> {
    // Merge prelude AST items before user items in memory (no source-text
    // concatenation). Prelude helpers lower first so user calls resolve to
    // real predeclared helper indices; a user declaration colliding with a
    // hygienic helper name fails as a duplicate like any other.
    let mut module = user.module;
    let mut parsed_base = user.base;
    let ref_prelude_items = if ref_mode {
        let prelude = prelude.expect("reference-array prelude must parse");
        let mut body = prelude.module.body;
        let count = body.len();
        body.extend(std::mem::take(&mut module.body));
        module.body = body;
        // Keep the user base for user spans; prelude spans live in their own
        // numeric space and are distinguished by item position (in_ref_prelude),
        // never by offsets into the user source.
        parsed_base = user.base;
        count
    } else {
        0
    };
    let host_imports = install_host_imports(&module, context, parsed_base)?;
    let imported_functions = context
        .imports
        .iter()
        .filter_map(|import| match import {
            FrontendImportedDeclaration::Function {
                local,
                symbol,
                declaration,
            } => Some((local.clone(), (*symbol, declaration.clone()))),
            FrontendImportedDeclaration::Type { .. } => None,
        })
        .collect();
    let mut named_types = context
        .imports
        .iter()
        .filter_map(|import| match import {
            FrontendImportedDeclaration::Type { local, schema, .. } => {
                Some((local.clone(), schema.clone()))
            }
            FrontendImportedDeclaration::Function { .. } => None,
        })
        .collect::<HashMap<_, _>>();
    named_types.extend(host_imports.named_types);
    let type_alias_schemas = context
        .imports
        .iter()
        .filter_map(|import| match import {
            FrontendImportedDeclaration::Type {
                local,
                schema,
                declaration: Some(declaration),
                ..
            } if !matches!(schema, TypeSchema::Named(name, _) if name == &declaration.name) => {
                Some((
                    local.clone(),
                    (declaration.type_params.clone(), schema.clone()),
                ))
            }
            FrontendImportedDeclaration::Type {
                local,
                schema,
                declaration: None,
                ..
            } => {
                let parameters = generic_params_in_schema(schema);
                (!parameters.is_empty()).then(|| (local.clone(), (parameters, schema.clone())))
            }
            _ => None,
        })
        .collect();
    let named_type_declarations = context
        .imports
        .iter()
        .filter_map(|import| match import {
            FrontendImportedDeclaration::Type {
                local,
                declaration: Some(declaration),
                ..
            } => Some((local.clone(), declaration.clone())),
            _ => None,
        })
        .collect();
    Lowerer {
        source,
        source_id: context.source_id,
        base: parsed_base,
        next_slot: 0,
        next_node_id: 0,
        next_scope_id: 1,
        next_function_index: host_imports.next_function_index,
        scope_id: 0,
        ref_mode,
        ref_prelude_items,
        in_ref_prelude: false,
        stmt_value_depth: 0,
        locals: HashMap::new(),
        local_types: HashMap::new(),
        mutable_locals: HashSet::new(),
        const_callable_origins: HashMap::new(),
        closure_captures: Vec::new(),
        imported_functions,
        host_function_indices: host_imports.by_local_name,
        host_parameter_passings: host_imports.parameter_passings,
        declared_host_functions: host_imports.declared_functions,
        host_api_metadata: host_imports.metadata,
        catalog_visibility: host_imports.visibility,
        named_types,
        named_type_declarations,
        type_alias_schemas,
        struct_schemas: host_imports.struct_schemas,
        resource_keys: host_imports.resource_keys,
        type_provenance: Vec::new(),
        struct_decl_sites: Vec::new(),
        generic_type_params: HashSet::new(),
        function_indices: HashMap::new(),
        function_parameters: HashMap::new(),
        generic_function_indices: HashSet::new(),
        predeclared_signatures: HashMap::new(),
        functions: host_imports.functions,
        function_impls: HashMap::new(),
        function_sites: Vec::new(),
        function_scopes: Vec::new(),
        call_sites: Vec::new(),
        local_bindings: Vec::new(),
        local_decls: Vec::new(),
        local_refs: Vec::new(),
        stmt_spans: Vec::new(),
        gen_iterators: HashMap::new(),
        gen_results: HashMap::new(),
        module_const_inits: HashMap::new(),
        in_gen_body: false,
    }
    .lower(module)
}

pub(crate) fn exports(
    source: &str,
    context: &SourceFrontendContext<'_>,
    ir: &FrontendIr,
) -> Result<Vec<FrontendTypeExport>, ParseError> {
    let parsed = parse_module(source)?;
    let mut exports = Vec::new();
    for item in parsed.module.body {
        let ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDecl(export)) = item else {
            continue;
        };
        let (declaration_name, is_alias) = match export.decl {
            ast::Decl::TsInterface(interface) => (interface.id.sym.to_string(), false),
            ast::Decl::TsTypeAlias(alias) => (alias.id.sym.to_string(), true),
            _ => continue,
        };
        let declaration = ir
            .struct_schemas
            .get(&declaration_name)
            .cloned()
            .ok_or_else(|| {
                ParseError::at_span(
                    byte_span(export.span, parsed.base, context.source_id),
                    format!("missing TypeScript schema for exported type '{declaration_name}'"),
                )
            })?;
        let type_args = declaration
            .type_params
            .iter()
            .cloned()
            .map(TypeSchema::GenericParam)
            .collect();
        let schema = if is_alias {
            declaration.body_schema.clone()
        } else {
            TypeSchema::Named(declaration_name.clone(), type_args)
        };
        exports.push(FrontendTypeExport {
            source_name: declaration_name,
            schema,
            declaration: Some(declaration),
        });
    }
    // Synthetic default carriers (our own generated, proven by IR presence +
    // provenance; user colliding hygienic names would have already failed as
    // duplicates during lowering, so any such carrier here is ours, not a
    // broad prefix grant to user shapes).
    // Export all carriers whose names use our prefix and whose provenance
    // marks them as generated from a real function (source_name is the
    // original function). This uses actual IR identities + metadata.
    if let Some(index) = &ir.parsed_semantic_index {
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();
        for prov in &index.type_provenance {
            if !prov
                .canonical_name
                .starts_with(super::default_lowering::CARRIER_PREFIX)
            {
                continue;
            }
            if !seen.insert(prov.canonical_name.clone()) {
                continue;
            }
            if let Some(declaration) = ir.struct_schemas.get(&prov.canonical_name) {
                exports.push(FrontendTypeExport {
                    source_name: prov.canonical_name.clone(),
                    schema: TypeSchema::Named(prov.canonical_name.clone(), Vec::new()),
                    declaration: Some(declaration.clone()),
                });
            }
        }
    } else {
        // Fallback (no semantic index): export carriers present in IR.
        for (name, declaration) in &ir.struct_schemas {
            if name.starts_with(super::default_lowering::CARRIER_PREFIX) {
                exports.push(FrontendTypeExport {
                    source_name: name.clone(),
                    schema: TypeSchema::Named(name.clone(), Vec::new()),
                    declaration: Some(declaration.clone()),
                });
            }
        }
    }
    Ok(exports)
}

pub(crate) fn module_imports(
    source: &str,
    source_id: u32,
) -> Result<Vec<vm::ModuleImport>, ParseError> {
    module_imports_with_reference_defaults(source, source_id, false)
}

fn module_imports_with_reference_defaults(
    source: &str,
    source_id: u32,
    reference_defaults: bool,
) -> Result<Vec<vm::ModuleImport>, ParseError> {
    let parsed = parse_module(source)?;
    let mut imports = Vec::new();
    for item in parsed.module.body {
        let ast::ModuleItem::ModuleDecl(ast::ModuleDecl::Import(import)) = item else {
            continue;
        };
        let source_name = import.src.value.to_string_lossy().into_owned();
        let mut clause = Vec::new();
        // Collect original value imports for synthetic getter/carrier synthesis.
        let mut value_pairs: Vec<(String, String)> = Vec::new();
        for specifier in import.specifiers {
            match specifier {
                ast::ImportSpecifier::Named(named) => {
                    let imported = named
                        .imported
                        .map(|name| match name {
                            ast::ModuleExportName::Ident(ident) => ident.sym.to_string(),
                            ast::ModuleExportName::Str(value) => {
                                value.value.to_string_lossy().into_owned()
                            }
                        })
                        .unwrap_or_else(|| named.local.sym.to_string());
                    let local = named.local.sym.to_string();
                    let is_type = import.type_only || named.is_type_only;
                    clause.push(if is_type {
                        vm::NamedImport::type_only(imported.clone(), local.clone())
                    } else {
                        vm::NamedImport::value(imported.clone(), local.clone())
                    });
                    if !is_type
                        && !super::default_lowering::is_synthetic_default_name(&imported)
                        && !super::default_lowering::is_synthetic_default_name(&local)
                        && !imported.starts_with("__compat_ref_")
                        && !local.starts_with("__compat_ref_")
                    {
                        value_pairs.push((imported, local));
                    }
                }
                ast::ImportSpecifier::Default(default) => clause.push(vm::NamedImport::value(
                    "default",
                    default.local.sym.to_string(),
                )),
                ast::ImportSpecifier::Namespace(namespace) => {
                    let span = byte_span(import.span, parsed.base, source_id);
                    let line = line_at(source, span.lo) as usize;
                    imports.push(vm::ModuleImport {
                        spec: source_name.clone(),
                        clause: vm::ImportClause::Namespace(namespace.local.sym.to_string()),
                        line,
                        namespace: if import.type_only {
                            vm::DeclarationNamespace::Type
                        } else {
                            vm::DeclarationNamespace::Value
                        },
                        span: Some(span),
                    });
                }
            }
        }
        // Synthetic callee-owned default imports (same spec, same line):
        // one zero-arg getter (value) + one carrier type per value import.
        // Uses actual binding identities (exported/local names) plus hygienic
        // derivation; existence is verified via actual imports at lowering
        // (no broad prefix authority, no filename reading). Skips synthetic
        // themselves to avoid recursion. Generated getters/carriers exist for
        // exported user functions in reference mode. Plain imports retain
        // their original clauses without private metadata dependencies.
        // Only for file imports (user modules with defaults); host imports
        // (arrays, proof, compat_math, etc.) never have defaults and must not
        // gain synthetic requests (would fail resolution + broaden authority).
        // Also only for TypeScript file imports (.ts/.mts); RSS/Lua/etc.
        // modules never have default carriers (would fail with missing type).
        if reference_defaults
            && !is_host_import_spec(&source_name)
            && (source_name.ends_with(".ts") || source_name.ends_with(".mts"))
        {
            for (imported, local) in value_pairs {
                let getter_imported = super::default_lowering::getter_name(&imported);
                let getter_local = super::default_lowering::getter_name(&local);
                clause.push(vm::NamedImport::value(getter_imported, getter_local));
                let carrier_imported = super::default_lowering::carrier_name(&imported);
                let carrier_local = super::default_lowering::carrier_name(&local);
                clause.push(vm::NamedImport::type_only(carrier_imported, carrier_local));
            }
        }
        if !clause.is_empty() {
            let span = byte_span(import.span, parsed.base, source_id);
            let line = line_at(source, span.lo) as usize;
            imports.push(vm::ModuleImport {
                spec: source_name,
                clause: vm::ImportClause::Named(clause),
                line,
                namespace: if import.type_only {
                    vm::DeclarationNamespace::Type
                } else {
                    vm::DeclarationNamespace::Value
                },
                span: Some(span),
            });
        }
    }
    Ok(imports)
}

pub(crate) fn parse_imports_for_plugin(
    source: &str,
    context: &SourceFrontendContext<'_>,
) -> Result<Vec<vm::ModuleImport>, vm::SourcePathError> {
    module_imports(source, context.source_id)
        .map_err(|error| vm::SourcePathError::Source(vm::SourceError::Parse(error)))
}

pub(crate) fn parse_reference_array_imports_for_plugin(
    source: &str,
    context: &SourceFrontendContext<'_>,
) -> Result<Vec<vm::ModuleImport>, vm::SourcePathError> {
    module_imports_with_reference_defaults(source, context.source_id, true)
        .map_err(|error| vm::SourcePathError::Source(vm::SourceError::Parse(error)))
}
