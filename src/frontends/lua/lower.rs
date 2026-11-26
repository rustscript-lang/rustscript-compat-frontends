//! Test-only dynamic completion lowering for the Lua protected-call preflight.
//!
//! Production `LuaPlugin` does not call this module. A test `SourcePlugin`
//! feeds the real lexer, parser and resolver, then asks here for Core IR. It
//! is not a file entry, a second VM, or a fallback to the line lowerer.
//!
//! Every Lua function is lowered to the private completion `[Bool, ...values]`:
//!
//! - `true` at slot 0 means the call returned; slots 1.. are exactly the
//!   source-visible values. Zero returns is `[true]`, one nil is `[true, nil]`.
//! - `false` at slot 0 means an explicit guest error raised by `error(value, 0)`;
//!   slot 1 is the original object and no further values exist.
//!
//! Only call sites whose callee is statically admitted to be a Lua callable
//! produced by this lowering read that completion. A call of a host import
//! returns an ordinary value and is never unpacked by array shape. A real
//! `VmError` from a host call is not transformed into a guest tag; it stops the
//! VM on the normal error path.
//!
//! Admission is a whole-chunk static analysis, not a fixture property. A
//! parameter may be invoked dynamically only when every argument that reaches
//! that parameter from a statically visible call site (including `pcall`
//! forwarding) is itself a Lua function literal or another admitted parameter.
//! A missing argument is padded with nil at the Lua call boundary and counts
//! as a possible non-Lua value, so it taints the parameter for callable
//! admission even when another call site supplies a function literal. A
//! callable binding must come from a function literal in this chunk and must
//! never be reassigned. Non-Lua data may flow through parameters and be
//! returned; only the dynamic callee position is constrained. Anything else is
//! refused at lowering time.
//!
//! The slice deliberately refuses unsupported statements, expressions,
//! multi-value forms, and extra arguments instead of guessing. Missing
//! arguments are padded with nil, matching the Lua call boundary, but return
//! values are never padded: the completion arity is the executed arity.
//!
//! `pcall` is a lowering form, not a callable value: it performs one dynamic
//! `CallValue` on its target and turns the target's completion into a private
//! sequence. `error(value, 0)` is a lowering form that completes the current
//! Lua function with `[false, value]` immediately. Both are limited to the
//! exact shapes documented here.
//!
//! A second private entry, [`lower_source_with_roots`], keeps this completion
//! and admission contract but wraps every lowered function in one real LuaHeap
//! root frame. Each admitted Lua call then hands its error payload to the
//! caller's reserved temp slot while the callee frame is still the top frame;
//! only after that write is the callee frame retired. The chunk's protected
//! receive moves the payload from the temp slot into its formal slot. That
//! entry is compiled only by the root-handoff test. Production `LuaPlugin`
//! never reaches it.
use std::collections::{HashMap, HashSet};

use super::ast::{
    Args, Block, Chunk, Expr as LuaExpr, ExprList, Field, FuncBody, Name, Span, Stmt as LuaStmt,
    Suffix,
};
use super::lexer;
use super::parser;
use super::resolve::{self, BindingKind, Resolution, ResolveError};
use vm::compiler::HostCallResolver;
use vm::{
    BuiltinFunction, ClosureExpr, Expr, FrontendIr, LocalIrBuilder, LocalSlot, ParseError, Stmt,
    TypeSchema,
};

/// The only globals this test candidate binds as host imports. They are real
/// host functions injected by the test, not guest callables.
const HOST_IMPORTS: &[&str] = &["mark", "note", "fault", "tick"];

/// Guest-visible host calls admitted only by the private root-handoff mode.
/// They allocate real LuaHeap tables; no Core array stands in for a table.
const ROOT_GUEST_IMPORTS: &[&str] = &["newtable", "setchild"];

/// Private root transport calls. Their host bodies live in the test; the
/// lowerer only emits the frame protocol around the existing completion
/// transport.
const ROOT_ENTER: &str = "lua_root_enter";
const ROOT_HANDOFF: &str = "lua_root_handoff";
const ROOT_FORMAL: &str = "lua_root_formal";
const ROOT_FINISH: &str = "lua_root_finish";

/// Slots reserved in every root frame: slot 0 receives the handoff payload
/// while the callee frame is still live; slot 1 receives the formal result of
/// a `pcall` in that function. Both start empty.
const ROOT_FRAME_WIDTH: i64 = 2;
const ROOT_HANDOFF_SLOT: i64 = 0;
const ROOT_FORMAL_SLOT: i64 = 1;

pub(super) fn lower_source_with(
    source: &str,
    hosts: Option<&HostCallResolver<'_>>,
) -> Result<FrontendIr, ParseError> {
    lower_source(source, hosts, false)
}

/// Private root-handoff entry. Same lexer, parser, resolver, admission
/// analysis and completion transport as [`lower_source_with`]; the only
/// addition is the real Lua heap frame protocol around each admitted Lua call.
/// Production `LuaPlugin` does not call this entry.
pub(super) fn lower_source_with_roots(
    source: &str,
    hosts: Option<&HostCallResolver<'_>>,
) -> Result<FrontendIr, ParseError> {
    lower_source(source, hosts, true)
}

fn lower_source(
    source: &str,
    hosts: Option<&HostCallResolver<'_>>,
    roots: bool,
) -> Result<FrontendIr, ParseError> {
    let chunk = parser::parse_chunk(source.as_bytes()).map_err(syntax_error)?;
    let resolution = resolve::resolve(&chunk);
    if let Some(error) = resolution.errors.first() {
        return Err(ParseError::at_line(
            line_of(source, error_span(error)),
            static_message(error),
        ));
    }
    if let Some(limit) = resolution.limits.first() {
        let kind = match limit.kind {
            resolve::LexicalLimitKind::Locals => "active local slots",
            resolve::LexicalLimitKind::Upvalues => "distinct upvalues",
        };
        return Err(ParseError::at_line(
            line_of(source, limit.span),
            format!(
                "Lua lexical limit exceeded: {kind} ({} > {})",
                limit.count, limit.limit
            ),
        ));
    }
    let mut lower = Lower::new(source, &resolution, hosts, roots);
    lower.analyze(&chunk)?;
    let mut pre = Vec::new();
    let chunk_frame = if roots {
        let frame = lower.alloc_temp("root_chunk")?;
        pre.push(Stmt::Let {
            index: frame,
            declared_schema: None,
            expr: lower.root_enter_call()?,
            line: 1,
        });
        Some(frame)
    } else {
        None
    };
    lower.push_frame(chunk.span, None, chunk_frame);
    let body = lower.lower_block(&chunk.body.stmts)?;
    let body = match chunk_frame {
        Some(frame) => lower.root_finish_call(body, frame, 1)?,
        None => body,
    };
    lower.pop_frame();
    pre.push(Stmt::Expr {
        expr: body,
        line: 1,
    });
    let mut ir = lower.builder.finish(pre);
    ir.implicit_extern_names = Vec::new();
    Ok(ir)
}

