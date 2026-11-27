//! Private P6-C lowering for structured iteration and labelled abrupt
//! completion.
//!
//! Only an activation whose own (non-deferred) syntax contains a loop or a
//! label uses this path. Every other activation keeps the accepted P6-A/P6-B
//! statement lowering byte-for-byte, including root allocation and emission.
//!
//! Source `break`/`continue` publish a private [`value::COMPLETION_BREAK`]/
//! [`value::COMPLETION_CONTINUE`] Completion with a checked positive static
//! target and set the activation's Normal flag false. The matching loop or
//! label boundary consumes it only after any enclosing `finally` has run.
//! Core sees exactly the existing unlabelled `Stmt::While` plus a boundary
//! `Stmt::Break`; no Core IR, label runtime or public API is added.
//!
//! Normal statement values follow ES5.1 `StatementList` UpdateEmpty through a
//! pair of rooted per-nesting accumulator locals. Abrupt values are carried
//! by the real pending Completion. Loop V follows ES5.1 12.6.1/.2/.3: it
//! starts as empty (never the outer sequence value and never Undefined), is
//! updated only from a nonempty body completion, and publishes V into the
//! enclosing StatementList only when it is nonempty. An empty loop therefore
//! never clobbers the previous value, while an explicit `undefined` body value
//! is a real nonempty completion.

use std::collections::HashSet;

use swc_common::{Span, Spanned};
use swc_ecma_ast as ast;
use vm::{AssignmentKind, Expr, ParseError, Stmt, TypeSchema};

use super::heap as layout;
use super::lowerer::expression::BranchFacts;
use super::lowerer::{Kind, Lowerer, assert_stmt};
use super::reference::{self, NameHit};
use super::representation as value;
use super::scope::BindingId;

/// One statically resolved jump boundary in the current activation. Loops
/// are iteration targets; a switch is breakable but never an iteration
/// target, so `continue` traverses it to the nearest loop while `break`
/// stops at it. Plain label boundaries accept only their own labelled jump.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BoundaryKind {
    Label,
    Loop,
    Switch,
}

/// One statically resolved jump boundary in the current activation.
#[derive(Clone)]
struct ControlTarget {
    target: u32,
    kind: BoundaryKind,
    labels: Vec<String>,
}

/// Reused per-loop-nesting slots: loop value V, its ES5 nonempty flag, the
/// continuation flag and the phase-tail scratch flag. Roots for the value slot
/// are bounded by syntactic loop nesting, never by iteration count. ES5 loop V
/// starts as empty: `has` is initialized false once per loop entry, so a loop
/// that never sees a nonempty body completion publishes Normal(empty) and
/// leaves the enclosing sequence value untouched.
#[derive(Clone, Copy)]
struct LoopSlots {
    value: u16,
    has: u16,
    again: u16,
    cont: u16,
}

/// One numeric dense-Array iterator. `array` is the rooted original handle;
/// `cursor` is the next index. Both are reused across loops at the same depth.
#[derive(Clone, Copy)]
struct ForOfSlots {
    array: u16,
    cursor: u16,
}

/// Activation-local lowering state. Saved/cleared/restored by
/// `append_source_function` so nested function bodies never share targets,
/// label scopes, slot pools or the empty record.
#[derive(Default)]
pub(super) struct IterationState {
    pub(super) mode: bool,
    empty: Option<u16>,
    control: Vec<ControlTarget>,
    list_pool: Vec<(u16, u16)>,
    list_depth: usize,
    loop_pool: Vec<LoopSlots>,
    loop_depth: usize,
    /// Iterator handle plus cursor, pooled by syntactic nesting. Not per
    /// iteration and not a retained value snapshot.
    for_of_pool: Vec<ForOfSlots>,
    for_of_depth: usize,
    /// Locals written anywhere inside an enclosing loop construct. Their
    /// entry kind is not stable across the back edge (for example `typeof`
    /// must inspect the real tag after the first iteration).
    unstable: Vec<HashSet<BindingId>>,
}

impl IterationState {
    pub(super) fn activate_if_needed(&mut self, statements: &[ast::Stmt]) -> bool {
        let needed = activation_needs_iteration(statements);
        if needed {
            self.mode = true;
        }
        needed
    }

    pub(super) fn kind_is_unstable(&self, id: BindingId) -> bool {
        self.unstable.iter().any(|written| written.contains(&id))
    }
}

/// True when this activation's own statement surface contains a new form.
/// Function bodies are separate activations and are never inspected here.
pub(super) fn activation_needs_iteration(statements: &[ast::Stmt]) -> bool {
    statements.iter().any(stmt_needs_iteration)
}

fn stmt_needs_iteration(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::While(_)
        | ast::Stmt::DoWhile(_)
        | ast::Stmt::For(_)
        | ast::Stmt::ForOf(_)
        | ast::Stmt::Switch(_)
        | ast::Stmt::Labeled(_)
        | ast::Stmt::Break(_)
        | ast::Stmt::Continue(_) => true,
        ast::Stmt::Block(block) => block.stmts.iter().any(stmt_needs_iteration),
        ast::Stmt::If(if_stmt) => {
            stmt_needs_iteration(&if_stmt.cons)
                || if_stmt
                    .alt
                    .as_ref()
                    .is_some_and(|alt| stmt_needs_iteration(alt))
        }
        ast::Stmt::Try(try_stmt) => {
            try_stmt.block.stmts.iter().any(stmt_needs_iteration)
                || try_stmt
                    .handler
                    .as_ref()
                    .is_some_and(|handler| handler.body.stmts.iter().any(stmt_needs_iteration))
                || try_stmt
                    .finalizer
                    .as_ref()
                    .is_some_and(|finalizer| finalizer.stmts.iter().any(stmt_needs_iteration))
        }
        // Decl Fn bodies are deferred activations; other forms keep their
        // existing refusal and never need the enhanced path.
        _ => false,
    }
}

/// Whether a statement subtree can publish `break`/`continue` into an
/// enclosing sequence (nested function bodies excluded).
fn stmt_may_jump(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::Break(_) | ast::Stmt::Continue(_) => true,
        ast::Stmt::Block(block) => block.stmts.iter().any(stmt_may_jump),
        ast::Stmt::If(if_stmt) => {
            stmt_may_jump(&if_stmt.cons)
                || if_stmt.alt.as_ref().is_some_and(|alt| stmt_may_jump(alt))
        }
        ast::Stmt::While(loop_) => stmt_may_jump(&loop_.body),
        ast::Stmt::DoWhile(loop_) => stmt_may_jump(&loop_.body),
        ast::Stmt::For(loop_) => stmt_may_jump(&loop_.body),
        ast::Stmt::ForOf(loop_) => stmt_may_jump(&loop_.body),
        ast::Stmt::Labeled(labeled) => stmt_may_jump(&labeled.body),
        ast::Stmt::Switch(select) => select
            .cases
            .iter()
            .any(|case| case.cons.iter().any(stmt_may_jump)),
        ast::Stmt::Try(try_stmt) => {
            try_stmt.block.stmts.iter().any(stmt_may_jump)
                || try_stmt
                    .handler
                    .as_ref()
                    .is_some_and(|handler| handler.body.stmts.iter().any(stmt_may_jump))
                || try_stmt
                    .finalizer
                    .as_ref()
                    .is_some_and(|finalizer| finalizer.stmts.iter().any(stmt_may_jump))
        }
        _ => false,
    }
}

fn assign(index: u16, expr: Expr, line: u32) -> Stmt {
    Stmt::Assign {
        kind: AssignmentKind::Set,
        index,
        expr,
        line,
    }
}

/// Read a Completion field without consuming it. A plain literal-key `Get`
/// on a named local is a partial field move, so the record must be read
/// through a borrow when a later consumer (a loop boundary, a labelled
/// boundary or the function epilogue) will still need the same field.
fn completion_field(slot: u16, key: &str) -> Expr {
    value::field_of(Expr::Borrow(Box::new(Expr::Var(slot))), key)
}

fn let_stmt(index: u16, schema: TypeSchema, expr: Expr, line: u32) -> Stmt {
    Stmt::Let {
        index,
        declared_schema: Some(schema),
        expr,
        line,
    }
}

fn break_stmt(line: u32) -> Stmt {
    Stmt::Break { line }
}

fn pending(lower: &Lowerer<'_>) -> Result<(u16, u16, u16), ParseError> {
    match (
        lower.pending_normal,
        lower.pending_completion,
        lower.pending_payload,
    ) {
        (Some(normal), Some(completion), Some(payload)) => Ok((normal, completion, payload)),
        _ => Err(ParseError::at_span(
            vm::compiler::source_map::Span::new(lower.source_id, 0, 0),
            "enhanced statement lowering requires the activation pending channel",
        )),
    }
}

fn tail(lower: &Lowerer<'_>) -> Result<reference::FunctionTail, ParseError> {
    let (normal, completion, payload) = pending(lower)?;
    Ok(reference::FunctionTail {
        normal,
        completion,
        payload,
    })
}

fn empty_record(lower: &Lowerer<'_>) -> Result<u16, ParseError> {
    lower.iteration.empty.ok_or_else(|| {
        ParseError::at_span(
            vm::compiler::source_map::Span::new(lower.source_id, 0, 0),
            "enhanced statement lowering requires the empty Completion record",
        )
    })
}

/// Allocate the activation's single immutable Normal(empty) record and the
/// throw payload root. Called once at activation entry.
fn init_activation(
    lower: &mut Lowerer<'_>,
    span: Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let line = lower.line(span);
    let empty = lower.slot(span)?;
    out.push(let_stmt(
        empty,
        value::completion_schema(),
        value::completion_normal(),
        line,
    ));
    lower.iteration.empty = Some(empty);
    Ok(())
}

