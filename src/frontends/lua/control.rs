//! Chunk scalar control lowering.
//!
//! Production `LuaPlugin` does not call this module. A test `SourcePlugin`
//! feeds the same parser and resolver, then asks here for Core IR. This is
//! not a file entry and not a fallback to the line lowerer.
//!
//! A chunk with no goto and no early return is emitted as ordinary
//! statements, including a source `while` or `repeat`. Any goto, or a return
//! that is not the chunk tail, becomes a private integer state loop. Each basic
//! block is one state. A transfer assigns that state and `Continue`s the dispatch
//! loop, so it is emitted only in the dispatch arm, never inside a nested
//! source loop. `break` and `continue` inside a source loop stay Core
//! statements of that loop. A source `repeat` is one Core `for`, not a copied
//! body. Its private bool starts true, so the body runs before `until`. The
//! `for` post stores whether another round is required. Fallthrough and
//! `continue` both reach that post; `break` skips it and leaves this `for`,
//! not an outer dispatch loop. `until` is placed once and can read the
//! same-round body locals. In this numeric/bool slice only `false` is falsy:
//! `0` and every other number are truthy, so `until 0` stops after one body.
//! `if`, `elseif`, `while`, and `not` / `!` pass the operand through
//! `core_truthy` before a Core branch or `Not`. `until` does the same, once,
//! in the `for` post. The operand expression is placed once. The state id is
//! a private Core int and is not a Lua number.
//!
//! A numeric `for` is also one Core `for`, in the same dispatch arm as the
//! surrounding chunk. Its three header expressions are assigned once, left to
//! right, into private float slots. The default step is `+1`. Those slots are
//! declared at the chunk root so a nested loop can assign them again without
//! a second `let`. The visible index is a different slot, copied from the
//! hidden counter at the start of each iteration, so a write to the Lua local
//! does not move the counter and a same-name header reference still sees the
//! outer binding. Core has no sign-bit opcode, so the condition is one call of
//! `lua_control::numeric_for_holds`. The test catalog owns that predicate:
//! direction is the step's sign bit, and the closed interval uses ordered
//! comparisons. `continue` reaches the post, which adds the saved step to the
//! hidden counter. `break` skips that post and leaves this `for`.
//!
use super::ast::{self, BinOp, Block, Chunk, Expr, Name, NumberKind, Span, Stmt, UnOp};
use super::lexer;
use super::parser;
use super::resolve::{self, Resolution, ResolveError};
use vm::compiler::HostCallResolver;
use vm::{
    AssignmentKind, Expr as CoreExpr, FrontendIr, LocalIrBuilder, LocalSlot, ParseError,
    Stmt as CoreStmt, TypeSchema,
};

const STATE_SLOT: &str = "$lua_dispatch_state";
const RESULT_SLOT: &str = "$lua_return_value";
const EXIT_STATE: i64 = 0;

pub(super) const NUMERIC_FOR_HOLDS: &str = "lua_control::numeric_for_holds";

pub(super) fn lower_source(source: &str) -> Result<FrontendIr, ParseError> {
    lower_source_with(source, None)
}

pub(super) fn lower_source_with(
    source: &str,
    holds: Option<&HostCallResolver<'_>>,
) -> Result<FrontendIr, ParseError> {
    let chunk = parser::parse_chunk(source.as_bytes()).map_err(syntax_error)?;
    let resolution = resolve::resolve(&chunk);
    if let Some(error) = resolution.errors.first() {
        let line = line_of(source, error_span(error));
        return Err(ParseError::at_line(line, static_message(error)));
    }
    let mut lower = Lower::new(source, &resolution, holds);
    let stmts = if needs_dispatch(&chunk) {
        lower.lower_dispatch(&chunk)?
    } else {
        lower.lower_direct(&chunk)?
    };
    Ok(lower.builder.finish(stmts))
}

struct Lower<'a> {
    source: &'a str,
    resolution: &'a Resolution,
    builder: LocalIrBuilder,
    slots: Vec<Option<LocalSlot>>,
    dispatching: bool,
    state: Option<LocalSlot>,
    result: Option<LocalSlot>,
    label_state: Vec<Option<i64>>,
    repeat_flags: u32,
    nfor_ordinal: u32,
    nfor_slots: Vec<LocalSlot>,
    for_indexes: Vec<LocalSlot>,
    holds: Option<&'a HostCallResolver<'a>>,
}

struct BlockBuf {
    finished: Vec<BuiltBlock>,
    current: Vec<Item>,
    current_label: Option<usize>,
}

struct BuiltBlock {
    label: Option<usize>,
    items: Vec<Item>,
    exit: Exit,
}

enum Item {
    Core(CoreStmt),
    If {
        cond: CoreExpr,
        then_body: Vec<Item>,
        else_body: Vec<Item>,
        line: u32,
    },
    Goto {
        label: usize,
        line: u32,
    },
    Return {
        expr: CoreExpr,
        line: u32,
    },
}

enum Exit {
    Fallthrough,
    Goto { label: usize, line: u32 },
    Return { expr: CoreExpr, line: u32 },
    Stop,
}

impl<'a> Lower<'a> {
    fn new(
        source: &'a str,
        resolution: &'a Resolution,
        holds: Option<&'a HostCallResolver<'a>>,
    ) -> Self {
        let slots = vec![None; resolution.bindings.len()];
        let label_state = vec![None; resolution.labels.len()];
        Self {
            source,
            resolution,
            builder: LocalIrBuilder::new(),
            slots,
            dispatching: false,
            state: None,
            result: None,
            label_state,
            repeat_flags: 0,
            nfor_ordinal: 0,
            nfor_slots: Vec::new(),
            for_indexes: Vec::new(),
            holds,
        }
    }

