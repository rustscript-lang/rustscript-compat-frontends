//! Scope, const, label and import resolution for a parsed Lua 5.2-compat chunk.
//!
//! Goto visibility follows the locked parser: a label is visible only in its
//! block, a trailing label sits at the block's starting local count, and a
//! forward jump into a local's scope is an error. Const bindings cannot be
//! redeclared or assigned, including from a nested function.

use super::ast::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub bindings: Vec<Binding>,
    pub labels: Vec<LabelBinding>,
    pub gotos: Vec<GotoUse>,
    pub imports: Vec<ImportSite>,
    pub errors: Vec<ResolveError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    Local,
    Param,
    SelfParam,
    ForIndex,
    Function,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: Vec<u8>,
    pub span: Span,
    pub kind: BindingKind,
    pub scope_id: u32,
    pub function_id: u32,
    pub visible_from: u32,
    pub visible_to: u32,
    pub const_binding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelBinding {
    pub name: Vec<u8>,
    pub span: Span,
    pub scope_id: u32,
    pub function_id: u32,
    pub active_locals: u32,
    pub trailing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GotoUse {
    pub name: Vec<u8>,
    pub span: Span,
    pub function_id: u32,
    pub active_locals: u32,
    pub target: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSite {
    pub call_span: Span,
    pub spec_span: Span,
    pub spec: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    JumpIntoLocal {
        goto_span: Span,
        local_name: Vec<u8>,
    },
    UndefinedLabel {
        span: Span,
        name: Vec<u8>,
    },
    DuplicateLabel {
        span: Span,
        name: Vec<u8>,
    },
    BreakOutsideLoop {
        span: Span,
    },
    ContinueOutsideLoop {
        span: Span,
    },
    AssignConst {
        span: Span,
        name: Vec<u8>,
    },
    RedeclareConst {
        span: Span,
        name: Vec<u8>,
    },
    VarargOutside {
        span: Span,
    },
}

struct LocalSlot {
    name: Vec<u8>,
    const_binding: bool,
    binding_index: usize,
}

struct LabelRec {
    name: Vec<u8>,
    index: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JumpKind {
    Goto,
    Break,
    Continue,
}

struct JumpRec {
    kind: JumpKind,
    name: Vec<u8>,
    slot: u32,
    span: Span,
    goto_index: Option<usize>,
    resolved: bool,
}

struct ScopeFrame {
    id: u32,
    start_local: usize,
    labels: Vec<LabelRec>,
    jumps: Vec<JumpRec>,
}

struct FuncFrame {
    id: u32,
    vararg: bool,
    scope_base: usize,
}

struct Cx {
    resolution: Resolution,
    funcs: Vec<FuncFrame>,
    scopes: Vec<ScopeFrame>,
    locals: Vec<LocalSlot>,
    next_scope: u32,
}

pub fn resolve(chunk: &Chunk) -> Resolution {
    let mut cx = Cx {
        resolution: Resolution {
            bindings: Vec::new(),
            labels: Vec::new(),
            gotos: Vec::new(),
            imports: Vec::new(),
            errors: Vec::new(),
        },
        funcs: Vec::new(),
        scopes: Vec::new(),
        locals: Vec::new(),
        next_scope: 1,
    };
    cx.enter_function(true);
    cx.walk_block(&chunk.body, false, true);
    cx.leave_scope(chunk.span.end);
    cx.funcs.pop();
    cx.resolution
}

impl Cx {
    fn enter_function(&mut self, vararg: bool) {
        let id = self.funcs.last().map(|func| func.id + 1).unwrap_or(0);
        self.funcs.push(FuncFrame {
            id,
            vararg,
            scope_base: self.scopes.len(),
        });
        self.enter_scope();
    }

    fn enter_scope(&mut self) {
        let id = self.next_scope;
        self.next_scope += 1;
        self.scopes.push(ScopeFrame {
            id,
            start_local: self.locals.len(),
            labels: Vec::new(),
            jumps: Vec::new(),
        });
    }

    fn reject_continues_into_locals(&mut self, scope: usize) {
        let active = self.locals.len() as u32;
        let mut into_local = Vec::new();
        for jump in &mut self.scopes[scope].jumps {
            if jump.kind != JumpKind::Continue || jump.resolved {
                continue;
            }
            if jump.slot < active {
                into_local.push((jump.span, jump.slot));
            }
            jump.resolved = true;
        }
        for (span, slot) in into_local {
            let local_name = self.locals[slot as usize].name.clone();
            self.resolution.errors.push(ResolveError::JumpIntoLocal {
                goto_span: span,
                local_name,
            });
        }
    }

    fn leave_scope(&mut self, end: u32) {
        let scope = self.scopes.pop().expect("scope");
        self.close_scope(scope, end, false);
    }

    fn leave_loop(&mut self, end: u32) {
        let scope = self.scopes.pop().expect("loop");
        self.close_scope(scope, end, true);
    }

    fn close_scope(&mut self, scope: ScopeFrame, end: u32, is_loop: bool) {
        for local in self.locals.iter().skip(scope.start_local) {
            self.resolution.bindings[local.binding_index].visible_to = end;
        }
        self.locals.truncate(scope.start_local);
        let function_root = self
            .funcs
            .last()
            .is_some_and(|func| func.scope_base == self.scopes.len());
        let mut pending = Vec::new();
        for jump in scope.jumps {
            if jump.resolved {
                continue;
            }
            if jump.kind != JumpKind::Goto {
                if is_loop {
                    continue;
                }
                if !function_root && let Some(parent) = self.scopes.last_mut() {
                    let mut propagated = jump;
                    propagated.slot = scope.start_local as u32;
                    parent.jumps.push(propagated);
                    continue;
                }
                if jump.kind == JumpKind::Break {
                    self.resolution
                        .errors
                        .push(ResolveError::BreakOutsideLoop { span: jump.span });
                } else {
                    self.resolution
                        .errors
                        .push(ResolveError::ContinueOutsideLoop { span: jump.span });
                }
                continue;
            }
            let matched = scope.labels.iter().find(|label| label.name == jump.name);
            if let Some(label) = matched {
                if let Some(index) = jump.goto_index {
                    self.resolution.gotos[index].target = Some(label.index);
                }
                continue;
            }
            pending.push(jump);
        }
        if function_root {
            for jump in pending {
                self.resolution.errors.push(ResolveError::UndefinedLabel {
                    span: jump.span,
                    name: jump.name,
                });
            }
        } else if let Some(parent) = self.scopes.last_mut() {
            for mut jump in pending {
                jump.slot = scope.start_local as u32;
                parent.jumps.push(jump);
            }
        }
    }

    fn add_binding(
        &mut self,
        name: &Name,
        kind: BindingKind,
        const_binding: bool,
        visible_from: u32,
        pending_consts: &[Vec<u8>],
    ) {
        let active_const = self
            .locals
            .iter()
            .any(|local| local.const_binding && local.name == name.bytes);
        let pending_const =
            const_binding && pending_consts.iter().any(|pending| pending == &name.bytes);
        if active_const || pending_const {
            self.resolution.errors.push(ResolveError::RedeclareConst {
                span: name.span,
                name: name.bytes.clone(),
            });
        }
        let scope = self.scopes.last().expect("scope");
        let function_id = self.funcs.last().expect("function").id;
        let binding_index = self.resolution.bindings.len();
        self.resolution.bindings.push(Binding {
            name: name.bytes.clone(),
            span: name.span,
            kind,
            scope_id: scope.id,
            function_id,
            visible_from,
            visible_to: u32::MAX,
            const_binding,
        });
        self.locals.push(LocalSlot {
            name: name.bytes.clone(),
            const_binding,
            binding_index,
        });
    }

    fn lookup_const(&self, name: &[u8]) -> bool {
        self.locals
            .iter()
            .rev()
            .any(|local| local.name == name && local.const_binding)
    }

    fn assign_name(&mut self, name: &Name) {
        if self.lookup_const(&name.bytes) {
            self.resolution.errors.push(ResolveError::AssignConst {
                span: name.span,
                name: name.bytes.clone(),
            });
        }
    }

    fn define_label(&mut self, name: &Name, trailing: bool) {
        let slot = if trailing {
            self.scopes.last().expect("scope").start_local as u32
        } else {
            self.locals.len() as u32
        };
        let function_id = self.funcs.last().expect("function").id;
        let scope_id = self.scopes.last().expect("scope").id;
        if self
            .scopes
            .last()
            .expect("scope")
            .labels
            .iter()
            .any(|label| label.name == name.bytes)
        {
            self.resolution.errors.push(ResolveError::DuplicateLabel {
                span: name.span,
                name: name.bytes.clone(),
            });
            return;
        }
        let index = self.resolution.labels.len();
        self.resolution.labels.push(LabelBinding {
            name: name.bytes.clone(),
            span: name.span,
            scope_id,
            function_id,
            active_locals: slot,
            trailing,
        });
        self.scopes
            .last_mut()
            .expect("scope")
            .labels
            .push(LabelRec {
                name: name.bytes.clone(),
                index,
            });
        let jumps = std::mem::take(&mut self.scopes.last_mut().expect("scope").jumps);
        let mut kept = Vec::new();
        for mut jump in jumps {
            if jump.kind == JumpKind::Goto && !jump.resolved && jump.name == name.bytes {
                if jump.slot < slot {
                    let local_name = self.local_at(jump.slot);
                    self.resolution.errors.push(ResolveError::JumpIntoLocal {
                        goto_span: jump.span,
                        local_name,
                    });
                }
                if let Some(goto_index) = jump.goto_index {
                    self.resolution.gotos[goto_index].target = Some(index);
                }
                jump.resolved = true;
            }
            kept.push(jump);
        }
        self.scopes.last_mut().expect("scope").jumps = kept;
    }

    fn local_at(&self, slot: u32) -> Vec<u8> {
        self.locals
            .get(slot as usize)
            .map(|local| local.name.clone())
            .unwrap_or_else(|| b"<local>".to_vec())
    }

    fn add_jump(&mut self, kind: JumpKind, name: Vec<u8>, span: Span) {
        let slot = self.locals.len() as u32;
        let function_id = self.funcs.last().expect("function").id;
        let goto_index = if kind == JumpKind::Goto {
            let index = self.resolution.gotos.len();
            self.resolution.gotos.push(GotoUse {
                name: name.clone(),
                span,
                function_id,
                active_locals: slot,
                target: None,
            });
            Some(index)
        } else {
            None
        };
        let mut resolved = false;
        if kind == JumpKind::Goto
            && let Some(label) = self
                .scopes
                .last()
                .and_then(|scope| scope.labels.iter().find(|label| label.name == name))
        {
            let index = label.index;
            if let Some(goto_index) = goto_index {
                self.resolution.gotos[goto_index].target = Some(index);
            }
            resolved = true;
        }
        self.scopes.last_mut().expect("scope").jumps.push(JumpRec {
            kind,
            name,
            slot,
            span,
            goto_index,
            resolved,
        });
    }

    fn walk_block(&mut self, block: &Block, is_loop_body: bool, adjust_trailing: bool) {
        if is_loop_body {
            self.enter_scope();
        }
        let trailing = trailing_labels(block, adjust_trailing);
        for (index, stmt) in block.stmts.iter().enumerate() {
            self.walk_stmt(stmt, trailing[index]);
        }
        if is_loop_body {
            self.leave_scope(block.span.end);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt, trailing_label: bool) {
        match stmt {
            Stmt::Empty { .. } => {}
            Stmt::Do { body, span, .. } => {
                self.enter_scope();
                self.walk_block(body, false, true);
                self.leave_scope(span.end);
            }
            Stmt::While {
                cond, body, span, ..
            } => {
                self.walk_expr(cond);
                self.enter_scope();
                self.walk_block(body, true, true);
                self.leave_loop(span.end);
            }
            Stmt::Repeat {
                body, until, span, ..
            } => {
                self.enter_scope();
                self.enter_scope();
                self.walk_block(body, false, false);
                self.walk_expr(until);
                let scope = self.scopes.len() - 1;
                self.reject_continues_into_locals(scope);
                self.leave_scope(until.span().end);
                self.leave_loop(span.end);
            }
            Stmt::If {
                arms,
                else_body,
                span,
                ..
            } => {
                for arm in arms {
                    self.walk_expr(&arm.cond);
                    self.enter_scope();
                    self.walk_block(&arm.body, false, true);
                    self.leave_scope(arm.body.span.end);
                }
                if let Some(body) = else_body {
                    self.enter_scope();
                    self.walk_block(body, false, true);
                    self.leave_scope(body.span.end);
                }
                let _ = span;
            }
            Stmt::NumericFor {
                name,
                start,
                limit,
                step,
                body,
                span,
                ..
            } => {
                self.walk_expr(start);
                self.walk_expr(limit);
                if let Some(step) = step {
                    self.walk_expr(step);
                }
                self.enter_scope();
                self.add_binding(name, BindingKind::ForIndex, false, body.span.start, &[]);
                self.walk_block(body, true, true);
                self.leave_loop(span.end);
            }
            Stmt::GenericFor {
                names,
                iters,
                body,
                span,
                ..
            } => {
                self.walk_expr_list(iters);
                self.enter_scope();
                for name in names {
                    self.add_binding(name, BindingKind::ForIndex, false, body.span.start, &[]);
                }
                self.walk_block(body, true, true);
                self.leave_loop(span.end);
            }
            Stmt::Local {
                names,
                values,
                span,
                ..
            } => {
                self.walk_expr_list(values);
                let mut pending = Vec::new();
                for local in names {
                    self.add_binding(
                        &local.name,
                        BindingKind::Local,
                        local.const_binding,
                        span.end,
                        &pending,
                    );
                    if local.const_binding {
                        pending.push(local.name.bytes.clone());
                    }
                }
            }
            Stmt::LocalFunction {
                name,
                const_binding,
                func,
                span,
                ..
            } => {
                self.add_binding(
                    name,
                    BindingKind::Function,
                    *const_binding,
                    name.span.start,
                    &[],
                );
                self.walk_func(func, false);
                let _ = span;
            }
            Stmt::Function { name, body, .. } => {
                if name.method.is_none() && name.segments.len() == 1 {
                    self.assign_name(&name.segments[0]);
                }
                self.walk_func(body, name.method.is_some());
            }
            Stmt::Assign {
                targets, values, ..
            } => {
                self.walk_expr_list(values);
                for target in targets {
                    self.walk_target(target, true);
                }
            }
            Stmt::CompoundAssign { target, value, .. } => {
                self.walk_target(target, true);
                self.walk_expr(value);
            }
            Stmt::Call { call, .. } => self.walk_expr(call),
            Stmt::Label { name, .. } => self.define_label(name, trailing_label),
            Stmt::Goto { name, span, .. } => {
                self.add_jump(JumpKind::Goto, name.bytes.clone(), *span)
            }
            Stmt::Break { span } => self.add_jump(JumpKind::Break, Vec::new(), *span),
            Stmt::Continue { span } => self.add_jump(JumpKind::Continue, Vec::new(), *span),
            Stmt::Return { values, .. } => self.walk_expr_list(values),
        }
    }

    fn walk_func(&mut self, func: &FuncBody, method: bool) {
        self.enter_function(func.vararg);
        if method || func.self_param {
            let self_name = Name {
                span: func.span,
                bytes: b"self".to_vec(),
            };
            self.add_binding(
                &self_name,
                BindingKind::SelfParam,
                false,
                func.body.span.start,
                &[],
            );
        }
        for param in &func.params {
            self.add_binding(param, BindingKind::Param, false, func.body.span.start, &[]);
        }
        self.walk_block(&func.body, false, true);
        self.leave_scope(func.span.end);
        self.funcs.pop();
    }

    fn walk_short(&mut self, params: &[Name], vararg: bool, body: &ShortBody, span: Span) {
        self.enter_function(vararg);
        for param in params {
            let visible = match body {
                ShortBody::Expr(expr) => expr.span().start,
                ShortBody::Block(block) => block.span.start,
            };
            self.add_binding(param, BindingKind::Param, false, visible, &[]);
        }
        match body {
            ShortBody::Expr(expr) => self.walk_expr(expr),
            ShortBody::Block(block) => self.walk_block(block, false, true),
        }
        self.leave_scope(span.end);
        self.funcs.pop();
    }

    fn walk_target(&mut self, target: &AssignTarget, assign: bool) {
        match target {
            AssignTarget::Name(name) => {
                if assign {
                    self.assign_name(name);
                }
            }
            AssignTarget::Index { table, key, .. } => {
                self.walk_expr(table);
                self.walk_expr(key);
            }
            AssignTarget::Field { table, .. } => self.walk_expr(table),
        }
    }

    fn walk_expr_list(&mut self, list: &ExprList) {
        for expr in &list.exprs {
            self.walk_expr(expr);
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Nil { .. }
            | Expr::Bool { .. }
            | Expr::Number { .. }
            | Expr::String { .. }
            | Expr::Name { .. } => {}
            Expr::Vararg { span } => {
                if !self.funcs.last().is_some_and(|func| func.vararg) {
                    self.resolution
                        .errors
                        .push(ResolveError::VarargOutside { span: *span });
                }
            }
            Expr::Paren { inner, .. } => self.walk_expr(inner),
            Expr::Unary { expr, .. } => self.walk_expr(expr),
            Expr::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            Expr::BinaryChain { base, steps, .. } => {
                self.walk_expr(base);
                for step in steps {
                    self.walk_expr(&step.expr);
                }
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
                ..
            } => {
                self.walk_expr(cond);
                self.walk_expr(then_expr);
                self.walk_expr(else_expr);
            }
            Expr::SuffixChain { base, steps, .. } => {
                self.walk_expr(base);
                for (index, step) in steps.iter().enumerate() {
                    match step {
                        Suffix::Field { .. } => {}
                        Suffix::Index { key, .. } => self.walk_expr(key),
                        Suffix::Call { span, args, .. } => {
                            // A later call's callee is a suffix, not the base name.
                            if index == 0 {
                                self.note_import(*span, base, args);
                            }
                            self.walk_args(args);
                        }
                        Suffix::Method { args, .. } => self.walk_args(args),
                    }
                }
            }
            Expr::Function { body, .. } => self.walk_func(body, false),
            Expr::ShortFunction {
                params,
                vararg,
                body,
                span,
                ..
            } => {
                self.walk_short(params, *vararg, body, *span);
            }
            Expr::Table { fields, .. } => {
                for field in fields {
                    match field {
                        Field::Index { key, value, .. } => {
                            self.walk_expr(key);
                            self.walk_expr(value);
                        }
                        Field::Name { value, .. } | Field::Item { value, .. } => {
                            self.walk_expr(value)
                        }
                    }
                }
            }
        }
    }

    fn walk_args(&mut self, args: &Args) {
        match args {
            Args::List(list) => self.walk_expr_list(list),
            Args::String { .. } => {}
            Args::Table { fields, .. } => {
                for field in fields {
                    match field {
                        Field::Index { key, value, .. } => {
                            self.walk_expr(key);
                            self.walk_expr(value);
                        }
                        Field::Name { value, .. } | Field::Item { value, .. } => {
                            self.walk_expr(value)
                        }
                    }
                }
            }
        }
    }

    fn note_import(&mut self, call_span: Span, callee: &Expr, args: &Args) {
        let Expr::Name { name, .. } = callee else {
            return;
        };
        if name.bytes != b"require" {
            return;
        }
        let (spec_span, spec) = match args {
            Args::String { span, bytes } => (*span, bytes.clone()),
            Args::List(list) => match list.exprs.first() {
                Some(Expr::String { span, bytes }) => (*span, bytes.clone()),
                _ => return,
            },
            Args::Table { .. } => return,
        };
        self.resolution.imports.push(ImportSite {
            call_span,
            spec_span,
            spec,
        });
    }
}

fn trailing_labels(block: &Block, adjust: bool) -> Vec<bool> {
    let mut flags = vec![false; block.stmts.len()];
    if !adjust {
        return flags;
    }
    for (index, stmt) in block.stmts.iter().enumerate() {
        if !matches!(stmt, Stmt::Label { .. }) {
            continue;
        }
        let rest_are_labels = block.stmts[index + 1..]
            .iter()
            .all(|stmt| matches!(stmt, Stmt::Label { .. } | Stmt::Empty { .. }));
        flags[index] = rest_are_labels;
    }
    flags
}