/// Per-list accumulator slots, pooled by syntactic list nesting.
fn alloc_list(
    lower: &mut Lowerer<'_>,
    span: Span,
    out: &mut Vec<Stmt>,
) -> Result<ListCtx, ParseError> {
    let depth = lower.iteration.list_depth;
    let (value_slot, has_slot) = if let Some(slots) = lower.iteration.list_pool.get(depth) {
        *slots
    } else {
        let slots = (lower.slot(span)?, lower.slot(span)?);
        lower.iteration.list_pool.push(slots);
        slots
    };
    lower.iteration.list_depth = depth.saturating_add(1);
    let line = lower.line(span);
    out.push(let_stmt(
        value_slot,
        value::schema(),
        value::undefined(),
        line,
    ));
    lower.refresh_root(value_slot, span, out)?;
    out.push(let_stmt(
        has_slot,
        TypeSchema::Bool,
        Expr::Bool(false),
        line,
    ));
    Ok(ListCtx {
        value: value_slot,
        has: has_slot,
    })
}

fn release_list(lower: &mut Lowerer<'_>) {
    lower.iteration.list_depth = lower.iteration.list_depth.saturating_sub(1);
}

/// One pending statement-list value pair. `has` is the ES5 nonempty test.
#[derive(Clone, Copy)]
struct ListCtx {
    value: u16,
    has: u16,
}

/// Script activation: the tail's present/value pair is the root sequence
/// accumulator, and the two trailing stack reads keep the accepted root
/// observation shape. ES5 Script completion is Empty when the list value is
/// empty, not a fabricated Undefined.
pub(super) fn lower_script(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let span = statements[0].span();
    init_activation(lower, span, out)?;
    let line = lower.line(span);
    let present = lower.slot(span)?;
    out.push(let_stmt(present, TypeSchema::Bool, Expr::Bool(false), line));
    let (value_slot, stmt) = lower.bind_temp_bare(value::undefined(), span)?;
    out.push(stmt);
    lower.refresh_root(value_slot, span, out)?;
    let ctx = ListCtx {
        value: value_slot,
        has: present,
    };
    lower_items(lower, statements, out, &ctx, true)?;
    let (normal, _, _) = pending(lower)?;
    out.push(Stmt::IfElse {
        condition: Expr::Not(Box::new(Expr::Var(normal))),
        then_branch: vec![assign(present, Expr::Bool(false), line)],
        else_branch: Vec::new(),
        line,
    });
    out.push(Stmt::Expr {
        expr: Expr::Var(present),
        line,
    });
    out.push(Stmt::Expr {
        expr: Expr::Var(value_slot),
        line,
    });
    Ok(())
}

/// Function activation: internal normal statement values stay private. A
/// normal boundary always restores Normal(empty)/undefined, so falling off a
/// function never returns the last expression value. Explicit `return`
/// remains RETURN with `has_value=true`.
pub(super) fn lower_function(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let span = statements[0].span();
    init_activation(lower, span, out)?;
    let ctx = alloc_list(lower, span, out)?;
    // Only an actual function entry owns a Directive Prologue. Ordinary
    // nested StatementLists (including case/default clauses) evaluate strings.
    let prologue_len = statements
        .iter()
        .take_while(|statement| is_string_expression_statement(statement))
        .count();
    lower_items(lower, &statements[prologue_len..], out, &ctx, false)?;
    release_list(lower);
    normalize_function_exit(lower, span, out)
}

fn normalize_function_exit(
    lower: &mut Lowerer<'_>,
    span: Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let (normal, completion, _) = pending(lower)?;
    let empty = empty_record(lower)?;
    let line = lower.line(span);
    out.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: vec![assign(completion, Expr::Var(empty), line)],
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

/// A `try`/`catch` block statement list. Its own value is materialized into
/// the pending Completion when normal, so the catch guard and the finally
/// snapshot observe the real ES5 completion.
pub(super) fn lower_try_list(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let span = statements
        .first()
        .map(Spanned::span)
        .unwrap_or_else(|| swc_common::Span::new(swc_common::BytePos(0), swc_common::BytePos(0)));
    let ctx = alloc_list(lower, span, out)?;
    lower_items(lower, statements, out, &ctx, false)?;
    release_list(lower);
    materialize_list_value(lower, span, out, &ctx)
}

/// A `finally` block statement list. A normal finalizer value is ignored by
/// the controlled finally restore; an abrupt one replaces the pending state.
pub(super) fn lower_finalizer_list(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let span = statements
        .first()
        .map(Spanned::span)
        .unwrap_or_else(|| swc_common::Span::new(swc_common::BytePos(0), swc_common::BytePos(0)));
    let ctx = alloc_list(lower, span, out)?;
    lower_items(lower, statements, out, &ctx, false)?;
    release_list(lower);
    Ok(())
}

fn materialize_list_value(
    lower: &mut Lowerer<'_>,
    span: Span,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
) -> Result<(), ParseError> {
    let (normal, completion, _) = pending(lower)?;
    let line = lower.line(span);
    let record = lower.slot(span)?;
    out.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: vec![
            let_stmt(
                record,
                value::completion_schema(),
                value::completion_from_parts(
                    Expr::Int(value::COMPLETION_NORMAL),
                    Expr::Var(ctx.value),
                    Expr::Var(ctx.has),
                    Expr::Int(0),
                ),
                line,
            ),
            assign(completion, Expr::Var(record), line),
        ],
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

fn lower_items(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
    script: bool,
) -> Result<(), ParseError> {
    let mut index = 0;
    while index < statements.len() {
        // Preserve the existing Script-entry strict-directive policy. All
        // non-Script callers are ordinary lists; function entry trims its
        // own prologue before calling this shared evaluator.
        if script && is_strict_directive_statement(lower, &statements[index]) {
            index += 1;
            continue;
        }
        if lower.statement_is_dead(statements[index].span()) {
            let start = index;
            while index < statements.len() {
                if script && is_strict_directive_statement(lower, &statements[index]) {
                    index += 1;
                    continue;
                }
                if !lower.statement_is_dead(statements[index].span()) {
                    break;
                }
                index += 1;
            }
            let run = &statements[start..index];
            lower.speculate_dead(|lower| {
                for statement in run {
                    if script && is_strict_directive_statement(lower, statement) {
                        continue;
                    }
                    let mut discarded = Vec::new();
                    lower_stmt(lower, statement, &mut discarded, ctx)?;
                }
                Ok(())
            })?;
            continue;
        }
        let statement = &statements[index];
        let line = lower.line(statement.span());
        let (normal, completion, _) = pending(lower)?;
        let empty = empty_record(lower)?;
        // Type-name caches created inside a statement must not dominate the
        // next statement (a conditional/loop body may not have executed).
        let incoming_names = lower.type_names;
        let mut guard = Vec::new();
        guard.push(assign(completion, Expr::Var(empty), line));
        lower_stmt(lower, statement, &mut guard, ctx)?;
        post_statement(lower, statement, &mut guard, ctx)?;
        lower.type_names = incoming_names;
        out.push(Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: guard,
            else_branch: Vec::new(),
            line,
        });
        index += 1;
    }
    Ok(())
}

fn is_strict_directive_statement(lower: &Lowerer<'_>, statement: &ast::Stmt) -> bool {
    let ast::Stmt::Expr(expr) = statement else {
        return false;
    };
    match expr.expr.as_ref() {
        ast::Expr::Lit(ast::Lit::Str(literal)) => lower.is_exact_use_strict_lexeme(literal.span),
        _ => false,
    }
}

fn is_string_expression_statement(statement: &ast::Stmt) -> bool {
    matches!(
        statement,
        ast::Stmt::Expr(expr) if matches!(expr.expr.as_ref(), ast::Expr::Lit(ast::Lit::Str(_)))
    )
}

/// ES5 StatementList UpdateEmpty for one statement: a normal value updates the
/// list accumulator; an abrupt break/continue with an empty value inherits the
/// preceding list value. Return/Throw already carry their own values.
fn post_statement(
    lower: &mut Lowerer<'_>,
    statement: &ast::Stmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
) -> Result<(), ParseError> {
    let span = statement.span();
    let line = lower.line(span);
    let (normal, completion, _) = pending(lower)?;
    if stmt_may_jump(statement) {
        let kind = completion_field(completion, "kind");
        let jump = Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Var(normal)))),
            Box::new(Expr::And(
                Box::new(Expr::Or(
                    Box::new(Expr::Eq(
                        Box::new(kind.clone()),
                        Box::new(Expr::Int(value::COMPLETION_BREAK)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(kind.clone()),
                        Box::new(Expr::Int(value::COMPLETION_CONTINUE)),
                    )),
                )),
                // Get resumes as Tagged in native SSA. Branches use the
                // existing tag-checked Bool conversion; Not does not.
                Box::new(Expr::IfElse {
                    condition: Box::new(completion_field(completion, "has_value")),
                    then_expr: Box::new(Expr::Bool(false)),
                    else_expr: Box::new(Expr::Bool(true)),
                }),
            )),
        );
        let record = lower.slot(span)?;
        out.push(Stmt::IfElse {
            condition: jump,
            then_branch: vec![
                let_stmt(
                    record,
                    value::completion_schema(),
                    value::completion_from_parts(
                        kind,
                        Expr::Var(ctx.value),
                        Expr::Var(ctx.has),
                        completion_field(completion, "target"),
                    ),
                    line,
                ),
                assign(completion, Expr::Var(record), line),
            ],
            else_branch: Vec::new(),
            line,
        });
    }
    if matches!(statement, ast::Stmt::Try(_) | ast::Stmt::Labeled(_)) {
        let mut merge = vec![assign(
            ctx.value,
            completion_field(completion, "value"),
            line,
        )];
        lower.refresh_root(ctx.value, span, &mut merge)?;
        merge.push(assign(ctx.has, Expr::Bool(true), line));
        out.push(Stmt::IfElse {
            condition: Expr::And(
                Box::new(Expr::Var(normal)),
                Box::new(completion_field(completion, "has_value")),
            ),
            then_branch: merge,
            else_branch: Vec::new(),
            line,
        });
    }
    Ok(())
}

