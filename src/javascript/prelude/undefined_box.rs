//! Factor only the exact pure canonical undefined primitive.
//!
//! The helper has no parameters, captures, or body locals. Each call still
//! evaluates the original MapNew plus five Set expression, so the map is
//! fresh and is never interned. If no index is disjoint from the complete
//! function set and the reserved builtin floor, the IR is left unchanged.

use super::super::representation as value;
use super::heap::declare_helper;
use std::collections::BTreeSet;
use vm::{BuiltinFunction, Expr, FrontendIr, Stmt};

const HELPER_NAME: &str = "__compat_es5_undefined";

pub(crate) fn factor(ir: &mut FrontendIr) {
    let Some(index) = collision_free_index(ir) else {
        return;
    };
    let mut replaced = 0usize;
    rewrite_stmts(&mut ir.stmts, index, &mut replaced);
    let keys: Vec<u16> = ir.function_impls.keys().copied().collect();
    for key in keys {
        let Some(body) = ir.function_impls.get_mut(&key) else {
            continue;
        };
        rewrite_stmts(&mut body.body_stmts, index, &mut replaced);
        rewrite_expr(&mut body.body_expr, index, &mut replaced);
    }
    if replaced == 0 {
        return;
    }
    let mut decls = Vec::new();
    declare_helper(
        ir,
        &mut decls,
        HELPER_NAME,
        index,
        0,
        Vec::new(),
        Vec::new(),
        Some(value::schema()),
        vm::ValueType::Map,
        Vec::new(),
        Vec::new(),
        value::undefined(),
        1,
    );
    if !ir.stmt_sources.is_empty() {
        ir.stmt_sources.push(None);
    }
    ir.stmts.extend(decls);
}

fn collision_free_index(ir: &FrontendIr) -> Option<u16> {
    if ir
        .functions
        .iter()
        .any(|function| function.name == HELPER_NAME)
    {
        return None;
    }
    let mut used = BTreeSet::new();
    for function in &ir.functions {
        used.insert(function.index);
    }
    for index in ir.function_impls.keys() {
        used.insert(*index);
    }
    for index in ir.function_sources.keys() {
        used.insert(*index);
    }
    if let Some(metadata) = &ir.host_api_metadata {
        for index in metadata.function_indices() {
            used.insert(index);
        }
    }
    let candidate = match used.iter().next_back() {
        Some(max) => max.checked_add(1)?,
        None => 0,
    };
    if used.contains(&candidate) || candidate >= BuiltinFunction::Len.call_index() {
        return None;
    }
    Some(candidate)
}

fn is_canonical_undefined(expr: &Expr) -> bool {
    let map_new = BuiltinFunction::MapNew.call_index();
    let set = BuiltinFunction::Set.call_index();
    let Some(inner) = peel_set(expr, set, "generation", |value| {
        matches!(value, Expr::Int(0))
    }) else {
        return false;
    };
    let Some(inner) = peel_set(inner, set, "ref_slot", |value| {
        matches!(value, Expr::Int(0))
    }) else {
        return false;
    };
    let Some(inner) = peel_set(inner, set, "flag", |value| {
        matches!(value, Expr::Bool(false))
    }) else {
        return false;
    };
    let Some(inner) = peel_set(
        inner,
        set,
        "number",
        |value| matches!(value, Expr::Float(number) if number.to_bits() == 0),
    ) else {
        return false;
    };
    let Some(inner) = peel_set(inner, set, "tag", |value| matches!(value, Expr::Int(0))) else {
        return false;
    };
    matches!(
        inner,
        Expr::Call(index, type_args, args, None, None)
            if *index == map_new && type_args.is_empty() && args.is_empty()
    )
}

fn peel_set<'a>(
    expr: &'a Expr,
    set: u16,
    key: &str,
    value_ok: impl Fn(&Expr) -> bool,
) -> Option<&'a Expr> {
    let Expr::Call(index, type_args, args, None, None) = expr else {
        return None;
    };
    if *index != set || !type_args.is_empty() || args.len() != 3 {
        return None;
    }
    let Expr::String(actual) = &args[1] else {
        return None;
    };
    if actual != key || !value_ok(&args[2]) {
        return None;
    }
    Some(&args[0])
}

