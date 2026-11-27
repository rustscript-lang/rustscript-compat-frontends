//! Bounded final-IR removal of three explicitly registered compiler helpers.
//!
//! Flat declaration IDs are keys, not Vec ordinals (Core typing/codegen use
//! maps; the linker creates its own dense IDs). Keep every retained ID exactly:
//! this identity bijection also preserves host metadata, source code constants,
//! semantic provenance and capture slots without rebuilding opaque authority.
use std::collections::{HashMap, HashSet};
use vm::compiler::ir::{FunctionDecl, FunctionRefTarget, ParsedCallTarget, ParsedSemanticIndex};
use vm::{Expr, FrontendIr, Stmt, TypeSchema};

#[derive(Clone, Copy)]
pub(super) enum Role {
    LengthKey,
    Put,
    Delete,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::LengthKey => "__compat_es5_function_length_key",
            Self::Put => "__compat_es5_ref_put",
            Self::Delete => "__compat_es5_delete_property",
        }
    }
    fn arity(self) -> u8 {
        match self {
            Self::LengthKey => 1,
            Self::Put => 3,
            Self::Delete => 2,
        }
    }
}

/// Only the lowerer's actual returned helper registrations construct these.
/// A matching user spelling alone never puts a declaration in the candidate set.
pub(super) struct OwnedHelper {
    pub index: u16,
    pub role: Role,
}

/// Exhaustive traversal, including bodies that may never execute. Closure
/// captures contain local slots, not function IDs; their source/target slots
/// invalidate the single-definition callable exclusion below.
fn expr(e: &Expr, visit: &mut impl FnMut(&Expr), stmt: &mut impl FnMut(&Stmt)) {
    visit(e);
    match e {
        Expr::Call(_, _, args, _, _)
        | Expr::ModuleCall(_, _, args, _)
        | Expr::LocalCall(_, _, args, _) => {
            for a in args {
                expr(a, visit, stmt);
            }
        }
        Expr::Closure(c) => expr(&c.body, visit, stmt),
        Expr::ClosureCall(c, args) => {
            expr(&c.body, visit, stmt);
            for a in args {
                expr(a, visit, stmt);
            }
        }
        Expr::Add(a, b)
        | Expr::Sub(a, b)
        | Expr::Mul(a, b)
        | Expr::Div(a, b)
        | Expr::Mod(a, b)
        | Expr::And(a, b)
        | Expr::Or(a, b)
        | Expr::Eq(a, b)
        | Expr::Lt(a, b)
        | Expr::Gt(a, b) => {
            expr(a, visit, stmt);
            expr(b, visit, stmt);
        }
        Expr::Neg(a)
        | Expr::Not(a)
        | Expr::ToOwned(a)
        | Expr::Borrow(a)
        | Expr::BorrowMut(a)
        | Expr::ResultConstruct { payload: a, .. }
        | Expr::ResultQuery { value: a, .. }
        | Expr::ResultTry { value: a, .. } => expr(a, visit, stmt),
        Expr::OptionalGet { container, key, .. } => {
            expr(container, visit, stmt);
            expr(key, visit, stmt);
        }
        Expr::OptionUnwrapOr {
            value, fallback, ..
        } => {
            expr(value, visit, stmt);
            expr(fallback, visit, stmt);
        }
        Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            expr(condition, visit, stmt);
            expr(then_expr, visit, stmt);
            expr(else_expr, visit, stmt);
        }
        Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            expr(value, visit, stmt);
            for (_, a) in arms {
                expr(a, visit, stmt);
            }
            expr(default, visit, stmt);
        }
        Expr::Block { stmts, expr: e } => {
            statements(stmts, visit, stmt);
            expr(e, visit, stmt);
        }
        Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            expr(value, visit, stmt);
            expr(ok_expr, visit, stmt);
            expr(err_expr, visit, stmt);
        }
        Expr::ResultConvert { value, .. } => expr(value, visit, stmt),
        Expr::ResultMethod { receiver, argument, .. }
        | Expr::OptionMethod { receiver, argument, .. } => {
            expr(receiver, visit, stmt);
            expr(argument, visit, stmt);
        }
        Expr::OptionConstruct { payload, .. } => {
            if let Some(payload) = payload {
                expr(payload, visit, stmt);
            }
        }
        Expr::OptionMatch {
            value,
            some_expr,
            none_expr,
            ..
        } => {
            expr(value, visit, stmt);
            expr(some_expr, visit, stmt);
            expr(none_expr, visit, stmt);
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
        | Expr::Unit => {}
    }
}
fn statement(s: &Stmt, visit: &mut impl FnMut(&Expr), stmt: &mut impl FnMut(&Stmt)) {
    stmt(s);
    match s {
        Stmt::Let { expr: e, .. } | Stmt::Assign { expr: e, .. } | Stmt::Expr { expr: e, .. } => {
            expr(e, visit, stmt)
        }
        Stmt::ClosureLet { closure, .. } => expr(&Expr::Closure(closure.clone()), visit, stmt),
        Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            expr(condition, visit, stmt);
            statements(then_branch, visit, stmt);
            statements(else_branch, visit, stmt);
        }
        Stmt::For {
            init,
            condition,
            post,
            body,
            ..
        } => {
            statement(init, visit, stmt);
            expr(condition, visit, stmt);
            statement(post, visit, stmt);
            statements(body, visit, stmt);
        }
        Stmt::While {
            condition, body, ..
        } => {
            expr(condition, visit, stmt);
            statements(body, visit, stmt);
        }
        Stmt::Noop { .. }
        | Stmt::FuncDecl { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Drop { .. } => {}
    }
}
fn statements(ss: &[Stmt], visit: &mut impl FnMut(&Expr), stmt: &mut impl FnMut(&Stmt)) {
    for s in ss {
        statement(s, visit, stmt);
    }
}
fn all(ir: &FrontendIr, visit: &mut impl FnMut(&Expr), stmt: &mut impl FnMut(&Stmt)) {
    statements(&ir.stmts, visit, stmt);
    for f in ir.function_impls.values() {
        statements(&f.body_stmts, visit, stmt);
        expr(&f.body_expr, visit, stmt);
    }
}