fn lower_stmt(
    lower: &mut Lowerer<'_>,
    statement: &ast::Stmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => lower_var(lower, declaration, out),
        ast::Stmt::Decl(ast::Decl::Fn(_)) => Ok(()),
        ast::Stmt::Empty(_) | ast::Stmt::Debugger(_) => Ok(()),
        ast::Stmt::Expr(expr) => lower_expression_statement(lower, expr, out, ctx),
        ast::Stmt::Block(block) => lower_block(lower, block, out, ctx),
        ast::Stmt::If(if_stmt) => lower_if(lower, if_stmt, out, ctx),
        ast::Stmt::Switch(select) => {
            let target = fresh_target(lower, select.span)?;
            lower_switch(lower, select, out, ctx, target)
        }
        ast::Stmt::Try(try_stmt) => {
            // The catch arm only runs on Throw, so its kind/length writes must
            // not dominate the normal path. Join the whole try result with the
            // incoming facts; the finally runs on both paths.
            let before = BranchFacts::save(lower);
            let result = reference::lower_controlled_try(lower, try_stmt, out, &tail(lower)?);
            let after = BranchFacts::save(lower);
            before.restore(lower);
            after.merge(lower);
            result
        }
        ast::Stmt::Return(ret) => {
            reference::lower_controlled_return(lower, ret, out, &tail(lower)?)
        }
        ast::Stmt::Throw(throw) => {
            reference::lower_controlled_throw(lower, throw, out, &tail(lower)?)
        }
        ast::Stmt::Break(stop) => lower_jump(lower, stop.span, stop.label.as_ref(), false, out),
        ast::Stmt::Continue(resume) => {
            lower_jump(lower, resume.span, resume.label.as_ref(), true, out)
        }
        ast::Stmt::While(loop_) => {
            let target = fresh_target(lower, loop_.span)?;
            lower_iteration(lower, statement, out, ctx, target, Vec::new())
        }
        ast::Stmt::DoWhile(loop_) => {
            let target = fresh_target(lower, loop_.span)?;
            lower_iteration(lower, statement, out, ctx, target, Vec::new())
        }
        ast::Stmt::For(loop_) => {
            let target = fresh_target(lower, loop_.span)?;
            lower_iteration(lower, statement, out, ctx, target, Vec::new())
        }
        ast::Stmt::ForOf(loop_) => {
            let target = fresh_target(lower, loop_.span)?;
            lower_iteration(lower, statement, out, ctx, target, Vec::new())
        }
        ast::Stmt::Labeled(labeled) => lower_labeled(lower, labeled, out, ctx),
        _ => Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement")),
    }
}

fn lower_var(
    lower: &mut Lowerer<'_>,
    declaration: &ast::VarDecl,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let (normal, _, _) = pending(lower)?;
    let line = lower.line(declaration.span);
    let mut declarators = Vec::new();
    for declarator in &declaration.decls {
        let mut inner = Vec::new();
        reference::lower_var_declarator(lower, declaration, declarator, &mut inner)?;
        declarators.push(Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: inner,
            else_branch: Vec::new(),
            line,
        });
    }
    out.append(&mut declarators);
    Ok(())
}

fn lower_expression_statement(
    lower: &mut Lowerer<'_>,
    expr: &ast::ExprStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
) -> Result<(), ParseError> {
    let slot = reference::lower_expr_stmt(lower, expr, out, false)?;
    if let Some(slot) = slot {
        let (normal, _, _) = pending(lower)?;
        let line = lower.line(expr.span);
        let mut then_branch = vec![assign(ctx.value, Expr::Var(slot), line)];
        lower.refresh_root(ctx.value, expr.span, &mut then_branch)?;
        then_branch.push(assign(ctx.has, Expr::Bool(true), line));
        out.push(Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch,
            else_branch: Vec::new(),
            line,
        });
    }
    Ok(())
}

fn lower_block(
    lower: &mut Lowerer<'_>,
    block: &ast::BlockStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
) -> Result<(), ParseError> {
    let span = block.span;
    let line = lower.line(span);
    let inner = alloc_list(lower, span, out)?;
    lower_items(lower, &block.stmts, out, &inner, false)?;
    release_list(lower);
    let (normal, _, _) = pending(lower)?;
    let mut merge = vec![assign(ctx.value, Expr::Var(inner.value), line)];
    lower.refresh_root(ctx.value, span, &mut merge)?;
    merge.push(assign(ctx.has, Expr::Bool(true), line));
    out.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: vec![Stmt::IfElse {
            condition: Expr::Var(inner.has),
            then_branch: merge,
            else_branch: Vec::new(),
            line,
        }],
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

fn lower_if(
    lower: &mut Lowerer<'_>,
    if_stmt: &ast::IfStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
) -> Result<(), ParseError> {
    let (flag, mut condition) = lower.condition_flag(&if_stmt.test)?;
    out.append(&mut condition);
    let line = lower.line(if_stmt.span);
    // Whole-branch flow facts: the then arm must not dominate the else arm,
    // and the join keeps both arm kinds after the statement.
    let before = BranchFacts::save(lower);
    let incoming_names = lower.type_names;
    let mut then_branch = Vec::new();
    let ((), then_threw) = lower.isolate_conditional_emission(|lower| {
        lower_stmt(lower, &if_stmt.cons, &mut then_branch, ctx)
    })?;
    let then_facts = BranchFacts::save(lower);
    before.clone().restore(lower);
    lower.type_names = incoming_names;
    let mut else_branch = Vec::new();
    let ((), else_threw) = lower.isolate_conditional_emission(|lower| {
        if let Some(alt) = &if_stmt.alt {
            lower_stmt(lower, alt, &mut else_branch, ctx)?;
        }
        Ok(())
    })?;
    if then_threw && (if_stmt.alt.is_none() || else_threw) {
        before.restore(lower);
        lower.note_emission_throw();
    } else if then_threw {
        // Only the else arm completes.
    } else if else_threw {
        then_facts.restore(lower);
    } else {
        then_facts.merge(lower);
    }
    lower.type_names = incoming_names;
    let (normal, _, _) = pending(lower)?;
    let select = Stmt::IfElse {
        condition: Expr::Var(flag),
        then_branch,
        else_branch,
        line,
    };
    // A throwing test must not run either arm. The condition flag is a falsy
    // placeholder in that case, so select only while Normal still holds.
    out.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: vec![select],
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

/// ES5.1 12.11 switch in two compile-time-emitted phases. The discriminant
/// is evaluated and rooted once. Each case selector is then evaluated at
/// most once, in source search order (cases before and after a middle
/// default), stopping at the first strict (`===`) match; a middle default
/// only supplies the fallback index after every selector fails. The selected
/// source suffix executes with fallthrough under ordinary IR branch gates.
/// A fresh per-entry CaseBlock value/has pair carries the suffix completion
/// (starting empty, never Undefined); this switch's own consumed break keeps
/// its value, while foreign jumps traverse untouched.
fn lower_switch(
    lower: &mut Lowerer<'_>,
    select: &ast::SwitchStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
    target: u32,
) -> Result<(), ParseError> {
    let span = select.span;
    let line = lower.line(span);
    let (normal, _, _) = pending(lower)?;
    // Static clause layout in source order. The grammar admits at most one
    // default; the first one supplies the fallback index.
    let mut default_index: Option<usize> = None;
    for (index, case) in select.cases.iter().enumerate() {
        if case.test.is_none() && default_index.is_none() {
            default_index = Some(index);
        }
    }
    let search: Vec<usize> = select
        .cases
        .iter()
        .enumerate()
        .filter(|(_, case)| case.test.is_some())
        .map(|(index, _)| index)
        .collect();
    // Whole-switch flow facts: no selector or clause body dominates its
    // siblings or post-switch code. Joins follow the loop/conditional
    // direction; allocations and roots stay monotonic.
    let incoming_names = lower.type_names;
    // Phase one: the once-evaluated discriminant. Its box stays rooted
    // across every selector call until its last comparison.
    let disc_lowered = lower.expr(&select.discriminant)?;
    let (disc_slot, mut disc_stmts) = lower.bind_evaluation(
        disc_lowered.expr,
        disc_lowered.kind,
        select.discriminant.span(),
    )?;
    out.append(&mut disc_stmts);
    let mut disc_roots = disc_lowered.owned_roots;
    if lower.has_root(disc_slot) {
        disc_roots.push(disc_slot);
    }
    reference::release_consumed_roots(lower, &disc_roots, disc_slot, span, out)?;
    let disc_facts = BranchFacts::save(lower);
    let matched = lower.slot(span)?;
    out.push(let_stmt(matched, TypeSchema::Bool, Expr::Bool(false), line));
    let selected = lower.slot(span)?;
    out.push(let_stmt(selected, TypeSchema::Int, Expr::Int(-1), line));
    // A fresh per-entry CaseBlock value/has pair carries the suffix.
    let switch_ctx = alloc_list(lower, span, out)?;
    // Phase two: search the selectors in order behind pending-normal and
    // no-match guards. A throwing selector leaves Normal false, so no later
    // selector runs and no suffix is selected.
    let mut selector_ends = Vec::new();
    for &index in &search {
        disc_facts.clone().restore(lower);
        lower.type_names = incoming_names;
        let case = &select.cases[index];
        let test = case.test.as_ref().expect("search holds case selectors");
        let test_span = test.span();
        let sel_lowered = lower.expr(test)?;
        let (sel_slot, mut sel_stmts) =
            lower.bind_evaluation(sel_lowered.expr, sel_lowered.kind, test_span)?;
        let mut phase = Vec::new();
        phase.append(&mut sel_stmts);
        let flag = lower.slot(test_span)?;
        phase.push(let_stmt(flag, TypeSchema::Bool, Expr::Bool(false), line));
        let strings = lower.string_equality(test_span);
        let index_tag = i64::try_from(index)
            .map_err(|_| lower.error(span, "JavaScript ES5 switch clause index overflow"))?;
        let mut compare = vec![assign(matched, Expr::Bool(true), line)];
        compare.push(assign(selected, Expr::Int(index_tag), line));
        phase.push(Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: vec![assign(
                flag,
                value::strict_equal_flag(disc_slot, sel_slot, strings),
                line,
            )],
            else_branch: Vec::new(),
            line,
        });
        phase.push(Stmt::IfElse {
            condition: Expr::And(Box::new(Expr::Var(normal)), Box::new(Expr::Var(flag))),
            then_branch: compare,
            else_branch: Vec::new(),
            line,
        });
        // Selector temps die with their comparison; the discriminant root
        // survives until the search completes.
        let mut sel_roots = sel_lowered.owned_roots;
        if lower.has_root(sel_slot) {
            sel_roots.push(sel_slot);
        }
        reference::release_consumed_roots(lower, &sel_roots, disc_slot, test_span, &mut phase)?;
        out.push(Stmt::IfElse {
            condition: Expr::And(
                Box::new(Expr::Var(normal)),
                Box::new(Expr::Not(Box::new(Expr::Var(matched)))),
            ),
            then_branch: phase,
            else_branch: Vec::new(),
            line,
        });
        selector_ends.push(BranchFacts::save(lower));
    }
    // Selector effects never dominate the suffix: join every search path.
    disc_facts.clone().restore(lower);
    for end in &selector_ends {
        end.clone().merge(lower);
    }
    let search_facts = BranchFacts::save(lower);
    // The default fallback runs only while Normal holds and no selector
    // matched. It changes no kinds, so it contributes no flow facts.
    if let Some(default) = default_index {
        let default_tag = i64::try_from(default)
            .map_err(|_| lower.error(span, "JavaScript ES5 switch clause index overflow"))?;
        out.push(Stmt::IfElse {
            condition: Expr::And(
                Box::new(Expr::Var(normal)),
                Box::new(Expr::Not(Box::new(Expr::Var(matched)))),
            ),
            then_branch: vec![assign(selected, Expr::Int(default_tag), line)],
            else_branch: Vec::new(),
            line,
        });
    }
    // The discriminant root dies with the search; the suffix needs only the
    // selected index. This runs unconditionally: entries are compile-time
    // reservations, and an abrupt search must not pin the box.
    reference::release_consumed_roots(lower, &disc_roots, switch_ctx.value, span, out)?;
    // The switch boundary is live for the whole suffix so an unlabeled break
    // stops here while continue still traverses to the nearest loop.
    lower.iteration.control.push(ControlTarget {
        target,
        kind: BoundaryKind::Switch,
        labels: Vec::new(),
    });
    // Phase three: execute the selected source suffix with fallthrough. Each
    // clause body starts from the joined entry (shared search effects plus
    // every fallthrough predecessor) and rejoins at the switch end, so no
    // non-dominating body leaks into siblings or post-switch code.
    let reached = lower.slot(span)?;
    out.push(let_stmt(reached, TypeSchema::Bool, Expr::Bool(false), line));
    let mut suffix_ends = Vec::new();
    let mut entry = search_facts.clone();
    for (index, case) in select.cases.iter().enumerate() {
        entry.clone().restore(lower);
        lower.type_names = incoming_names;
        let mut gated = vec![assign(reached, Expr::Bool(true), line)];
        lower_items(lower, &case.cons, &mut gated, &switch_ctx, false)?;
        let index_tag = i64::try_from(index)
            .map_err(|_| lower.error(span, "JavaScript ES5 switch clause index overflow"))?;
        out.push(Stmt::IfElse {
            condition: Expr::And(
                Box::new(Expr::Var(normal)),
                Box::new(Expr::Or(
                    Box::new(Expr::Var(reached)),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(selected)),
                        Box::new(Expr::Int(index_tag)),
                    )),
                )),
            ),
            then_branch: gated,
            else_branch: Vec::new(),
            line,
        });
        let exit = BranchFacts::save(lower);
        suffix_ends.push(exit.clone());
        // The next clause is reachable by direct match (the shared search
        // facts) or by fallthrough (this clause's end).
        search_facts.clone().restore(lower);
        exit.merge(lower);
        entry = BranchFacts::save(lower);
    }
    release_list(lower);
    // Post-switch join: the no-match path plus every clause end.
    search_facts.clone().restore(lower);
    for exit in &suffix_ends {
        exit.clone().merge(lower);
    }
    lower.type_names = incoming_names;
    let result = consume_switch_break(lower, out, &switch_ctx, ctx, span, target);
    lower.iteration.control.pop();
    result
}