    fn lower_direct(&mut self, chunk: &Chunk) -> Result<Vec<CoreStmt>, ParseError> {
        let (mut stmts, value) = self.straight(&chunk.body.stmts, false)?;
        if let Some(expr) = value {
            stmts.push(CoreStmt::Expr { expr, line: 1 });
        }
        Ok(self.hoist_numeric_for(stmts))
    }

    fn hoist_numeric_for(&self, stmts: Vec<CoreStmt>) -> Vec<CoreStmt> {
        let mut root = Vec::new();
        for slot in &self.nfor_slots {
            root.push(let_float(*slot, CoreExpr::Float(0.0), 1));
        }
        for slot in &self.for_indexes {
            root.push(let_float(*slot, CoreExpr::Float(0.0), 1));
        }
        root.extend(stmts);
        root
    }

    fn lower_dispatch(&mut self, chunk: &Chunk) -> Result<Vec<CoreStmt>, ParseError> {
        self.dispatching = true;
        let mut blocks = BlockBuf::new();
        self.walk_list(&chunk.body.stmts, &mut blocks)?;
        blocks.finish();
        if blocks.finished.is_empty() {
            return Err(self.fail_line(1, "empty dispatch chunk"));
        }
        for (index, block) in blocks.finished.iter().enumerate() {
            if let Some(label) = block.label {
                self.label_state[label] = Some(self.state_id_of(index));
            }
        }
        let state = self.alloc_private(STATE_SLOT)?;
        self.state = Some(state);
        if contains_return_block(&chunk.body) {
            let result = self.alloc_private(RESULT_SLOT)?;
            self.result = Some(result);
        }
        let mut arms = Vec::with_capacity(blocks.finished.len());
        for (index, block) in blocks.finished.iter().enumerate() {
            let state_id = index as i64 + 1;
            let mut body = self.emit_items(&block.items)?;
            body.extend(self.emit_exit(&block.exit, index, blocks.finished.len())?);
            arms.push((state_id, body));
        }
        let chain = dispatch_chain(state, arms);
        // Core requires a `let` on every path before an assign. The arms do
        // not dominate each other, so user slots are declared here. The
        // source initializer still runs at the declaration, as an assign.
        let mut root = Vec::new();
        for slot in self.slots.iter().flatten().copied() {
            root.push(let_expr(slot, CoreExpr::Null, 1));
        }
        for slot in &self.nfor_slots {
            root.push(let_float(*slot, CoreExpr::Float(0.0), 1));
        }
        root.push(let_expr(state, CoreExpr::Int(1), 1));
        if let Some(result) = self.result {
            root.push(let_expr(result, CoreExpr::Null, 1));
        }
        root.push(CoreStmt::While {
            condition: CoreExpr::Not(Box::new(CoreExpr::Eq(
                Box::new(CoreExpr::Var(state)),
                Box::new(CoreExpr::Int(EXIT_STATE)),
            ))),
            body: vec![chain],
            line: 1,
        });
        if let Some(result) = self.result {
            root.push(CoreStmt::Expr {
                expr: CoreExpr::Var(result),
                line: 1,
            });
        }
        Ok(root)
    }

    fn state_id_of(&self, block_index: usize) -> i64 {
        i64::try_from(block_index).expect("block index") + 1
    }

    fn walk_list(&mut self, stmts: &[Stmt], blocks: &mut BlockBuf) -> Result<(), ParseError> {
        for stmt in stmts {
            match stmt {
                Stmt::Empty { .. } => {}
                Stmt::Do { body, .. } => self.walk_list(&body.stmts, blocks)?,
                Stmt::Label { name, .. } => {
                    let index = self.label_index(name)?;
                    blocks.split_label(index);
                }
                Stmt::Goto { span, .. } => {
                    let label = self.goto_target(*span)?;
                    blocks.flush(Exit::Goto {
                        label,
                        line: self.line(*span),
                    });
                }
                Stmt::Return { values, span } => {
                    blocks.flush(Exit::Return {
                        expr: self.one_value(values, *span)?,
                        line: self.line(*span),
                    });
                }
                Stmt::NumericFor {
                    name,
                    start,
                    limit,
                    step,
                    body,
                    span,
                } => {
                    for stmt in
                        self.numeric_for_stmts(name, start, limit, step.as_ref(), body, *span)?
                    {
                        blocks.current.push(Item::Core(stmt));
                    }
                }
                other => blocks.current.push(self.arm_item(other)?),
            }
        }
        Ok(())
    }