fn direct_call(index: u16) -> Expr {
    Expr::Call(index, Vec::new(), Vec::new(), None, None)
}

fn rewrite_expr(expr: &mut Expr, helper: u16, replaced: &mut usize) {
    if is_canonical_undefined(expr) {
        *expr = direct_call(helper);
        *replaced += 1;
        return;
    }
    match expr {
        Expr::Call(_, _, args, _, _)
        | Expr::ModuleCall(_, _, args, _)
        | Expr::LocalCall(_, _, args, _) => {
            for arg in args.iter_mut() {
                rewrite_expr(arg, helper, replaced);
            }
        }
        Expr::Closure(closure) => rewrite_expr(&mut closure.body, helper, replaced),
        Expr::ClosureCall(closure, args) => {
            rewrite_expr(&mut closure.body, helper, replaced);
            for arg in args.iter_mut() {
                rewrite_expr(arg, helper, replaced);
            }
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
            rewrite_expr(left, helper, replaced);
            rewrite_expr(right, helper, replaced);
        }
        Expr::Neg(inner)
        | Expr::Not(inner)
        | Expr::ToOwned(inner)
        | Expr::Borrow(inner)
        | Expr::BorrowMut(inner) => rewrite_expr(inner, helper, replaced),
        Expr::OptionalGet { container, key, .. } => {
            rewrite_expr(container, helper, replaced);
            rewrite_expr(key, helper, replaced);
        }
        Expr::OptionUnwrapOr {
            value, fallback, ..
        } => {
            rewrite_expr(value, helper, replaced);
            rewrite_expr(fallback, helper, replaced);
        }
        Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_expr(condition, helper, replaced);
            rewrite_expr(then_expr, helper, replaced);
            rewrite_expr(else_expr, helper, replaced);
        }
        Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            rewrite_expr(value, helper, replaced);
            for (_, arm) in arms.iter_mut() {
                rewrite_expr(arm, helper, replaced);
            }
            rewrite_expr(default, helper, replaced);
        }
        Expr::Block { stmts, expr } => {
            rewrite_stmts(stmts, helper, replaced);
            rewrite_expr(expr, helper, replaced);
        }
        Expr::ResultConstruct { payload, .. } | Expr::ResultQuery { value: payload, .. } => {
            rewrite_expr(payload, helper, replaced);
        }
        Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            rewrite_expr(value, helper, replaced);
            rewrite_expr(ok_expr, helper, replaced);
            rewrite_expr(err_expr, helper, replaced);
        }
        Expr::ResultTry { value, .. } => rewrite_expr(value, helper, replaced),
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
}