/// Conservative static query over the real lowered body and completion
/// expression: may any of them still consume `slot`? Only the frame's own
/// slot consumers count, and every slot-bearing IR field is inspected
/// exhaustively, so an absent hit proves the activation frame is unused.
/// Anonymous closures and closure statements fail closed: their capture
/// copies can reach the frame through their own slot metadata even when
/// no direct expression names it. The traversal reuses the exhaustive
/// `expr`/`statement` visitors above with their node callbacks.
pub(super) fn slot_may_be_used(stmts: &[Stmt], completion: &Expr, slot: u16) -> bool {
    let used = std::cell::Cell::new(false);
    let mut probe_expr = |e: &Expr| {
        let hit = match e {
            Expr::Var(i) | Expr::MoveVar(i) => *i == slot,
            Expr::MoveField { root, .. } | Expr::MoveIndex { root, .. } => *root == slot,
            Expr::LocalCall(callee, _, _, _) => *callee == slot,
            Expr::OptionalGet {
                container_slot,
                key_slot,
                ..
            } => *container_slot == slot || *key_slot == slot,
            Expr::OptionUnwrapOr { value_slot, .. } => *value_slot == slot,
            Expr::Match {
                value_slot,
                result_slot,
                arms,
                ..
            } => {
                *value_slot == slot
                    || *result_slot == slot
                    || arms.iter().any(|(pattern, _)| {
                        matches!(
                            pattern,
                            vm::compiler::ir::MatchPattern::SomeBinding(i) if *i == slot
                        )
                    })
            }
            Expr::ResultMatch {
                value_slot,
                ok_binding,
                err_binding,
                ..
            } => *value_slot == slot || *ok_binding == Some(slot) || *err_binding == Some(slot),
            Expr::ResultTry {
                value_slot,
                payload_slot,
                exit_cleanup,
                ..
            } => {
                *value_slot == slot
                    || *payload_slot == slot
                    || exit_cleanup.iter().any(|(local, _)| *local == slot)
            }
            Expr::ResultConvert {
                value_slot,
                ok_binding,
                err_binding,
                ..
            } => *value_slot == slot || *ok_binding == slot || *err_binding == slot,
            Expr::ResultMethod { slots, .. } => {
                slots.receiver == slot
                    || slots.argument == slot
                    || slots.ok_binding == slot
                    || slots.err_binding == Some(slot)
            }
            Expr::OptionMethod { slots, .. } => {
                slots.receiver == slot || slots.argument == slot || slots.some_binding == slot
            }
            Expr::OptionMatch {
                value_slot,
                some_binding,
                ..
            } => *value_slot == slot || *some_binding == Some(slot),
            Expr::Closure(_) | Expr::ClosureCall(_, _) => true,
            Expr::Null
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::String(_)
            | Expr::Bytes(_)
            | Expr::FunctionRef(_, _)
            | Expr::ModuleFunctionRef(_, _)
            | Expr::UnresolvedFunctionRef { .. }
            | Expr::Call(..)
            | Expr::ModuleCall(..)
            | Expr::Add(..)
            | Expr::Sub(..)
            | Expr::Mul(..)
            | Expr::Div(..)
            | Expr::Mod(..)
            | Expr::Neg(..)
            | Expr::Not(..)
            | Expr::And(..)
            | Expr::Or(..)
            | Expr::Eq(..)
            | Expr::Lt(..)
            | Expr::Gt(..)
            | Expr::IfElse { .. }
            | Expr::ToOwned(..)
            | Expr::Borrow(..)
            | Expr::BorrowMut(..)
            | Expr::Block { .. }
            | Expr::ResultConstruct { .. }
            | Expr::ResultQuery { .. } => false,
            | Expr::Unit
            | Expr::OptionConstruct { .. } => false,
        };
        if hit {
            used.set(true);
        }
    };
    let mut probe_stmt = |s: &Stmt| {
        let hit = match s {
            Stmt::Let { index, .. } | Stmt::Assign { index, .. } | Stmt::Drop { index, .. } => {
                *index == slot
            }
            Stmt::ClosureLet { .. } => true,
            Stmt::Noop { .. }
            | Stmt::FuncDecl { .. }
            | Stmt::Expr { .. }
            | Stmt::IfElse { .. }
            | Stmt::For { .. }
            | Stmt::While { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. } => false,
        };
        if hit {
            used.set(true);
        }
    };
    statements(stmts, &mut probe_expr, &mut probe_stmt);
    expr(completion, &mut probe_expr, &mut probe_stmt);
    used.get()
}