/// Consume this switch's own break after any enclosing finally has run: the
/// pending completion keeps its value/has pair and returns to Normal. A
/// foreign abrupt completion (a labelled break outward, a continue to the
/// nearest loop, return or throw) traverses untouched. The switch value then
/// merges into the enclosing list only when nonempty, and a normal exit
/// republishes the nominal Normal(empty) completion.
fn consume_switch_break(
    lower: &mut Lowerer<'_>,
    out: &mut Vec<Stmt>,
    switch_ctx: &ListCtx,
    ctx: &ListCtx,
    span: Span,
    target: u32,
) -> Result<(), ParseError> {
    let (normal, completion, _) = pending(lower)?;
    let empty = empty_record(lower)?;
    let line = lower.line(span);
    let kept_value = lower.slot(span)?;
    let kept_has = lower.slot(span)?;
    let record = lower.slot(span)?;
    let consumed = lower.slot(span)?;
    out.push(let_stmt(
        consumed,
        TypeSchema::Bool,
        Expr::Bool(false),
        line,
    ));
    // Definite-initialization: the merge reads the kept pair under the
    // consumed flag, so both must hold a value on every path even when the
    // consume gate never fires.
    out.push(let_stmt(
        kept_value,
        value::schema(),
        value::undefined(),
        line,
    ));
    lower.refresh_root(kept_value, span, out)?;
    out.push(let_stmt(
        kept_has,
        TypeSchema::Bool,
        Expr::Bool(false),
        line,
    ));
    let mut then_branch = vec![let_stmt(
        kept_value,
        value::schema(),
        completion_field(completion, "value"),
        line,
    )];
    lower.refresh_root(kept_value, span, &mut then_branch)?;
    then_branch.push(let_stmt(
        kept_has,
        TypeSchema::Bool,
        completion_field(completion, "has_value"),
        line,
    ));
    then_branch.push(let_stmt(
        record,
        value::completion_schema(),
        value::completion_from_parts(
            Expr::Int(value::COMPLETION_NORMAL),
            Expr::Var(kept_value),
            Expr::Var(kept_has),
            Expr::Int(0),
        ),
        line,
    ));
    then_branch.push(assign(completion, Expr::Var(record), line));
    then_branch.push(assign(normal, Expr::Bool(true), line));
    then_branch.push(assign(consumed, Expr::Bool(true), line));
    out.push(Stmt::IfElse {
        condition: jump_condition(completion, value::COMPLETION_BREAK, target),
        then_branch,
        else_branch: Vec::new(),
        line,
    });
    // A consumed break carries the suffix value in the pending completion,
    // while the normal path carries it in the switch accumulator: a `try`
    // body publishes its value only through the completion, never into the
    // accumulator, so the merge must follow the consumed flag.
    let mut kept_merge = vec![assign(ctx.value, Expr::Var(kept_value), line)];
    lower.refresh_root(ctx.value, span, &mut kept_merge)?;
    kept_merge.push(assign(ctx.has, Expr::Var(kept_has), line));
    // ES5.1 12.11 + StatementList UpdateEmpty: a consumed break retains an
    // empty value, which must not clobber the enclosing list value. Merge
    // the kept pair only when it actually carries a value.
    let consumed_merge = vec![Stmt::IfElse {
        condition: Expr::Var(kept_has),
        then_branch: kept_merge,
        else_branch: Vec::new(),
        line,
    }];
    let mut plain_merge = vec![assign(ctx.value, Expr::Var(switch_ctx.value), line)];
    lower.refresh_root(ctx.value, span, &mut plain_merge)?;
    plain_merge.push(assign(ctx.has, Expr::Bool(true), line));
    // ES5 UpdateEmpty: an empty switch value never clobbers the enclosing
    // StatementList value; a normal exit republishes Nominal Normal(empty).
    out.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: vec![
            Stmt::IfElse {
                condition: Expr::Var(consumed),
                then_branch: consumed_merge,
                else_branch: vec![Stmt::IfElse {
                    condition: Expr::Var(switch_ctx.has),
                    then_branch: plain_merge,
                    else_branch: Vec::new(),
                    line,
                }],
                line,
            },
            assign(completion, Expr::Var(empty), line),
        ],
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

fn fresh_target(lower: &mut Lowerer<'_>, span: Span) -> Result<u32, ParseError> {
    let target = lower.next_target;
    lower.next_target = target
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "JavaScript ES5 jump target space exhausted"))?;
    Ok(target)
}

fn resolve_target(
    lower: &Lowerer<'_>,
    span: Span,
    label: Option<&ast::Ident>,
    iteration: bool,
) -> Result<u32, ParseError> {
    if let Some(label) = label {
        for entry in lower.iteration.control.iter().rev() {
            if entry.labels.iter().any(|name| name == label.sym.as_str()) {
                if iteration && entry.kind != BoundaryKind::Loop {
                    return Err(
                        lower.error(span, "a 'continue' label must name an iteration statement")
                    );
                }
                return Ok(entry.target);
            }
        }
        return Err(lower.error(span, &format!("unresolved 'label' target '{}'", label.sym)));
    }
    // An unlabeled break stops at the nearest loop or switch; an unlabeled
    // continue traverses switches to the nearest loop.
    lower
        .iteration
        .control
        .iter()
        .rev()
        .find(|entry| {
            entry.kind == BoundaryKind::Loop || (!iteration && entry.kind == BoundaryKind::Switch)
        })
        .map(|entry| entry.target)
        .ok_or_else(|| {
            lower.error(
                span,
                "a jump statement has no enclosing iteration target in this activation",
            )
        })
}

fn lower_jump(
    lower: &mut Lowerer<'_>,
    span: Span,
    label: Option<&ast::Ident>,
    iteration: bool,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let (normal, completion, _) = pending(lower)?;
    let target = resolve_target(lower, span, label, iteration)?;
    let kind = if iteration {
        value::COMPLETION_CONTINUE
    } else {
        value::COMPLETION_BREAK
    };
    let line = lower.line(span);
    let record = lower.slot(span)?;
    out.push(let_stmt(
        record,
        value::completion_schema(),
        value::completion_from_parts(
            Expr::Int(kind),
            value::undefined(),
            Expr::Bool(false),
            Expr::Int(i64::from(target)),
        ),
        line,
    ));
    out.push(assign(completion, Expr::Var(record), line));
    out.push(assign(normal, Expr::Bool(false), line));
    Ok(())
}