    fn arm_item(&mut self, stmt: &Stmt) -> Result<Item, ParseError> {
        match stmt {
            Stmt::Empty { .. } => Ok(Item::Core(CoreStmt::Noop { line: 1 })),
            Stmt::Local {
                names,
                values,
                span,
            } => Ok(Item::Core(self.local_stmt(names, values, *span)?)),
            Stmt::Assign {
                targets,
                values,
                span,
            } => Ok(Item::Core(self.assign_stmt(targets, values, *span)?)),
            Stmt::If {
                arms,
                else_body,
                span,
            } => self.if_item(arms, else_body.as_ref(), *span),
            Stmt::While { cond, body, span } => Ok(Item::Core(self.while_stmt(cond, body, *span)?)),
            Stmt::Repeat { body, until, span } => {
                Ok(Item::Core(self.repeat_stmt(body, until, *span)?))
            }
            Stmt::Break { span } | Stmt::Continue { span } => {
                Err(self.fail(*span, "break or continue outside a source loop"))
            }
            Stmt::Goto { span, .. } => Ok(Item::Goto {
                label: self.goto_target(*span)?,
                line: self.line(*span),
            }),
            Stmt::Return { values, span } => Ok(Item::Return {
                expr: self.one_value(values, *span)?,
                line: self.line(*span),
            }),
            Stmt::Label { span, .. } => Err(self.fail(
                *span,
                "a label inside if or a transferred branch is not in this slice",
            )),
            other => Err(self.fail(
                stmt_span(other),
                "statement is not in this chunk control slice",
            )),
        }
    }

    fn branch_items(&mut self, stmts: &[Stmt]) -> Result<Vec<Item>, ParseError> {
        let mut items = Vec::new();
        for stmt in stmts {
            if matches!(stmt, Stmt::Empty { .. }) {
                continue;
            }
            if let Stmt::Do { body, .. } = stmt {
                items.extend(self.branch_items(&body.stmts)?);
                continue;
            }
            if let Stmt::NumericFor {
                name,
                start,
                limit,
                step,
                body,
                span,
            } = stmt
            {
                for core in
                    self.numeric_for_stmts(name, start, limit, step.as_ref(), body, *span)?
                {
                    items.push(Item::Core(core));
                }
                continue;
            }
            items.push(self.arm_item(stmt)?);
        }
        Ok(items)
    }

    fn if_item(
        &mut self,
        arms: &[ast::IfArm],
        else_body: Option<&Block>,
        span: Span,
    ) -> Result<Item, ParseError> {
        if arms.is_empty() {
            return Err(self.fail(span, "if has no arm"));
        }
        if if_has_label(arms, else_body) {
            return Err(self.fail(span, "a label inside if is not in this slice"));
        }
        if !if_has_transfer(arms, else_body) {
            return Ok(Item::Core(self.if_core(arms, else_body, span, false)?));
        }
        let line = self.line(span);
        let mut else_body_items = match else_body {
            Some(body) => self.branch_items(&body.stmts)?,
            None => Vec::new(),
        };
        for arm in arms.iter().rev() {
            let then_body = self.branch_items(&arm.body.stmts)?;
            let cond = core_truthy(self.expr(&arm.cond)?);
            else_body_items = vec![Item::If {
                cond,
                then_body,
                else_body: else_body_items,
                line,
            }];
        }
        else_body_items
            .pop()
            .ok_or_else(|| self.fail(span, "if has no arm"))
    }

    fn if_core(
        &mut self,
        arms: &[ast::IfArm],
        else_body: Option<&Block>,
        span: Span,
        in_loop: bool,
    ) -> Result<CoreStmt, ParseError> {
        let line = self.line(span);
        let mut else_branch = match else_body {
            Some(body) => {
                let (stmts, value) = self.straight(&body.stmts, in_loop)?;
                if value.is_some() {
                    return Err(self.fail(span, "return inside if is not a direct tail"));
                }
                stmts
            }
            None => Vec::new(),
        };
        for arm in arms.iter().rev() {
            let (then_branch, value) = self.straight(&arm.body.stmts, in_loop)?;
            if value.is_some() {
                return Err(self.fail(span, "return inside if is not a direct tail"));
            }
            else_branch = vec![CoreStmt::IfElse {
                condition: core_truthy(self.expr(&arm.cond)?),
                then_branch,
                else_branch,
                line,
            }];
        }
        else_branch
            .pop()
            .ok_or_else(|| self.fail(span, "if has no arm"))
    }

    fn while_stmt(
        &mut self,
        cond: &Expr,
        body: &Block,
        span: Span,
    ) -> Result<CoreStmt, ParseError> {
        if contains_transfer(body) || contains_label(body) {
            return Err(self.fail(
                span,
                "goto, label, or return inside a loop is not in this slice",
            ));
        }
        let (body_stmts, value) = self.straight(&body.stmts, true)?;
        if value.is_some() {
            return Err(self.fail(span, "return inside a loop is not in this slice"));
        }
        Ok(CoreStmt::While {
            condition: core_truthy(self.expr(cond)?),
            body: body_stmts,
            line: self.line(span),
        })
    }

    fn repeat_stmt(
        &mut self,
        body: &Block,
        until: &Expr,
        span: Span,
    ) -> Result<CoreStmt, ParseError> {
        if contains_transfer(body) || contains_label(body) {
            return Err(self.fail(
                span,
                "goto, label, or return inside a loop is not in this slice",
            ));
        }
        let (body_stmts, value) = self.straight(&body.stmts, true)?;
        if value.is_some() {
            return Err(self.fail(span, "return inside a loop is not in this slice"));
        }
        let line = self.line(span);
        let again = self.alloc_repeat_flag()?;
        let until_value = self.expr(until)?;
        // Core `while` tests its condition before the body and sends
        // `continue` there, so `until` cannot live in that condition: it
        // would run before the body locals exist, and a Core `continue`
        // would skip it. A `for` post is reached only after the body or a
        // `continue`, and a `break` in the body skips the post. The private
        // flag is a Core bool, not a Lua value.
        Ok(CoreStmt::For {
            init: Box::new(let_expr(again, CoreExpr::Bool(true), line)),
            condition: CoreExpr::Var(again),
            post: Box::new(set_slot(
                again,
                CoreExpr::Not(Box::new(core_truthy(until_value))),
                line,
            )),
            body: body_stmts,
            line,
        })
    }