#[derive(Clone, Copy, Debug)]
struct Proof {
    arity: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallClass {
    /// The expression evaluates to a private `[Bool, ...values]` completion of
    /// a Lua callable that was statically admitted.
    Lua,
    /// The expression evaluates to a private sequence `[ok, ...values]` of one
    /// `pcall` target call. It never raises the guest error itself.
    Pcall,
    /// The expression evaluates to the explicit error object argument.
    Error,
    /// The expression evaluates to an ordinary host call result.
    Host,
}

struct Site<'a> {
    callee_binding: Option<usize>,
    literal: Option<u32>,
    pcall: bool,
    args: Vec<&'a LuaExpr>,
}

struct Frame {
    span: Span,
    declared: HashSet<usize>,
    captures: HashMap<usize, LocalSlot>,
    capture_copies: Vec<(LocalSlot, LocalSlot)>,
    /// Binding of the local function currently being created, if any. A use of
    /// that binding inside its own body would capture a not-yet-assigned slot.
    creating: Option<usize>,
    /// Core local holding this function's Lua heap root-frame token in the
    /// private root-handoff mode. `None` in the completion-only mode.
    root: Option<LocalSlot>,
}

struct Lower<'a> {
    source: &'a str,
    resolution: &'a Resolution,
    hosts: Option<&'a HostCallResolver<'a>>,
    roots: bool,
    builder: LocalIrBuilder,
    binding_slots: Vec<Option<LocalSlot>>,
    frames: Vec<Frame>,
    temp: u32,
    body_by_id: HashMap<u32, &'a FuncBody>,
    function_body_of_binding: Vec<Option<u32>>,
    param_bindings: HashMap<u32, Vec<usize>>,
    possible: Vec<HashSet<u32>>,
    non_lua: Vec<bool>,
    assigned: Vec<bool>,
    sites: Vec<Site<'a>>,
}

impl<'a> Lower<'a> {
    fn new(
        source: &'a str,
        resolution: &'a Resolution,
        hosts: Option<&'a HostCallResolver<'a>>,
        roots: bool,
    ) -> Self {
        let count = resolution.bindings.len();
        Self {
            source,
            resolution,
            hosts,
            roots,
            builder: LocalIrBuilder::new(),
            binding_slots: vec![None; count],
            frames: Vec::new(),
            temp: 0,
            body_by_id: HashMap::new(),
            function_body_of_binding: vec![None; count],
            param_bindings: HashMap::new(),
            possible: vec![HashSet::new(); count],
            non_lua: vec![false; count],
            assigned: vec![false; count],
            sites: Vec::new(),
        }
    }

    // ---- static admission -------------------------------------------------

    fn analyze(&mut self, chunk: &'a Chunk) -> Result<(), ParseError> {
        self.walk_block(&chunk.body)?;
        self.propagate();
        Ok(())
    }

    fn walk_block(&mut self, block: &'a Block) -> Result<(), ParseError> {
        for stmt in &block.stmts {
            self.walk_stmt(stmt)?;
        }
        Ok(())
    }

    fn walk_stmt(&mut self, stmt: &'a LuaStmt) -> Result<(), ParseError> {
        match stmt {
            LuaStmt::Empty { .. }
            | LuaStmt::Break { .. }
            | LuaStmt::Continue { .. }
            | LuaStmt::Goto { .. }
            | LuaStmt::Label { .. } => Ok(()),
            LuaStmt::Do { body, .. }
            | LuaStmt::While { body, .. }
            | LuaStmt::Repeat { body, .. }
            | LuaStmt::NumericFor { body, .. }
            | LuaStmt::GenericFor { body, .. } => {
                match stmt {
                    LuaStmt::While { cond, .. } => self.walk_expr(cond)?,
                    LuaStmt::Repeat { until, .. } => self.walk_expr(until)?,
                    LuaStmt::NumericFor {
                        start, limit, step, ..
                    } => {
                        self.walk_expr(start)?;
                        self.walk_expr(limit)?;
                        if let Some(step) = step {
                            self.walk_expr(step)?;
                        }
                    }
                    LuaStmt::GenericFor { iters, .. } => self.walk_expr_list(iters)?,
                    _ => {}
                }
                self.walk_block(body)
            }
            LuaStmt::If {
                arms, else_body, ..
            } => {
                for arm in arms {
                    self.walk_expr(&arm.cond)?;
                    self.walk_block(&arm.body)?;
                }
                if let Some(body) = else_body {
                    self.walk_block(body)?;
                }
                Ok(())
            }
            LuaStmt::Local { names, values, .. } => {
                self.walk_expr_list(values)?;
                if names.len() == 1
                    && values.exprs.len() == 1
                    && let Some(body) = function_literal(&values.exprs[0])
                {
                    let id = body.span.start;
                    if let Some(binding) =
                        self.declaration_binding(&names[0].name, BindingKind::Local)
                    {
                        self.function_body_of_binding[binding] = Some(id);
                        self.possible[binding].insert(id);
                    }
                }
                Ok(())
            }
            LuaStmt::LocalFunction { name, func, .. } => {
                let body = func;
                self.register_function(body)?;
                if let Some(binding) = self.declaration_binding(name, BindingKind::Function) {
                    self.function_body_of_binding[binding] = Some(body.span.start);
                    self.possible[binding].insert(body.span.start);
                }
                self.walk_block(&body.body)
            }
            LuaStmt::Function { name, body, .. } => {
                let _ = name;
                self.register_function(body)?;
                self.walk_block(&body.body)
            }
            LuaStmt::Assign {
                targets, values, ..
            } => {
                self.walk_expr_list(values)?;
                for target in targets {
                    self.mark_target_assigned(target);
                    self.walk_target(target)?;
                }
                Ok(())
            }
            LuaStmt::CompoundAssign { target, value, .. } => {
                self.mark_target_assigned(target);
                self.walk_target(target)?;
                self.walk_expr(value)
            }
            LuaStmt::Call { call, .. } => self.walk_expr(call),
            LuaStmt::Return { values, .. } => self.walk_expr_list(values),
        }
    }

    fn walk_target(&mut self, target: &'a super::ast::AssignTarget) -> Result<(), ParseError> {
        match target {
            super::ast::AssignTarget::Name(_) => Ok(()),
            super::ast::AssignTarget::Index { table, key, .. } => {
                self.walk_expr(table)?;
                self.walk_expr(key)
            }
            super::ast::AssignTarget::Field { table, .. } => self.walk_expr(table),
        }
    }

    fn mark_target_assigned(&mut self, target: &super::ast::AssignTarget) {
        if let super::ast::AssignTarget::Name(name) = target
            && let Some(binding) = self.use_binding(name)
        {
            self.assigned[binding] = true;
        }
    }

    fn walk_expr_list(&mut self, list: &'a ExprList) -> Result<(), ParseError> {
        for expr in &list.exprs {
            self.walk_expr(expr)?;
        }
        Ok(())
    }