fn lower_labeled(
    lower: &mut Lowerer<'_>,
    labeled: &ast::LabeledStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
) -> Result<(), ParseError> {
    let span = labeled.span;
    let line = lower.line(span);
    let mut labels = Vec::new();
    let mut current = labeled;
    let body = loop {
        labels.push(current.label.sym.to_string());
        match current.body.as_ref() {
            ast::Stmt::Labeled(inner) => current = inner,
            body => break body,
        }
    };
    let iteration = matches!(
        body,
        ast::Stmt::While(_) | ast::Stmt::DoWhile(_) | ast::Stmt::For(_) | ast::Stmt::ForOf(_)
    );
    let target = fresh_target(lower, span)?;
    if iteration {
        // Consecutive labels around one iteration share its single target.
        return lower_iteration(lower, body, out, ctx, target, labels);
    }
    lower.iteration.control.push(ControlTarget {
        target,
        kind: BoundaryKind::Label,
        labels,
    });
    let result = lower_stmt(lower, body, out, ctx);
    if let Err(error) = result {
        lower.iteration.control.pop();
        return Err(error);
    }
    // A matching labelled break becomes a Normal completion that preserves the
    // pending value/has_value pair. Anything else propagates unchanged.
    let (normal, completion, _) = pending(lower)?;
    let matched = Expr::And(
        Box::new(Expr::Eq(
            Box::new(completion_field(completion, "kind")),
            Box::new(Expr::Int(value::COMPLETION_BREAK)),
        )),
        Box::new(Expr::Eq(
            Box::new(completion_field(completion, "target")),
            Box::new(Expr::Int(i64::from(target))),
        )),
    );
    let kept_value = lower.slot(span)?;
    let kept_has = lower.slot(span)?;
    let record = lower.slot(span)?;
    let mut then_branch = vec![let_stmt(
        kept_value,
        value::schema(),
        completion_field(completion, "value"),
        line,
    )];
    lower.refresh_root(kept_value, span, &mut then_branch)?;
    then_branch.push(let_stmt(
        kept_has,
        TypeSchema::Bool,
        completion_field(completion, "has_value"),
        line,
    ));
    then_branch.push(let_stmt(
        record,
        value::completion_schema(),
        value::completion_from_parts(
            Expr::Int(value::COMPLETION_NORMAL),
            Expr::Var(kept_value),
            Expr::Var(kept_has),
            Expr::Int(0),
        ),
        line,
    ));
    then_branch.push(assign(completion, Expr::Var(record), line));
    then_branch.push(assign(normal, Expr::Bool(true), line));
    out.push(Stmt::IfElse {
        condition: matched,
        then_branch,
        else_branch: Vec::new(),
        line,
    });
    lower.iteration.control.pop();
    Ok(())
}

fn alloc_loop(
    lower: &mut Lowerer<'_>,
    span: Span,
    out: &mut Vec<Stmt>,
) -> Result<LoopSlots, ParseError> {
    let depth = lower.iteration.loop_depth;
    let slots = if let Some(slots) = lower.iteration.loop_pool.get(depth) {
        *slots
    } else {
        let slots = LoopSlots {
            value: lower.slot(span)?,
            has: lower.slot(span)?,
            again: lower.slot(span)?,
            cont: lower.slot(span)?,
        };
        lower.iteration.loop_pool.push(slots);
        slots
    };
    lower.iteration.loop_depth = depth.saturating_add(1);
    let line = lower.line(span);
    // A pooled value slot may still hold a sibling loop's boxed payload; the
    // fresh `Let` plus root refresh drops both the logical value and its root
    // on every new loop invocation, not per iteration.
    out.push(let_stmt(
        slots.value,
        value::schema(),
        value::undefined(),
        line,
    ));
    lower.refresh_root(slots.value, span, out)?;
    out.push(let_stmt(
        slots.has,
        TypeSchema::Bool,
        Expr::Bool(false),
        line,
    ));
    out.push(let_stmt(
        slots.again,
        TypeSchema::Bool,
        Expr::Bool(true),
        line,
    ));
    out.push(let_stmt(
        slots.cont,
        TypeSchema::Bool,
        Expr::Bool(false),
        line,
    ));
    Ok(slots)
}

fn release_loop(lower: &mut Lowerer<'_>) {
    lower.iteration.loop_depth = lower.iteration.loop_depth.saturating_sub(1);
}

fn lower_iteration(
    lower: &mut Lowerer<'_>,
    statement: &ast::Stmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
    target: u32,
    labels: Vec<String>,
) -> Result<(), ParseError> {
    lower.iteration.control.push(ControlTarget {
        target,
        kind: BoundaryKind::Loop,
        labels,
    });
    let mut mutations = HashSet::new();
    collect_mutations(statement, lower, &mut mutations);
    lower.iteration.unstable.push(mutations);
    // Whole-loop flow facts: a zero-iteration path and every later iteration
    // matter, so entry facts are joined with the final body/update state after
    // the loop. Allocations and root entries stay monotonic.
    let before = BranchFacts::save(lower);
    let result = match statement {
        ast::Stmt::While(loop_) => lower_while(lower, loop_, out, ctx, target),
        ast::Stmt::DoWhile(loop_) => lower_do_while(lower, loop_, out, ctx, target),
        ast::Stmt::For(loop_) => lower_for(lower, loop_, out, ctx, target),
        ast::Stmt::ForOf(loop_) => lower_for_of(lower, loop_, out, ctx, target),
        _ => Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement")),
    };
    let after = BranchFacts::save(lower);
    before.restore(lower);
    after.merge(lower);
    lower.iteration.unstable.pop();
    lower.iteration.control.pop();
    result
}

fn lower_while(
    lower: &mut Lowerer<'_>,
    loop_: &ast::WhileStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
    target: u32,
) -> Result<(), ParseError> {
    let span = loop_.span;
    let line = lower.line(span);
    let slots = alloc_loop(lower, span, out)?;
    let (normal, _, _) = pending(lower)?;
    let mut body = vec![assign(slots.again, Expr::Bool(false), line)];

    // A loop re-executes its test and body. Drop length constants folded from
    // the shape at entry so a later write, alias, or call is read live.
    lower.array_lengths.clear();
    lower.array_length_ids.clear();
    // ES5 12.6.2: the test runs once before each body execution.
    let names = lower.type_names;
    let (flag, condition) = lower.condition_flag(&loop_.test)?;
    lower.type_names = names;

    let mut iteration = Vec::new();
    let body_names = lower.type_names;
    let body_ctx = lower_loop_body_statement(lower, &loop_.body, &mut iteration)?;
    lower.type_names = body_names;
    iteration.extend(loop_value_and_consume(
        lower, &slots, &body_ctx, target, span,
    )?);
    iteration.push(Stmt::IfElse {
        condition: Expr::Var(slots.cont),
        then_branch: vec![assign(slots.again, Expr::Bool(true), line)],
        else_branch: Vec::new(),
        line,
    });

    body.extend(condition);
    body.push(Stmt::IfElse {
        condition: Expr::And(Box::new(Expr::Var(normal)), Box::new(Expr::Var(flag))),
        then_branch: iteration,
        else_branch: vec![break_stmt(line)],
        line,
    });
    let guarded = vec![Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: body,
        else_branch: vec![break_stmt(line)],
        line,
    }];
    out.push(Stmt::While {
        condition: Expr::Var(slots.again),
        body: guarded,
        line,
    });
    finish_loop(lower, &slots, ctx, span, out)?;
    release_loop(lower);
    Ok(())
}

fn lower_do_while(
    lower: &mut Lowerer<'_>,
    loop_: &ast::DoWhileStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
    target: u32,
) -> Result<(), ParseError> {
    let span = loop_.span;
    let line = lower.line(span);
    let slots = alloc_loop(lower, span, out)?;
    let (normal, _, _) = pending(lower)?;
    lower.array_lengths.clear();
    lower.array_length_ids.clear();
    let mut iteration = Vec::new();
    let body_names = lower.type_names;
    let body_ctx = lower_loop_body_statement(lower, &loop_.body, &mut iteration)?;
    lower.type_names = body_names;
    iteration.extend(loop_value_and_consume(
        lower, &slots, &body_ctx, target, span,
    )?);

    // ES5 12.6.1: the test runs after the body, including on own continue,
    // and only while the body phase left Normal true.
    let mut continuation = Vec::new();
    let names = lower.type_names;
    let (flag, mut condition) = lower.condition_flag(&loop_.test)?;
    lower.type_names = names;
    continuation.append(&mut condition);
    continuation.push(Stmt::IfElse {
        condition: Expr::And(Box::new(Expr::Var(normal)), Box::new(Expr::Var(flag))),
        then_branch: vec![assign(slots.again, Expr::Bool(true), line)],
        else_branch: vec![break_stmt(line)],
        line,
    });
    iteration.push(Stmt::IfElse {
        condition: Expr::Var(slots.cont),
        then_branch: continuation,
        else_branch: Vec::new(),
        line,
    });
    let body = vec![
        assign(slots.again, Expr::Bool(false), line),
        Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: iteration,
            else_branch: vec![break_stmt(line)],
            line,
        },
    ];
    out.push(Stmt::While {
        condition: Expr::Var(slots.again),
        body,
        line,
    });
    finish_loop(lower, &slots, ctx, span, out)?;
    release_loop(lower);
    Ok(())
}