    fn numeric_for_stmts(
        &mut self,
        name: &Name,
        start: &Expr,
        limit: &Expr,
        step: Option<&Expr>,
        body: &Block,
        span: Span,
    ) -> Result<Vec<CoreStmt>, ParseError> {
        if contains_transfer(body) || contains_label(body) {
            return Err(self.fail(
                span,
                "goto, label, or return inside a loop is not in this slice",
            ));
        }
        let (body_stmts, value) = self.straight(&body.stmts, true)?;
        if value.is_some() {
            return Err(self.fail(span, "return inside a loop is not in this slice"));
        }
        let line = self.line(span);
        let visible = self.slot(self.binding_for_index(name, body)?)?;
        if !self.dispatching {
            self.for_indexes.push(visible);
        }
        let hidden = self.alloc_nfor("index")?;
        let limit_slot = self.alloc_nfor("limit")?;
        let step_slot = self.alloc_nfor("step")?;
        // `expr` builds IR. The assigns below are the only runtime evaluation,
        // left to right, once per time this loop statement is reached.
        let start_expr = self.expr(start)?;
        let limit_expr = self.expr(limit)?;
        let step_expr = match step {
            Some(expr) => self.expr(expr)?,
            None => CoreExpr::Float(1.0),
        };
        let mut loop_body = vec![set_slot(visible, CoreExpr::Var(hidden), line)];
        loop_body.extend(body_stmts);
        Ok(vec![
            set_slot(hidden, start_expr, line),
            set_slot(limit_slot, limit_expr, line),
            set_slot(step_slot, step_expr, line),
            CoreStmt::For {
                init: Box::new(CoreStmt::Noop { line }),
                condition: self.holds_call(hidden, limit_slot, step_slot, span)?,
                post: Box::new(set_slot(
                    hidden,
                    CoreExpr::Add(
                        Box::new(CoreExpr::Var(hidden)),
                        Box::new(CoreExpr::Var(step_slot)),
                    ),
                    line,
                )),
                body: loop_body,
                line,
            },
        ])
    }

    fn holds_call(
        &mut self,
        index: LocalSlot,
        limit: LocalSlot,
        step: LocalSlot,
        span: Span,
    ) -> Result<CoreExpr, ParseError> {
        let Some(resolver) = self.holds else {
            return Err(self.fail(span, "numeric for predicate is not bound"));
        };
        let signature = [TypeSchema::Float, TypeSchema::Float, TypeSchema::Float];
        let resolved = resolver
            .resolve(NUMERIC_FOR_HOLDS, &signature)
            .map_err(|error| self.fail(span, format!("numeric for predicate: {error}")))?;
        if resolved.return_type != TypeSchema::Bool
            || resolved.params.len() != signature.len()
            || resolved
                .params
                .iter()
                .any(|param| param.schema != TypeSchema::Float)
        {
            return Err(self.fail(
                span,
                "numeric for predicate signature is not three floats to bool",
            ));
        }
        self.builder
            .declare_function(NUMERIC_FOR_HOLDS, Some(signature.len() as u8))?;
        let index_fn = self
            .builder
            .function_index(NUMERIC_FOR_HOLDS)
            .ok_or_else(|| self.fail(span, "numeric for predicate was not declared"))?;
        Ok(CoreExpr::Call(
            index_fn,
            Vec::new(),
            vec![
                CoreExpr::Var(index),
                CoreExpr::Var(limit),
                CoreExpr::Var(step),
            ],
            Some(Box::new(resolved)),
            None,
        ))
    }

    fn straight(
        &mut self,
        stmts: &[Stmt],
        in_loop: bool,
    ) -> Result<(Vec<CoreStmt>, Option<CoreExpr>), ParseError> {
        let mut out = Vec::new();
        for (index, stmt) in stmts.iter().enumerate() {
            let last = index + 1 == stmts.len();
            match stmt {
                Stmt::Empty { .. } => {}
                Stmt::Return { values, span } => {
                    if !last || in_loop {
                        return Err(self.fail(*span, "early return needs dispatch"));
                    }
                    return Ok((out, Some(self.one_value(values, *span)?)));
                }
                Stmt::Do { body, .. } => {
                    let (inner, value) = self.straight(&body.stmts, in_loop)?;
                    out.extend(inner);
                    if value.is_some() {
                        if !last || in_loop {
                            return Err(self.fail(body.span, "early return needs dispatch"));
                        }
                        return Ok((out, value));
                    }
                }
                Stmt::Break { span } => {
                    if !in_loop {
                        return Err(self.fail(*span, "break outside a source loop"));
                    }
                    out.push(CoreStmt::Break {
                        line: self.line(*span),
                    });
                }
                Stmt::Continue { span } => {
                    if !in_loop {
                        return Err(self.fail(*span, "continue outside a source loop"));
                    }
                    out.push(CoreStmt::Continue {
                        line: self.line(*span),
                    });
                }
                Stmt::Goto { span, .. } | Stmt::Label { span, .. } => {
                    return Err(self.fail(*span, "goto or label in a direct statement"));
                }
                Stmt::NumericFor {
                    name,
                    start,
                    limit,
                    step,
                    body,
                    span,
                } => {
                    out.extend(self.numeric_for_stmts(
                        name,
                        start,
                        limit,
                        step.as_ref(),
                        body,
                        *span,
                    )?);
                }
                other => out.push(self.straight_stmt(other, in_loop)?),
            }
        }
        Ok((out, None))
    }