/// A deliberately narrow, strict callable exclusion, not a points-to guess.
/// Number arguments cannot denote our nominal carrier parameter. This proves
/// all existing SDK `(Number,Number,Number)->RefResult` LocalCalls exclude
/// these helpers, including every operation of their owning closure. Other
/// dynamic calls retain every candidate whose signature is not excluded.
/// Any reassignment, duplicate definition, parameter or capture invalidates
/// this fact. Unknown types never establish disjointness.
fn excludes(schema: Option<&TypeSchema>, candidate: &FunctionDecl) -> bool {
    let Some(TypeSchema::Callable { params, .. }) = schema else {
        return false;
    };
    if params.len() != usize::from(candidate.arity) {
        return true;
    }
    params.iter().zip(&candidate.arg_schemas).any(|(a, b)| {
        matches!(
            (a, b),
            (
                TypeSchema::Number
                    | TypeSchema::Int
                    | TypeSchema::Float
                    | TypeSchema::Bool
                    | TypeSchema::String
                    | TypeSchema::Null,
                Some(TypeSchema::Named(_, _))
            )
        )
    })
}

fn metadata_roots(
    parsed: &ParsedSemanticIndex,
    roots: &mut HashSet<u16>,
    unknown: &mut bool,
    locals: &mut Vec<u16>,
) {
    for c in &parsed.call_sites {
        match c.target {
            ParsedCallTarget::Function(i) => {
                roots.insert(i);
            }
            ParsedCallTarget::Local(i) => locals.push(i),
            ParsedCallTarget::Module(_) | ParsedCallTarget::Unresolved => *unknown = true,
        }
    }
    for r in &parsed.func_refs {
        match r.target {
            FunctionRefTarget::Function(i) => {
                roots.insert(i);
            }
            FunctionRefTarget::Module(_) => *unknown = true,
        }
    }
    roots.extend(parsed.func_decls.iter().map(|f| f.function_index));
    for s in &parsed.scopes {
        roots.extend(&s.functions);
    }
}

