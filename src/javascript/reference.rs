//! Compiler Reference lowering for the closed, statically resolved subset.
//!
//! Gets and puts go through this module so declaration presence is distinct
//! from the current value. Unresolved ordinary reads stay compile refusals.
//! Outer bindings are a labeled P4 refusal, not copied locals. `typeof` of an
//! unresolved identifier does not perform GetValue.

use swc_common::Spanned;
use swc_ecma_ast as ast;
use vm::{BuiltinFunction, Expr, ParseError, Stmt, TypeSchema};

use super::heap as layout;
use super::lowerer::{Kind, Lowered, Lowerer};
use super::representation as value;
use super::scope::{BindingId, ScopeId};

#[derive(Clone, Copy)]
pub(super) struct TypeNames {
    pub undefined: u16,
    pub object: u16,
    pub boolean: u16,
    pub number: u16,
    pub string: u16,
    pub function: u16,
}

pub(super) enum NameHit {
    Local(BindingId),
    Outer(#[allow(dead_code)] BindingId),
    BuiltinUndefined,
    Missing,
}

pub(super) fn resolve(lower: &Lowerer<'_>, name: &str) -> NameHit {
    let mut scope = lower.active_scope;
    let mut crossed = false;
    loop {
        if let Some(id) = lower.graph.scope(scope).by_name.get(name).copied() {
            return if crossed {
                NameHit::Outer(id)
            } else {
                NameHit::Local(id)
            };
        }
        let Some(parent) = lower.graph.scope(scope).parent else {
            break;
        };
        crossed = true;
        scope = parent;
    }
    if name == "undefined" {
        NameHit::BuiltinUndefined
    } else {
        NameHit::Missing
    }
}

pub(super) fn lower_get(lower: &Lowerer<'_>, ident: &ast::Ident) -> Result<Lowered, ParseError> {
    match resolve(lower, ident.sym.as_str()) {
        NameHit::Local(id) => {
            let slot = *lower.binding_slots.get(&id).ok_or_else(|| {
                lower.error(ident.span, "P3 binding was collected but not instantiated")
            })?;
            let kind = lower
                .binding_kinds
                .get(&id)
                .copied()
                .unwrap_or(Kind::Undefined);
            let array_length = if kind == Kind::Array {
                lower.array_length_ids.get(&id).copied()
            } else {
                None
            };
            Ok(Lowered {
                expr: Expr::Var(slot),
                kind,
                array_length,
                owned_roots: Vec::new(),
            })
        }
        NameHit::BuiltinUndefined => Ok(Lowered {
            expr: value::undefined(),
            kind: Kind::Undefined,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::Outer(_) => Err(lower.error(
            ident.span,
            &format!(
                "P4 free-variable capture of '{}' is outside this closed subset",
                ident.sym
            ),
        )),
        NameHit::Missing => Err(lower.error(ident.span, &format!("unknown local '{}'", ident.sym))),
    }
}

pub(super) fn lower_assign_expr(
    lower: &mut Lowerer<'_>,
    assign: &ast::AssignExpr,
) -> Result<Lowered, ParseError> {
    if assign.op != ast::AssignOp::Assign {
        return Err(lower.error(
            assign.span,
            "P3 admits only simple = assignment; compound and update forms are outside this scope",
        ));
    }
    let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(binding)) = &assign.left else {
        return Err(lower.error(
            assign.span,
            "unsupported JavaScript ES5 P0 primitive expression",
        ));
    };
    let rhs = lower.expr(&assign.right)?;
    let mut stmts = Vec::new();
    let (expr, kind) = store_local(lower, binding.sym.as_str(), binding.span, rhs, &mut stmts)?;
    let array_length = if kind == Kind::Array {
        lower.array_lengths.get(binding.sym.as_str()).copied()
    } else {
        None
    };
    Ok(Lowered {
        expr: Expr::Block {
            stmts,
            expr: Box::new(expr),
        },
        kind,
        array_length,
        owned_roots: Vec::new(),
    })
}

pub(super) fn lower_typeof(
    lower: &mut Lowerer<'_>,
    unary: &ast::UnaryExpr,
) -> Result<Lowered, ParseError> {
    if lower.root_scope.is_none() {
        return Err(lower.error(
            unary.span,
            "P3 typeof requires the realm heap for guest string payloads",
        ));
    }
    if let ast::Expr::Ident(ident) = unwrap_parens(unary.arg.as_ref())
        && matches!(resolve(lower, ident.sym.as_str()), NameHit::Missing)
    {
        let mut stmts = Vec::new();
        let slot = emit_type_name(lower, "undefined", unary.span, &mut stmts)?;
        return Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(slot)),
            },
            kind: Kind::String,
            array_length: None,
            owned_roots: vec![slot],
        });
    }
    let operand = lower.expr(&unary.arg)?;
    let mut stmts = Vec::new();
    let mut operand_roots = operand.owned_roots;
    let (value_slot, mut bound) = lower.bind_evaluation(operand.expr, operand.kind, unary.span)?;
    stmts.append(&mut bound);
    ensure_type_names(lower, unary.span, &mut stmts)?;
    let names = lower
        .type_names
        .as_ref()
        .ok_or_else(|| lower.error(unary.span, "P3 typeof type-name cache was not installed"))?;
    let undefined_name = names.undefined;
    let object_name = names.object;
    let boolean_name = names.boolean;
    let number_name = names.number;
    let string_name = names.string;
    let function_name = names.function;
    let line = lower.line(unary.span);
    let result = lower.slot(unary.span)?;
    stmts.push(Stmt::Let {
        index: result,
        declared_schema: Some(value::schema()),
        expr: Expr::Var(undefined_name),
        line,
    });
    lower.refresh_root(result, unary.span, &mut stmts)?;
    let tag = lower.slot(unary.span)?;
    stmts.push(Stmt::Let {
        index: tag,
        declared_schema: Some(TypeSchema::Int),
        expr: value::field(value_slot, "tag"),
        line,
    });
    let base = lower.slot(unary.span)?;
    let kind_num = lower.slot(unary.span)?;
    stmts.push(Stmt::Let {
        index: base,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    stmts.push(Stmt::Let {
        index: kind_num,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    let carrier = lower.active_carrier(unary.span)?;
    let heap = lower.heap_ref(unary.span)?;
    let (helper_read, checked_base) = (heap.helper_read, heap.helpers.checked_base);
    let decode = lower
        .helper_decode
        .ok_or_else(|| lower.error(unary.span, "P3 typeof requires the realm decode helper"))?;
    let assign_name = |name: u16| Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: result,
        expr: Expr::Var(name),
        line,
    };
    let object_branch = vec![
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: base,
            expr: Lowerer::guest_call(
                checked_base,
                vec![
                    Expr::Var(carrier),
                    value::field(value_slot, "ref_slot"),
                    value::field(value_slot, "generation"),
                ],
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: kind_num,
            expr: Lowerer::guest_call(
                decode,
                vec![Lowerer::guest_call(
                    helper_read,
                    vec![Expr::Var(carrier), Lowerer::as_number(Expr::Var(base))],
                )],
            ),
            line,
        },
        Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(Expr::Var(kind_num)),
                Box::new(Expr::Int(layout::KIND_FUNCTION)),
            ),
            then_branch: vec![assign_name(function_name)],
            else_branch: vec![assign_name(object_name)],
            line,
        },
    ];
    stmts.push(Stmt::IfElse {
        condition: Expr::Eq(Box::new(Expr::Var(tag)), Box::new(Expr::Int(value::OBJECT))),
        then_branch: object_branch,
        else_branch: vec![Stmt::IfElse {
            condition: Expr::Eq(Box::new(Expr::Var(tag)), Box::new(Expr::Int(value::NULL))),
            then_branch: vec![assign_name(object_name)],
            else_branch: vec![Stmt::IfElse {
                condition: Expr::Eq(
                    Box::new(Expr::Var(tag)),
                    Box::new(Expr::Int(value::BOOLEAN)),
                ),
                then_branch: vec![assign_name(boolean_name)],
                else_branch: vec![Stmt::IfElse {
                    condition: Expr::Eq(
                        Box::new(Expr::Var(tag)),
                        Box::new(Expr::Int(value::NUMBER)),
                    ),
                    then_branch: vec![assign_name(number_name)],
                    else_branch: vec![Stmt::IfElse {
                        condition: Expr::Eq(
                            Box::new(Expr::Var(tag)),
                            Box::new(Expr::Int(value::STRING)),
                        ),
                        then_branch: vec![assign_name(string_name)],
                        else_branch: vec![Stmt::IfElse {
                            condition: Expr::Eq(
                                Box::new(Expr::Var(tag)),
                                Box::new(Expr::Int(value::UNDEFINED)),
                            ),
                            then_branch: vec![assign_name(undefined_name)],
                            else_branch: Vec::new(),
                            line,
                        }],
                        line,
                    }],
                    line,
                }],
                line,
            }],
            line,
        }],
        line,
    });
    lower.refresh_root(result, unary.span, &mut stmts)?;
    if lower.has_root(value_slot) {
        operand_roots.push(value_slot);
    }
    release_consumed_roots(lower, &operand_roots, result, unary.span, &mut stmts)?;
    Ok(Lowered {
        expr: Expr::Block {
            stmts,
            expr: Box::new(Expr::Var(result)),
        },
        kind: Kind::String,
        array_length: None,
        owned_roots: vec![result],
    })
}