fn rewrite_stmts(stmts: &mut [Stmt], helper: u16, replaced: &mut usize) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                rewrite_expr(expr, helper, replaced);
            }
            Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                rewrite_expr(condition, helper, replaced);
                rewrite_stmts(then_branch, helper, replaced);
                rewrite_stmts(else_branch, helper, replaced);
            }
            Stmt::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                rewrite_stmts(std::slice::from_mut(init.as_mut()), helper, replaced);
                rewrite_expr(condition, helper, replaced);
                rewrite_stmts(std::slice::from_mut(post.as_mut()), helper, replaced);
                rewrite_stmts(body, helper, replaced);
            }
            Stmt::While {
                condition, body, ..
            } => {
                rewrite_expr(condition, helper, replaced);
                rewrite_stmts(body, helper, replaced);
            }
            Stmt::ClosureLet { closure, .. } => rewrite_expr(&mut closure.body, helper, replaced),
            Stmt::Noop { .. }
            | Stmt::FuncDecl { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Drop { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm::compiler::ir::{FunctionDecl, SemanticNodeId};
    use vm::{LocalIrBuilder, TypeSchema, ValueType};

    fn bare(expr: Expr) -> FrontendIr {
        LocalIrBuilder::new().finish(vec![Stmt::Expr { expr, line: 1 }])
    }

    fn occupy_builtin_floor(ir: &mut FrontendIr) {
        let index = BuiltinFunction::Len.call_index() - 1;
        ir.functions.push(FunctionDecl {
            name: "occupied".to_owned(),
            arity: 0,
            index,
            args: Vec::new(),
            arg_schemas: Vec::new(),
            return_schema: None,
            type_params: Vec::new(),
            exported: false,
            return_type: ValueType::Map,
            symbol: None,
        });
    }

    #[test]
    fn unavailable_index_leaves_ir_unchanged() {
        let mut ir = bare(value::undefined());
        occupy_builtin_floor(&mut ir);
        let before = format!("{ir:?}");
        factor(&mut ir);
        assert_eq!(format!("{ir:?}"), before);
        assert!(
            ir.functions
                .iter()
                .all(|function| function.name != HELPER_NAME)
        );
    }

    #[test]
    fn malformed_and_other_tags_stay_inline() {
        let mut wrong_key = value::undefined();
        if let Expr::Call(_, _, args, _, _) = &mut wrong_key {
            args[1] = Expr::String("not-generation".to_owned());
        }
        let mut typed = value::undefined();
        if let Expr::Call(_, type_args, _, _, _) = &mut typed {
            type_args.push(TypeSchema::Int);
        }
        let mut identified = value::undefined();
        if let Expr::Call(_, _, _, _, semantic_id) = &mut identified {
            *semantic_id = Some(SemanticNodeId(7));
        }
        let mut negative_zero = value::undefined();
        let mut cursor = &mut negative_zero;
        for _ in 0..3 {
            let Expr::Call(_, _, args, _, _) = cursor else {
                panic!("chain");
            };
            cursor = &mut args[0];
        }
        let Expr::Call(_, _, args, _, _) = cursor else {
            panic!("number set");
        };
        args[2] = Expr::Float(-0.0);
        let dynamic = value::dynamic_value(
            Expr::Int(0),
            Expr::Var(1),
            Expr::Bool(false),
            Expr::Int(0),
            Expr::Int(0),
        );
        let samples = [
            wrong_key,
            typed,
            identified,
            negative_zero,
            dynamic,
            value::null(),
            value::boolean(Expr::Bool(false)),
            value::boolean(Expr::Bool(true)),
            value::number(Expr::Float(0.0)),
            value::number(Expr::Float(1.0)),
        ];
        for sample in samples {
            let before = format!("{sample:?}");
            let mut ir = bare(sample);
            factor(&mut ir);
            let Stmt::Expr { expr, .. } = &ir.stmts[0] else {
                panic!("stmt");
            };
            assert_eq!(
                format!("{expr:?}"),
                before,
                "nonmatching expression was rewritten"
            );
            assert!(
                ir.functions
                    .iter()
                    .all(|function| function.name != HELPER_NAME)
            );
        }
    }

    #[test]
    fn completion_keeps_outer_record() {
        let mut ir = bare(value::completion_return(value::undefined()));
        factor(&mut ir);
        let Stmt::Expr { expr, .. } = &ir.stmts[0] else {
            panic!("stmt");
        };
        let set = BuiltinFunction::Set.call_index();
        let helper = ir
            .functions
            .iter()
            .find(|function| function.name == HELPER_NAME)
            .expect("helper")
            .index;
        let target = peel_set(expr, set, "target", |value| matches!(value, Expr::Int(0)))
            .expect("completion target");
        let has_value = peel_set(target, set, "has_value", |value| {
            matches!(value, Expr::Bool(true))
        })
        .expect("completion has_value");
        let value_receiver = peel_set(
            has_value,
            set,
            "value",
            |value| matches!(value, Expr::Call(index, type_args, args, None, None) if *index == helper && type_args.is_empty() && args.is_empty()),
        );
        assert!(
            value_receiver.is_some(),
            "nested undefined must be a direct call"
        );
        let body = ir.function_impls.get(&helper).expect("body");
        assert!(body.param_slots.is_empty());
        assert!(body.capture_copies.is_empty());
        assert!(body.body_stmts.is_empty());
        assert!(is_canonical_undefined(&body.body_expr));
    }
}