    fn straight_stmt(&mut self, stmt: &Stmt, in_loop: bool) -> Result<CoreStmt, ParseError> {
        match stmt {
            Stmt::Local {
                names,
                values,
                span,
            } => self.local_stmt(names, values, *span),
            Stmt::Assign {
                targets,
                values,
                span,
            } => self.assign_stmt(targets, values, *span),
            Stmt::If {
                arms,
                else_body,
                span,
            } => self.if_core(arms, else_body.as_ref(), *span, in_loop),
            Stmt::While { cond, body, span } => self.while_stmt(cond, body, *span),
            Stmt::Repeat { body, until, span } => self.repeat_stmt(body, until, *span),
            Stmt::Break { span } if in_loop => Ok(CoreStmt::Break {
                line: self.line(*span),
            }),
            Stmt::Continue { span } if in_loop => Ok(CoreStmt::Continue {
                line: self.line(*span),
            }),
            other => Err(self.fail(
                stmt_span(other),
                "statement is not in this chunk control slice",
            )),
        }
    }

    fn local_stmt(
        &mut self,
        names: &[ast::LocalName],
        values: &ast::ExprList,
        span: Span,
    ) -> Result<CoreStmt, ParseError> {
        if names.len() != 1 || values.exprs.len() != 1 {
            return Err(self.fail(span, "this slice lowers one local and one initializer"));
        }
        if names[0].const_binding {
            return Err(self.fail(span, "const locals are not in this slice"));
        }
        let init = self.expr(&values.exprs[0])?;
        let index = self.binding_for_local(&names[0].name, span)?;
        let slot = self.slot(index)?;
        let line = self.line(span);
        if self.dispatching {
            Ok(set_slot(slot, init, line))
        } else {
            Ok(let_expr(slot, init, line))
        }
    }

    fn assign_stmt(
        &mut self,
        targets: &[ast::AssignTarget],
        values: &ast::ExprList,
        span: Span,
    ) -> Result<CoreStmt, ParseError> {
        if targets.len() != 1 || values.exprs.len() != 1 {
            return Err(self.fail(span, "this slice lowers one assignment"));
        }
        let ast::AssignTarget::Name(name) = &targets[0] else {
            return Err(self.fail(span, "only a local name can be assigned in this slice"));
        };
        let expr = self.expr(&values.exprs[0])?;
        let index = self.binding_at(&name.bytes, name.span)?;
        let slot = self.slot(index)?;
        Ok(set_slot(slot, expr, self.line(span)))
    }

    fn one_value(&mut self, values: &ast::ExprList, span: Span) -> Result<CoreExpr, ParseError> {
        if values.exprs.len() != 1 {
            return Err(self.fail(span, "this slice returns one value"));
        }
        self.expr(&values.exprs[0])
    }

    fn expr(&mut self, expr: &Expr) -> Result<CoreExpr, ParseError> {
        match expr {
            Expr::Bool { value, .. } => Ok(CoreExpr::Bool(*value)),
            Expr::Number { raw, kind, span } => self.number(raw, *kind, *span),
            Expr::Name { name, .. } => {
                let index = self.binding_at(&name.bytes, name.span)?;
                let slot = self.slot(index)?;
                Ok(CoreExpr::Var(slot))
            }
            Expr::Paren { inner, .. } => self.expr(inner),
            Expr::Unary { op, expr, span } => {
                let inner = Box::new(self.expr(expr)?);
                match op {
                    UnOp::Not | UnOp::Bang => Ok(CoreExpr::Not(Box::new(core_truthy(*inner)))),
                    UnOp::Neg => Ok(CoreExpr::Neg(inner)),
                    _ => Err(self.fail(*span, "unary operator is not in this slice")),
                }
            }
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => {
                let lhs = self.expr(left)?;
                let rhs = self.expr(right)?;
                self.bin(*op, lhs, rhs, *span)
            }
            Expr::BinaryChain { base, steps, span } => {
                let mut acc = self.expr(base)?;
                for step in steps {
                    let rhs = self.expr(&step.expr)?;
                    acc = self.bin(step.op, acc, rhs, *span)?;
                }
                Ok(acc)
            }
            other => Err(self.fail(expr_span(other), "expression is not in this slice")),
        }
    }

    fn bin(
        &self,
        op: BinOp,
        lhs: CoreExpr,
        rhs: CoreExpr,
        span: Span,
    ) -> Result<CoreExpr, ParseError> {
        let lhs = Box::new(lhs);
        let rhs = Box::new(rhs);
        match op {
            BinOp::Add => Ok(CoreExpr::Add(lhs, rhs)),
            BinOp::Sub => Ok(CoreExpr::Sub(lhs, rhs)),
            BinOp::Mul => Ok(CoreExpr::Mul(lhs, rhs)),
            BinOp::Eq => Ok(CoreExpr::Eq(lhs, rhs)),
            BinOp::Lt => Ok(CoreExpr::Lt(lhs, rhs)),
            BinOp::Gt => Ok(CoreExpr::Gt(lhs, rhs)),
            BinOp::Ne => Ok(CoreExpr::Not(Box::new(CoreExpr::Eq(lhs, rhs)))),
            _ => Err(self.fail(span, "operator is not in this slice")),
        }
    }