pub(super) fn lower_script_statements(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let scope = lower.graph.script;
    lower.active_scope = scope;
    validate_statements(lower, statements)?;
    instantiate(lower, scope, out)?;
    lower_stmt_list(lower, statements, out, false)
}

pub(super) fn lower_function_body(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<Option<Expr>, ParseError> {
    validate_statements(lower, statements)?;
    instantiate(lower, lower.active_scope, out)?;
    let mut completion = None;
    let mut stopped = false;
    let mut in_prologue = true;
    for statement in statements {
        if in_prologue
            && let ast::Stmt::Expr(expr) = statement
            && matches!(expr.expr.as_ref(), ast::Expr::Lit(ast::Lit::Str(_)))
        {
            continue;
        }
        in_prologue = false;
        if let ast::Stmt::Return(ret) = statement {
            if completion.is_some() || stopped {
                return Err(lower.error(
                    ret.span,
                    "P0 source-function bodies admit only a single Return",
                ));
            }
            completion = Some(lower_return(lower, ret, out)?);
            stopped = true;
            continue;
        }
        if stopped {
            if !skippable_after_return(statement) {
                return Err(lower.error(
                    statement.span(),
                    "P0 source-function bodies do not admit this statement in the closed declaration subset",
                ));
            }
            continue;
        }
        lower_one(lower, statement, out, true)?;
    }
    Ok(completion)
}

fn instantiate(
    lower: &mut Lowerer<'_>,
    scope: ScopeId,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let order = lower.graph.scope(scope).order.clone();
    for id in &order {
        predeclare(lower, *id, scope, out)?;
    }
    for (id, ordinal, span) in lower.graph.scope(scope).formals.clone() {
        let args = lower.function_args.ok_or_else(|| {
            lower.error(
                span,
                "P4 formal initialization requires the invocation argument vector",
            )
        })?;
        // The ordinal is a compiler-owned nonnegative constant. Get occurs
        // only in the in-bounds branch; missing actuals are genuine Undefined.
        // Borrow prevents Core's consuming element-read optimization from
        // detaching a supplied box from the retained invocation vector.
        let argument = Expr::IfElse {
            condition: Box::new(Expr::Lt(
                Box::new(Expr::Int(ordinal as i64)),
                Box::new(Lowerer::builtin_call(
                    BuiltinFunction::Len.call_index(),
                    vec![Expr::Var(args)],
                )),
            )),
            then_expr: Box::new(Lowerer::builtin_call(
                BuiltinFunction::Get.call_index(),
                vec![
                    Expr::Borrow(Box::new(Expr::Var(args))),
                    Expr::Int(ordinal as i64),
                ],
            )),
            else_expr: Box::new(value::undefined()),
        };
        let name = lower.graph.binding(id).name.clone();
        store_local(
            lower,
            &name,
            span,
            Lowered {
                expr: argument,
                kind: Kind::Dynamic,
                array_length: None,
                owned_roots: Vec::new(),
            },
            out,
        )?;
    }
    for id in order {
        install_functions(lower, id, out)?;
    }
    Ok(())
}

fn predeclare(
    lower: &mut Lowerer<'_>,
    id: BindingId,
    scope: ScopeId,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let name = lower.graph.binding(id).name.clone();
    let span = lower.graph.binding(id).first_span;
    if scope == lower.graph.script && name == "undefined" {
        return Err(lower.error(
            span,
            "global name 'undefined' remains outside supported var redeclaration until global-object descriptors exist",
        ));
    }
    let slot = lower.slot(span)?;
    let line = lower.line(span);
    out.push(Stmt::Let {
        index: slot,
        declared_schema: Some(value::schema()),
        expr: value::undefined(),
        line,
    });
    lower.binding_slots.insert(id, slot);
    lower.binding_kinds.insert(id, Kind::Undefined);
    lower.bindings.insert(name.clone(), (slot, Kind::Undefined));
    if scope == lower.graph.script {
        lower.source_bindings.push((name, slot));
    }
    Ok(())
}

fn install_functions(
    lower: &mut Lowerer<'_>,
    id: BindingId,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let sites = lower.graph.binding(id).fn_sites.clone();
    if sites.is_empty() {
        return Ok(());
    }
    if lower.heap.is_none() {
        return Err(lower.error(
            sites[0].span,
            "P3 function declarations require the realm heap",
        ));
    }
    let name = lower.graph.binding(id).name.clone();
    for site in sites {
        let synthetic = ast::FnExpr {
            ident: None,
            function: site.function,
        };
        let lowered = lower.lower_function_expr(&synthetic)?;
        let _ = store_local(lower, &name, site.span, lowered, out)?;
    }
    Ok(())
}

fn lower_stmt_list(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
    in_function: bool,
) -> Result<(), ParseError> {
    for statement in statements {
        if let ast::Stmt::Expr(expr) = statement
            && let ast::Expr::Lit(ast::Lit::Str(literal)) = expr.expr.as_ref()
            && lower.is_exact_use_strict_lexeme(literal.span)
            && !in_function
        {
            continue;
        }
        lower_one(lower, statement, out, in_function)?;
    }
    Ok(())
}

fn lower_one(
    lower: &mut Lowerer<'_>,
    statement: &ast::Stmt,
    out: &mut Vec<Stmt>,
    in_function: bool,
) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => lower_var(lower, declaration, out),
        ast::Stmt::Decl(ast::Decl::Fn(_)) => Ok(()),
        ast::Stmt::Block(block) => lower_stmt_list(lower, &block.stmts, out, in_function),
        ast::Stmt::If(if_stmt) => lower_declaration_if(lower, if_stmt, out, in_function),
        ast::Stmt::Expr(expr) => lower_expr_stmt(lower, expr, out),
        ast::Stmt::Empty(_) => Ok(()),
        ast::Stmt::Return(_) if in_function => Err(lower.error(
            statement.span(),
            "P0 source-function bodies admit only a single Return",
        )),
        _ => Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement")),
    }
}