    fn walk_expr(&mut self, expr: &'a LuaExpr) -> Result<(), ParseError> {
        match expr {
            LuaExpr::Nil { .. }
            | LuaExpr::Bool { .. }
            | LuaExpr::Number { .. }
            | LuaExpr::String { .. }
            | LuaExpr::Vararg { .. }
            | LuaExpr::Name { .. } => Ok(()),
            LuaExpr::Paren { inner, .. } => self.walk_expr(inner),
            LuaExpr::Unary { expr, .. } => self.walk_expr(expr),
            LuaExpr::Binary { left, right, .. } => {
                self.walk_expr(left)?;
                self.walk_expr(right)
            }
            LuaExpr::BinaryChain { base, steps, .. } => {
                self.walk_expr(base)?;
                for step in steps {
                    self.walk_expr(&step.expr)?;
                }
                Ok(())
            }
            LuaExpr::Ternary {
                cond,
                then_expr,
                else_expr,
                ..
            } => {
                self.walk_expr(cond)?;
                self.walk_expr(then_expr)?;
                self.walk_expr(else_expr)
            }
            LuaExpr::SuffixChain { base, steps, .. } => {
                if let Some(site) = self.site_of(expr) {
                    self.sites.push(site);
                }
                self.walk_expr(base)?;
                for step in steps {
                    match step {
                        Suffix::Field { .. } => {}
                        Suffix::Index { key, .. } => self.walk_expr(key)?,
                        Suffix::Call { args, .. } | Suffix::Method { args, .. } => match args {
                            Args::List(list) => self.walk_expr_list(list)?,
                            Args::String { .. } => {}
                            Args::Table { fields, .. } => {
                                for field in fields {
                                    self.walk_field(field)?;
                                }
                            }
                        },
                    }
                }
                Ok(())
            }
            LuaExpr::Function { body, .. } => {
                self.register_function(body)?;
                self.walk_block(&body.body)
            }
            LuaExpr::ShortFunction { body, .. } => match body.as_ref() {
                super::ast::ShortBody::Expr(expr) => self.walk_expr(expr),
                super::ast::ShortBody::Block(block) => self.walk_block(block),
            },
            LuaExpr::Table { fields, .. } => {
                for field in fields {
                    self.walk_field(field)?;
                }
                Ok(())
            }
        }
    }

    fn walk_field(&mut self, field: &'a Field) -> Result<(), ParseError> {
        match field {
            Field::Item { value, .. } | Field::Name { value, .. } => self.walk_expr(value),
            Field::Index { key, value, .. } => {
                self.walk_expr(key)?;
                self.walk_expr(value)
            }
        }
    }

    fn register_function(&mut self, func: &'a FuncBody) -> Result<(), ParseError> {
        let id = func.span.start;
        if self.body_by_id.contains_key(&id) {
            return Err(ParseError::at_line(
                line_of(self.source, func.span),
                "duplicate function body span in the completion slice",
            ));
        }
        self.body_by_id.insert(id, func);
        let mut params = Vec::with_capacity(func.params.len());
        for param in &func.params {
            let binding = self
                .declaration_binding(param, BindingKind::Param)
                .ok_or_else(|| {
                    ParseError::at_line(
                        line_of(self.source, param.span),
                        "function parameter has no resolver binding",
                    )
                })?;
            params.push(binding);
        }
        self.param_bindings.insert(id, params);
        Ok(())
    }

