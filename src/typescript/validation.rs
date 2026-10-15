use vm::compiler::ir::{ClosureExpr, FunctionDecl, ParsedCallSite};
use vm::compiler::{CompileError, FrontendTypedIr};
use vm::{Expr, Stmt, TypeSchema};

pub(crate) fn validate(typed: &FrontendTypedIr<'_>) -> Result<(), CompileError> {
    let Some(source_id) = typed.source_id else {
        return Ok(());
    };
    for statement in &typed.ir.stmts {
        validate_statement(statement, typed, source_id)?;
    }
    for implementation in typed.ir.function_impls.values() {
        for statement in &implementation.body_stmts {
            validate_statement(statement, typed, source_id)?;
        }
        validate_expression(&implementation.body_expr, typed, source_id)?;
    }
    Ok(())
}

fn validate_statement(
    statement: &Stmt,
    typed: &FrontendTypedIr<'_>,
    source_id: u32,
) -> Result<(), CompileError> {
    match statement {
        Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
            validate_expression(expr, typed, source_id)?;
        }
        Stmt::ClosureLet { closure, .. } => validate_closure(closure, typed, source_id)?,
        Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            validate_expression(condition, typed, source_id)?;
            for statement in then_branch.iter().chain(else_branch) {
                validate_statement(statement, typed, source_id)?;
            }
        }
        Stmt::For {
            init,
            condition,
            post,
            body,
            ..
        } => {
            validate_statement(init, typed, source_id)?;
            validate_expression(condition, typed, source_id)?;
            validate_statement(post, typed, source_id)?;
            for statement in body {
                validate_statement(statement, typed, source_id)?;
            }
        }
        Stmt::While {
            condition, body, ..
        } => {
            validate_expression(condition, typed, source_id)?;
            for statement in body {
                validate_statement(statement, typed, source_id)?;
            }
        }
        Stmt::Noop { .. }
        | Stmt::FuncDecl { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Drop { .. } => {}
    }
    Ok(())
}

fn validate_closure(
    closure: &ClosureExpr,
    typed: &FrontendTypedIr<'_>,
    source_id: u32,
) -> Result<(), CompileError> {
    validate_expression(&closure.body, typed, source_id)
}

fn validate_expression(
    expression: &Expr,
    typed: &FrontendTypedIr<'_>,
    source_id: u32,
) -> Result<(), CompileError> {
    match expression {
        Expr::Call(_, _, args, resolution, semantic_id) => {
            let call_is_owned = semantic_id
                .and_then(|id| call_site(typed, id))
                .is_some_and(|site| site.expr_span.source_id == source_id);
            if call_is_owned && let Some(resolution) = resolution {
                for (index, (argument, parameter)) in
                    args.iter().zip(resolution.params.iter()).enumerate()
                {
                    if expression_schema(argument, typed).is_some_and(|actual| {
                        schema_requires_numeric_narrowing(&actual, &parameter.schema, typed, 0)
                    }) {
                        return Err(numeric_abi_error(
                            typed,
                            source_id,
                            *semantic_id,
                            &resolution.name,
                            index,
                            &parameter.schema,
                        ));
                    }
                }
            }
            for argument in args {
                validate_expression(argument, typed, source_id)?;
            }
        }
        Expr::ModuleCall(_, _, args, _) | Expr::LocalCall(_, _, args, _) => {
            for argument in args {
                validate_expression(argument, typed, source_id)?;
            }
        }
        Expr::Closure(closure) => validate_closure(closure, typed, source_id)?,
        Expr::ClosureCall(closure, args) => {
            validate_closure(closure, typed, source_id)?;
            for argument in args {
                validate_expression(argument, typed, source_id)?;
            }
        }
        Expr::OptionalGet { container, key, .. } => {
            validate_expression(container, typed, source_id)?;
            validate_expression(key, typed, source_id)?;
        }
        Expr::OptionUnwrapOr {
            value, fallback, ..
        } => {
            validate_expression(value, typed, source_id)?;
            validate_expression(fallback, typed, source_id)?;
        }
        Expr::Add(left, right)
        | Expr::Sub(left, right)
        | Expr::Mul(left, right)
        | Expr::Div(left, right)
        | Expr::Mod(left, right)
        | Expr::And(left, right)
        | Expr::Or(left, right)
        | Expr::Eq(left, right)
        | Expr::Lt(left, right)
        | Expr::Gt(left, right) => {
            validate_expression(left, typed, source_id)?;
            validate_expression(right, typed, source_id)?;
        }
        Expr::Neg(value)
        | Expr::Not(value)
        | Expr::ToOwned(value)
        | Expr::Borrow(value)
        | Expr::BorrowMut(value) => {
            validate_expression(value, typed, source_id)?;
        }
        Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            validate_expression(condition, typed, source_id)?;
            validate_expression(then_expr, typed, source_id)?;
            validate_expression(else_expr, typed, source_id)?;
        }
        Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            validate_expression(value, typed, source_id)?;
            for (_, arm) in arms {
                validate_expression(arm, typed, source_id)?;
            }
            validate_expression(default, typed, source_id)?;
        }
        Expr::Block { stmts, expr } => {
            for statement in stmts {
                validate_statement(statement, typed, source_id)?;
            }
            validate_expression(expr, typed, source_id)?;
        }
        Expr::Null
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::String(_)
        | Expr::Bytes(_)
        | Expr::FunctionRef(_, _)
        | Expr::ModuleFunctionRef(_, _)
        | Expr::UnresolvedFunctionRef { .. }
        | Expr::Var(_)
        | Expr::MoveVar(_)
        | Expr::MoveField { .. }
        | Expr::MoveIndex { .. } => {}
    }
    Ok(())
}