fn lower_var(
    lower: &mut Lowerer<'_>,
    declaration: &ast::VarDecl,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    if declaration.kind != ast::VarDeclKind::Var {
        return Err(lower.error(
            declaration.span,
            "JavaScript ES5 supports var declarations; let and const are rejected",
        ));
    }
    for declarator in &declaration.decls {
        let ast::Pat::Ident(binding) = &declarator.name else {
            return Err(lower.error(declarator.span, "P0 requires an identifier binding"));
        };
        let Some(initializer) = declarator.init.as_deref() else {
            continue;
        };
        if let ast::Expr::Array(literal) = initializer {
            super::lowerer::lower_array_binding(
                lower,
                binding.id.sym.as_str(),
                literal,
                declarator.span,
                out,
                false,
            )?;
            continue;
        }
        let initial = lower.expr(initializer)?;
        let _ = store_local(
            lower,
            binding.id.sym.as_str(),
            declarator.span,
            initial,
            out,
        )?;
    }
    Ok(())
}

fn lower_declaration_if(
    lower: &mut Lowerer<'_>,
    if_stmt: &ast::IfStmt,
    out: &mut Vec<Stmt>,
    in_function: bool,
) -> Result<(), ParseError> {
    if !is_declaration_only_if(if_stmt) {
        return Err(lower.error(
            if_stmt.span,
            "P3 declaration-only if requires a literal boolean condition and declaration-only branches; general if is outside this scope",
        ));
    }
    let selected = match literal_bool(&if_stmt.test) {
        Some(true) => Some(if_stmt.cons.as_ref()),
        Some(false) => if_stmt.alt.as_deref(),
        None => None,
    };
    if let Some(arm) = selected {
        lower_one(lower, arm, out, in_function)?;
    }
    Ok(())
}