    /// A call site shape this slice can reason about: one direct call step with
    /// a list argument on a plain name or a function literal.
    fn site_of(&self, expr: &'a LuaExpr) -> Option<Site<'a>> {
        let LuaExpr::SuffixChain { base, steps, .. } = expr else {
            return None;
        };
        let [
            Suffix::Call {
                args: Args::List(list),
                ..
            },
        ] = steps.as_slice()
        else {
            return None;
        };
        let mut site = Site {
            callee_binding: None,
            literal: None,
            pcall: false,
            args: list.exprs.iter().collect(),
        };
        match base.as_ref() {
            LuaExpr::Name { name, .. } => match self.use_binding(name) {
                Some(binding) => site.callee_binding = Some(binding),
                None if name.is("pcall") => site.pcall = true,
                None => {}
            },
            LuaExpr::Function { body, .. } => site.literal = Some(body.span.start),
            _ => {}
        }
        Some(site)
    }

    /// One fixpoint over both facts admission needs: which function bodies can
    /// reach a parameter, and whether any reachable argument is a non-Lua
    /// value. A missing argument is nil at the Lua call boundary, so it is a
    /// non-Lua source even when another call site supplies a function literal.
    fn propagate(&mut self) {
        loop {
            let mut changed = false;
            for site in &self.sites {
                let targets = self.site_targets(site);
                let offset = usize::from(site.pcall);
                for target in targets {
                    let params = self
                        .param_bindings
                        .get(&target)
                        .cloned()
                        .unwrap_or_default();
                    for (index, &param) in params.iter().enumerate() {
                        let Some(arg) = site.args.get(offset + index) else {
                            if !self.non_lua[param] {
                                self.non_lua[param] = true;
                                changed = true;
                            }
                            continue;
                        };
                        let sources = self.expr_sources(arg);
                        for source in sources {
                            if self.possible[param].insert(source) {
                                changed = true;
                            }
                        }
                        if !self.expr_lua_only(arg) && !self.non_lua[param] {
                            self.non_lua[param] = true;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn site_targets(&self, site: &Site<'a>) -> Vec<u32> {
        if let Some(binding) = site.callee_binding {
            return self.possible[binding].iter().copied().collect();
        }
        if let Some(literal) = site.literal {
            return vec![literal];
        }
        if site.pcall {
            return site
                .args
                .first()
                .map(|callee| self.expr_sources(callee))
                .unwrap_or_default();
        }
        Vec::new()
    }

    fn expr_sources(&self, expr: &LuaExpr) -> Vec<u32> {
        match expr {
            LuaExpr::Function { body, .. } => vec![body.span.start],
            LuaExpr::Paren { inner, .. } => self.expr_sources(inner),
            LuaExpr::Name { name, .. } => self
                .use_binding(name)
                .map(|binding| self.possible[binding].iter().copied().collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Whether every value this expression can evaluate to is a Lua callable
    /// produced by this lowering: a function literal, or a name whose binding
    /// is a non-reassigned function literal or an admitted parameter with no
    /// known non-Lua source. Any other expression may be ordinary data.
    fn expr_lua_only(&self, expr: &LuaExpr) -> bool {
        match expr {
            LuaExpr::Function { .. } => true,
            LuaExpr::Paren { inner, .. } => self.expr_lua_only(inner),
            LuaExpr::Name { name, .. } => self
                .use_binding(name)
                .is_some_and(|binding| self.binding_lua_only(binding)),
            _ => false,
        }
    }

    fn binding_lua_only(&self, binding: usize) -> bool {
        if self.assigned[binding] {
            return false;
        }
        if self.function_body_of_binding[binding].is_some() {
            return true;
        }
        self.resolution.bindings[binding].kind == BindingKind::Param && !self.non_lua[binding]
    }

    /// Whether `binding` is an immutable binding whose every admitted source
    /// is a Lua callable produced by this lowering. This is the value-position
    /// counterpart of `proof_for`: a read never selects an arity, so a
    /// parameter whose callable sources disagree on arity is still admitted
    /// here, while one with no known source or any non-Lua source is not.
    fn binding_is_admitted_callable(&self, binding: usize) -> bool {
        if self.assigned[binding] {
            return false;
        }
        if self.function_body_of_binding[binding].is_some() {
            return true;
        }
        self.resolution.bindings[binding].kind == BindingKind::Param
            && !self.possible[binding].is_empty()
            && !self.non_lua[binding]
    }

    // ---- binding and slot plumbing ---------------------------------------

    fn use_binding(&self, name: &Name) -> Option<usize> {
        self.resolution
            .uses
            .iter()
            .find(|usage| usage.span == name.span)
            .and_then(|usage| usage.binding)
    }

    fn declaration_binding(&self, name: &Name, kind: BindingKind) -> Option<usize> {
        self.resolution
            .bindings
            .iter()
            .position(|binding| binding.span == name.span && binding.kind == kind)
    }

    fn push_frame(&mut self, span: Span, creating: Option<usize>, root: Option<LocalSlot>) {
        self.frames.push(Frame {
            span,
            declared: HashSet::new(),
            captures: HashMap::new(),
            capture_copies: Vec::new(),
            creating,
            root,
        });
    }

    fn pop_frame(&mut self) -> Frame {
        self.frames.pop().expect("completion frame stack")
    }

    fn declare(&mut self, binding: usize) -> Result<LocalSlot, ParseError> {
        let slot = match self.binding_slots[binding] {
            Some(slot) => slot,
            None => {
                let name = format!("$lua_completion_b{binding}");
                let slot = self.builder.alloc_local_named(&name)?;
                self.binding_slots[binding] = Some(slot);
                slot
            }
        };
        self.frames
            .last_mut()
            .expect("completion frame")
            .declared
            .insert(binding);
        Ok(slot)
    }

    fn slot_for(&mut self, binding: usize) -> Result<LocalSlot, ParseError> {
        let top = self.frames.len().checked_sub(1).expect("completion frame");
        self.access_in_frame(top, binding)
    }

    fn access_in_frame(
        &mut self,
        frame_index: usize,
        binding: usize,
    ) -> Result<LocalSlot, ParseError> {
        if self.frames[frame_index].declared.contains(&binding) {
            return self.binding_slots[binding].ok_or_else(|| {
                ParseError::at_line(1, "declared completion binding has no local slot")
            });
        }
        if let Some(slot) = self.frames[frame_index].captures.get(&binding) {
            return Ok(*slot);
        }
        if self.frames[frame_index].creating == Some(binding) {
            return Err(ParseError::at_line(
                line_of(self.source, self.frames[frame_index].span),
                "recursive Lua function references are not in the completion slice",
            ));
        }
        if frame_index == 0 {
            return Err(ParseError::at_line(
                1,
                "binding is not visible in this completion slice",
            ));
        }
        let source = self.access_in_frame(frame_index - 1, binding)?;
        let name = format!("$lua_completion_cap_{frame_index}_{binding}");
        let target = self.builder.alloc_local_named(&name)?;
        self.frames[frame_index].captures.insert(binding, target);
        self.frames[frame_index]
            .capture_copies
            .push((source, target));
        Ok(target)
    }

    /// A nested Lua function reaches an outer callable through a capture copy.
    /// The fixed Core classifies a capture by scanning the closure body for a
    /// read of the captured slot, and `Expr::LocalCall` names its callee slot
    /// without counting as a read. Left implicit, the capture falls back to
    /// `Move`, which detaches the original Lua binding even though Lua keeps
    /// that binding usable after the closure is created. Return an explicit
    /// by-value copy the call performs (`.copy()`, a `ToOwned` read) as the
    /// callee slot, so the capture is classified `Copy` and the original
    /// binding survives. This is the callee half of one read policy: every
    /// read of an admitted immutable Lua callable binding, in any value
    /// position, uses the same explicit non-consuming read (see
    /// `binding_is_admitted_callable`).
    fn captured_callee_slot(
        &mut self,
        binding: usize,
        span: Span,
    ) -> Result<(LocalSlot, Option<Stmt>), ParseError> {
        let slot = self.slot_for(binding)?;
        let Some(captured) = self
            .frames
            .last()
            .expect("completion frame")
            .captures
            .get(&binding)
            .copied()
        else {
            return Ok((slot, None));
        };
        let copy = self.alloc_temp("callee_copy")?;
        Ok((
            copy,
            Some(Stmt::Let {
                index: copy,
                declared_schema: None,
                expr: explicit_value_read(captured),
                line: self.line(span),
            }),
        ))
    }

    fn alloc_temp(&mut self, role: &str) -> Result<LocalSlot, ParseError> {
        let name = format!("$lua_completion_tmp_{role}_{}", self.temp);
        self.temp += 1;
        self.builder.alloc_local_named(&name)
    }

    /// Opens one private Lua heap root frame of the fixed width. Emitted only
    /// by the root-handoff entry; the completion-only entry never calls it.
    fn root_enter_call(&mut self) -> Result<Expr, ParseError> {
        self.root_host_call(
            ROOT_ENTER,
            vec![Expr::Int(ROOT_FRAME_WIDTH)],
            Span { start: 0, end: 0 },
        )
    }

    /// Resolves one private root transport host call through the same test host
    /// catalog the guest-visible host imports use. These names never appear in
    /// a Lua source; they are compiler-emitted frame protocol.
    fn root_host_call(
        &mut self,
        name: &str,
        args: Vec<Expr>,
        span: Span,
    ) -> Result<Expr, ParseError> {
        let arity = u8::try_from(args.len())
            .map_err(|_| self.fail(span, "root transport call has too many arguments"))?;
        self.builder
            .declare_function(name, Some(arity))
            .map_err(|error| self.fail(span, error.message))?;
        let signature = vec![TypeSchema::Unknown; args.len()];
        let resolver = self.hosts.ok_or_else(|| {
            self.fail(
                span,
                "root transport requires the completion test host catalog",
            )
        })?;
        let resolved = resolver
            .resolve(name, &signature)
            .map_err(|error| self.fail(span, format!("root transport resolution: {error}")))?;
        let index = self
            .builder
            .function_index(name)
            .ok_or_else(|| self.fail(span, "root transport function was not declared"))?;
        Ok(Expr::Call(
            index,
            Vec::new(),
            args,
            Some(Box::new(resolved)),
            None,
        ))
    }

    /// Wraps one completed admitted Lua call with the caller-side handoff.
    /// The callee's Lua root frame is still the top frame here: publishing to
    /// the parent happens before that frame is retired, inside the host.
    fn wrap_handoff(&mut self, completion: Expr, span: Span) -> Result<Expr, ParseError> {
        if !self.roots {
            return Ok(completion);
        }
        let frame = self
            .frames
            .last()
            .and_then(|frame| frame.root)
            .ok_or_else(|| {
                self.fail(
                    span,
                    "a Lua call cannot hand off without a current root frame",
                )
            })?;
        self.root_host_call(ROOT_HANDOFF, vec![completion, Expr::Var(frame)], span)
    }

    /// Closes the chunk root frame. Only the chunk uses this: a callee frame
    /// is retired by its caller's handoff after the parent root is written.
    fn root_finish_call(
        &mut self,
        completion: Expr,
        frame: LocalSlot,
        span_start: u32,
    ) -> Result<Expr, ParseError> {
        self.root_host_call(
            ROOT_FINISH,
            vec![completion, Expr::Var(frame), Expr::Int(ROOT_FORMAL_SLOT)],
            Span {
                start: span_start,
                end: span_start,
            },
        )
    }

    // ---- function and statement lowering --------------------------------

    fn lower_function(
        &mut self,
        func: &'a FuncBody,
        creating: Option<usize>,
    ) -> Result<ClosureExpr, ParseError> {
        let root = if self.roots {
            Some(self.alloc_temp("root_frame")?)
        } else {
            None
        };
        self.push_frame(func.span, creating, root);
        let mut param_slots = Vec::with_capacity(func.params.len());
        for param in &func.params {
            let binding = self
                .declaration_binding(param, BindingKind::Param)
                .ok_or_else(|| {
                    ParseError::at_line(
                        line_of(self.source, param.span),
                        "function parameter has no resolver binding",
                    )
                })?;
            param_slots.push(self.declare(binding)?);
        }
        let body = self.lower_block(&func.body.stmts)?;
        let body = match root {
            Some(slot) => Expr::Block {
                stmts: vec![Stmt::Let {
                    index: slot,
                    declared_schema: None,
                    expr: self.root_enter_call()?,
                    line: self.line(func.span),
                }],
                expr: Box::new(body),
            },
            None => body,
        };
        let frame = self.pop_frame();
        Ok(ClosureExpr {
            param_slots,
            capture_copies: frame.capture_copies,
            body: Box::new(body),
        })
    }

    fn lower_block(&mut self, stmts: &'a [LuaStmt]) -> Result<Expr, ParseError> {
        let mut pre: Vec<Stmt> = Vec::new();
        let mut index = 0;
        while index < stmts.len() {
            match &stmts[index] {
                LuaStmt::Empty { .. } => index += 1,
                LuaStmt::Local {
                    names,
                    values,
                    span,
                } => {
                    if let Some(tail) =
                        self.lower_local(names, values, *span, &stmts[index + 1..], &mut pre)?
                    {
                        return Ok(block(pre, tail));
                    }
                    index += 1;
                }
                LuaStmt::LocalFunction {
                    name, func, span, ..
                } => {
                    let binding = self
                        .declaration_binding(name, BindingKind::Function)
                        .ok_or_else(|| {
                            self.fail(
                                name.span,
                                "local function has no resolver binding in the completion slice",
                            )
                        })?;
                    let slot = self.declare(binding)?;
                    let closure = self.lower_function(func, Some(binding))?;
                    pre.push(Stmt::Let {
                        index: slot,
                        declared_schema: None,
                        expr: Expr::Closure(closure),
                        line: self.line(*span),
                    });
                    index += 1;
                }
                LuaStmt::Call { call, span } => {
                    if let Some(tail) =
                        self.lower_call_statement(call, *span, &stmts[index + 1..], &mut pre)?
                    {
                        return Ok(block(pre, tail));
                    }
                    index += 1;
                }
                LuaStmt::Return { values, span } => {
                    let expr = self.lower_return(values, *span)?;
                    return Ok(block(pre, expr));
                }
                other => {
                    return Err(
                        self.fail(stmt_span(other), "statement is not in the completion slice")
                    );
                }
            }
        }
        Ok(block(pre, completion(&[])))
    }

    fn lower_local(
        &mut self,
        names: &'a [super::ast::LocalName],
        values: &'a ExprList,
        span: Span,
        rest: &'a [LuaStmt],
        pre: &mut Vec<Stmt>,
    ) -> Result<Option<Expr>, ParseError> {
        let line = self.line(span);
        if names.is_empty() {
            return Err(self.fail(span, "empty local declaration in the completion slice"));
        }
        if names.len() == 1
            && values.exprs.len() == 1
            && let Some(body) = function_literal(&values.exprs[0])
        {
            let binding = self
                .declaration_binding(&names[0].name, BindingKind::Local)
                .ok_or_else(|| self.fail(names[0].name.span, "local has no resolver binding"))?;
            let slot = self.declare(binding)?;
            let closure = self.lower_function(body, Some(binding))?;
            pre.push(Stmt::Let {
                index: slot,
                declared_schema: None,
                expr: Expr::Closure(closure),
                line,
            });
            return Ok(None);
        }
        if values.exprs.len() == 1 && is_call_shape(&values.exprs[0]) {
            let (class, value) = self.lower_call(&values.exprs[0])?;
            match class {
                CallClass::Lua => {
                    let completion = self.alloc_temp("completion")?;
                    pre.push(Stmt::Let {
                        index: completion,
                        declared_schema: None,
                        expr: value,
                        line,
                    });
                    let mut then_stmts = Vec::new();
                    for (index, name) in names.iter().enumerate() {
                        let binding = self
                            .declaration_binding(&name.name, BindingKind::Local)
                            .ok_or_else(|| {
                                self.fail(name.name.span, "local has no resolver binding")
                            })?;
                        let slot = self.declare(binding)?;
                        then_stmts.push(Stmt::Let {
                            index: slot,
                            declared_schema: None,
                            expr: guarded_get(completion, index as i64 + 1),
                            line,
                        });
                    }
                    let rest_expr = self.lower_block(rest)?;
                    return Ok(Some(Expr::IfElse {
                        condition: Box::new(status_of(completion)),
                        then_expr: Box::new(block(then_stmts, rest_expr)),
                        else_expr: Box::new(Expr::Var(completion)),
                    }));
                }
                CallClass::Pcall => {
                    let sequence = self.alloc_temp("pcall")?;
                    pre.push(Stmt::Let {
                        index: sequence,
                        declared_schema: None,
                        expr: value,
                        line,
                    });
                    let mut formal_slot = None;
                    for (index, name) in names.iter().enumerate() {
                        let binding = self
                            .declaration_binding(&name.name, BindingKind::Local)
                            .ok_or_else(|| {
                                self.fail(name.name.span, "local has no resolver binding")
                            })?;
                        let slot = self.declare(binding)?;
                        pre.push(Stmt::Let {
                            index: slot,
                            declared_schema: None,
                            expr: guarded_get(sequence, index as i64),
                            line,
                        });
                        if index == 1 {
                            formal_slot = Some(slot);
                        }
                    }
                    if self.roots
                        && let (Some(err), Some(frame)) =
                            (formal_slot, self.frames.last().and_then(|frame| frame.root))
                    {
                        // The protected result is now in its formal local. Move
                        // the temp handoff root into the formal frame slot and
                        // clear the temp slot, both in one host call.
                        let formal = self.root_host_call(
                            ROOT_FORMAL,
                            vec![
                                Expr::Var(err),
                                Expr::Var(frame),
                                Expr::Int(ROOT_HANDOFF_SLOT),
                                Expr::Int(ROOT_FORMAL_SLOT),
                            ],
                            span,
                        )?;
                        let discarded = self.alloc_temp("root_formal")?;
                        pre.push(Stmt::Let {
                            index: discarded,
                            declared_schema: None,
                            expr: formal,
                            line,
                        });
                    }
                    return Ok(None);
                }
                CallClass::Error => return Ok(Some(completion_error(value))),
                CallClass::Host => {
                    for (index, name) in names.iter().enumerate() {
                        let binding = self
                            .declaration_binding(&name.name, BindingKind::Local)
                            .ok_or_else(|| {
                                self.fail(name.name.span, "local has no resolver binding")
                            })?;
                        let slot = self.declare(binding)?;
                        let expr = if index == 0 {
                            value.clone()
                        } else {
                            Expr::Null
                        };
                        pre.push(Stmt::Let {
                            index: slot,
                            declared_schema: None,
                            expr,
                            line,
                        });
                    }
                    return Ok(None);
                }
            }
        }
        if values.exprs.len() > names.len() {
            return Err(self.fail(
                span,
                "more initializer values than locals are not in the completion slice",
            ));
        }
        for (index, name) in names.iter().enumerate() {
            let binding = self
                .declaration_binding(&name.name, BindingKind::Local)
                .ok_or_else(|| self.fail(name.name.span, "local has no resolver binding"))?;
            let slot = self.declare(binding)?;
            let expr = match values.exprs.get(index) {
                Some(value) => self.lower_scalar(value)?,
                None => Expr::Null,
            };
            pre.push(Stmt::Let {
                index: slot,
                declared_schema: None,
                expr,
                line,
            });
        }
        Ok(None)
    }

    fn lower_call_statement(
        &mut self,
        call: &'a LuaExpr,
        span: Span,
        rest: &'a [LuaStmt],
        pre: &mut Vec<Stmt>,
    ) -> Result<Option<Expr>, ParseError> {
        let line = self.line(span);
        let (class, value) = self.lower_call(call)?;
        match class {
            CallClass::Lua => {
                let completion = self.alloc_temp("completion")?;
                pre.push(Stmt::Let {
                    index: completion,
                    declared_schema: None,
                    expr: value,
                    line,
                });
                let rest_expr = self.lower_block(rest)?;
                Ok(Some(Expr::IfElse {
                    condition: Box::new(status_of(completion)),
                    then_expr: Box::new(rest_expr),
                    else_expr: Box::new(Expr::Var(completion)),
                }))
            }
            CallClass::Pcall | CallClass::Host => {
                let discarded = self.alloc_temp("discarded")?;
                pre.push(Stmt::Let {
                    index: discarded,
                    declared_schema: None,
                    expr: value,
                    line,
                });
                Ok(None)
            }
            CallClass::Error => Ok(Some(completion_error(value))),
        }
    }

    fn lower_return(&mut self, values: &'a ExprList, span: Span) -> Result<Expr, ParseError> {
        if values.exprs.is_empty() {
            return Ok(completion(&[]));
        }
        if values.exprs.len() == 1 {
            let expr = &values.exprs[0];
            if is_call_shape(expr) {
                let (class, value) = self.lower_call(expr)?;
                return Ok(match class {
                    CallClass::Lua => value,
                    CallClass::Pcall => pcall_tail_completion(value),
                    CallClass::Error => completion_error(value),
                    CallClass::Host => completion(&[value]),
                });
            }
            return Ok(completion(&[self.lower_scalar(expr)?]));
        }
        for expr in &values.exprs {
            if is_call_shape(expr) {
                return Err(self.fail(
                    span,
                    "a call in a multi-value return list is outside the completion slice",
                ));
            }
        }
        let mut lowered = Vec::with_capacity(values.exprs.len());
        for expr in &values.exprs {
            lowered.push(self.lower_scalar(expr)?);
        }
        Ok(completion(&lowered))
    }

    fn lower_call(&mut self, call: &'a LuaExpr) -> Result<(CallClass, Expr), ParseError> {
        let LuaExpr::SuffixChain { base, steps, span } = call else {
            return Err(self.fail(expr_span(call), "call shape is not in the completion slice"));
        };
        let [
            Suffix::Call {
                args: Args::List(list),
                ..
            },
        ] = steps.as_slice()
        else {
            return Err(self.fail(*span, "call shape is not in the completion slice"));
        };
        match base.as_ref() {
            LuaExpr::Name { name, .. } => self.lower_named_call(name, &list.exprs, *span),
            LuaExpr::Function { body, .. } => {
                let closure = self.lower_function(body, None)?;
                let adapted = self.adapt_arguments(&list.exprs, body.params.len(), *span)?;
                let call = Expr::ClosureCall(closure, adapted);
                Ok((CallClass::Lua, self.wrap_handoff(call, *span)?))
            }
            other => Err(self.fail(
                expr_span(other),
                "call target shape is not in the completion slice",
            )),
        }
    }

    fn lower_named_call(
        &mut self,
        name: &'a Name,
        args: &'a [LuaExpr],
        span: Span,
    ) -> Result<(CallClass, Expr), ParseError> {
        if let Some(binding) = self.use_binding(name) {
            let proof = self.proof_for(binding, name.span)?;
            let (slot, copy) = self.captured_callee_slot(binding, span)?;
            let adapted = self.adapt_arguments(args, proof.arity, span)?;
            let call = Expr::LocalCall(slot, Vec::new(), adapted, None);
            let value = match copy {
                Some(stmt) => Expr::Block {
                    stmts: vec![stmt],
                    expr: Box::new(call),
                },
                None => call,
            };
            return Ok((CallClass::Lua, self.wrap_handoff(value, span)?));
        }
        if name.is("pcall") {
            return self.lower_pcall(args, span);
        }
        if name.is("error") {
            return self.lower_error(args, span);
        }
        if HOST_IMPORTS.iter().any(|host| name.is(host))
            || (self.roots && ROOT_GUEST_IMPORTS.iter().any(|host| name.is(host)))
        {
            return self.lower_host_call(name, args, span);
        }
        Err(self.fail(name.span, "unsupported global call in the completion slice"))
    }

    fn proof_for(&self, binding: usize, span: Span) -> Result<Proof, ParseError> {
        if self.assigned[binding] {
            return Err(ParseError::at_line(
                line_of(self.source, span),
                "callable binding is reassigned, so its Lua provenance is not provable",
            ));
        }
        if let Some(id) = self.function_body_of_binding[binding] {
            let body = self
                .body_by_id
                .get(&id)
                .ok_or_else(|| ParseError::at_line(1, "function binding has no registered body"))?;
            return Ok(Proof {
                arity: body.params.len(),
            });
        }
        if self.resolution.bindings[binding].kind == BindingKind::Param {
            let mut arity: Option<usize> = None;
            if self.possible[binding].is_empty() || self.non_lua[binding] {
                return Err(ParseError::at_line(
                    line_of(self.source, span),
                    "dynamic call target is not a proven Lua callable",
                ));
            }
            for id in &self.possible[binding] {
                let body = self
                    .body_by_id
                    .get(id)
                    .ok_or_else(|| ParseError::at_line(1, "argument function has no body"))?;
                let candidate = body.params.len();
                match arity {
                    None => arity = Some(candidate),
                    Some(existing) if existing == candidate => {}
                    Some(_) => {
                        return Err(ParseError::at_line(
                            line_of(self.source, span),
                            "dynamic call target arity is not uniquely known in this slice",
                        ));
                    }
                }
            }
            return Ok(Proof {
                arity: arity.expect("non-empty source set"),
            });
        }
        Err(ParseError::at_line(
            line_of(self.source, span),
            "call target is not a Lua function binding or admitted parameter",
        ))
    }

    fn lower_pcall(
        &mut self,
        args: &'a [LuaExpr],
        span: Span,
    ) -> Result<(CallClass, Expr), ParseError> {
        let Some((callee, forwarded)) = args.split_first() else {
            return Err(self.fail(span, "pcall requires a callable argument"));
        };
        let LuaExpr::SuffixChain { .. } = callee else {
            // The callee is a plain expression (name or literal), not a call.
            return match callee {
                LuaExpr::Name { name, .. } => {
                    let binding = self.use_binding(name).ok_or_else(|| {
                        self.fail(name.span, "pcall target is not a proven Lua callable")
                    })?;
                    let proof = self.proof_for(binding, name.span)?;
                    let (slot, copy) = self.captured_callee_slot(binding, span)?;
                    let adapted = self.adapt_arguments(forwarded, proof.arity, span)?;
                    let call = Expr::LocalCall(slot, Vec::new(), adapted, None);
                    let value = match copy {
                        Some(stmt) => Expr::Block {
                            stmts: vec![stmt],
                            expr: Box::new(call),
                        },
                        None => call,
                    };
                    Ok((CallClass::Pcall, self.wrap_handoff(value, span)?))
                }
                LuaExpr::Function { body, .. } => {
                    let closure = self.lower_function(body, None)?;
                    let adapted = self.adapt_arguments(forwarded, body.params.len(), span)?;
                    let call = Expr::ClosureCall(closure, adapted);
                    Ok((CallClass::Pcall, self.wrap_handoff(call, span)?))
                }
                other => Err(self.fail(
                    expr_span(other),
                    "pcall target is not a proven Lua callable",
                )),
            };
        };
        Err(self.fail(span, "pcall target shape is not in the completion slice"))
    }

    fn lower_error(
        &mut self,
        args: &'a [LuaExpr],
        span: Span,
    ) -> Result<(CallClass, Expr), ParseError> {
        let [payload, level] = args else {
            return Err(self.fail(span, "error requires exactly two arguments in this slice"));
        };
        if !is_zero_level(level) {
            return Err(self.fail(
                expr_span(level),
                "only error(value, 0) is in the completion slice",
            ));
        }
        let value = self.lower_scalar(payload)?;
        Ok((CallClass::Error, value))
    }

    fn lower_host_call(
        &mut self,
        name: &'a Name,
        args: &'a [LuaExpr],
        span: Span,
    ) -> Result<(CallClass, Expr), ParseError> {
        let text = std::str::from_utf8(&name.bytes)
            .map_err(|_| self.fail(name.span, "host import name is not UTF-8"))?;
        let arity = u8::try_from(args.len())
            .map_err(|_| self.fail(span, "host call has too many arguments"))?;
        self.builder
            .declare_function(text, Some(arity))
            .map_err(|error| self.fail(name.span, error.message))?;
        let signature = vec![TypeSchema::Unknown; args.len()];
        let resolver = self.hosts.ok_or_else(|| {
            self.fail(
                name.span,
                "host call requires the completion test host catalog",
            )
        })?;
        let resolved = resolver
            .resolve(text, &signature)
            .map_err(|error| self.fail(name.span, format!("host call resolution: {error}")))?;
        let index = self
            .builder
            .function_index(text)
            .ok_or_else(|| self.fail(name.span, "host function was not declared"))?;
        let mut lowered = Vec::with_capacity(args.len());
        for arg in args {
            lowered.push(self.lower_scalar(arg)?);
        }
        Ok((
            CallClass::Host,
            Expr::Call(index, Vec::new(), lowered, Some(Box::new(resolved)), None),
        ))
    }

    fn adapt_arguments(
        &mut self,
        args: &'a [LuaExpr],
        arity: usize,
        span: Span,
    ) -> Result<Vec<Expr>, ParseError> {
        if args.len() > arity {
            return Err(self.fail(
                span,
                "extra call arguments are outside the completion slice",
            ));
        }
        let mut lowered = Vec::with_capacity(arity);
        for arg in args {
            lowered.push(self.lower_scalar(arg)?);
        }
        while lowered.len() < arity {
            lowered.push(Expr::Null);
        }
        Ok(lowered)
    }

    fn lower_scalar(&mut self, expr: &'a LuaExpr) -> Result<Expr, ParseError> {
        match expr {
            LuaExpr::Nil { .. } => Ok(Expr::Null),
            LuaExpr::Bool { value, .. } => Ok(Expr::Bool(*value)),
            LuaExpr::Number { raw, kind, span } => {
                number_literal(raw, *kind).ok_or_else(|| self.fail(*span, "unsupported number"))
            }
            LuaExpr::String { bytes, span } => String::from_utf8(bytes.clone())
                .map(Expr::String)
                .map_err(|_| self.fail(*span, "non-UTF-8 string is outside this slice")),
            LuaExpr::Name { name, .. } => {
                let binding = self.use_binding(name).ok_or_else(|| {
                    self.fail(
                        name.span,
                        "global name used as a value is outside the completion slice",
                    )
                })?;
                let slot = self.slot_for(binding)?;
                if self.binding_is_admitted_callable(binding) {
                    // A Lua callable is an immutable shared reference. Reading
                    // it never consumes the binding, whether the value feeds a
                    // forwarded pcall argument, a host argument, a return, or
                    // any other value position. Emit the same explicit
                    // by-value read the callee position uses so the fixed
                    // Core's capture scan sees a copy, not a move, on every
                    // capture edge; `ToOwned` compiles to the same plain local
                    // read as `Var`.
                    Ok(explicit_value_read(slot))
                } else {
                    Ok(Expr::Var(slot))
                }
            }
            LuaExpr::Paren { inner, .. } => self.lower_scalar(inner),
            LuaExpr::Function { body, .. } => {
                let closure = self.lower_function(body, None)?;
                Ok(Expr::Closure(closure))
            }
            LuaExpr::Table { fields, .. } => {
                let mut array = lua_ir_call(BuiltinFunction::ArrayNew.call_index(), Vec::new());
                for field in fields {
                    let Field::Item { value, .. } = field else {
                        return Err(self.fail(
                            expr_span(expr),
                            "only array items are in the completion slice",
                        ));
                    };
                    let value = self.lower_scalar(value)?;
                    array =
                        lua_ir_call(BuiltinFunction::ArrayPush.call_index(), vec![array, value]);
                }
                Ok(array)
            }
            other if is_call_shape(other) => {
                // A host call in a value/argument position is an ordinary
                // value. Only the host class is admitted here: a Lua callable
                // or pcall in scalar position would need a multi-value
                // decision that this slice does not make.
                let (class, value) = self.lower_call(other)?;
                match class {
                    CallClass::Host => Ok(value),
                    _ => Err(self.fail(
                        expr_span(other),
                        "a Lua callable call in scalar value position is outside the completion slice",
                    )),
                }
            }
            other => Err(self.fail(
                expr_span(other),
                "expression is not in the completion slice",
            )),
        }
    }

    // ---- small helpers ----------------------------------------------------

    fn fail(&self, span: Span, message: impl Into<String>) -> ParseError {
        ParseError::at_line(line_of(self.source, span), message)
    }

    fn line(&self, span: Span) -> u32 {
        u32::try_from(line_of(self.source, span)).unwrap_or(u32::MAX)
    }
}

fn block(stmts: Vec<Stmt>, expr: Expr) -> Expr {
    if stmts.is_empty() {
        expr
    } else {
        Expr::Block {
            stmts,
            expr: Box::new(expr),
        }
    }
}

fn lua_ir_call(index: u16, args: Vec<Expr>) -> Expr {
    Expr::Call(index, Vec::new(), args, None, None)
}

/// One non-consuming by-value read of an immutable Lua callable slot. The
/// fixed Core's availability and capture classification treat `ToOwned` as an
/// explicit copy and a bare `Var` as an implicit read that can move a movable
/// captured binding; codegen compiles both to the same plain local read.
fn explicit_value_read(slot: LocalSlot) -> Expr {
    Expr::ToOwned(Box::new(Expr::Var(slot)))
}

fn packed_array(values: Vec<Expr>) -> Expr {
    values.into_iter().fold(
        lua_ir_call(BuiltinFunction::ArrayNew.call_index(), Vec::new()),
        |array, value| lua_ir_call(BuiltinFunction::ArrayPush.call_index(), vec![array, value]),
    )
}

fn completion(values: &[Expr]) -> Expr {
    let mut all = Vec::with_capacity(values.len() + 1);
    all.push(Expr::Bool(true));
    all.extend(values.iter().cloned());
    packed_array(all)
}

fn completion_error(value: Expr) -> Expr {
    packed_array(vec![Expr::Bool(false), value])
}

fn pcall_tail_completion(sequence: Expr) -> Expr {
    lua_ir_call(
        BuiltinFunction::Concat.call_index(),
        vec![completion(&[]), sequence],
    )
}

fn status_of(slot: LocalSlot) -> Expr {
    lua_ir_call(
        BuiltinFunction::Get.call_index(),
        vec![Expr::Var(slot), Expr::Int(0)],
    )
}

/// Reads a completion slot only when it exists. The bounds check is a Core
/// branch, so the `get` call is evaluated lazily and a zero-return completion
/// never reaches the out-of-bounds host error.
fn guarded_get(slot: LocalSlot, index: i64) -> Expr {
    let length = lua_ir_call(BuiltinFunction::Len.call_index(), vec![Expr::Var(slot)]);
    Expr::IfElse {
        condition: Box::new(Expr::Gt(Box::new(length), Box::new(Expr::Int(index)))),
        then_expr: Box::new(lua_ir_call(
            BuiltinFunction::Get.call_index(),
            vec![Expr::Var(slot), Expr::Int(index)],
        )),
        else_expr: Box::new(Expr::Null),
    }
}

fn is_call_shape(expr: &LuaExpr) -> bool {
    let LuaExpr::SuffixChain { steps, .. } = expr else {
        return false;
    };
    matches!(
        steps.as_slice(),
        [Suffix::Call {
            args: Args::List(_),
            ..
        }]
    )
}

fn function_literal(expr: &LuaExpr) -> Option<&FuncBody> {
    match expr {
        LuaExpr::Function { body, .. } => Some(body),
        _ => None,
    }
}

fn is_zero_level(expr: &LuaExpr) -> bool {
    matches!(
        expr,
        LuaExpr::Number {
            raw,
            kind: super::ast::NumberKind::Integer,
            ..
        } if raw == "0"
    )
}

fn number_literal(raw: &str, kind: super::ast::NumberKind) -> Option<Expr> {
    use super::ast::NumberKind;
    match kind {
        NumberKind::Integer => raw.parse::<i64>().ok().map(Expr::Int),
        NumberKind::HexInteger => i64::from_str_radix(raw.trim_start_matches("0x"), 16)
            .ok()
            .map(Expr::Int),
        NumberKind::Float | NumberKind::HexFloat => raw.parse::<f64>().ok().map(Expr::Float),
        _ => None,
    }
}

fn stmt_span(stmt: &LuaStmt) -> Span {
    match stmt {
        LuaStmt::Empty { span }
        | LuaStmt::Do { span, .. }
        | LuaStmt::While { span, .. }
        | LuaStmt::Repeat { span, .. }
        | LuaStmt::If { span, .. }
        | LuaStmt::NumericFor { span, .. }
        | LuaStmt::GenericFor { span, .. }
        | LuaStmt::Local { span, .. }
        | LuaStmt::LocalFunction { span, .. }
        | LuaStmt::Function { span, .. }
        | LuaStmt::Assign { span, .. }
        | LuaStmt::CompoundAssign { span, .. }
        | LuaStmt::Call { span, .. }
        | LuaStmt::Label { span, .. }
        | LuaStmt::Goto { span, .. }
        | LuaStmt::Break { span }
        | LuaStmt::Continue { span }
        | LuaStmt::Return { span, .. } => *span,
    }
}

fn expr_span(expr: &LuaExpr) -> Span {
    match expr {
        LuaExpr::Nil { span }
        | LuaExpr::Bool { span, .. }
        | LuaExpr::Number { span, .. }
        | LuaExpr::String { span, .. }
        | LuaExpr::Vararg { span }
        | LuaExpr::Name { span, .. }
        | LuaExpr::Paren { span, .. }
        | LuaExpr::Unary { span, .. }
        | LuaExpr::Binary { span, .. }
        | LuaExpr::BinaryChain { span, .. }
        | LuaExpr::Ternary { span, .. }
        | LuaExpr::SuffixChain { span, .. }
        | LuaExpr::Function { span, .. }
        | LuaExpr::ShortFunction { span, .. }
        | LuaExpr::Table { span, .. } => *span,
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