    fn number(&self, raw: &str, kind: NumberKind, span: Span) -> Result<CoreExpr, ParseError> {
        match kind {
            NumberKind::Integer | NumberKind::Float => {}
            _ => return Err(self.fail(span, "only decimal binary64 literals are in this slice")),
        }
        let value = raw
            .parse::<f64>()
            .map_err(|_| self.fail(span, format!("not a binary64 literal: {raw}")))?;
        if !value.is_finite() {
            return Err(self.fail(span, "non-finite number is not in this slice"));
        }
        Ok(CoreExpr::Float(value))
    }

    fn emit_items(&self, items: &[Item]) -> Result<Vec<CoreStmt>, ParseError> {
        let mut out = Vec::new();
        for item in items {
            out.extend(self.emit_item(item)?);
        }
        Ok(out)
    }

    fn emit_item(&self, item: &Item) -> Result<Vec<CoreStmt>, ParseError> {
        match item {
            Item::Core(stmt) => Ok(vec![stmt.clone()]),
            Item::If {
                cond,
                then_body,
                else_body,
                line,
            } => Ok(vec![CoreStmt::IfElse {
                condition: cond.clone(),
                then_branch: self.emit_items(then_body)?,
                else_branch: self.emit_items(else_body)?,
                line: *line,
            }]),
            Item::Goto { label, line } => Ok(self.transfer(self.label_state_id(*label)?, *line)),
            Item::Return { expr, line } => Ok(self.return_transfer(expr.clone(), *line)?),
        }
    }

    fn emit_exit(
        &self,
        exit: &Exit,
        index: usize,
        len: usize,
    ) -> Result<Vec<CoreStmt>, ParseError> {
        match exit {
            Exit::Fallthrough => {
                let next = if index + 1 < len {
                    self.state_id_of(index + 1)
                } else {
                    EXIT_STATE
                };
                Ok(self.transfer(next, 1))
            }
            Exit::Goto { label, line } => Ok(self.transfer(self.label_state_id(*label)?, *line)),
            Exit::Return { expr, line } => self.return_transfer(expr.clone(), *line),
            Exit::Stop => Ok(self.transfer(EXIT_STATE, 1)),
        }
    }

    fn transfer(&self, target: i64, line: u32) -> Vec<CoreStmt> {
        let state = self.state.expect("dispatch state");
        vec![
            set_slot(state, CoreExpr::Int(target), line),
            CoreStmt::Continue { line },
        ]
    }

    fn return_transfer(&self, expr: CoreExpr, line: u32) -> Result<Vec<CoreStmt>, ParseError> {
        let Some(result) = self.result else {
            return Err(self.fail_line(line, "return without a result slot"));
        };
        let mut stmts = vec![set_slot(result, expr, line)];
        stmts.extend(self.transfer(EXIT_STATE, line));
        Ok(stmts)
    }

    fn label_state_id(&self, label: usize) -> Result<i64, ParseError> {
        self.label_state
            .get(label)
            .copied()
            .flatten()
            .ok_or_else(|| {
                self.fail_line(
                    1,
                    format!("label {label} was not placed in a dispatch block"),
                )
            })
    }

    fn binding_for_local(&self, name: &Name, stmt_span: Span) -> Result<usize, ParseError> {
        self.resolution
            .bindings
            .iter()
            .position(|binding| {
                binding.function_id == 0
                    && binding.name == name.bytes
                    && binding.visible_from == stmt_span.end
            })
            .ok_or_else(|| self.fail(name.span, "local has no resolver binding"))
    }

    fn binding_for_index(&self, name: &Name, body: &Block) -> Result<usize, ParseError> {
        self.resolution
            .bindings
            .iter()
            .position(|binding| {
                binding.function_id == 0
                    && binding.name == name.bytes
                    && binding.visible_from == body.span.start
            })
            .ok_or_else(|| self.fail(name.span, "numeric for index has no resolver binding"))
    }

    fn binding_at(&self, name: &[u8], span: Span) -> Result<usize, ParseError> {
        let mut best: Option<(usize, u32)> = None;
        for (index, binding) in self.resolution.bindings.iter().enumerate() {
            if binding.function_id != 0 || binding.name != name {
                continue;
            }
            if span.start < binding.visible_from || span.start >= binding.visible_to {
                continue;
            }
            if best.is_none_or(|(_, from)| binding.visible_from >= from) {
                best = Some((index, binding.visible_from));
            }
        }
        best.map(|(index, _)| index).ok_or_else(|| {
            self.fail(
                span,
                format!("unresolved local {}", String::from_utf8_lossy(name)),
            )
        })
    }

    fn slot(&mut self, index: usize) -> Result<LocalSlot, ParseError> {
        if let Some(slot) = self.slots[index] {
            return Ok(slot);
        }
        let name = format!("$binding_{index}");
        let slot = self.alloc_private(&name)?;
        self.slots[index] = Some(slot);
        Ok(slot)
    }

    fn alloc_private(&mut self, name: &str) -> Result<LocalSlot, ParseError> {
        self.builder.alloc_local_named(name)
    }

    fn alloc_repeat_flag(&mut self) -> Result<LocalSlot, ParseError> {
        let name = format!("$lua_repeat_again_{}", self.repeat_flags);
        self.repeat_flags += 1;
        self.alloc_private(&name)
    }