fn lower_expr_stmt(
    lower: &mut Lowerer<'_>,
    expr: &ast::ExprStmt,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    if let ast::Expr::Assign(assign) = expr.expr.as_ref()
        && assign.op == ast::AssignOp::Assign
        && let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) = &assign.left
    {
        super::lowerer::lower_member_write_stmt(lower, member, &assign.right, expr.span, out)?;
        return Ok(());
    }
    let lowered = lower.expr(&expr.expr)?;
    if lower.root_scope.is_some() {
        let (slot, mut stmts) = lower.bind_evaluation(lowered.expr, lowered.kind, expr.span)?;
        out.append(&mut stmts);
        out.push(Stmt::Expr {
            expr: Expr::Var(slot),
            line: lower.line(expr.span),
        });
        // The observer/result root survives; only its consumed operands are
        // released. Primitive outputs need no logical entry.
        release_consumed_roots(lower, &lowered.owned_roots, slot, expr.span, out)?;
    } else {
        let (slot, mut stmts) = lower.bind_temp(lowered.expr, expr.span)?;
        out.append(&mut stmts);
        out.push(Stmt::Expr {
            expr: Expr::Var(slot),
            line: lower.line(expr.span),
        });
    }
    Ok(())
}

fn lower_return(
    lower: &mut Lowerer<'_>,
    ret: &ast::ReturnStmt,
    out: &mut Vec<Stmt>,
) -> Result<Expr, ParseError> {
    match ret.arg.as_deref() {
        Some(arg) => {
            let value = lower.expr(arg)?;
            let (slot, mut stmts) = lower.bind_temp(value.expr, ret.span)?;
            out.append(&mut stmts);
            Ok(value::completion_return(Expr::Var(slot)))
        }
        None => Ok(value::completion_return(value::undefined())),
    }
}