fn lower_for(
    lower: &mut Lowerer<'_>,
    loop_: &ast::ForStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
    target: u32,
) -> Result<(), ParseError> {
    let span = loop_.span;
    let line = lower.line(span);
    let (normal, _, _) = pending(lower)?;
    // ES5 12.6.3: init runs exactly once, before V exists.
    match &loop_.init {
        Some(ast::VarDeclOrExpr::VarDecl(declaration)) => {
            let mut init = Vec::new();
            for declarator in &declaration.decls {
                let mut one = Vec::new();
                reference::lower_var_declarator(lower, declaration, declarator, &mut one)?;
                init.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: one,
                    else_branch: Vec::new(),
                    line,
                });
            }
            out.extend(init);
        }
        Some(ast::VarDeclOrExpr::Expr(expression)) => {
            let mut init = Vec::new();
            reference::lower_expr_value(lower, expression, expression.span(), &mut init, false)?;
            out.extend(init);
        }
        None => {}
    }
    lower.array_lengths.clear();
    lower.array_length_ids.clear();
    let slots = alloc_loop(lower, span, out)?;
    let mut iteration = Vec::new();
    if let Some(test) = &loop_.test {
        let names = lower.type_names;
        let (flag, mut condition) = lower.condition_flag(test)?;
        lower.type_names = names;
        iteration.append(&mut condition);
        iteration.push(Stmt::IfElse {
            condition: Expr::And(Box::new(Expr::Var(normal)), Box::new(Expr::Var(flag))),
            then_branch: Vec::new(),
            else_branch: vec![break_stmt(line)],
            line,
        });
    }
    let body_names = lower.type_names;
    let body_ctx = lower_loop_body_statement(lower, &loop_.body, &mut iteration)?;
    lower.type_names = body_names;
    iteration.extend(loop_value_and_consume(
        lower, &slots, &body_ctx, target, span,
    )?);
    // The update runs only after a normal body or this loop's own continue,
    // and after any finally. A throw in the update skips the next test.
    let mut continuation = Vec::new();
    if let Some(update) = &loop_.update {
        reference::lower_expr_value(lower, update, update.span(), &mut continuation, false)?;
    }
    continuation.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: vec![assign(slots.again, Expr::Bool(true), line)],
        else_branch: vec![break_stmt(line)],
        line,
    });
    iteration.push(Stmt::IfElse {
        condition: Expr::Var(slots.cont),
        then_branch: continuation,
        else_branch: Vec::new(),
        line,
    });
    let body = vec![
        assign(slots.again, Expr::Bool(false), line),
        Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: iteration,
            else_branch: vec![break_stmt(line)],
            line,
        },
    ];
    out.push(Stmt::While {
        condition: Expr::Var(slots.again),
        body,
        line,
    });
    finish_loop(lower, &slots, ctx, span, out)?;
    release_loop(lower);
    Ok(())
}

/// Numeric dense-Array `for-of`. The RHS is evaluated once and kept as a
/// rooted handle; each step re-reads length, extent and the number cell.
/// The cursor advances only after `finally`, and only for a normal or
/// own-continue completion.
fn lower_for_of(
    lower: &mut Lowerer<'_>,
    loop_: &ast::ForOfStmt,
    out: &mut Vec<Stmt>,
    ctx: &ListCtx,
    target: u32,
) -> Result<(), ParseError> {
    let span = loop_.span;
    let line = lower.line(span);
    let (normal, _, _) = pending(lower)?;
    let of_slots = alloc_for_of(lower, span, out)?;
    let mut rhs_body = Vec::new();
    let (temp, rhs_shape) = reference::lower_expr_value(
        lower,
        &loop_.right,
        loop_.right.span(),
        &mut rhs_body,
        false,
    )?
    .ok_or_else(|| lower.error(span, "for-of right-hand side produced no value"))?;
    // Only a statically packed source keeps the accepted numeric fastpath.
    let packed_source =
        rhs_shape.is_some_and(|shape| shape.repr == super::lowerer::ArrayRepr::Packed);
    rhs_body.push(assign(of_slots.array, Expr::Var(temp), line));
    lower.refresh_root(of_slots.array, span, &mut rhs_body)?;
    if lower.has_root(temp) && temp != of_slots.array {
        lower.clear_root(temp, span, &mut rhs_body)?;
    }
    let mut check = Vec::new();
    emit_for_of_guard(lower, &of_slots, span, &mut check, !packed_source)?;
    rhs_body.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: check,
        else_branch: Vec::new(),
        line,
    });
    out.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: rhs_body,
        else_branch: Vec::new(),
        line,
    });

    let slots = alloc_loop(lower, span, out)?;
    let (checked_base, helper_read, decode, decode_u32, carrier) = for_of_helpers(lower, span)?;
    let array_get = lower.heap_ref(span)?.helpers.array_get;
    // A statically packed source keeps the exact accepted numeric IR. Every
    // other source (tagged literal, length-unknown join, call result) reads
    // a real JsValue element with runtime kind dispatch and roots the
    // current item across LHS base/key evaluation and the store.
    let mut rooted_item = None;
    if packed_source {
        let mut iteration = Vec::new();
        let base = lower.slot(span)?;
        iteration.push(Stmt::Let {
            index: base,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                checked_base,
                vec![
                    Expr::Var(carrier),
                    value::field(of_slots.array, "ref_slot"),
                    value::field(of_slots.array, "generation"),
                ],
            ),
            line,
        });
        let length = read_decoded(
            lower,
            carrier,
            helper_read,
            decode_u32,
            Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(2))),
            span,
            &mut iteration,
        )?;
        let raw = read_decoded(
            lower,
            carrier,
            helper_read,
            decode,
            Expr::Sub(
                Box::new(Expr::Var(base)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            ),
            span,
            &mut iteration,
        )?;
        let extent = lower.slot(span)?;
        iteration.push(Stmt::Let {
            index: extent,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Sub(
                Box::new(Expr::Var(raw)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            ),
            line,
        });
        let in_range = Expr::And(
            Box::new(Expr::Lt(
                Box::new(Expr::Var(of_slots.cursor)),
                Box::new(Expr::Var(length)),
            )),
            Box::new(Expr::Lt(
                Box::new(Expr::Add(
                    Box::new(Expr::Var(of_slots.cursor)),
                    Box::new(Expr::Int(layout::ARRAY_HEADER_CELLS)),
                )),
                Box::new(Expr::Var(extent)),
            )),
        );
        let mut step = Vec::new();
        let item = lower.slot(span)?;
        step.push(Stmt::Let {
            index: item,
            declared_schema: Some(value::schema()),
            expr: value::number(Lowerer::as_number(Lowerer::guest_call(
                helper_read,
                vec![
                    Expr::Var(carrier),
                    Lowerer::as_number(Expr::Add(
                        Box::new(Expr::Var(base)),
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(of_slots.cursor)),
                            Box::new(Expr::Int(layout::ARRAY_HEADER_CELLS)),
                        )),
                    )),
                ],
            ))),
            line,
        });
        // The key runs only on this iteration, after the item exists. A proved
        // key throw must not replace loop-entry facts or type names.
        let names = lower.type_names;
        let before = BranchFacts::save(lower);
        let ((), lhs_threw) = lower.isolate_conditional_emission(|lower| {
            lower_for_of_lhs(lower, &loop_.left, item, Kind::Number, span, &mut step)
        })?;
        lower.type_names = names;
        if lhs_threw {
            before.restore(lower);
        }
        let names = lower.type_names;
        let mut body_stmts = Vec::new();
        let body_ctx = lower_loop_body_statement(lower, &loop_.body, &mut body_stmts)?;
        lower.type_names = names;
        let mut after_put = body_stmts;
        after_put.extend(loop_value_and_consume(
            lower, &slots, &body_ctx, target, span,
        )?);
        after_put.push(Stmt::IfElse {
            condition: Expr::Var(slots.cont),
            then_branch: vec![
                assign(
                    of_slots.cursor,
                    Expr::Add(Box::new(Expr::Var(of_slots.cursor)), Box::new(Expr::Int(1))),
                    line,
                ),
                assign(slots.again, Expr::Bool(true), line),
            ],
            else_branch: Vec::new(),
            line,
        });
        step.push(Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: after_put,
            else_branch: Vec::new(),
            line,
        });
        iteration.push(Stmt::IfElse {
            condition: in_range,
            then_branch: step,
            else_branch: vec![break_stmt(line)],
            line,
        });
        let body = vec![
            assign(slots.again, Expr::Bool(false), line),
            Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: iteration,
                else_branch: vec![break_stmt(line)],
                line,
            },
        ];
        out.push(Stmt::While {
            condition: Expr::Var(slots.again),
            body,
            line,
        });
    } else {
        let mut iteration = Vec::new();
        let base = lower.slot(span)?;
        iteration.push(Stmt::Let {
            index: base,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                checked_base,
                vec![
                    Expr::Var(carrier),
                    value::field(of_slots.array, "ref_slot"),
                    value::field(of_slots.array, "generation"),
                ],
            ),
            line,
        });
        let kind_num = lower.slot(span)?;
        iteration.push(Stmt::Let {
            index: kind_num,
            declared_schema: Some(TypeSchema::Number),
            expr: super::prelude::heap::read_cell(helper_read, carrier, Expr::Var(base)),
            line,
        });
        let kind_slot = lower.slot(span)?;
        iteration.push(Stmt::Let {
            index: kind_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(decode, vec![Expr::Var(kind_num)]),
            line,
        });
        iteration.push(assert_stmt(
            Expr::Or(
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind_slot)),
                    Box::new(Expr::Int(layout::KIND_ARRAY)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind_slot)),
                    Box::new(Expr::Int(layout::KIND_TAGGED_ARRAY)),
                )),
            ),
            line,
        ));
        let length = read_decoded(
            lower,
            carrier,
            helper_read,
            decode_u32,
            Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(2))),
            span,
            &mut iteration,
        )?;
        let raw = read_decoded(
            lower,
            carrier,
            helper_read,
            decode,
            Expr::Sub(
                Box::new(Expr::Var(base)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            ),
            span,
            &mut iteration,
        )?;
        let extent = lower.slot(span)?;
        iteration.push(Stmt::Let {
            index: extent,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Sub(
                Box::new(Expr::Var(raw)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            ),
            line,
        });
        let stride = lower.slot(span)?;
        iteration.push(Stmt::Let {
            index: stride,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::IfElse {
                condition: Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind_slot)),
                    Box::new(Expr::Int(layout::KIND_ARRAY)),
                )),
                then_expr: Box::new(Expr::Int(1)),
                else_expr: Box::new(Expr::Int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY)),
            },
            line,
        });
        let offset = lower.slot(span)?;
        iteration.push(Stmt::Let {
            index: offset,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Add(
                Box::new(Expr::Int(layout::ARRAY_HEADER_CELLS)),
                Box::new(Expr::Mul(
                    Box::new(Expr::Var(stride)),
                    Box::new(Expr::Var(of_slots.cursor)),
                )),
            ),
            line,
        });
        // Public length, not the physical extent. array_get returns undefined
        // for a logical hole and does not read past the block.
        let in_range = Expr::Lt(
            Box::new(Expr::Var(of_slots.cursor)),
            Box::new(Expr::Var(length)),
        );
        let mut step = Vec::new();
        let item = lower.slot(span)?;
        step.push(Stmt::Let {
            index: item,
            declared_schema: Some(value::schema()),
            expr: value::undefined(),
            line,
        });
        lower.refresh_root(item, span, &mut step)?;
        let got = Lowerer::guest_call(
            array_get,
            vec![
                Expr::Var(carrier),
                Expr::Var(base),
                Expr::Var(of_slots.cursor),
            ],
        );
        step.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: item,
            expr: got,
            line,
        });
        lower.refresh_root(item, span, &mut step)?;
        rooted_item = Some(item);
        // The key runs only on this iteration, after the item exists and is
        // rooted. A proved key throw must not replace loop-entry facts or
        // type names.
        let names = lower.type_names;
        let before = BranchFacts::save(lower);
        let ((), lhs_threw) = lower.isolate_conditional_emission(|lower| {
            lower_for_of_lhs(lower, &loop_.left, item, Kind::Dynamic, span, &mut step)
        })?;
        lower.type_names = names;
        if lhs_threw {
            before.restore(lower);
        }
        let names = lower.type_names;
        let mut body_stmts = Vec::new();
        let body_ctx = lower_loop_body_statement(lower, &loop_.body, &mut body_stmts)?;
        lower.type_names = names;
        let mut after_put = body_stmts;
        after_put.extend(loop_value_and_consume(
            lower, &slots, &body_ctx, target, span,
        )?);
        after_put.push(Stmt::IfElse {
            condition: Expr::Var(slots.cont),
            then_branch: vec![
                assign(
                    of_slots.cursor,
                    Expr::Add(Box::new(Expr::Var(of_slots.cursor)), Box::new(Expr::Int(1))),
                    line,
                ),
                assign(slots.again, Expr::Bool(true), line),
            ],
            else_branch: Vec::new(),
            line,
        });
        step.push(Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: after_put,
            else_branch: Vec::new(),
            line,
        });
        iteration.push(Stmt::IfElse {
            condition: in_range,
            then_branch: step,
            else_branch: vec![break_stmt(line)],
            line,
        });
        let body = vec![
            assign(slots.again, Expr::Bool(false), line),
            Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: iteration,
                else_branch: vec![break_stmt(line)],
                line,
            },
        ];
        out.push(Stmt::While {
            condition: Expr::Var(slots.again),
            body,
            line,
        });
    }
    if let Some(item) = rooted_item {
        lower.clear_root(item, span, out)?;
    }
    finish_loop(lower, &slots, ctx, span, out)?;
    lower.clear_root(of_slots.array, span, out)?;
    release_loop(lower);
    release_for_of(lower);
    Ok(())
}