fn call_site<'a, 'b>(
    typed: &'a FrontendTypedIr<'b>,
    id: vm::compiler::ir::SemanticNodeId,
) -> Option<&'a ParsedCallSite> {
    typed
        .ir
        .parsed_semantic_index
        .as_ref()?
        .call_sites
        .iter()
        .find(|site| site.id == id)
}

fn numeric_abi_error(
    typed: &FrontendTypedIr<'_>,
    source_id: u32,
    semantic_id: Option<vm::compiler::ir::SemanticNodeId>,
    name: &str,
    parameter_index: usize,
    expected: &TypeSchema,
) -> CompileError {
    let site = semantic_id
        .and_then(|id| call_site(typed, id))
        .filter(|site| site.expr_span.source_id == source_id);
    let span = site.map(|site| site.expr_span);
    let line = span.and_then(|span| {
        typed
            .source_map?
            .line_col_for_offset(span.source_id, span.lo)
            .and_then(|(line, _)| u32::try_from(line).ok())
    });
    CompileError::StrictTypingRequired {
        line,
        source_name: typed
            .source_map
            .and_then(|map| map.file_name(source_id))
            .map(str::to_owned),
        detail: format!(
            "TypeScript numeric ABI: argument {} of host function '{name}' has no concrete proof for {expected:?}",
            parameter_index + 1
        ),
        span,
    }
}

fn expression_schema(expression: &Expr, typed: &FrontendTypedIr<'_>) -> Option<TypeSchema> {
    match expression {
        Expr::Null => Some(TypeSchema::Null),
        Expr::Int(_) => Some(TypeSchema::Int),
        Expr::Float(_) => Some(TypeSchema::Float),
        Expr::Bool(_) => Some(TypeSchema::Bool),
        Expr::String(_) => Some(TypeSchema::String),
        Expr::Bytes(_) => Some(TypeSchema::Bytes),
        Expr::Var(slot) | Expr::MoveVar(slot) => typed
            .local_schemas
            .get(usize::from(*slot))
            .and_then(Clone::clone),
        Expr::Call(_, _, _, Some(resolution), _) => Some(resolution.return_type.clone()),
        Expr::Call(index, _, _, _, _) | Expr::FunctionRef(index, _) => typed
            .ir
            .functions
            .iter()
            .find(|function| function.index == *index)
            .map(|function| {
                function
                    .return_schema
                    .clone()
                    .unwrap_or(TypeSchema::Unknown)
            }),
        Expr::ModuleCall(symbol, _, _, _) => typed
            .ir
            .functions
            .iter()
            .find(|function| function.symbol.as_ref() == Some(symbol))
            .map(|function| {
                function
                    .return_schema
                    .clone()
                    .unwrap_or(TypeSchema::Unknown)
            }),
        Expr::ModuleFunctionRef(symbol, _) => typed
            .ir
            .functions
            .iter()
            .find(|function| function.symbol.as_ref() == Some(symbol))
            .map(callable_schema),
        Expr::LocalCall(slot, _, _, _) => typed
            .local_schemas
            .get(usize::from(*slot))
            .and_then(Clone::clone)
            .and_then(|schema| match schema {
                TypeSchema::Callable { result, .. } => Some(*result),
                _ => None,
            }),
        Expr::Neg(value) | Expr::ToOwned(value) | Expr::Borrow(value) | Expr::BorrowMut(value) => {
            expression_schema(value, typed)
        }
        Expr::Add(left, right)
        | Expr::Sub(left, right)
        | Expr::Mul(left, right)
        | Expr::Div(left, right)
        | Expr::Mod(left, right) => {
            let left = expression_schema(left, typed);
            let right = expression_schema(right, typed);
            Some(match (left.as_ref(), right.as_ref()) {
                (Some(TypeSchema::Number), _) | (_, Some(TypeSchema::Number)) => TypeSchema::Number,
                (Some(TypeSchema::Float), _) | (_, Some(TypeSchema::Float)) => TypeSchema::Float,
                (Some(TypeSchema::Int), Some(TypeSchema::Int)) => TypeSchema::Int,
                _ => return None,
            })
        }
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => {
            let then_schema = expression_schema(then_expr, typed)?;
            let else_schema = expression_schema(else_expr, typed)?;
            (then_schema == else_schema).then_some(then_schema)
        }
        Expr::Block { expr, .. } => expression_schema(expr, typed),
        Expr::OptionUnwrapOr { value, .. } => match expression_schema(value, typed)? {
            TypeSchema::Optional(inner) => Some(*inner),
            schema => Some(schema),
        },
        Expr::Closure(closure) => Some(closure_schema(closure, typed)?),
        Expr::ClosureCall(closure, _) => expression_schema(&closure.body, typed),
        _ => None,
    }
}