    fn alloc_nfor(&mut self, role: &str) -> Result<LocalSlot, ParseError> {
        let name = format!("$lua_nfor_{role}_{}", self.nfor_ordinal);
        self.nfor_ordinal += 1;
        let slot = self.alloc_private(&name)?;
        self.nfor_slots.push(slot);
        Ok(slot)
    }

    fn goto_target(&self, span: Span) -> Result<usize, ParseError> {
        let goto = self
            .resolution
            .gotos
            .iter()
            .find(|goto| goto.span == span)
            .ok_or_else(|| self.fail(span, "goto has no resolver record"))?;
        goto.target
            .ok_or_else(|| self.fail(span, "goto has no resolved target"))
    }

    fn label_index(&self, name: &Name) -> Result<usize, ParseError> {
        self.resolution
            .labels
            .iter()
            .position(|label| label.span == name.span)
            .ok_or_else(|| self.fail(name.span, "label has no resolver record"))
    }

    fn line(&self, span: Span) -> u32 {
        u32::try_from(line_of(self.source, span)).unwrap_or(u32::MAX)
    }

    fn fail(&self, span: Span, message: impl Into<String>) -> ParseError {
        ParseError::at_line(self.line(span) as usize, message.into())
    }

    fn fail_line(&self, line: u32, message: impl Into<String>) -> ParseError {
        ParseError::at_line(line as usize, message.into())
    }
}

impl BlockBuf {
    fn new() -> Self {
        Self {
            finished: Vec::new(),
            current: Vec::new(),
            current_label: None,
        }
    }

    fn split_label(&mut self, index: usize) {
        if self.current.is_empty() && self.current_label.is_none() {
            self.current_label = Some(index);
            return;
        }
        self.flush(Exit::Fallthrough);
        self.current_label = Some(index);
    }

    fn flush(&mut self, exit: Exit) {
        self.finished.push(BuiltBlock {
            label: self.current_label.take(),
            items: std::mem::take(&mut self.current),
            exit,
        });
    }

    fn finish(&mut self) {
        if self.current.is_empty() && self.current_label.is_none() {
            if self.finished.is_empty() {
                self.flush(Exit::Stop);
            }
            return;
        }
        self.flush(Exit::Stop);
    }
}

fn dispatch_chain(state: LocalSlot, arms: Vec<(i64, Vec<CoreStmt>)>) -> CoreStmt {
    let mut pending = vec![CoreStmt::Break { line: 1 }];
    for (state_id, body) in arms.into_iter().rev() {
        pending = vec![CoreStmt::IfElse {
            condition: CoreExpr::Eq(
                Box::new(CoreExpr::Var(state)),
                Box::new(CoreExpr::Int(state_id)),
            ),
            then_branch: body,
            else_branch: pending,
            line: 1,
        }];
    }
    pending
        .pop()
        .expect("dispatch chain includes the unknown-state break")
}

fn core_truthy(value: CoreExpr) -> CoreExpr {
    // Core equality keeps bool and float apart, so this matches only `false`.
    // `0` and every other number stay truthy. The value expression is used once.
    CoreExpr::Not(Box::new(CoreExpr::Eq(
        Box::new(value),
        Box::new(CoreExpr::Bool(false)),
    )))
}

fn let_expr(slot: LocalSlot, expr: CoreExpr, line: u32) -> CoreStmt {
    CoreStmt::Let {
        index: slot,
        declared_schema: None,
        expr,
        line,
    }
}

fn let_float(slot: LocalSlot, expr: CoreExpr, line: u32) -> CoreStmt {
    CoreStmt::Let {
        index: slot,
        declared_schema: Some(TypeSchema::Float),
        expr,
        line,
    }
}

fn set_slot(slot: LocalSlot, expr: CoreExpr, line: u32) -> CoreStmt {
    CoreStmt::Assign {
        kind: AssignmentKind::Set,
        index: slot,
        expr,
        line,
    }
}

fn needs_dispatch(chunk: &Chunk) -> bool {
    contains_goto(&chunk.body) || has_early_return(&chunk.body.stmts)
}

fn has_early_return(stmts: &[Stmt]) -> bool {
    if stmts.is_empty() {
        return false;
    }
    let last = stmts.len() - 1;
    if stmts[..last].iter().any(contains_return_stmt) {
        return true;
    }
    match &stmts[last] {
        Stmt::Return { .. } => false,
        Stmt::Do { body, .. } => has_early_return(&body.stmts),
        other => contains_return_stmt(other),
    }
}

fn contains_goto(block: &Block) -> bool {
    block.stmts.iter().any(stmt_contains_goto)
}

fn stmt_contains_goto(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Goto { .. } => true,
        Stmt::Do { body, .. } | Stmt::While { body, .. } | Stmt::Repeat { body, .. } => {
            contains_goto(body)
        }
        Stmt::If {
            arms, else_body, ..
        } => {
            arms.iter().any(|arm| contains_goto(&arm.body))
                || else_body.as_ref().is_some_and(contains_goto)
        }
        Stmt::NumericFor { body, .. } | Stmt::GenericFor { body, .. } => contains_goto(body),
        _ => false,
    }
}

fn contains_return_block(block: &Block) -> bool {
    block.stmts.iter().any(contains_return_stmt)
}