fn for_of_helpers(
    lower: &Lowerer<'_>,
    span: Span,
) -> Result<(u16, u16, u16, u16, u16), ParseError> {
    let heap = lower.heap_ref(span)?;
    let checked_base = heap.helpers.checked_base;
    let helper_read = heap.helper_read;
    let decode = lower
        .helper_decode
        .ok_or_else(|| lower.error(span, "for-of requires the realm decode helper"))?;
    let decode_u32 = heap.helpers.decode_u32;
    let carrier = lower.active_carrier(span)?;
    Ok((checked_base, helper_read, decode, decode_u32, carrier))
}

#[allow(clippy::too_many_arguments)]
fn emit_for_of_guard(
    lower: &mut Lowerer<'_>,
    slots: &ForOfSlots,
    span: Span,
    out: &mut Vec<Stmt>,
    accept_tagged: bool,
) -> Result<(), ParseError> {
    let (checked_base, helper_read, decode, _decode_u32, carrier) = for_of_helpers(lower, span)?;
    let line = lower.line(span);
    let mut object_arm = Vec::new();
    let base = lower.slot(span)?;
    object_arm.push(Stmt::Let {
        index: base,
        declared_schema: Some(TypeSchema::Int),
        expr: Lowerer::guest_call(
            checked_base,
            vec![
                Expr::Var(carrier),
                value::field(slots.array, "ref_slot"),
                value::field(slots.array, "generation"),
            ],
        ),
        line,
    });
    let kind = read_decoded(
        lower,
        carrier,
        helper_read,
        decode,
        Expr::Var(base),
        span,
        &mut object_arm,
    )?;
    let accepted = vec![assign(slots.cursor, Expr::Int(0), line)];
    let mut rejected = Vec::new();
    lower.raise_managed_type_error(span, &mut rejected)?;
    let kind_condition = if accept_tagged {
        Expr::Or(
            Box::new(Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_ARRAY)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_TAGGED_ARRAY)),
            )),
        )
    } else {
        Expr::Eq(
            Box::new(Expr::Var(kind)),
            Box::new(Expr::Int(layout::KIND_ARRAY)),
        )
    };
    object_arm.push(Stmt::IfElse {
        condition: kind_condition,
        then_branch: accepted,
        else_branch: rejected,
        line,
    });
    let mut primitive = Vec::new();
    lower.raise_managed_type_error(span, &mut primitive)?;
    out.push(Stmt::IfElse {
        condition: Expr::Eq(
            Box::new(Lowerer::as_number(value::field(slots.array, "tag"))),
            Box::new(Lowerer::as_number(Expr::Int(value::OBJECT))),
        ),
        then_branch: object_arm,
        else_branch: primitive,
        line,
    });
    Ok(())
}

fn read_decoded(
    lower: &mut Lowerer<'_>,
    carrier: u16,
    helper_read: u16,
    decode: u16,
    addr: Expr,
    span: Span,
    out: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let slot = lower.slot(span)?;
    out.push(Stmt::Let {
        index: slot,
        declared_schema: Some(TypeSchema::Int),
        expr: Lowerer::guest_call(
            decode,
            vec![Lowerer::guest_call(
                helper_read,
                vec![Expr::Var(carrier), Lowerer::as_number(addr)],
            )],
        ),
        line: lower.line(span),
    });
    Ok(slot)
}

fn lower_for_of_lhs(
    lower: &mut Lowerer<'_>,
    head: &ast::ForHead,
    item: u16,
    item_kind: super::lowerer::Kind,
    span: Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let number = super::lowerer::Lowered {
        expr: Expr::Var(item),
        kind: item_kind,
        array_length: None,
        owned_roots: Vec::new(),
    };
    match head {
        ast::ForHead::VarDecl(declaration) => {
            let ast::Pat::Ident(binding) = &declaration.decls[0].name else {
                return Err(lower.error(span, "for-of head is not an identifier"));
            };
            reference::store_local(lower, binding.id.sym.as_str(), binding.span, number, out)?;
            Ok(())
        }
        ast::ForHead::Pat(pat) => match pat.as_ref() {
            ast::Pat::Ident(binding) => {
                reference::store_local(lower, binding.id.sym.as_str(), binding.span, number, out)?;
                Ok(())
            }
            ast::Pat::Expr(expr) => {
                let member = for_of_member(expr).ok_or_else(|| {
                    lower.error(expr.span(), "for-of assignment target must be a member")
                })?;
                lower.put_member_slot(member, item, span, out)
            }
            _ => Err(lower.error(span, "for-of head is not an admitted assignment target")),
        },
        _ => Err(lower.error(span, "for-of head is not an admitted assignment target")),
    }
}

fn for_of_member(expr: &ast::Expr) -> Option<&ast::MemberExpr> {
    match expr {
        ast::Expr::Paren(paren) => for_of_member(&paren.expr),
        ast::Expr::Member(member) => Some(member),
        _ => None,
    }
}

fn alloc_for_of(
    lower: &mut Lowerer<'_>,
    span: Span,
    out: &mut Vec<Stmt>,
) -> Result<ForOfSlots, ParseError> {
    let line = lower.line(span);
    let depth = lower.iteration.for_of_depth;
    if let Some(slots) = lower.iteration.for_of_pool.get(depth).copied() {
        lower.iteration.for_of_depth += 1;
        // Each statement is lowered under its own `if normal` guard, so a
        // sibling loop cannot see the earlier Let. Re-declare, matching
        // alloc_loop, instead of assigning an unavailable local.
        out.push(Stmt::Let {
            index: slots.array,
            declared_schema: Some(value::schema()),
            expr: value::undefined(),
            line,
        });
        lower.refresh_root(slots.array, span, out)?;
        out.push(Stmt::Let {
            index: slots.cursor,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        });
        return Ok(slots);
    }
    let array = lower.slot(span)?;
    out.push(Stmt::Let {
        index: array,
        declared_schema: Some(value::schema()),
        expr: value::undefined(),
        line,
    });
    lower.refresh_root(array, span, out)?;
    let cursor = lower.slot(span)?;
    out.push(Stmt::Let {
        index: cursor,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    let slots = ForOfSlots { array, cursor };
    lower.iteration.for_of_pool.push(slots);
    lower.iteration.for_of_depth += 1;
    Ok(slots)
}

fn release_for_of(lower: &mut Lowerer<'_>) {
    lower.iteration.for_of_depth = lower.iteration.for_of_depth.saturating_sub(1);
}

/// Per-iteration tail shared by all three loop forms: update the loop value
/// from the body completion (ES5 step "if stmt.value is not empty"), then
/// consume exactly this loop's own break/continue. Foreign abrupt completions
/// leave Normal false and the continuation flag false, so the Core loop exits
/// with the pending Completion intact.
fn loop_value_and_consume(
    lower: &mut Lowerer<'_>,
    slots: &LoopSlots,
    body: &ListCtx,
    target: u32,
    span: Span,
) -> Result<Vec<Stmt>, ParseError> {
    let (normal, completion, _) = pending(lower)?;
    let empty = empty_record(lower)?;
    let line = lower.line(span);
    let mut stmts = vec![assign(slots.cont, Expr::Bool(false), line)];

    let mut from_body = vec![assign(slots.value, Expr::Var(body.value), line)];
    lower.refresh_root(slots.value, span, &mut from_body)?;
    from_body.push(assign(slots.has, Expr::Bool(true), line));
    let mut normal_actions = vec![Stmt::IfElse {
        condition: Expr::Var(body.has),
        then_branch: from_body,
        else_branch: Vec::new(),
        line,
    }];
    normal_actions.push(assign(slots.cont, Expr::Bool(true), line));

    let mut from_abrupt = vec![assign(
        slots.value,
        completion_field(completion, "value"),
        line,
    )];
    lower.refresh_root(slots.value, span, &mut from_abrupt)?;
    from_abrupt.push(assign(slots.has, Expr::Bool(true), line));
    let mut abrupt = vec![Stmt::IfElse {
        condition: completion_field(completion, "has_value"),
        then_branch: from_abrupt,
        else_branch: Vec::new(),
        line,
    }];
    let own_break = jump_condition(completion, value::COMPLETION_BREAK, target);
    let own_continue = jump_condition(completion, value::COMPLETION_CONTINUE, target);
    let continue_actions = vec![
        assign(normal, Expr::Bool(true), line),
        assign(completion, Expr::Var(empty), line),
        assign(slots.cont, Expr::Bool(true), line),
    ];
    abrupt.push(Stmt::IfElse {
        condition: own_break,
        then_branch: vec![assign(normal, Expr::Bool(true), line)],
        else_branch: vec![Stmt::IfElse {
            condition: own_continue,
            then_branch: continue_actions,
            else_branch: Vec::new(),
            line,
        }],
        line,
    });
    stmts.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: normal_actions,
        else_branch: abrupt,
        line,
    });
    Ok(stmts)
}