fn store_local(
    lower: &mut Lowerer<'_>,
    name: &str,
    span: swc_common::Span,
    lowered: Lowered,
    out: &mut Vec<Stmt>,
) -> Result<(Expr, Kind), ParseError> {
    let id = match resolve(lower, name) {
        NameHit::Local(id) => id,
        NameHit::Outer(_) => {
            return Err(lower.error(
                span,
                &format!("P4 free-variable capture of '{name}' is outside this closed subset"),
            ));
        }
        NameHit::Missing | NameHit::BuiltinUndefined => {
            return Err(lower.error(span, &format!("unknown local '{name}'")));
        }
    };
    let slot = *lower
        .binding_slots
        .get(&id)
        .ok_or_else(|| lower.error(span, "P3 binding was collected but not instantiated"))?;
    let kind = lowered.kind;
    let array_len = lowered.array_length.filter(|_| kind == Kind::Array);
    let owned_roots = lowered.owned_roots;
    let line = lower.line(span);
    match lowered.expr {
        Expr::Block { mut stmts, expr } => {
            out.append(&mut stmts);
            out.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: slot,
                expr: *expr,
                line,
            });
        }
        Expr::Var(src) if src == slot => {}
        other => {
            out.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: slot,
                expr: other,
                line,
            });
        }
    }
    lower.refresh_root(slot, span, out)?;
    release_consumed_roots(lower, &owned_roots, slot, span, out)?;
    lower.binding_kinds.insert(id, kind);
    lower.bindings.insert(name.to_owned(), (slot, kind));
    if let Some(length) = array_len {
        lower.array_lengths.insert(name.to_owned(), length);
        lower.array_length_ids.insert(id, length);
    } else {
        lower.array_lengths.remove(name);
        lower.array_length_ids.remove(&id);
    }
    Ok((Expr::Var(slot), kind))
}

pub(super) fn release_consumed_roots(
    lower: &mut Lowerer<'_>,
    owned: &[u16],
    destination: u16,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    for slot in Lowerer::compose_owned(owned.iter().copied()) {
        if slot == destination || persistent_root(lower, slot) || !lower.has_root(slot) {
            continue;
        }
        clear_temp(lower, slot, span, out)?;
    }
    Ok(())
}

fn persistent_root(lower: &Lowerer<'_>, slot: u16) -> bool {
    if lower.binding_slots.values().any(|bound| *bound == slot) {
        return true;
    }
    let Some(names) = &lower.type_names else {
        return false;
    };
    slot == names.undefined
        || slot == names.object
        || slot == names.boolean
        || slot == names.number
        || slot == names.string
        || slot == names.function
}

fn unwrap_parens(expr: &ast::Expr) -> &ast::Expr {
    let mut current = expr;
    while let ast::Expr::Paren(paren) = current {
        current = &paren.expr;
    }
    current
}

pub(super) fn clear_temp(
    lower: &mut Lowerer<'_>,
    slot: u16,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    lower.clear_root(slot, span, out)
}

fn ensure_type_names(
    lower: &mut Lowerer<'_>,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    if lower.type_names.is_some() {
        return Ok(());
    }
    let undefined = emit_type_name(lower, "undefined", span, out)?;
    let object = emit_type_name(lower, "object", span, out)?;
    let boolean = emit_type_name(lower, "boolean", span, out)?;
    let number = emit_type_name(lower, "number", span, out)?;
    let string = emit_type_name(lower, "string", span, out)?;
    let function = emit_type_name(lower, "function", span, out)?;
    lower.type_names = Some(TypeNames {
        undefined,
        object,
        boolean,
        number,
        string,
        function,
    });
    Ok(())
}

fn emit_type_name(
    lower: &mut Lowerer<'_>,
    text: &str,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let units: Vec<u16> = text.encode_utf16().collect();
    lower.emit_utf16_node(&units, span, out)
}

fn skippable_after_return(statement: &ast::Stmt) -> bool {
    matches!(
        statement,
        ast::Stmt::Empty(_)
            | ast::Stmt::Expr(_)
            | ast::Stmt::Decl(_)
            | ast::Stmt::Block(_)
            | ast::Stmt::If(_)
    )
}

pub(super) fn is_declaration_only_if(if_stmt: &ast::IfStmt) -> bool {
    literal_bool(&if_stmt.test).is_some()
        && is_declaration_only(&if_stmt.cons)
        && if_stmt
            .alt
            .as_ref()
            .is_none_or(|alt| is_declaration_only(alt))
}

fn is_declaration_only(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::Empty(_) | ast::Stmt::Decl(ast::Decl::Var(_)) => true,
        ast::Stmt::Block(block) => block.stmts.iter().all(is_declaration_only),
        ast::Stmt::If(if_stmt) => is_declaration_only_if(if_stmt),
        _ => false,
    }
}

fn literal_bool(expression: &ast::Expr) -> Option<bool> {
    match expression {
        ast::Expr::Lit(ast::Lit::Bool(value)) => Some(value.value),
        ast::Expr::Paren(paren) => literal_bool(&paren.expr),
        _ => None,
    }
}

// Pure profile facts are keyed by declaration identity, never physical slots.
// Each scope starts from the same hoisted Undefined/function state as emission.
#[derive(Clone, Copy)]
struct ProfileFact {
    kind: Kind,
    array_length: Option<usize>,
}