fn contains_return_stmt(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } => true,
        Stmt::Do { body, .. } | Stmt::While { body, .. } | Stmt::Repeat { body, .. } => {
            contains_return_block(body)
        }
        Stmt::If {
            arms, else_body, ..
        } => {
            arms.iter().any(|arm| contains_return_block(&arm.body))
                || else_body.as_ref().is_some_and(contains_return_block)
        }
        Stmt::NumericFor { body, .. } | Stmt::GenericFor { body, .. } => {
            contains_return_block(body)
        }
        _ => false,
    }
}

fn contains_transfer(block: &Block) -> bool {
    block.stmts.iter().any(stmt_contains_transfer)
}

fn stmt_contains_transfer(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Goto { .. } | Stmt::Return { .. } => true,
        Stmt::Do { body, .. } | Stmt::While { body, .. } | Stmt::Repeat { body, .. } => {
            contains_transfer(body)
        }
        Stmt::If {
            arms, else_body, ..
        } => {
            arms.iter().any(|arm| contains_transfer(&arm.body))
                || else_body.as_ref().is_some_and(contains_transfer)
        }
        Stmt::NumericFor { body, .. } | Stmt::GenericFor { body, .. } => contains_transfer(body),
        _ => false,
    }
}

fn contains_label(block: &Block) -> bool {
    block.stmts.iter().any(stmt_contains_label)
}

fn stmt_contains_label(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Label { .. } => true,
        Stmt::Do { body, .. } | Stmt::While { body, .. } | Stmt::Repeat { body, .. } => {
            contains_label(body)
        }
        Stmt::If {
            arms, else_body, ..
        } => {
            arms.iter().any(|arm| contains_label(&arm.body))
                || else_body.as_ref().is_some_and(contains_label)
        }
        Stmt::NumericFor { body, .. } | Stmt::GenericFor { body, .. } => contains_label(body),
        _ => false,
    }
}

fn if_has_label(arms: &[ast::IfArm], else_body: Option<&Block>) -> bool {
    arms.iter().any(|arm| contains_label(&arm.body)) || else_body.is_some_and(contains_label)
}

fn if_has_transfer(arms: &[ast::IfArm], else_body: Option<&Block>) -> bool {
    arms.iter().any(|arm| contains_transfer(&arm.body)) || else_body.is_some_and(contains_transfer)
}

fn stmt_span(stmt: &Stmt) -> Span {
    match stmt {
        Stmt::Empty { span }
        | Stmt::Do { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Repeat { span, .. }
        | Stmt::If { span, .. }
        | Stmt::NumericFor { span, .. }
        | Stmt::GenericFor { span, .. }
        | Stmt::Local { span, .. }
        | Stmt::LocalFunction { span, .. }
        | Stmt::Function { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::CompoundAssign { span, .. }
        | Stmt::Call { span, .. }
        | Stmt::Label { span, .. }
        | Stmt::Goto { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span }
        | Stmt::Return { span, .. } => *span,
    }
}

fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::Nil { span }
        | Expr::Bool { span, .. }
        | Expr::Number { span, .. }
        | Expr::String { span, .. }
        | Expr::Vararg { span }
        | Expr::Name { span, .. }
        | Expr::Paren { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Binary { span, .. }
        | Expr::BinaryChain { span, .. }
        | Expr::Ternary { span, .. }
        | Expr::SuffixChain { span, .. }
        | Expr::Function { span, .. }
        | Expr::ShortFunction { span, .. }
        | Expr::Table { span, .. } => *span,
    }
}

fn syntax_error(error: lexer::SyntaxError) -> ParseError {
    ParseError::at_line(
        usize::try_from(error.line).unwrap_or(usize::MAX),
        error.message,
    )
}

fn line_of(source: &str, span: Span) -> usize {
    let end = usize::try_from(span.start).unwrap_or(source.len());
    let prefix = &source.as_bytes()[..end.min(source.len())];
    1 + prefix.iter().filter(|byte| **byte == b'\n').count()
}

fn error_span(error: &ResolveError) -> Span {
    match error {
        ResolveError::JumpIntoLocal { goto_span, .. }
        | ResolveError::UndefinedLabel {
            span: goto_span, ..
        }
        | ResolveError::DuplicateLabel {
            span: goto_span, ..
        }
        | ResolveError::BreakOutsideLoop { span: goto_span }
        | ResolveError::ContinueOutsideLoop { span: goto_span }
        | ResolveError::AssignConst {
            span: goto_span, ..
        }
        | ResolveError::RedeclareConst {
            span: goto_span, ..
        }
        | ResolveError::VarargOutside { span: goto_span } => *goto_span,
    }
}

fn static_message(error: &ResolveError) -> String {
    match error {
        ResolveError::JumpIntoLocal { local_name, .. } => format!(
            "jump into the scope of local {}",
            String::from_utf8_lossy(local_name)
        ),
        ResolveError::UndefinedLabel { name, .. } => {
            format!("undefined label {}", String::from_utf8_lossy(name))
        }
        ResolveError::DuplicateLabel { name, .. } => {
            format!("duplicate label {}", String::from_utf8_lossy(name))
        }
        ResolveError::BreakOutsideLoop { .. } => "break outside loop".to_string(),
        ResolveError::ContinueOutsideLoop { .. } => "continue outside loop".to_string(),
        ResolveError::AssignConst { name, .. } => {
            format!("assign to const {}", String::from_utf8_lossy(name))
        }
        ResolveError::RedeclareConst { name, .. } => {
            format!("redeclare const {}", String::from_utf8_lossy(name))
        }
        ResolveError::VarargOutside { .. } => "vararg outside vararg function".to_string(),
    }
}