fn jump_condition(completion: u16, kind: i64, target: u32) -> Expr {
    Expr::And(
        Box::new(Expr::Eq(
            Box::new(completion_field(completion, "kind")),
            Box::new(Expr::Int(kind)),
        )),
        Box::new(Expr::Eq(
            Box::new(completion_field(completion, "target")),
            Box::new(Expr::Int(i64::from(target))),
        )),
    )
}

/// After the Core loop: a Normal exit (test exhaustion or this loop's own
/// break) stays the nominal Normal(empty) completion. The only place the loop
/// publishes V is the enclosing StatementList accumulator, and that merge is
/// guarded by `has`: ES5.1 UpdateEmpty leaves the previous list value intact
/// when V stayed empty. A foreign abrupt completion is left exactly as the
/// loop propagated it.
fn finish_loop(
    lower: &mut Lowerer<'_>,
    slots: &LoopSlots,
    ctx: &ListCtx,
    span: Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let (normal, completion, _) = pending(lower)?;
    let empty = empty_record(lower)?;
    let line = lower.line(span);
    let mut merge = vec![assign(ctx.value, Expr::Var(slots.value), line)];
    lower.refresh_root(ctx.value, span, &mut merge)?;
    merge.push(assign(ctx.has, Expr::Bool(true), line));
    out.push(Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch: vec![
            // ES5 UpdateEmpty only when V is nonempty: an empty loop leaves the
            // enclosing StatementList value untouched.
            Stmt::IfElse {
                condition: Expr::Var(slots.has),
                then_branch: merge,
                else_branch: Vec::new(),
                line,
            },
            assign(completion, Expr::Var(empty), line),
        ],
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

fn lower_loop_body_statement(
    lower: &mut Lowerer<'_>,
    body: &ast::Stmt,
    out: &mut Vec<Stmt>,
) -> Result<ListCtx, ParseError> {
    let span = body.span();
    let ctx = alloc_list(lower, span, out)?;
    let ((), _body_definite) = lower.isolate_conditional_emission(|lower| {
        lower_stmt(lower, body, out, &ctx)?;
        post_statement(lower, body, out, &ctx)?;
        Ok(())
    })?;
    release_list(lower);
    Ok(ctx)
}

fn collect_mutations(statement: &ast::Stmt, lower: &Lowerer<'_>, out: &mut HashSet<BindingId>) {
    match statement {
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => {
            for declarator in &declaration.decls {
                if let ast::Pat::Ident(binding) = &declarator.name
                    && let NameHit::Local(id) = reference::resolve(lower, binding.id.sym.as_str())
                {
                    out.insert(id);
                }
                if let Some(init) = declarator.init.as_deref() {
                    collect_expr_mutations(init, lower, out);
                }
            }
        }
        ast::Stmt::Decl(ast::Decl::Fn(_)) => {}
        ast::Stmt::Expr(expr) => collect_expr_mutations(&expr.expr, lower, out),
        ast::Stmt::Block(block) => {
            for statement in &block.stmts {
                collect_mutations(statement, lower, out);
            }
        }
        ast::Stmt::If(if_stmt) => {
            collect_expr_mutations(&if_stmt.test, lower, out);
            collect_mutations(&if_stmt.cons, lower, out);
            if let Some(alt) = &if_stmt.alt {
                collect_mutations(alt, lower, out);
            }
        }
        ast::Stmt::While(loop_) => {
            collect_expr_mutations(&loop_.test, lower, out);
            collect_mutations(&loop_.body, lower, out);
        }
        ast::Stmt::DoWhile(loop_) => {
            collect_mutations(&loop_.body, lower, out);
            collect_expr_mutations(&loop_.test, lower, out);
        }
        ast::Stmt::For(loop_) => {
            if let Some(init) = &loop_.init {
                match init {
                    ast::VarDeclOrExpr::VarDecl(declaration) => {
                        collect_mutations(
                            &ast::Stmt::Decl(ast::Decl::Var(declaration.clone())),
                            lower,
                            out,
                        );
                    }
                    ast::VarDeclOrExpr::Expr(expression) => {
                        collect_expr_mutations(expression, lower, out);
                    }
                }
            }
            if let Some(test) = &loop_.test {
                collect_expr_mutations(test, lower, out);
            }
            if let Some(update) = &loop_.update {
                collect_expr_mutations(update, lower, out);
            }
            collect_mutations(&loop_.body, lower, out);
        }
        ast::Stmt::ForOf(loop_) => {
            match &loop_.left {
                ast::ForHead::VarDecl(declaration) => {
                    for declarator in &declaration.decls {
                        if let ast::Pat::Ident(binding) = &declarator.name
                            && let NameHit::Local(id) =
                                reference::resolve(lower, binding.id.sym.as_str())
                        {
                            out.insert(id);
                        }
                    }
                }
                ast::ForHead::Pat(pat) => match pat.as_ref() {
                    ast::Pat::Ident(binding) => {
                        if let NameHit::Local(id) =
                            reference::resolve(lower, binding.id.sym.as_str())
                        {
                            out.insert(id);
                        }
                    }
                    ast::Pat::Expr(expr) => {
                        collect_expr_mutations(expr, lower, out);
                    }
                    _ => {}
                },
                _ => {}
            }
            collect_expr_mutations(&loop_.right, lower, out);
            collect_mutations(&loop_.body, lower, out);
        }
        ast::Stmt::Labeled(labeled) => collect_mutations(&labeled.body, lower, out),
        ast::Stmt::Switch(select) => {
            collect_expr_mutations(&select.discriminant, lower, out);
            for case in &select.cases {
                if let Some(test) = &case.test {
                    collect_expr_mutations(test, lower, out);
                }
                for statement in &case.cons {
                    collect_mutations(statement, lower, out);
                }
            }
        }
        ast::Stmt::Return(ret) => {
            if let Some(arg) = ret.arg.as_deref() {
                collect_expr_mutations(arg, lower, out);
            }
        }
        ast::Stmt::Throw(throw) => collect_expr_mutations(&throw.arg, lower, out),
        ast::Stmt::Try(try_stmt) => {
            for statement in &try_stmt.block.stmts {
                collect_mutations(statement, lower, out);
            }
            if let Some(handler) = &try_stmt.handler {
                for statement in &handler.body.stmts {
                    collect_mutations(statement, lower, out);
                }
            }
            if let Some(finalizer) = &try_stmt.finalizer {
                for statement in &finalizer.stmts {
                    collect_mutations(statement, lower, out);
                }
            }
        }
        _ => {}
    }
}

fn collect_expr_mutations(
    expression: &ast::Expr,
    lower: &Lowerer<'_>,
    out: &mut HashSet<BindingId>,
) {
    match expression {
        ast::Expr::Assign(assign) => {
            if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(binding)) = &assign.left
                && let NameHit::Local(id) = reference::resolve(lower, binding.id.sym.as_str())
            {
                out.insert(id);
            }
            collect_expr_mutations(&assign.right, lower, out);
        }
        ast::Expr::Bin(binary) => {
            collect_expr_mutations(&binary.left, lower, out);
            collect_expr_mutations(&binary.right, lower, out);
        }
        ast::Expr::Unary(unary) => collect_expr_mutations(&unary.arg, lower, out),
        ast::Expr::Update(update) => {
            let mut arg = update.arg.as_ref();
            while let ast::Expr::Paren(paren) = arg {
                arg = &paren.expr;
            }
            if let ast::Expr::Ident(ident) = arg
                && let NameHit::Local(id) = reference::resolve(lower, ident.sym.as_str())
            {
                out.insert(id);
            }
            collect_expr_mutations(&update.arg, lower, out);
        }
        ast::Expr::Paren(paren) => collect_expr_mutations(&paren.expr, lower, out),
        ast::Expr::Cond(branch) => {
            collect_expr_mutations(&branch.test, lower, out);
            collect_expr_mutations(&branch.cons, lower, out);
            collect_expr_mutations(&branch.alt, lower, out);
        }
        ast::Expr::Seq(sequence) => {
            for expression in &sequence.exprs {
                collect_expr_mutations(expression, lower, out);
            }
        }
        ast::Expr::Call(call) => {
            if let ast::Callee::Expr(callee) = &call.callee {
                collect_expr_mutations(callee, lower, out);
            }
            for arg in &call.args {
                collect_expr_mutations(&arg.expr, lower, out);
            }
        }
        ast::Expr::Member(member) => {
            collect_expr_mutations(&member.obj, lower, out);
            if let ast::MemberProp::Computed(computed) = &member.prop {
                collect_expr_mutations(&computed.expr, lower, out);
            }
        }
        ast::Expr::Array(array) => {
            for element in array.elems.iter().flatten() {
                collect_expr_mutations(&element.expr, lower, out);
            }
        }
        ast::Expr::Object(object) => {
            for prop in &object.props {
                if let ast::PropOrSpread::Prop(boxed) = prop
                    && let ast::Prop::KeyValue(pair) = boxed.as_ref()
                {
                    collect_expr_mutations(&pair.value, lower, out);
                }
            }
        }
        _ => {}
    }
}
