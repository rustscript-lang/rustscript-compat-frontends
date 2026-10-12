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

fn byte_span(span: swc_common::Span, base: u32, source_id: u32) -> Span {
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

fn parameter_binding(parameter: &ast::Pat) -> Option<(&ast::BindingIdent, Option<&ast::Expr>)> {
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

fn numeric_literal(value: f64) -> Expr {
    if value.is_finite()
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

fn schema_value_type(schema: &TypeSchema) -> vm::ValueType {
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

fn line_at(source: &str, byte: usize) -> u32 {
    let end = byte.min(source.len());
    let prefix = &source[..end];
    u32::try_from(prefix.bytes().filter(|byte| *byte == b'\n').count() + 1).unwrap_or(u32::MAX)
}

fn unsupported(span: swc_common::Span, base: u32, source_id: u32, what: &str) -> ParseError {
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

struct ClosureCaptureContext {
    local_slots: HashSet<u16>,
    captures: HashMap<u16, u16>,
    capture_copies: Vec<(u16, u16)>,
}

struct Lowerer<'a> {
    source: &'a str,
    source_id: u32,
    base: u32,
    next_slot: u16,
    next_node_id: u32,
    next_scope_id: u32,
    next_function_index: u16,
    scope_id: u32,
    locals: HashMap<String, u16>,
    local_types: HashMap<String, TypeSchema>,
    mutable_locals: HashSet<u16>,
    closure_captures: Vec<ClosureCaptureContext>,
    imported_functions: HashMap<String, (vm::SymbolId, FunctionDecl)>,
    host_function_indices: HashMap<String, HashMap<u8, u16>>,
    host_parameter_passings: HashMap<String, HashMap<u8, Vec<vm::HostParamPassing>>>,
    declared_host_functions: HashMap<String, Vec<vm::HostFunctionSchema>>,
    host_api_metadata: Option<HostApiIrMetadata>,
    catalog_visibility: CatalogVisibility,
    named_types: HashMap<String, TypeSchema>,
    named_type_declarations: HashMap<String, StructDecl>,
    type_alias_schemas: HashMap<String, (Vec<String>, TypeSchema)>,
    struct_schemas: HashMap<String, StructDecl>,
    resource_keys: HashSet<String>,
    type_provenance: Vec<GuestTypeProvenance>,
    struct_decl_sites: Vec<StructDeclSite>,
    generic_type_params: HashSet<String>,
    function_indices: HashMap<String, (u16, u8)>,
    function_parameters: HashMap<String, Vec<ast::Pat>>,
    generic_function_indices: HashSet<u16>,
    functions: Vec<FunctionDecl>,
    function_impls: HashMap<u16, FunctionImpl>,
    function_sites: Vec<FunctionDeclSite>,
    function_scopes: Vec<ParsedLexicalScope>,
    call_sites: Vec<ParsedCallSite>,
    local_bindings: Vec<(String, u16)>,
    local_decls: Vec<LocalDeclSite>,
    local_refs: Vec<LocalRefSite>,
    stmt_spans: Vec<StmtSpanSite>,
}

impl<'a> Lowerer<'a> {
    fn alloc_node_id(&mut self) -> SemanticNodeId {
        let id = SemanticNodeId(self.next_node_id);
        self.next_node_id = self
            .next_node_id
            .checked_add(1)
            .expect("TypeScript semantic node id overflow");
        id
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
            self.function_indices.insert(name, (index, arity));
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
                self.function_indices.insert(name, (index, arity));
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
        for item in module.body {
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
            return Err(unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "async or generator function",
            ));
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

        let return_schema = function
            .function
            .return_type
            .as_ref()
            .map(|annotation| self.lower_type(&annotation.type_ann))
            .transpose()?;
        let mut body_stmts = Vec::new();
        let mut body_expr = Expr::Null;
        let mut body_expr_line = line_at(
            self.source,
            byte_span(body.span, self.base, self.source_id).lo,
        );
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
                self.lower_stmt(statement.clone(), &mut body_stmts)?;
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
        let lowered = (|| -> Result<(Vec<u16>, Vec<Stmt>, Vec<Expr>), ParseError> {
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
                    let expression = self.lower_expr(expression.clone())?;
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
            if self.scope_id == 0 {
                if let (ast::Pat::Ident(binding), Some(initializer)) =
                    (&declarator.name, &declarator.init)
                    && let ast::Expr::Arrow(arrow) = initializer.as_ref()
                    && self.function_indices.contains_key(binding.id.sym.as_ref())
                {
                    let function = arrow_function_decl(binding.id.clone(), arrow.clone());
                    self.lower_function(&function, exported, output)?;
                    continue;
                }
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
            let expr = self.lower_expr(*init)?;
            let slot = self.next_slot;
            self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    "TypeScript local slot limit exceeded",
                )
            })?;
            let local_type = declared_schema.clone().or(inferred_schema);
            self.locals.insert(name.clone(), slot);
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

    fn lower_function_body(&mut self, body: ast::FunctionBody) -> Result<Vec<Stmt>, ParseError> {
        self.lower_block_contents(body.span, body.stmts)
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

    fn lower_assignment_statement(
        &mut self,
        expr: ast::Expr,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let span = expr.span();
        let site = byte_span(span, self.base, self.source_id);
        let line = line_at(self.source, site.lo);
        match expr {
            ast::Expr::Assign(assignment) => {
                let ast::AssignTarget::Simple(target) = assignment.left else {
                    return Err(unsupported(
                        assignment.span,
                        self.base,
                        self.source_id,
                        "non-local assignment target",
                    ));
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
                let kind = match assignment.op {
                    ast::AssignOp::Assign => AssignmentKind::Set,
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
                let value = self.lower_expr(*assignment.right)?;
                output.push(Stmt::Assign {
                    kind,
                    index: slot,
                    expr: value,
                    line,
                });
            }
            ast::Expr::Update(update) => {
                if update.op != ast::UpdateOp::PlusPlus {
                    return Err(unsupported(
                        update.span,
                        self.base,
                        self.source_id,
                        "decrement update",
                    ));
                }
                let ast::Expr::Ident(ident) = *update.arg else {
                    return Err(unsupported(
                        update.span,
                        self.base,
                        self.source_id,
                        "non-local increment target",
                    ));
                };
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
                output.push(Stmt::Assign {
                    kind: AssignmentKind::Increment,
                    index: slot,
                    expr: Expr::Int(1),
                    line,
                });
            }
            other => {
                let lowered = self.lower_expr(other)?;
                output.push(Stmt::Expr {
                    expr: lowered,
                    line,
                });
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
                    self.lower_assignment_statement(*expression, &mut lowered)?;
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
                    self.lower_assignment_statement(*update, &mut lowered)?;
                    lowered
                        .pop()
                        .expect("update expression emits one statement")
                }
                None => Stmt::Noop { line },
            };
            let mut body = Vec::new();
            self.lower_stmt(*statement.body, &mut body)?;
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

    fn lower_stmt(&mut self, stmt: ast::Stmt, output: &mut Vec<Stmt>) -> Result<(), ParseError> {
        match stmt {
            ast::Stmt::Decl(ast::Decl::Var(decl)) => self.lower_var_decl(*decl, false, output)?,
            ast::Stmt::Expr(expr) => {
                self.lower_assignment_statement(*expr.expr, output)?;
            }
            ast::Stmt::Block(block) => {
                output.extend(self.lower_block(block)?);
            }
            ast::Stmt::If(if_statement) => {
                let condition = self.lower_expr(*if_statement.test)?;
                let mut then_branch = Vec::new();
                self.lower_stmt(*if_statement.cons, &mut then_branch)?;
                let mut else_branch = Vec::new();
                if let Some(else_statement) = if_statement.alt {
                    self.lower_stmt(*else_statement, &mut else_branch)?;
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
                self.lower_stmt(*while_statement.body, &mut body)?;
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

    fn capture_slot(
        &mut self,
        source_slot: u16,
        name: &str,
        span: swc_common::Span,
    ) -> Result<u16, ParseError> {
        let mut slot = source_slot;
        for index in 0..self.closure_captures.len() {
            if self.closure_captures[index].local_slots.contains(&slot) {
                continue;
            }
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
                .map(|annotation| self.lower_type(&annotation.type_ann))
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
            ast::ArrowFunctionBody::FunctionBody(block) => {
                self.lower_function_body(block)
                    .map(|statements| Expr::Block {
                        stmts: statements,
                        expr: Box::new(Expr::Null),
                    })
            }
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

    fn expression_schema(&self, expression: &ast::Expr) -> Option<TypeSchema> {
        match expression {
            ast::Expr::Ident(ident) => self.local_types.get(ident.sym.as_ref()).cloned(),
            ast::Expr::Paren(paren) => self.expression_schema(&paren.expr),
            ast::Expr::Arrow(arrow) => {
                let mut params = Vec::with_capacity(arrow.params.len());
                for parameter in &arrow.params {
                    let (binding, default) = parameter_binding(parameter)?;
                    if default.is_some() {
                        return None;
                    }
                    let annotation = binding.type_ann.as_ref()?;
                    params.push(self.lower_type(&annotation.type_ann).ok()?);
                }
                let return_type = arrow.return_type.as_ref()?;
                Some(TypeSchema::Callable {
                    params,
                    result: Box::new(self.lower_type(&return_type.type_ann).ok()?),
                })
            }
            ast::Expr::Member(member) => {
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

    fn call_result_schema(&self, call: &ast::CallExpr) -> Option<TypeSchema> {
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
        let declaration = if let Some((index, _)) = self.function_indices.get(&callee) {
            self.functions.get(usize::from(*index))
        } else if let Some((_, declaration)) = self.imported_functions.get(&callee) {
            Some(declaration)
        } else {
            let arity = u8::try_from(call.args.len()).ok()?;
            let index = *self.host_function_indices.get(&callee)?.get(&arity)?;
            return self.host_return_schema(index);
        }?;
        if declaration.type_params.is_empty() {
            declaration.return_schema.clone()
        } else {
            None
        }
    }

    fn alloc_hidden_local(&mut self, span: swc_common::Span) -> Result<u16, ParseError> {
        let slot = self.next_slot;
        self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
            unsupported(span, self.base, self.source_id, "function local slot limit")
        })?;
        Ok(slot)
    }

    fn lower_member_get(&mut self, member: ast::MemberExpr) -> Result<Expr, ParseError> {
        let member_span = member.span;
        let object = member.obj;
        let property = match member.prop {
            ast::MemberProp::Ident(property) => property,
            ast::MemberProp::Computed(property) => {
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

    fn builtin_call(builtin: vm::BuiltinFunction, args: Vec<Expr>) -> Expr {
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

    fn lower_expr(&mut self, expr: ast::Expr) -> Result<Expr, ParseError> {
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
                let local_slot = if is_namespace_call {
                    None
                } else {
                    self.locals.get(&callee).copied()
                };
                let local_function = local_slot
                    .is_none()
                    .then(|| self.function_indices.get(&callee).copied())
                    .flatten();
                let imported_function = local_slot
                    .is_none()
                    .then(|| self.imported_functions.get(&callee).cloned())
                    .flatten();
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
                        "Math::round" => Some(vm::BuiltinFunction::MathRound),
                        "Number::isNaN" | "isNaN" => Some(vm::BuiltinFunction::MathIsNaN),
                        _ => None,
                    }
                } else {
                    None
                };
                let standard_integer = local_slot.is_none()
                    && local_function.is_none()
                    && imported_function.is_none()
                    && host_index.is_none()
                    && callee == "Number::isInteger";
                let standard_arity = (standard_builtin.is_some() || standard_integer).then_some(1);
                let host_passings = call_arity
                    .filter(|_| host_index.is_some())
                    .and_then(|arity| {
                        self.host_parameter_passings
                            .get(&callee)
                            .and_then(|by_arity| by_arity.get(&arity))
                    })
                    .cloned()
                    .unwrap_or_default();
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
                if let Some(arity) = arity
                    && call.args.len() != usize::from(arity)
                    && !defaulted_call
                {
                    return Err(ParseError::at_span(
                        call_span,
                        format!(
                            "TypeScript function '{callee}' expects {arity} arguments but received {}",
                            call.args.len()
                        ),
                    ));
                }
                if host_index.is_some() && !explicit_type_args.is_empty() {
                    return Err(unsupported(
                        call.span,
                        self.base,
                        self.source_id,
                        "generic host function type arguments",
                    ));
                }
                let args = call
                    .args
                    .into_iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        let argument_span = argument.span();
                        if argument.spread.is_some() {
                            return Err(unsupported(
                                argument_span,
                                self.base,
                                self.source_id,
                                "spread call argument",
                            ));
                        }
                        let expression = self.lower_expr(*argument.expr)?;
                        match host_passings.get(index) {
                            Some(vm::HostParamPassing::Borrow) => match expression {
                                variable @ Expr::Var(_) => Ok(Expr::Borrow(Box::new(variable))),
                                Expr::Borrow(inner) if matches!(inner.as_ref(), Expr::Var(_)) => {
                                    Ok(Expr::Borrow(inner))
                                }
                                _ => Err(unsupported(
                                    argument_span,
                                    self.base,
                                    self.source_id,
                                    "borrowed host argument must be a local binding",
                                )),
                            },
                            Some(vm::HostParamPassing::BorrowMut) => match expression {
                                variable @ Expr::Var(_) => Ok(Expr::BorrowMut(Box::new(variable))),
                                Expr::Borrow(inner) if matches!(inner.as_ref(), Expr::Var(_)) => {
                                    Ok(Expr::BorrowMut(inner))
                                }
                                _ => Err(unsupported(
                                    argument_span,
                                    self.base,
                                    self.source_id,
                                    "mutable-borrow host argument must be a local binding",
                                )),
                            },
                            Some(vm::HostParamPassing::TakeOwned) => match expression {
                                Expr::Borrow(inner) if matches!(inner.as_ref(), Expr::Var(_)) => {
                                    Ok(*inner)
                                }
                                other => Ok(other),
                            },
                            _ => Ok(expression),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
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
                    (
                        ParsedCallTarget::Module(symbol),
                        Expr::ModuleCall(symbol, explicit_type_args, args, Some(id)),
                    )
                } else if standard_integer {
                    let argument = args
                        .into_iter()
                        .next()
                        .expect("Number.isInteger has one argument");
                    let finite = Expr::Call(
                        vm::BuiltinFunction::MathIsFinite.call_index(),
                        Vec::new(),
                        vec![argument.clone()],
                        None,
                        Some(id),
                    );
                    let floored =
                        Self::builtin_call(vm::BuiltinFunction::MathFloor, vec![argument.clone()]);
                    (
                        ParsedCallTarget::Function(vm::BuiltinFunction::MathIsFinite.call_index()),
                        Expr::And(
                            Box::new(finite),
                            Box::new(Expr::Eq(Box::new(argument), Box::new(floored))),
                        ),
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
                    name: callee,
                    scope_id: self.scope_id,
                    is_namespace_call,
                });
                Ok(expression)
            }
            ast::Expr::Cond(conditional) => Ok(Expr::IfElse {
                condition: Box::new(self.lower_expr(*conditional.test)?),
                then_expr: Box::new(self.lower_expr(*conditional.cons)?),
                else_expr: Box::new(self.lower_expr(*conditional.alt)?),
            }),
            ast::Expr::Member(member) => self.lower_member_get(member),
            ast::Expr::Arrow(arrow) => self.lower_arrow(arrow),
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
                let left = Box::new(self.lower_expr(*value.left)?);
                let right = Box::new(self.lower_expr(*value.right)?);
                match value.op {
                    ast::BinaryOp::Add => Ok(Expr::Add(left, right)),
                    ast::BinaryOp::Sub => Ok(Expr::Sub(left, right)),
                    ast::BinaryOp::Mul => Ok(Expr::Mul(left, right)),
                    ast::BinaryOp::Div => Ok(Expr::Div(left, right)),
                    ast::BinaryOp::Mod => Ok(Expr::Mod(left, right)),
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

    fn lower_type(&self, ty: &ast::TsType) -> Result<TypeSchema, ParseError> {
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
    let parsed = parse_module(source)?;
    let host_imports = install_host_imports(&parsed.module, context, parsed.base)?;
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
        base: parsed.base,
        next_slot: 0,
        next_node_id: 0,
        next_scope_id: 1,
        next_function_index: host_imports.next_function_index,
        scope_id: 0,
        locals: HashMap::new(),
        local_types: HashMap::new(),
        mutable_locals: HashSet::new(),
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
        functions: host_imports.functions,
        function_impls: HashMap::new(),
        function_sites: Vec::new(),
        function_scopes: Vec::new(),
        call_sites: Vec::new(),
        local_bindings: Vec::new(),
        local_decls: Vec::new(),
        local_refs: Vec::new(),
        stmt_spans: Vec::new(),
    }
    .lower(parsed.module)
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
    Ok(exports)
}

pub(crate) fn module_imports(
    source: &str,
    source_id: u32,
) -> Result<Vec<vm::ModuleImport>, ParseError> {
    let parsed = parse_module(source)?;
    let mut imports = Vec::new();
    for item in parsed.module.body {
        let ast::ModuleItem::ModuleDecl(ast::ModuleDecl::Import(import)) = item else {
            continue;
        };
        let source_name = import.src.value.to_string_lossy().into_owned();
        let mut clause = Vec::new();
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
                    clause.push(if import.type_only || named.is_type_only {
                        vm::NamedImport::type_only(imported, local)
                    } else {
                        vm::NamedImport::value(imported, local)
                    });
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