impl ProfileFact {
    fn scalar(kind: Kind) -> Self {
        Self {
            kind,
            array_length: None,
        }
    }
}

type ProfileFacts = std::collections::HashMap<BindingId, ProfileFact>;

fn validate_statements(lower: &Lowerer<'_>, statements: &[ast::Stmt]) -> Result<(), ParseError> {
    let mut facts = lower
        .graph
        .scope(lower.active_scope)
        .order
        .iter()
        .map(|id| {
            let kind = if !lower.graph.binding(*id).fn_sites.is_empty() {
                Kind::Function
            } else if lower
                .graph
                .scope(lower.active_scope)
                .formals
                .iter()
                .any(|(formal, _, _)| formal == id)
            {
                Kind::Dynamic
            } else {
                Kind::Undefined
            };
            (*id, ProfileFact::scalar(kind))
        })
        .collect();
    validate_list(lower, statements, &mut facts)
}

fn validate_list(
    lower: &Lowerer<'_>,
    statements: &[ast::Stmt],
    facts: &mut ProfileFacts,
) -> Result<(), ParseError> {
    // Continue the pure source-order traversal after return. This validates
    // capability only and cannot emit an initializer or mutate Lowerer state.
    for statement in statements {
        validate_stmt(lower, statement, facts)?;
    }
    Ok(())
}

fn validate_stmt(
    lower: &Lowerer<'_>,
    statement: &ast::Stmt,
    facts: &mut ProfileFacts,
) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::Empty(_) | ast::Stmt::Decl(ast::Decl::Fn(_)) => Ok(()),
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => {
            if declaration.kind != ast::VarDeclKind::Var {
                return Err(lower.error(
                    declaration.span,
                    "JavaScript ES5 supports var declarations; let and const are rejected",
                ));
            }
            for declarator in &declaration.decls {
                let ast::Pat::Ident(binding) = &declarator.name else {
                    return Err(lower.error(declarator.span, "P0 requires an identifier binding"));
                };
                if let Some(init) = &declarator.init {
                    let fact = validate_expr(lower, init, facts)?;
                    if let NameHit::Local(id) = resolve(lower, binding.id.sym.as_str()) {
                        facts.insert(id, fact);
                    }
                }
            }
            Ok(())
        }
        ast::Stmt::Block(block) => validate_list(lower, &block.stmts, facts),
        ast::Stmt::Expr(expr) => {
            if let ast::Expr::Assign(assign) = expr.expr.as_ref()
                && assign.op == ast::AssignOp::Assign
                && let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) =
                    &assign.left
            {
                let base = validate_expr(lower, &member.obj, facts)?;
                admit_member_write(lower, member, base.kind, base.array_length)?;
                let rhs = validate_expr(lower, &assign.right, facts)?;
                return admit_numeric_element(lower, assign.right.span(), rhs.kind);
            }
            validate_expr(lower, &expr.expr, facts).map(|_| ())
        }
        ast::Stmt::Return(ret) => {
            if let Some(arg) = &ret.arg {
                validate_expr(lower, arg, facts)?;
            }
            Ok(())
        }
        ast::Stmt::If(if_stmt) => {
            validate_expr(lower, &if_stmt.test, facts)?;
            let mut consequent = facts.clone();
            let mut alternate = facts.clone();
            validate_stmt(lower, &if_stmt.cons, &mut consequent)?;
            if let Some(alt) = &if_stmt.alt {
                validate_stmt(lower, alt, &mut alternate)?;
            }
            if !is_declaration_only_if(if_stmt) {
                return Err(lower.error(
                    if_stmt.span,
                    "unsupported JavaScript ES5 P0 statement: general if is outside this scope",
                ));
            }
            *facts = if literal_bool(&if_stmt.test) == Some(true) {
                consequent
            } else {
                alternate
            };
            Ok(())
        }
        ast::Stmt::Throw(throw) => {
            Err(lower.error(throw.span, "unsupported JavaScript ES5 P0 throw statement"))
        }
        _ => Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement")),
    }
}

fn ident_fact(
    lower: &Lowerer<'_>,
    ident: &ast::Ident,
    facts: &ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    match resolve(lower, ident.sym.as_str()) {
        NameHit::Local(id) => facts
            .get(&id)
            .copied()
            .ok_or_else(|| lower.error(ident.span, "P3 profile binding fact missing")),
        NameHit::BuiltinUndefined => Ok(ProfileFact::scalar(Kind::Undefined)),
        NameHit::Outer(_) => Err(lower.error(
            ident.span,
            &format!(
                "P4 free-variable capture of '{}' is outside this closed subset",
                ident.sym
            ),
        )),
        NameHit::Missing => Err(lower.error(ident.span, &format!("unknown local '{}'", ident.sym))),
    }
}