fn callable_schema(function: &FunctionDecl) -> TypeSchema {
    TypeSchema::Callable {
        params: function
            .arg_schemas
            .iter()
            .map(|schema| schema.clone().unwrap_or(TypeSchema::Unknown))
            .collect(),
        result: Box::new(
            function
                .return_schema
                .clone()
                .unwrap_or(TypeSchema::Unknown),
        ),
    }
}

fn closure_schema(closure: &ClosureExpr, typed: &FrontendTypedIr<'_>) -> Option<TypeSchema> {
    let params = closure
        .param_slots
        .iter()
        .map(|slot| {
            typed
                .local_schemas
                .get(usize::from(*slot))
                .and_then(Clone::clone)
        })
        .collect::<Option<Vec<_>>>()?;
    let result = expression_schema(&closure.body, typed)?;
    Some(TypeSchema::Callable {
        params,
        result: Box::new(result),
    })
}

fn schema_requires_numeric_narrowing(
    actual: &TypeSchema,
    expected: &TypeSchema,
    typed: &FrontendTypedIr<'_>,
    depth: usize,
) -> bool {
    if depth >= 48 {
        return true;
    }
    match (actual, expected) {
        (TypeSchema::Number, TypeSchema::Int | TypeSchema::Float) => true,
        (TypeSchema::Optional(actual), TypeSchema::Optional(expected)) => {
            schema_requires_numeric_narrowing(actual, expected, typed, depth + 1)
        }
        (actual, TypeSchema::Optional(expected)) => {
            schema_requires_numeric_narrowing(actual, expected, typed, depth + 1)
        }
        (TypeSchema::Array(actual), TypeSchema::Array(expected))
        | (TypeSchema::Map(actual), TypeSchema::Map(expected)) => {
            schema_requires_numeric_narrowing(actual, expected, typed, depth + 1)
        }
        (TypeSchema::ArrayTuple(actual), TypeSchema::ArrayTuple(expected)) => {
            actual.len() == expected.len()
                && actual.iter().zip(expected).any(|(actual, expected)| {
                    schema_requires_numeric_narrowing(actual, expected, typed, depth + 1)
                })
        }
        (
            TypeSchema::ArrayTupleRest {
                prefix: actual_prefix,
                rest: actual_rest,
            },
            TypeSchema::ArrayTupleRest {
                prefix: expected_prefix,
                rest: expected_rest,
            },
        ) => {
            actual_prefix.len() == expected_prefix.len()
                && (actual_prefix
                    .iter()
                    .zip(expected_prefix)
                    .any(|(actual, expected)| {
                        schema_requires_numeric_narrowing(actual, expected, typed, depth + 1)
                    })
                    || schema_requires_numeric_narrowing(
                        actual_rest,
                        expected_rest,
                        typed,
                        depth + 1,
                    ))
        }
        (TypeSchema::Object(actual), TypeSchema::Object(expected)) => {
            expected.iter().any(|(name, expected)| {
                actual.get(name).is_some_and(|actual| {
                    schema_requires_numeric_narrowing(actual, expected, typed, depth + 1)
                })
            })
        }
        (
            TypeSchema::Callable {
                params: actual_params,
                result: actual_result,
            },
            TypeSchema::Callable {
                params: expected_params,
                result: expected_result,
            },
        ) => {
            actual_params.len() != expected_params.len()
                || actual_params
                    .iter()
                    .zip(expected_params)
                    .any(|(actual, expected)| {
                        schema_requires_numeric_narrowing(expected, actual, typed, depth + 1)
                    })
                || schema_requires_numeric_narrowing(
                    actual_result,
                    expected_result,
                    typed,
                    depth + 1,
                )
        }
        (TypeSchema::Named(_, _), _) | (_, TypeSchema::Named(_, _)) => {
            let actual = expand_named(actual, typed);
            let expected = expand_named(expected, typed);
            match (actual, expected) {
                (Some(actual), Some(expected)) => {
                    schema_requires_numeric_narrowing(&actual, &expected, typed, depth + 1)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn expand_named(schema: &TypeSchema, typed: &FrontendTypedIr<'_>) -> Option<TypeSchema> {
    let TypeSchema::Named(name, arguments) = schema else {
        return Some(schema.clone());
    };
    if let Some(declaration) = typed.ir.struct_schemas.get(name) {
        if declaration.type_params.len() != arguments.len() {
            return None;
        }
        let bindings = declaration
            .type_params
            .iter()
            .cloned()
            .zip(arguments.iter().cloned())
            .collect::<std::collections::HashMap<_, _>>();
        return Some(substitute(&declaration.body_schema, &bindings));
    }
    if !arguments.is_empty() {
        return None;
    }
    let declaration = typed.host_api_catalog?.struct_named(name)?;
    Some(TypeSchema::Object(
        declaration
            .fields
            .iter()
            .map(|field| (field.name.clone(), host_type_schema(&field.ty)))
            .collect(),
    ))
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
        vm::HostTypeSchema::Array(inner) => TypeSchema::Array(Box::new(host_type_schema(inner))),
        vm::HostTypeSchema::Map(inner) => TypeSchema::Map(Box::new(host_type_schema(inner))),
        vm::HostTypeSchema::Optional(inner) => {
            TypeSchema::Optional(Box::new(host_type_schema(inner)))
        }
        vm::HostTypeSchema::Callable { params, result } => TypeSchema::Callable {
            params: params.iter().map(host_type_schema).collect(),
            result: Box::new(host_type_schema(result)),
        },
        vm::HostTypeSchema::Resource(key) => TypeSchema::Resource(key.clone()),
        vm::HostTypeSchema::Named { fields, .. } => TypeSchema::Object(
            fields
                .iter()
                .map(|field| (field.name.clone(), host_type_schema(&field.ty)))
                .collect(),
        ),
    }
}

fn substitute(
    schema: &TypeSchema,
    bindings: &std::collections::HashMap<String, TypeSchema>,
) -> TypeSchema {
    match schema {
        TypeSchema::GenericParam(name) => bindings
            .get(name)
            .cloned()
            .unwrap_or_else(|| schema.clone()),
        TypeSchema::Optional(inner) => TypeSchema::Optional(Box::new(substitute(inner, bindings))),
        TypeSchema::Named(name, arguments) => TypeSchema::Named(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute(argument, bindings))
                .collect(),
        ),
        TypeSchema::Array(inner) => TypeSchema::Array(Box::new(substitute(inner, bindings))),
        TypeSchema::ArrayTuple(items) => TypeSchema::ArrayTuple(
            items
                .iter()
                .map(|item| substitute(item, bindings))
                .collect(),
        ),
        TypeSchema::ArrayTupleRest { prefix, rest } => TypeSchema::ArrayTupleRest {
            prefix: prefix
                .iter()
                .map(|item| substitute(item, bindings))
                .collect(),
            rest: Box::new(substitute(rest, bindings)),
        },
        TypeSchema::Map(inner) => TypeSchema::Map(Box::new(substitute(inner, bindings))),
        TypeSchema::Object(fields) => TypeSchema::Object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), substitute(field, bindings)))
                .collect(),
        ),
        TypeSchema::Callable { params, result } => TypeSchema::Callable {
            params: params
                .iter()
                .map(|param| substitute(param, bindings))
                .collect(),
            result: Box::new(substitute(result, bindings)),
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