pub(super) fn prune(ir: &mut FrontendIr, owned: &[OwnedHelper]) {
    if owned.is_empty() {
        return;
    }
    let decls: HashMap<_, _> = ir.functions.iter().map(|d| (d.index, d)).collect();
    if decls.len() != ir.functions.len()
        || ir.semantic_index.is_some()
        || !ir.use_declarations.is_empty()
        || !ir.implicit_extern_names.is_empty()
        || ir
            .catalog_visibility
            .as_ref()
            .is_some_and(|v| !v.module_namespace_aliases.is_empty())
        || (!ir.stmt_sources.is_empty() && ir.stmt_sources.len() != ir.stmts.len())
    {
        return;
    }
    let declaration_sites = std::cell::RefCell::new(HashMap::<u16, usize>::new());
    all(ir, &mut |_| {}, &mut |s| {
        if let Stmt::FuncDecl { index, .. } = s {
            *declaration_sites.borrow_mut().entry(*index).or_default() += 1;
        }
    });
    let declaration_sites = declaration_sites.into_inner();
    let mut candidates = HashSet::new();
    for o in owned {
        let Some(d) = decls.get(&o.index) else {
            return;
        };
        let Some(f) = ir.function_impls.get(&o.index) else {
            return;
        };
        // Private compiler kind plus actual owned registration, never a prefix.
        if d.name != o.role.name()
            || d.arity != o.role.arity()
            || d.exported
            || d.symbol.is_some()
            || declaration_sites.get(&o.index) != Some(&1)
            || !d.type_params.is_empty()
            || !f.capture_copies.is_empty()
            || f.param_slots.len() != usize::from(d.arity)
            || !matches!(d.arg_schemas.first(), Some(Some(TypeSchema::Named(n,args)))
                if n == "__CompatRefCarrier" && args.is_empty())
            || ir
                .host_api_metadata
                .as_ref()
                .is_some_and(|m| m.candidates(o.index).is_some())
        {
            continue;
        }
        // Definitions are the only removable root statements. Anything other
        // than the unique top-level defining statement is an unsupported kind.
        if ir
            .stmts
            .iter()
            .filter(|s| {
                matches!(s, Stmt::FuncDecl { index,name,arity,exported:false,has_impl:true,.. }
            if *index == o.index && name == &d.name && *arity == d.arity)
            })
            .count()
            == 1
        {
            candidates.insert(o.index);
        }
    }
    if candidates.is_empty() {
        return;
    }

    let definitions = std::cell::RefCell::new(HashMap::<u16, Option<TypeSchema>>::new());
    let invalid = std::cell::RefCell::new(HashSet::new());
    for f in ir.function_impls.values() {
        invalid.borrow_mut().extend(&f.param_slots);
        for (a, b) in &f.capture_copies {
            invalid.borrow_mut().extend([*a, *b]);
        }
    }
    all(
        ir,
        &mut |e| match e {
            Expr::Closure(c) | Expr::ClosureCall(c, _) => {
                invalid.borrow_mut().extend(&c.param_slots);
                for (a, b) in &c.capture_copies {
                    invalid.borrow_mut().extend([*a, *b]);
                }
            }
            Expr::OptionalGet {
                container_slot,
                key_slot,
                ..
            } => {
                invalid.borrow_mut().extend([*container_slot, *key_slot]);
            }
            Expr::OptionUnwrapOr { value_slot, .. } => {
                invalid.borrow_mut().insert(*value_slot);
            }
            Expr::Match {
                value_slot,
                result_slot,
                arms,
                ..
            } => {
                invalid.borrow_mut().extend([*value_slot, *result_slot]);
                for (p, _) in arms {
                    if let vm::compiler::ir::MatchPattern::SomeBinding(i) = p {
                        invalid.borrow_mut().insert(*i);
                    }
                }
            }
            Expr::ResultMatch {
                value_slot,
                ok_binding,
                err_binding,
                ..
            } => {
                invalid.borrow_mut().insert(*value_slot);
                invalid
                    .borrow_mut()
                    .extend(ok_binding.iter().chain(err_binding).copied());
            }
            Expr::ResultTry {
                value_slot,
                payload_slot,
                ..
            } => {
                invalid.borrow_mut().extend([*value_slot, *payload_slot]);
            }
            Expr::MoveVar(i)
            | Expr::MoveField { root: i, .. }
            | Expr::MoveIndex { root: i, .. } => {
                invalid.borrow_mut().insert(*i);
            }
            Expr::BorrowMut(value) => expr(
                value,
                &mut |e| {
                    if let Expr::Var(i) | Expr::MoveVar(i) = e {
                        invalid.borrow_mut().insert(*i);
                    }
                },
                &mut |_| {},
            ),
            _ => {}
        },
        &mut |s| match s {
            Stmt::Let {
                index,
                declared_schema,
                ..
            } => {
                if definitions
                    .borrow_mut()
                    .insert(*index, declared_schema.clone())
                    .is_some()
                {
                    invalid.borrow_mut().insert(*index);
                }
            }
            Stmt::Assign { index, .. } | Stmt::Drop { index, .. } => {
                invalid.borrow_mut().insert(*index);
            }
            _ => {}
        },
    );
    let mut definitions = definitions.into_inner();
    for i in invalid.into_inner() {
        definitions.remove(&i);
    }
    let schema = |i: &u16| definitions.get(i).and_then(Option::as_ref);

    // Root, every noncandidate named body, and every anonymous body are roots.
    // Build complete edges first; no condition/selector/constant evaluation.
    let mut roots: HashSet<u16> = decls
        .keys()
        .filter(|i| !candidates.contains(i))
        .copied()
        .collect();
    let mut edges = HashMap::<u16, HashSet<u16>>::new();
    let mut unknown = false;
    let mut metadata_locals = Vec::new();
    if let Some(p) = &ir.parsed_semantic_index {
        metadata_roots(p, &mut roots, &mut unknown, &mut metadata_locals);
    }
    for i in metadata_locals {
        for c in &candidates {
            if !excludes(schema(&i), decls[c]) {
                roots.insert(*c);
            }
        }
    }
    // Metadata keyed by function IDs must never be orphaned; source ownership
    // on a candidate is conservatively a root instead of being discarded.
    roots.extend(ir.function_sources.keys());
    let mut collect = |ss: &[Stmt], tail: Option<&Expr>| {
        let found = std::cell::RefCell::new(HashSet::new());
        let mut scan = |e: &Expr| match e {
            Expr::Call(i, _, _, _, _) if *i == vm::BuiltinFunction::BindCallable.call_index() => {
                // An explicit runtime prototype constructor is not a source
                // code ID or an ordinary direct call. Its target is unproved.
                unknown = true;
            }
            Expr::Call(i, _, _, _, _) | Expr::FunctionRef(i, _) => {
                found.borrow_mut().insert(*i);
            }
            Expr::LocalCall(i, _, _, _) => {
                for c in &candidates {
                    if !excludes(schema(i), decls[c]) {
                        found.borrow_mut().insert(*c);
                    }
                }
            }
            Expr::ModuleCall(..)
            | Expr::ModuleFunctionRef(..)
            | Expr::UnresolvedFunctionRef { .. } => unknown = true,
            _ => {}
        };
        // A nested declaration is an index-bearing reference, not removable.
        let mut nested = |s: &Stmt| {
            if let Stmt::FuncDecl { index, .. } = s {
                found.borrow_mut().insert(*index);
            }
        };
        statements(ss, &mut scan, &mut nested);
        if let Some(e) = tail {
            expr(e, &mut scan, &mut nested);
        }
        found.into_inner()
    };
    for s in &ir.stmts {
        if !matches!(s, Stmt::FuncDecl { index,.. } if candidates.contains(index)) {
            roots.extend(collect(std::slice::from_ref(s), None));
        }
    }
    for (i, f) in &ir.function_impls {
        edges.insert(*i, collect(&f.body_stmts, Some(&f.body_expr)));
    }
    // Keep the defining container too: rooting only a closure's callees would
    // still erase the closure itself when its containing candidate is removed.
    for i in &candidates {
        let f = &ir.function_impls[i];
        let mut has_closure = false;
        let mut scan = |e: &Expr| {
            if matches!(e, Expr::Closure(_) | Expr::ClosureCall(_, _)) {
                has_closure = true;
            }
        };
        statements(&f.body_stmts, &mut scan, &mut |_| {});
        expr(&f.body_expr, &mut scan, &mut |_| {});
        if has_closure {
            roots.insert(*i);
        }
    }
    // Even a closure inside an otherwise-dead named body remains a root.
    all(
        ir,
        &mut |e| match e {
            Expr::FunctionRef(i, _) => {
                roots.insert(*i);
            }
            Expr::LocalCall(i, _, _, _) => {
                for c in &candidates {
                    if !excludes(schema(i), decls[c]) {
                        roots.insert(*c);
                    }
                }
            }
            Expr::Closure(c) | Expr::ClosureCall(c, _) => {
                roots.extend(collect(&[], Some(&c.body)));
            }
            _ => {}
        },
        &mut |_| {},
    );
    if unknown {
        return;
    }
    let mut pending: Vec<_> = roots.iter().copied().collect();
    while let Some(i) = pending.pop() {
        if let Some(es) = edges.get(&i) {
            for c in es {
                if roots.insert(*c) {
                    pending.push(*c);
                }
            }
        }
    }
    let dropped: HashSet<_> = candidates.difference(&roots).copied().collect();
    if dropped.is_empty() {
        return;
    }
    ir.functions.retain(|f| !dropped.contains(&f.index));
    ir.function_impls.retain(|i, _| !dropped.contains(i));
    let mut at = 0;
    ir.stmts.retain(|s| {
        let keep = !matches!(s, Stmt::FuncDecl { index,.. } if dropped.contains(index));
        if !ir.stmt_sources.is_empty() && !keep {
            ir.stmt_sources.remove(at);
        }
        if keep {
            at += 1;
        }
        keep
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm::compiler::ir::{ClosureExpr, FunctionImpl};
    fn call(i: u16) -> Expr {
        Expr::Call(i, vec![], vec![], None, None)
    }
    fn let_stmt(index: u16, expr: Expr) -> Stmt {
        Stmt::Let {
            index,
            declared_schema: Some(TypeSchema::Null),
            expr,
            line: 1,
        }
    }
    fn assign_stmt(index: u16) -> Stmt {
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr: Expr::Null,
            line: 1,
        }
    }
    fn drop_stmt(index: u16) -> Stmt {
        Stmt::Drop { index, line: 1 }
    }
    fn expr_stmt(expr: Expr) -> Stmt {
        Stmt::Expr { expr, line: 1 }
    }
    fn empty_closure() -> ClosureExpr {
        ClosureExpr {
            param_slots: vec![],
            capture_copies: vec![],
            body: Box::new(Expr::Null),
        }
    }
    fn result_try(
        value_slot: u16,
        payload_slot: u16,
        exit_cleanup: Vec<(u16, TypeSchema)>,
    ) -> Expr {
        Expr::ResultTry {
            value: Box::new(Expr::Null),
            value_slot,
            payload_slot,
            operand_schema: TypeSchema::Null,
            return_schema: TypeSchema::Null,
            exit_cleanup,
            span: vm::Span::new(0, 0, 0),
            semantic_id: None,
        }
    }
    fn fixture() -> (FrontendIr, Vec<OwnedHelper>) {
        let mut ir = vm::LocalIrBuilder::new().finish(vec![]);
        let owned = vec![
            OwnedHelper {
                index: 0,
                role: Role::LengthKey,
            },
            OwnedHelper {
                index: 1,
                role: Role::Put,
            },
            OwnedHelper {
                index: 2,
                role: Role::Delete,
            },
        ];
        for o in &owned {
            let args = (0..o.role.arity())
                .map(|i| format!("p{i}"))
                .collect::<Vec<_>>();
            ir.functions.push(FunctionDecl {
                name: o.role.name().into(),
                arity: o.role.arity(),
                index: o.index,
                args: args.clone(),
                arg_schemas: vec![
                    Some(TypeSchema::Named("__CompatRefCarrier".into(), vec![]));
                    usize::from(o.role.arity())
                ],
                return_schema: Some(TypeSchema::Null),
                type_params: vec![],
                exported: false,
                return_type: vm::ValueType::Null,
                symbol: None,
            });
            ir.stmts.push(Stmt::FuncDecl {
                name: o.role.name().into(),
                index: o.index,
                arity: o.role.arity(),
                args,
                exported: false,
                has_impl: true,
                line: 1,
            });
            ir.function_impls.insert(
                o.index,
                FunctionImpl {
                    param_slots: (0..u16::from(o.role.arity()))
                        .map(|p| 10 + o.index * 4 + p)
                        .collect(),
                    capture_copies: vec![],
                    body_stmts: vec![],
                    body_expr: Expr::Null,
                    body_expr_line: 1,
                },
            );
        }
        (ir, owned)
    }
    #[test]
    fn p7_private_prune_firstclass_reference_is_global_root() {
        let (mut ir, owned) = fixture();
        ir.function_impls.get_mut(&0).unwrap().body_expr = Expr::FunctionRef(1, vec![]);
        prune(&mut ir, &owned);
        assert_eq!(
            ir.functions.iter().map(|f| f.index).collect::<Vec<_>>(),
            vec![1]
        );
    }
    #[test]
    fn p7_private_prune_dead_chain_scc_and_identity_indices() {
        let (mut ir, owned) = fixture();
        ir.function_impls.get_mut(&0).unwrap().body_expr = call(1);
        ir.function_impls.get_mut(&1).unwrap().body_expr = call(0);
        ir.stmts.push(Stmt::Expr {
            expr: Expr::Int(257),
            line: 4,
        });
        prune(&mut ir, &owned);
        assert!(ir.functions.is_empty() && ir.function_impls.is_empty());
        assert_eq!(
            format!("{:?}", ir.stmts),
            "[Expr { expr: Int(257), line: 4 }]"
        );
        let (mut ir, owned) = fixture();
        ir.function_impls.get_mut(&0).unwrap().body_expr = call(2);
        ir.stmts.push(Stmt::Expr {
            expr: call(0),
            line: 4,
        });
        prune(&mut ir, &owned);
        assert_eq!(
            ir.functions.iter().map(|f| f.index).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert!(matches!(ir.function_impls[&0].body_expr, Expr::Call(2, ..)));
    }
    #[test]
    fn p7_private_prune_unknown_and_typed_indirect_calls() {
        let (mut ir, owned) = fixture();
        ir.stmts.push(Stmt::Expr {
            expr: Expr::LocalCall(100, vec![], vec![], None),
            line: 1,
        });
        let before = ir.functions.clone();
        prune(&mut ir, &owned);
        assert_eq!(ir.functions, before);
        ir.stmts.insert(
            0,
            Stmt::Let {
                index: 100,
                declared_schema: Some(TypeSchema::Callable {
                    params: vec![TypeSchema::Named("__CompatRefCarrier".into(), vec![]); 3],
                    result: Box::new(TypeSchema::Null),
                }),
                expr: Expr::Null,
                line: 1,
            },
        );
        prune(&mut ir, &owned);
        assert_eq!(
            ir.functions.iter().map(|f| f.index).collect::<Vec<_>>(),
            vec![1]
        );
    }
    #[test]
    fn p7_private_prune_export_capture_and_unregistered_spelling() {
        let (mut ir, owned) = fixture();
        ir.functions[0].exported = true;
        ir.function_impls
            .get_mut(&1)
            .unwrap()
            .capture_copies
            .push((100, 101));
        prune(&mut ir, &owned);
        assert_eq!(
            ir.functions.iter().map(|f| f.index).collect::<Vec<_>>(),
            vec![0, 1]
        );
        let (mut ir, _) = fixture();
        let before = ir.functions.clone();
        prune(&mut ir, &[]);
        assert_eq!(ir.functions, before);
    }
    #[test]
    fn p7_private_prune_anonymous_and_noncandidate_source_roots() {
        let (mut ir, owned) = fixture();
        ir.function_impls.get_mut(&0).unwrap().body_expr = Expr::Closure(ClosureExpr {
            param_slots: vec![],
            capture_copies: vec![(100, 101)],
            body: Box::new(call(2)),
        });
        prune(&mut ir, &owned);
        assert_eq!(
            ir.functions.iter().map(|f| f.index).collect::<Vec<_>>(),
            vec![0, 2],
            "the closure's containing definition is also retained"
        );
        let (mut ir, owned) = fixture();
        ir.function_impls.get_mut(&0).unwrap().body_expr = Expr::Closure(ClosureExpr {
            param_slots: vec![],
            capture_copies: vec![(100, 101)],
            body: Box::new(call(2)),
        });
        ir.functions[1].name = "__compat_es5_source_1".into();
        ir.function_impls.get_mut(&1).unwrap().body_expr = call(0);
        prune(&mut ir, &owned);
        assert_eq!(ir.functions.len(), 3);
    }
    #[test]
    fn p7_private_prune_hidden_binding_invalidates_callable_exclusion() {
        for binder in [
            Expr::OptionUnwrapOr {
                value: Box::new(Expr::Null),
                value_slot: 100,
                fallback: Box::new(Expr::Null),
                semantic_id: None,
            },
            Expr::OptionalGet {
                field_projection: false,
                logical_projection: false,
                option_receiver: false,
                logical_unit_field: false,
                container: Box::new(Expr::Null),
                key: Box::new(Expr::Null),
                container_slot: 100,
                key_slot: 101,
                semantic_id: None,
            },
            Expr::Match {
                value_slot: 101,
                result_slot: 100,
                value: Box::new(Expr::Null),
                arms: vec![],
                default: Box::new(Expr::Null),
            },
        ] {
            let (mut ir, owned) = fixture();
            ir.stmts.push(Stmt::Let {
                index: 100,
                declared_schema: Some(TypeSchema::Callable {
                    params: vec![TypeSchema::Number; 3],
                    result: Box::new(TypeSchema::Null),
                }),
                expr: Expr::Null,
                line: 1,
            });
            ir.stmts.push(Stmt::Expr {
                expr: binder,
                line: 1,
            });
            ir.stmts.push(Stmt::Expr {
                expr: Expr::LocalCall(100, vec![], vec![], None),
                line: 1,
            });
            prune(&mut ir, &owned);
            assert_eq!(ir.functions.len(), 3);
        }
    }
    #[test]
    fn p7_private_prune_metadata_module_and_source_identity() {
        use vm::compiler::{ModuleId, SymbolId};
        let symbol = SymbolId {
            module: ModuleId(4),
            index: 2,
        };
        for e in [
            Expr::ModuleFunctionRef(symbol, vec![]),
            Expr::ModuleCall(symbol, vec![], vec![], None),
        ] {
            let (mut ir, owned) = fixture();
            ir.function_impls.get_mut(&0).unwrap().body_expr = e;
            prune(&mut ir, &owned);
            assert_eq!(ir.functions.len(), 3);
        }
        let (mut ir, owned) = fixture();
        let mut parsed = ParsedSemanticIndex::default();
        parsed.func_refs.push(vm::compiler::ir::FunctionRefSite {
            id: vm::compiler::ir::SemanticNodeId(1),
            ident_span: vm::Span::new(0, 0, 0),
            target: FunctionRefTarget::Function(2),
            name: "persistent".into(),
            scope_id: 0,
        });
        ir.parsed_semantic_index = Some(parsed);
        ir.function_sources.insert(0, "owned-source".into());
        prune(&mut ir, &owned);
        assert_eq!(
            ir.functions.iter().map(|f| f.index).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(ir.function_sources[&0], "owned-source");
    }
    #[test]
    fn p7_private_prune_unresolved_and_unknown_are_closed() {
        for e in [
            Expr::UnresolvedFunctionRef {
                name: "unknown".into(),
                type_args: vec![],
            },
            Expr::LocalCall(100, vec![], vec![], None),
            Expr::Call(
                vm::BuiltinFunction::BindCallable.call_index(),
                vec![],
                vec![Expr::Int(1), Expr::Null],
                None,
                None,
            ),
        ] {
            let (mut ir, owned) = fixture();
            ir.function_impls.get_mut(&0).unwrap().body_expr = e;
            prune(&mut ir, &owned);
            assert_eq!(ir.functions.len(), 3);
        }
    }
    #[test]
    fn p7_private_slot_query_scalar_and_call_slots() {
        let empty: [Stmt; 0] = [];
        assert!(
            !slot_may_be_used(&empty, &Expr::Null, 7),
            "a bare expression never names a slot"
        );
        let unrelated = [
            expr_stmt(Expr::Var(9)),
            expr_stmt(Expr::MoveVar(9)),
            expr_stmt(Expr::MoveField {
                root: 9,
                key: "k".into(),
            }),
            expr_stmt(Expr::MoveIndex { root: 9, index: 0 }),
            expr_stmt(Expr::LocalCall(9, vec![], vec![], None)),
        ];
        assert!(
            !slot_may_be_used(&unrelated, &Expr::Null, 7),
            "an unrelated slot is never a hit"
        );
        assert!(slot_may_be_used(&[expr_stmt(Expr::Var(7))], &Expr::Null, 7));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::MoveVar(7))],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::MoveField {
                root: 7,
                key: "k".into(),
            })],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::MoveIndex { root: 7, index: 0 })],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::LocalCall(7, vec![], vec![], None))],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(&[let_stmt(7, Expr::Null)], &Expr::Null, 7));
    }
    #[test]
    fn p7_private_slot_query_statement_writes_and_completion() {
        assert!(slot_may_be_used(&[assign_stmt(7)], &Expr::Null, 7));
        assert!(slot_may_be_used(&[drop_stmt(7)], &Expr::Null, 7));
        assert!(!slot_may_be_used(
            &[let_stmt(9, Expr::Null), assign_stmt(9), drop_stmt(9)],
            &Expr::Null,
            7
        ));
        assert!(
            slot_may_be_used(&[], &Expr::Var(7), 7),
            "the completion expression is part of the query"
        );
        assert!(!slot_may_be_used(&[], &Expr::Var(9), 7));
        assert!(slot_may_be_used(
            &[],
            &Expr::LocalCall(7, vec![], vec![], None),
            7
        ));
        assert!(
            slot_may_be_used(&[let_stmt(9, Expr::Var(7))], &Expr::Null, 7),
            "a body read still counts when the completion is clean"
        );
        assert!(!slot_may_be_used(
            &[let_stmt(9, Expr::Var(9))],
            &Expr::Null,
            7
        ));
    }
    #[test]
    fn p7_private_slot_query_optional_match_result_slots() {
        let optional_get = |container_slot: u16, key_slot: u16| {
            expr_stmt(Expr::OptionalGet {
                field_projection: false,
                logical_projection: false,
                option_receiver: false,
                logical_unit_field: false,
                container: Box::new(Expr::Null),
                key: Box::new(Expr::Null),
                container_slot,
                key_slot,
                semantic_id: None,
            })
        };
        assert!(slot_may_be_used(&[optional_get(7, 9)], &Expr::Null, 7));
        assert!(slot_may_be_used(&[optional_get(9, 7)], &Expr::Null, 7));
        assert!(!slot_may_be_used(&[optional_get(9, 9)], &Expr::Null, 7));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::OptionUnwrapOr {
                value: Box::new(Expr::Null),
                value_slot: 7,
                fallback: Box::new(Expr::Null),
                semantic_id: None,
            })],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::Match {
                value_slot: 9,
                result_slot: 7,
                value: Box::new(Expr::Null),
                arms: vec![],
                default: Box::new(Expr::Null),
            })],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::Match {
                value_slot: 9,
                result_slot: 9,
                value: Box::new(Expr::Null),
                arms: vec![(vm::compiler::ir::MatchPattern::SomeBinding(7), Expr::Null)],
                default: Box::new(Expr::Null),
            })],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::ResultMatch {
                value: Box::new(Expr::Null),
                value_slot: 9,
                ok_binding: Some(7),
                ok_expr: Box::new(Expr::Null),
                err_binding: None,
                err_expr: Box::new(Expr::Null),
                scrutinee_schema: TypeSchema::Null,
                span: vm::Span::new(0, 0, 0),
                semantic_id: None,
            })],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::ResultTry {
                value: Box::new(Expr::Null),
                value_slot: 9,
                payload_slot: 9,
                operand_schema: TypeSchema::Null,
                return_schema: TypeSchema::Null,
                exit_cleanup: vec![(7, TypeSchema::Number)],
                span: vm::Span::new(0, 0, 0),
                semantic_id: None,
            })],
            &Expr::Null,
            7
        ));
    }
    #[test]
    fn p7_private_slot_query_completion_exit_cleanup_and_closures_fail_closed() {
        assert!(slot_may_be_used(&[], &result_try(7, 9, vec![]), 7));
        assert!(slot_may_be_used(&[], &result_try(9, 7, vec![]), 7));
        assert!(!slot_may_be_used(
            &[],
            &result_try(9, 9, vec![(9, TypeSchema::Number)]),
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::Closure(empty_closure()))],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::ClosureCall(empty_closure(), vec![]))],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[Stmt::ClosureLet {
                line: 1,
                closure: empty_closure(),
                expected_schema: None,
            }],
            &Expr::Null,
            7
        ));
        assert!(slot_may_be_used(
            &[expr_stmt(Expr::Block {
                stmts: vec![let_stmt(7, Expr::Null)],
                expr: Box::new(Expr::Null),
            })],
            &Expr::Null,
            7
        ));
        assert!(!slot_may_be_used(
            &[Stmt::While {
                condition: Expr::Bool(true),
                body: vec![expr_stmt(Expr::Var(9))],
                line: 1,
            }],
            &Expr::Null,
            7
        ));
    }
}