fn validate_expr(
    lower: &Lowerer<'_>,
    expression: &ast::Expr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    let kind = match expression {
        ast::Expr::Lit(ast::Lit::Num(_)) => Kind::Number,
        ast::Expr::Lit(ast::Lit::Bool(_)) => Kind::Boolean,
        ast::Expr::Lit(ast::Lit::Null(_)) => Kind::Null,
        ast::Expr::Lit(ast::Lit::Str(_)) => Kind::String,
        ast::Expr::Paren(paren) => return validate_expr(lower, &paren.expr, facts),
        ast::Expr::Ident(ident) => return ident_fact(lower, ident, facts),
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::TypeOf => {
            if let ast::Expr::Ident(ident) = unwrap_parens(&unary.arg)
                && matches!(resolve(lower, ident.sym.as_str()), NameHit::Missing)
            {
                return Ok(ProfileFact::scalar(Kind::String));
            }
            validate_expr(lower, &unary.arg, facts)?;
            Kind::String
        }
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::Minus => {
            let operand = validate_expr(lower, &unary.arg, facts)?;
            admit_numeric_operand(lower, unary.span, operand.kind)?;
            Kind::Number
        }
        ast::Expr::Unary(unary) => {
            return Err(lower.error(unary.span, "unsupported JavaScript ES5 P0 unary expression"));
        }
        ast::Expr::Bin(binary) => {
            if !matches!(
                binary.op,
                ast::BinaryOp::Add
                    | ast::BinaryOp::Sub
                    | ast::BinaryOp::Mul
                    | ast::BinaryOp::Div
                    | ast::BinaryOp::EqEqEq
                    | ast::BinaryOp::NotEqEq
            ) {
                return Err(
                    lower.error(binary.span, "unsupported JavaScript ES5 P0 binary operator")
                );
            }
            let left = validate_expr(lower, &binary.left, facts)?;
            let right = validate_expr(lower, &binary.right, facts)?;
            if matches!(binary.op, ast::BinaryOp::EqEqEq | ast::BinaryOp::NotEqEq) {
                Kind::Boolean
            } else {
                admit_numeric_operand(lower, binary.span, left.kind)?;
                admit_numeric_operand(lower, binary.span, right.kind)?;
                Kind::Number
            }
        }
        ast::Expr::Assign(assign) => {
            if assign.op != ast::AssignOp::Assign {
                return Err(lower.error(assign.span, "P3 admits only simple = assignment; compound and update forms are outside this scope"));
            }
            match &assign.left {
                ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(binding)) => {
                    ident_fact(lower, &binding.id, facts)?;
                    let rhs = validate_expr(lower, &assign.right, facts)?;
                    if let NameHit::Local(id) = resolve(lower, binding.id.sym.as_str()) {
                        facts.insert(id, rhs);
                    }
                    return Ok(rhs);
                }
                _ => {
                    return Err(lower.error(
                        assign.span,
                        "unsupported JavaScript ES5 P0 primitive expression",
                    ));
                }
            }
        }
        ast::Expr::Array(literal) => {
            for element in &literal.elems {
                let Some(element) = element else {
                    return Err(lower.error(literal.span, "P0 numeric arrays reject holes"));
                };
                if element.spread.is_some() {
                    return Err(
                        lower.error(element.span(), "P0 numeric arrays reject spread elements")
                    );
                }
                let fact = validate_expr(lower, &element.expr, facts)?;
                admit_numeric_element(lower, element.span(), fact.kind)?;
            }
            return Ok(ProfileFact {
                kind: Kind::Array,
                array_length: Some(literal.elems.len()),
            });
        }
        ast::Expr::Object(literal) => {
            for (_, expression, span) in lower.object_literal_entries(literal)? {
                let value = validate_expr(lower, &expression, facts)?;
                admit_object_value(lower, span, value.kind)?;
            }
            Kind::Object
        }
        ast::Expr::Member(member) => {
            let base = validate_expr(lower, &member.obj, facts)?;
            return admit_member_read(lower, member, base.kind, base.array_length)
                .map(ProfileFact::scalar);
        }
        ast::Expr::Call(call) => {
            admit_call_callee(lower, call)?;
            let ast::Callee::Expr(callee) = &call.callee else {
                unreachable!()
            };
            match unwrap_parens(callee) {
                ast::Expr::Ident(ident) => {
                    let fact = ident_fact(lower, ident, facts)?;
                    admit_callable_kind(lower, ident.span, fact.kind)?;
                }
                ast::Expr::Member(member) => {
                    let base = validate_expr(lower, &member.obj, facts)?;
                    admit_object_receiver(lower, member.span, base.kind)?;
                    let callee_kind =
                        admit_member_read(lower, member, base.kind, base.array_length)?;
                    admit_callable_kind(lower, member.span, callee_kind)?;
                }
                _ => unreachable!(),
            }
            for arg in &call.args {
                validate_expr(lower, &arg.expr, facts)?;
            }
            // The current physical Completion ABI carries a dynamic JsValue;
            // match the live lowerer's broad object-like result classification.
            Kind::Object
        }
        ast::Expr::Fn(_) => Kind::Function,
        ast::Expr::This(this) => {
            if lower.graph.scope(lower.active_scope).kind != super::scope::ScopeKind::Function {
                return Err(lower.error(
                    this.span,
                    "P0 `this` is admitted only inside closed strict source functions",
                ));
            }
            Kind::Object
        }
        _ => {
            return Err(lower.error(
                expression.span(),
                "unsupported JavaScript ES5 P0 primitive expression",
            ));
        }
    };
    Ok(ProfileFact::scalar(kind))
}

/// Shared pure callee-shape and arity guard; no slot or root mutation.
pub(super) fn admit_call_callee(
    lower: &Lowerer<'_>,
    call: &ast::CallExpr,
) -> Result<(), ParseError> {
    if call.args.len() > super::scope::MAX_ARGUMENTS {
        return Err(lower.error(
            call.span,
            "P4 source calls exceed the 64 actual argument profile limit",
        ));
    }
    if call.type_args.is_some() {
        return Err(lower.error(call.span, "P0 source functions reject type arguments"));
    }
    for arg in &call.args {
        if arg.spread.is_some() {
            return Err(lower.error(arg.span(), "P0 source functions reject spread arguments"));
        }
    }
    let ast::Callee::Expr(callee) = &call.callee else {
        return Err(lower.error(
            call.span,
            "P0 calls admit only identifier and member callees",
        ));
    };
    match unwrap_parens(callee) {
        ast::Expr::Ident(_) | ast::Expr::Member(_) => Ok(()),
        _ => Err(lower.error(
            call.span,
            "P0 calls admit only identifier and member callees",
        )),
    }
}

pub(super) fn admit_callable_kind(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if !matches!(kind, Kind::Function | Kind::Object) {
        return Err(lower.error(span, "P0 bare calls require a function binding"));
    }
    Ok(())
}

pub(super) fn admit_object_receiver(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if !matches!(kind, Kind::Object | Kind::Function) {
        return Err(lower.error(
            span,
            "P0 plain-object member reads require an object receiver",
        ));
    }
    Ok(())
}

pub(super) fn admit_member_read(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    kind: Kind,
    length: Option<usize>,
) -> Result<Kind, ParseError> {
    if kind == Kind::Array
        && matches!(member.obj.as_ref(), ast::Expr::Ident(_))
        && let ast::MemberProp::Computed(property) = &member.prop
        && let Some(index) = Lowerer::member_index_literal(&property.expr)
    {
        let position = usize::try_from(index).unwrap_or(usize::MAX);
        return Ok(if length.is_some_and(|len| position >= len) {
            Kind::Undefined
        } else {
            Kind::Number
        });
    }
    admit_object_receiver(lower, member.span, kind)?;
    let key = lower.member_key_units(member)?;
    if kind == Kind::Function {
        if key != "length".encode_utf16().collect::<Vec<_>>() {
            return Err(lower.error(
                member.span,
                "P4 source functions admit only the intrinsic length member",
            ));
        }
        return Ok(Kind::Number);
    }
    Ok(Kind::Object)
}

pub(super) fn admit_member_write(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    kind: Kind,
    length: Option<usize>,
) -> Result<(), ParseError> {
    if matches!(kind, Kind::Object | Kind::Function)
        || !matches!(member.prop, ast::MemberProp::Computed(_))
    {
        return Err(lower.error(
            member.span,
            "P0 plain objects reject property assignment until P7 descriptors",
        ));
    }
    let ast::MemberProp::Computed(property) = &member.prop else {
        unreachable!()
    };
    let Some(index) = Lowerer::member_index_literal(&property.expr) else {
        return Err(lower.error(
            property.span(),
            "P0 numeric arrays admit only integer literal subscripts",
        ));
    };
    if kind != Kind::Array || !matches!(member.obj.as_ref(), ast::Expr::Ident(_)) {
        return Err(lower.error(
            member.span,
            "P0 indexed writes require a numeric-array binding",
        ));
    }
    let position = usize::try_from(index).unwrap_or(usize::MAX);
    if length.is_some_and(|len| position >= len) {
        return Err(lower.error(
            property.span(),
            "P0 numeric arrays reject length-changing writes",
        ));
    }
    Ok(())
}

pub(super) fn admit_numeric_element(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if kind != Kind::Number {
        return Err(lower.error(span, "P0 numeric arrays admit only numeric elements"));
    }
    Ok(())
}

pub(super) fn admit_object_value(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if kind == Kind::Array {
        return Err(lower.error(span, "P0 plain-object values admit primitives, strings, objects and functions; numeric arrays stay separate bindings"));
    }
    Ok(())
}

pub(super) fn admit_numeric_operand(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if kind != Kind::Number {
        return Err(lower.error(span, "P0 arithmetic currently requires numeric operands"));
    }
    Ok(())
}
