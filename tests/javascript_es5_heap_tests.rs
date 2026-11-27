//! P2-B managed logical heap proofs for the javascript-es5 frontend.
//!
//! `p2_heap_history_*` is the pre-product behavioral RED: a real source
//! zero-argument object factory through its real existing dispatch/static
//! compiled body, plus a private bounded IR history driver (64 vs 4096
//! iterations) with the existing B1 length observation. The original
//! append-only producer failed the history-independent warmed bound in the
//! preserved RED receipt. Current fixtures also dereference actual payloads
//! and independently observe logical, physical, B1 and Core metadata.

use pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN;
use std::sync::atomic::{AtomicUsize, Ordering};
use vm::{Value, VmStatus};

#[path = "javascript_es5/pending_store_site.rs"]
mod pending_store_site;

#[path = "javascript_es5_heap/acceptance.rs"]
mod acceptance;

// ---- Shared realm harness (production catalog/adapters, no JS oracle) ----

fn heap_catalog() -> std::sync::Arc<vm::HostApiCatalog> {
    pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog()
}

fn install_heap_production(
    registry: &mut vm::HostFunctionRegistry,
    catalog: &vm::HostApiCatalog,
    program: &vm::Program,
) {
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("manual arrays adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("owned adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(registry, catalog)
        .expect("snapshot adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(registry, catalog)
        .expect("scalar-math adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(registry, catalog)
        .expect("scalar-error adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
    // URI-expanded origin probe reuses the existing public URI prerequisites,
    // which need the same narrow builtins as the runtime URI observer.
    registry
        .allow_builtin("math::copysign")
        .expect("math::copysign should be allow-listable");
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bytes::from_utf8 should be allow-listable");
    // Explicit host-capability audit for the immutable certified program.
    // Installed host modules grant host imports only; the pure Core builtins
    // emitted by the realm bootstrap remain ungranted in a restricted
    // registry. The public namespaced builtin catalog is exactly the
    // generated explicit-host-capability surface, so every OpCode::Call
    // immediate and HostImport callable prototype that resolves through it
    // is a required capability. Audit before bind and grant only the actual
    // canonical gaps.
    let namespaced = {
        let mut catalog = std::collections::BTreeMap::new();
        for namespace in vm::builtin_namespace_specs() {
            for member in namespace.members {
                let builtin =
                    vm::resolve_builtin_namespace_call(namespace.namespace, member.name)
                        .unwrap_or_else(|| {
                            panic!(
                                "namespace member {}.{} is missing from the builtin catalog",
                                namespace.namespace, member.name
                            )
                        });
                catalog.insert(
                    builtin.call_index(),
                    format!("{}::{}", namespace.namespace, member.name),
                );
            }
        }
        catalog
    };
    let required: Vec<u16> = {
        let mut required = std::collections::BTreeSet::new();
        let mut ip = 0usize;
        while let Some(&raw) = program.code.get(ip) {
            let opcode = vm::OpCode::try_from(raw)
                .unwrap_or_else(|_| panic!("invalid opcode {raw} at program ip {ip}"));
            let end = ip
                .checked_add(1 + opcode.operand_len())
                .unwrap_or_else(|| panic!("opcode operand overflows at program ip {ip}"));
            assert!(
                end <= program.code.len(),
                "opcode operand exceeds program at ip {ip}"
            );
            if matches!(opcode, vm::OpCode::Call) {
                let index = u16::from_le_bytes(
                    program.code[ip + 1..ip + 3]
                        .try_into()
                        .expect("Call immediate is two bytes"),
                );
                if namespaced.contains_key(&index) {
                    required.insert(index);
                }
            }
            ip = end;
        }
        for prototype in &program.callable_prototypes {
            if let vm::CallableTarget::HostImport(index) = prototype.target {
                if namespaced.contains_key(&index) {
                    required.insert(index);
                }
            }
        }
        required.into_iter().collect()
    };
    let base_grants = ["math::floor", "math::copysign", "bytes::from_utf8"];
    let granted: Vec<u16> = base_grants
        .iter()
        .map(|&name| {
            vm::BuiltinFunction::from_namespaced_name(name)
                .unwrap_or_else(|| panic!("unknown namespaced builtin {name}"))
                .call_index()
        })
        .collect();
    let missing: Vec<u16> = required
        .iter()
        .copied()
        .filter(|index| !granted.contains(index))
        .collect();
    // Current certified producer's narrow explicit-capability needs beyond
    // the three established grants. Any other missing canonical intrinsic is
    // a real, unexpected gap rather than a broad permit.
    const VERIFIED_CURRENT_PRODUCER_CAPABILITIES: &[(&str, vm::BuiltinFunction)] = &[
        ("math::trunc", vm::BuiltinFunction::MathTrunc),
        ("math::is_finite", vm::BuiltinFunction::MathIsFinite),
        ("math::powf", vm::BuiltinFunction::MathPowF),
    ];
    let refs = |indices: &[u16]| -> Vec<serde_json::Value> {
        indices
            .iter()
            .map(|index| {
                serde_json::json!({
                    "index": index,
                    "hex": format!("0x{index:04X}"),
                    "canonical": namespaced.get(index),
                })
            })
            .collect()
    };
    eprintln!(
        "P7_HEAP_CAPABILITIES {}",
        serde_json::json!({
            "required": refs(&required),
            "granted": refs(&granted),
            "missing": refs(&missing),
        })
    );
    for &index in &missing {
        let canonical = namespaced
            .get(&index)
            .unwrap_or_else(|| panic!("required builtin index {index} is not namespaced"));
        let expected = VERIFIED_CURRENT_PRODUCER_CAPABILITIES
            .iter()
            .find(|entry| entry.0 == canonical.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "unexpected explicit host capability gap: {}",
                    serde_json::json!({
                        "required": refs(&required),
                        "granted": refs(&granted),
                        "missing": refs(&missing),
                    })
                )
            });
        assert_eq!(
            vm::BuiltinFunction::from_namespaced_name(canonical.as_str()),
            Some(expected.1),
            "missing capability {canonical} must resolve to its canonical intrinsic"
        );
        assert_eq!(
            expected.1.call_index(),
            index,
            "namespaced catalog and canonical intrinsic disagree on {canonical}"
        );
    }
    for &index in &missing {
        let canonical = namespaced
            .get(&index)
            .expect("audited namespaced capability");
        registry
            .allow_builtin(canonical.as_str())
            .unwrap_or_else(|error| {
                panic!("audited capability {canonical} must be grantable: {error:?}")
            });
    }
}

// ---- RED driver: bounded factory-call history with B1 length readback ----
// Delegates the original real JS source to the REAL JS plugin (real B1
// prelude, carrier, source policy, catalog and certification) and appends a
// bounded `While` loop replaying the real `var o = f();` dispatch region
// (same slots as `Assign`s, no new allocation sites) plus one genuine B1
// length observation through the EXISTING length helper on the ACTUAL
// carrier discovered structurally from real JS lowering. No synthetic
// allocator, no new helper/schema: this must RED on append-only storage.

const HEAP_HISTORY_SOURCE: &str = "var keep={n:7,t:true,f:false,s:\"\\uD800\\0\"};var alias=keep;var a=[7,8];var f=function(){\"use strict\";return {n:7};};var o=f();var r=o.n;var old=alias.n;var st=alias.s;var bt=alias.t;var bf=alias.f;var av=a[1];";

const HEAP_MAKE_NAME: &str = "__compat_ref_make";
const HEAP_LENGTH_NAME: &str = "__compat_ref_length";

/// History-independent warmed bound, in backing cells: one spare directory
/// page (64 rows x 7 cells + 3 header = 451), at most 32 raw 15-cell
/// one-property objects, and a spare 4-entry source frame (4+3*4=16):947
/// cells rounded up to the original1024 ceiling before measurement.
/// Append-only growth over 4032 extra factory calls exceeds this by an
/// order of magnitude (each `{}` commits a fresh multi-cell extent).
const HEAP_HISTORY_BOUND_CELLS: f64 = 1024.0;

fn heap_rejection(message: String) -> vm::ParseError {
    vm::ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}

fn heap_helper_index(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.index)
        .ok_or_else(|| heap_rejection(format!("heap fixture requires helper '{name}'")))
}

struct HeapHistoryPlugin {
    iters: u64,
}

/// Every statement the current emitter nests inside an expression: `Block`
/// bodies, `IfElse` branches, `ResultMatch`/`ResultTry` arms and call
/// arguments. The real callee and result cleanup now runs inside those arms,
/// so a flat statement scan can no longer see it.
fn heap_expr_stmts<'a>(expr: &'a vm::Expr, out: &mut Vec<&'a vm::Stmt>) {
    match expr {
        vm::Expr::Block { stmts, expr } => {
            heap_tree_stmts(stmts, out);
            heap_expr_stmts(expr, out);
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            heap_expr_stmts(condition, out);
            heap_expr_stmts(then_expr, out);
            heap_expr_stmts(else_expr, out);
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            heap_expr_stmts(value, out);
            heap_expr_stmts(ok_expr, out);
            heap_expr_stmts(err_expr, out);
        }
        vm::Expr::ResultTry { value, .. } => heap_expr_stmts(value, out),
        vm::Expr::Call(_, _, args, _, _) => {
            for arg in args {
                heap_expr_stmts(arg, out);
            }
        }
        _ => {}
    }
}

fn heap_tree_stmts<'a>(statements: &'a [vm::Stmt], out: &mut Vec<&'a vm::Stmt>) {
    for statement in statements {
        out.push(statement);
        match statement {
            vm::Stmt::Let { expr, .. }
            | vm::Stmt::Assign { expr, .. }
            | vm::Stmt::Expr { expr, .. } => heap_expr_stmts(expr, out),
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                heap_expr_stmts(condition, out);
                heap_tree_stmts(then_branch, out);
                heap_tree_stmts(else_branch, out);
            }
            _ => {}
        }
    }
}

/// The real callee-root cleanup is the canonical three-slot clear inside a
/// real Result arm that retires the invocation-owned entry in the actual
/// carrier and activation frame. It is never a flat statement after the
/// store, never the generic four-argument write and never a fresh Undefined
/// payload.
fn heap_arm_clears_root(
    arm_stmts: &[&vm::Stmt],
    root_clear: u16,
    carrier: u16,
    frame: u16,
    entry: i64,
) -> bool {
    arm_stmts.iter().any(|stmt| {
        matches!(stmt,
            vm::Stmt::Let {expr: vm::Expr::Call(target, types, args, None, None), ..}
            if *target == root_clear
                && types.is_empty()
                && args.len() == 3
                && matches!(args.first(), Some(vm::Expr::Var(slot)) if *slot == carrier)
                && matches!(args.get(1), Some(vm::Expr::Var(slot)) if *slot == frame)
                && matches!(args.get(2), Some(vm::Expr::Int(value)) if *value == entry)
        )
    })
}

/// The real `Result` consumer behind one store, with every statement nested in
/// its arms. The current emitter stores the match directly, or hands it
/// through one temporary under the normal guard; both are the actual dispatch
/// `Result`, so the scrutinee slot must be the dispatch call's own slot.
struct HeapResultConsumer<'a> {
    value_slot: u16,
    arm_stmts: Vec<&'a vm::Stmt>,
}

fn heap_result_consumer<'a>(
    ir: &'a vm::FrontendIr,
    store: usize,
) -> Result<HeapResultConsumer<'a>, vm::ParseError> {
    let vm::Stmt::Assign { expr, .. } = &ir.stmts[store] else {
        return Err(heap_rejection("actual result store".to_owned()));
    };
    let mut matched = expr;
    if let vm::Expr::Var(temp) = expr {
        matched = ir.stmts[..store]
            .iter()
            .rev()
            .find_map(|stmt| match stmt {
                vm::Stmt::Assign {
                    index,
                    expr: inner @ vm::Expr::ResultMatch { .. },
                    ..
                } if *index == *temp => Some(inner),
                _ => None,
            })
            .ok_or_else(|| heap_rejection("actual Result consumer handoff".to_owned()))?;
    }
    let vm::Expr::ResultMatch {
        ok_expr,
        err_expr,
        value_slot,
        ..
    } = matched
    else {
        return Err(heap_rejection(format!(
            "heap fixture requires the real ResultMatch consumer, got {matched:?}"
        )));
    };
    let mut arm_stmts = Vec::new();
    heap_expr_stmts(ok_expr, &mut arm_stmts);
    heap_expr_stmts(err_expr, &mut arm_stmts);
    Ok(HeapResultConsumer {
        value_slot: *value_slot,
        arm_stmts,
    })
}

/// The real bare-call sequence starts at its callee snapshot and ends after
/// the real result binding refresh. The callee root is retired through the
/// canonical three-slot clear, and the completion handoff and both arms are
/// consumed inside the real `ResultMatch`; the ordered statements replayed
/// below keep the same slots and the same allocation sites, never a synthetic
/// call.
fn heap_call_replay_bounds(
    ir: &vm::FrontendIr,
    store: usize,
) -> Result<(usize, usize), vm::ParseError> {
    let dispatch = heap_helper_index(ir, "__compat_es5_dispatch")?;
    let root_write = heap_helper_index(ir, HEAP_ROOT_WRITE_NAME)?;
    let root_clear = heap_helper_index(ir, HEAP_ROOT_CLEAR_NAME)?;
    let vm::Stmt::Assign { index: binding, .. } = &ir.stmts[store] else {
        return Err(heap_rejection("actual result store".to_owned()));
    };
    let binding = *binding;
    let mut dispatched = None;
    for (at, stmt) in ir.stmts[..store].iter().enumerate().rev() {
        if let vm::Stmt::Let {
            index,
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == dispatch
            && let Some(vm::Expr::Var(callee)) = args.get(1)
        {
            dispatched = Some((at, *index, *callee));
            break;
        }
    }
    let (dispatch_at, result_slot, callee) =
        dispatched.ok_or_else(|| heap_rejection("actual call dispatch and callee".to_owned()))?;
    let start = ir.stmts[..dispatch_at]
        .iter()
        .rposition(|stmt| matches!(stmt, vm::Stmt::Let { index, .. } if *index == callee))
        .ok_or_else(|| heap_rejection("actual callee snapshot".to_owned()))?;
    let callee_entry = ir.stmts[start..store]
        .iter()
        .find_map(|stmt| match stmt {
            vm::Stmt::Let {
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } if *target == root_write
                && matches!(args.get(3), Some(vm::Expr::Var(slot)) if *slot == callee) =>
            {
                match args.get(2) {
                    Some(vm::Expr::Int(entry)) => Some(*entry),
                    _ => None,
                }
            }
            _ => None,
        })
        .ok_or_else(|| heap_rejection("actual saved-callee root entry".to_owned()))?;
    let consumer = heap_result_consumer(ir, store)?;
    if consumer.value_slot != result_slot {
        return Err(heap_rejection(
            "stored result must consume the actual dispatch Result".to_owned(),
        ));
    }
    let mut end = store + 1;
    let mut trailing = Vec::new();
    while let Some(vm::Stmt::Let {
        expr: vm::Expr::Call(target, _, args, _, _),
        ..
    }) = ir.stmts.get(end)
    {
        if *target != root_write {
            break;
        }
        trailing.push(args);
        end += 1;
    }
    // The binding refresh is the only generic root write after the store: an
    // immediate local result handoff publishes no result temp, and the retired
    // result clear is the canonical three-slot clear, never a second write.
    let refresh = match trailing.as_slice() {
        [refresh] => *refresh,
        _ => {
            return Err(heap_rejection(
                "exactly one binding refresh required".to_owned(),
            ));
        }
    };
    // The canonical clear inside the arm runs in the same actual carrier and
    // activation frame the binding refresh publishes through.
    let (Some(vm::Expr::Var(carrier)), Some(vm::Expr::Var(frame))) =
        (refresh.first(), refresh.get(1))
    else {
        return Err(heap_rejection(
            "binding refresh must publish the actual carrier and frame".to_owned(),
        ));
    };
    let cleared_in_call = heap_arm_clears_root(
        &consumer.arm_stmts,
        root_clear,
        *carrier,
        *frame,
        callee_entry,
    );
    if !cleared_in_call {
        return Err(heap_rejection("in-call callee cleanup required".to_owned()));
    }
    let rooted = matches!(refresh.get(3), Some(vm::Expr::Var(slot)) if *slot == binding);
    if !rooted {
        return Err(heap_rejection(
            "binding refresh must root the actual result binding".to_owned(),
        ));
    }
    Ok((start, end))
}

/// Primitive-history-only selection of the real no-argument guarded call.
///
/// The current common-continuation lowering emits the call guard, then retires
/// the actual callee root once through the canonical three-slot clear, then
/// stores and refreshes the binding. The retired callee is the only
/// evaluation-owned root; the receiver and the actuals vector stay bare. Both
/// Result arms leave the callee root alone and keep only their own
/// result/payload identities. The older flat-call APIs and their callers
/// deliberately remain unchanged.
fn heap_guarded_primitive_replay(
    ir: &vm::FrontendIr,
    site: &pending_store_site::Site,
) -> Result<(usize, usize), vm::ParseError> {
    use pending_store_site::ListPath;
    let fail = || heap_rejection("real guarded primitive call/cleanup chain".into());
    if site.path != ListPath::Guard || site.assign != 0 {
        return Err(fail());
    }
    let normal = site.pending.ok_or_else(fail)?;
    let vm::Expr::Var(result) = site.assigned else {
        return Err(fail());
    };
    let root = heap_helper_index(ir, HEAP_ROOT_WRITE_NAME)?;
    let dispatch = heap_helper_index(ir, "__compat_es5_dispatch")?;
    let clear = heap_helper_index(ir, HEAP_ROOT_CLEAR_NAME)?;

    // Locate the actual call guard by its complete shape. Never skip an
    // arbitrary number of statements and assume the nearest one is the guard.
    let mut guard = None;
    for (at, stmt) in ir.stmts[..site.top].iter().enumerate() {
        let vm::Stmt::IfElse {
            condition: vm::Expr::Var(slot),
            then_branch,
            else_branch,
            ..
        } = stmt
        else {
            continue;
        };
        if *slot != normal || !else_branch.is_empty() {
            continue;
        }
        let mut dispatched = None;
        for (inner_at, inner) in then_branch.iter().enumerate() {
            let vm::Stmt::Let {
                index: dispatch_result,
                expr: vm::Expr::Call(target, types, args, None, None),
                ..
            } = inner
            else {
                continue;
            };
            if *target != dispatch || !types.is_empty() || args.len() != 4 {
                continue;
            }
            let (vm::Expr::Var(callee), vm::Expr::Var(actuals)) = (&args[1], &args[3]) else {
                continue;
            };
            // This source is no-argument: the actuals vector must be its real
            // empty ArrayNew allocation, bound before the dispatch.
            let allocated = then_branch[..inner_at].iter().any(|candidate| {
                matches!(candidate,
                    vm::Stmt::Let { index, expr: vm::Expr::Call(target, _, args, _, _), .. }
                        if *index == *actuals
                            && *target == vm::BuiltinFunction::ArrayNew.call_index()
                            && args.is_empty())
            });
            if !allocated {
                continue;
            }
            if dispatched
                .replace((inner_at, *dispatch_result, *callee))
                .is_some()
            {
                return Err(fail());
            }
        }
        let Some((dispatch_at, raw, callee)) = dispatched else {
            continue;
        };
        // The store consumed by `site` must be the unique write of the result
        // temp and the unique ResultMatch of the raw dispatch result.
        let mut assignments = 0usize;
        for candidate in then_branch {
            if matches!(candidate, vm::Stmt::Assign { index, .. } if *index == result) {
                assignments += 1;
            }
        }
        if assignments != 1 {
            continue;
        }
        let mut consumers = 0usize;
        for candidate in &then_branch[dispatch_at + 1..] {
            let vm::Stmt::Assign {
                index,
                expr:
                    vm::Expr::ResultMatch {
                        value, value_slot, ..
                    },
                ..
            } = candidate
            else {
                continue;
            };
            if *index == result
                && *value_slot == raw
                && matches!(&**value, vm::Expr::Var(provider) if *provider == raw)
            {
                consumers += 1;
            }
        }
        if consumers != 1 {
            continue;
        }
        if guard.replace((at, callee, raw)).is_some() {
            return Err(fail());
        }
    }
    let (call_at, callee, dispatch_result) = guard.ok_or_else(fail)?;

    // The common retirement region is exactly one statement between the call
    // guard and the primitive store. No extra interposition may be swallowed.
    if call_at + 2 != site.top {
        return Err(fail());
    }
    let start = ir.stmts[..call_at]
        .iter()
        .rposition(|stmt| {
            matches!(stmt,
        vm::Stmt::Let {index, expr: vm::Expr::Var(_), ..} if *index == callee)
        })
        .ok_or_else(fail)?;
    let callee_entry = ir
        .stmts
        .get(start + 1)
        .and_then(|stmt| pending_store_site::root_entry(stmt, root, callee))
        .ok_or_else(fail)?;
    let entry_of = |stmt: &vm::Stmt| -> Option<i64> {
        let vm::Stmt::Let {
            expr: vm::Expr::Call(target, types, args, None, None),
            ..
        } = stmt
        else {
            return None;
        };
        if *target != root || !types.is_empty() || args.len() != 4 {
            return None;
        }
        match &args[2] {
            vm::Expr::Int(entry) => Some(*entry),
            _ => None,
        }
    };
    // The snapshot's adjacent root is the only evaluation-owned root before
    // the call; receiver and actuals stay bare by this source's contract.
    let roots: Vec<_> = ir.stmts[start..call_at]
        .iter()
        .filter_map(|stmt| entry_of(stmt))
        .collect();
    if !matches!(roots.as_slice(), [entry] if *entry == callee_entry) {
        return Err(fail());
    }
    let binding_entry =
        pending_store_site::root_entry(&site.refresh, root, site.binding).ok_or_else(fail)?;
    if callee_entry == binding_entry {
        return Err(fail());
    }
    // The binding refresh publishes through the actual carrier and activation
    // frame; the retirement below must clear in those same two slots.
    let (carrier, frame) = match &site.refresh {
        vm::Stmt::Let {
            expr: vm::Expr::Call(target, types, args, None, None),
            ..
        } if *target == root && types.is_empty() && args.len() == 4 => {
            match (args.first(), args.get(1)) {
                (Some(vm::Expr::Var(carrier)), Some(vm::Expr::Var(frame))) => (*carrier, *frame),
                _ => return Err(fail()),
            }
        }
        _ => return Err(fail()),
    };
    // The exact canonical three-slot clear: `root_clear(carrier, frame, entry)`.
    // No third target and no optional fourth payload argument are accepted.
    let canonical_clear = |stmt: &vm::Stmt| -> Option<(u16, u16, i64)> {
        let vm::Stmt::Let {
            expr: vm::Expr::Call(target, types, args, None, None),
            ..
        } = stmt
        else {
            return None;
        };
        if *target != clear || !types.is_empty() || args.len() != 3 {
            return None;
        }
        let (Some(vm::Expr::Var(carrier)), Some(vm::Expr::Var(frame)), Some(vm::Expr::Int(entry))) =
            (args.first(), args.get(1), args.get(2))
        else {
            return None;
        };
        Some((*carrier, *frame, *entry))
    };
    let clear_entry = |stmt: &vm::Stmt| -> Option<i64> {
        match canonical_clear(stmt) {
            Some((at_carrier, at_frame, entry)) if at_carrier == carrier && at_frame == frame => {
                Some(entry)
            }
            _ => None,
        }
    };
    // The single retirement clears the actual callee entry through the
    // canonical clear in this frame, never through the generic four-argument
    // write, the binding or the result temp.
    let vm::Stmt::Let {
        declared_schema: Some(vm::TypeSchema::Int),
        ..
    } = &ir.stmts[call_at + 1]
    else {
        return Err(fail());
    };
    if clear_entry(&ir.stmts[call_at + 1]) != Some(callee_entry) {
        return Err(fail());
    }
    let vm::Stmt::IfElse {
        then_branch: call, ..
    } = &ir.stmts[call_at]
    else {
        return Err(fail());
    };
    let mut matched = None;
    for candidate in call {
        let vm::Stmt::Assign {
            index,
            expr:
                vm::Expr::ResultMatch {
                    value,
                    value_slot,
                    ok_expr,
                    err_expr,
                    ok_binding,
                    err_binding,
                    ..
                },
            ..
        } = candidate
        else {
            continue;
        };
        if *index == result
            && *value_slot == dispatch_result
            && matches!(&**value, vm::Expr::Var(provider) if *provider == dispatch_result)
        {
            if matched.is_some() {
                return Err(fail());
            }
            matched = Some((&**ok_expr, &**err_expr, *ok_binding, *err_binding));
        }
    }
    let (ok, err, ok_binding, err_binding) = matched.ok_or_else(fail)?;
    if ok_binding.is_none() {
        return Err(fail());
    }
    let err_binding = err_binding.ok_or_else(fail)?;
    let vm::Expr::Block {
        stmts: ok_stmts, ..
    } = ok
    else {
        return Err(fail());
    };
    let vm::Expr::Block {
        stmts: err_stmts, ..
    } = err
    else {
        return Err(fail());
    };
    // Neither arm may clear the callee (that is the one common retirement) or
    // touch the binding root, through either the generic write or the
    // canonical clear.
    for arm in [ok_stmts, err_stmts] {
        let forbidden = arm.iter().any(|candidate| {
            entry_of(candidate) == Some(callee_entry)
                || entry_of(candidate) == Some(binding_entry)
                || clear_entry(candidate) == Some(callee_entry)
                || clear_entry(candidate) == Some(binding_entry)
                || pending_store_site::root_entry(candidate, root, callee).is_some()
        });
        if forbidden {
            return Err(fail());
        }
    }
    // The immediate local handoff publishes no Ok result root: the private
    // result slot must reach the binding store unrooted.
    if ok_stmts
        .iter()
        .any(|stmt| pending_store_site::root_entry(stmt, root, result).is_some())
    {
        return Err(fail());
    }
    if err_stmts
        .iter()
        .any(|stmt| pending_store_site::root_entry(stmt, root, result).is_some())
    {
        return Err(fail());
    }
    let err_roots: Vec<_> = err_stmts
        .iter()
        .filter_map(|stmt| pending_store_site::root_entry(stmt, root, err_binding))
        .collect();
    let [err_entry] = err_roots.as_slice() else {
        return Err(fail());
    };
    let err_entry = *err_entry;
    if err_entry == callee_entry || err_entry == binding_entry {
        return Err(fail());
    }
    // The err arm releases the caller slot through the canonical clear, so
    // only the surviving pending owner roots the payload.
    let err_clears = err_stmts
        .iter()
        .any(|candidate| clear_entry(candidate) == Some(err_entry));
    if !err_clears {
        return Err(fail());
    }
    // A leftover result-temp clear would be one canonical three-slot clear
    // following the binding refresh. The canonical call initializer has none:
    // the binding store is the last statement.
    if let Some((_, _, entry)) = ir
        .stmts
        .get(site.after_anchor())
        .and_then(|stmt| canonical_clear(stmt))
        && entry != binding_entry
        && entry != callee_entry
        && entry != err_entry
    {
        return Err(fail());
    }
    eprintln!(
        "PRIMITIVE_REPLAY_CHAIN start={start} call={call_at} retirement={} store={} end={} normal={normal} dispatch_result={dispatch_result} result={result} callee={callee} callee_root={callee_entry} err_root={err_entry} binding_root={binding_entry}",
        call_at + 1,
        site.top,
        site.after_anchor()
    );
    Ok((start, site.after_anchor()))
}

/// Pure-IR negative controls for the current common-continuation retirement.
///
/// Each case changes exactly one real statement or field on a clone of the
/// actual parsed IR; no source reparse, VM, Core, or generic probe framework.
#[test]
fn primitive_common_retirement_negative_controls() {
    use vm::SourcePlugin;
    let source = "var f=function(){\"use strict\";return 7;};var o=f();";
    let catalog = heap_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let original = JAVASCRIPT_ES5_PLUGIN
        .parse_source(source, &context)
        .expect("real primitive control source");
    let binding = heap_binding_index(&original, "o").expect("actual o binding");
    let root = heap_helper_index(&original, HEAP_ROOT_WRITE_NAME).expect("root helper");
    let clear = heap_helper_index(&original, HEAP_ROOT_CLEAR_NAME).expect("root clear helper");
    let dispatch = heap_helper_index(&original, "__compat_es5_dispatch").expect("dispatch helper");
    let sites = pending_store_site::sites(&original, binding, root).expect("bounded store sites");
    let site = pending_store_site::unique(&sites)
        .expect("unique primitive store")
        .clone();
    let (start, end) =
        heap_guarded_primitive_replay(&original, &site).expect("real common retirement");
    assert!(
        start + 1 < site.top,
        "callee snapshot and root precede the call"
    );
    assert_eq!(
        end,
        site.after_anchor(),
        "binding store bounds without a result clear"
    );
    let call_at = site
        .top
        .checked_sub(2)
        .expect("real call guard precedes the store");
    let result = match &site.assigned {
        vm::Expr::Var(slot) => *slot,
        _ => unreachable!("actual primitive store value"),
    };
    let callee = {
        let vm::Stmt::IfElse { then_branch, .. } = &original.stmts[call_at] else {
            unreachable!("real call guard");
        };
        then_branch
            .iter()
            .find_map(|inner| {
                let vm::Stmt::Let {
                    expr: vm::Expr::Call(target, _, args, _, _),
                    ..
                } = inner
                else {
                    return None;
                };
                if *target != dispatch || args.len() != 4 {
                    return None;
                }
                match &args[1] {
                    vm::Expr::Var(slot) => Some(*slot),
                    _ => None,
                }
            })
            .expect("real dispatch callee")
    };
    let retirement = original.stmts[call_at + 1].clone();
    let binding_entry = pending_store_site::root_entry(&site.refresh, root, binding)
        .expect("actual binding refresh root");
    let refuse = |label: &str, ir: &vm::FrontendIr, candidate: &pending_store_site::Site| {
        assert!(
            heap_guarded_primitive_replay(ir, candidate).is_err(),
            "{label} must be refused"
        );
        eprintln!("PRIMITIVE_RETIREMENT_NEGATIVE {label} refused");
    };

    // Missing common callee retirement: the region keeps one unrelated stmt.
    let mut missing = original.clone();
    missing.stmts[call_at + 1] = vm::Stmt::Expr {
        expr: vm::Expr::Int(0),
        line: 1,
    };
    refuse("missing-callee-retirement", &missing, &site);

    // Duplicate common callee retirement (double clear of the same entry).
    let mut duplicate = original.clone();
    duplicate.stmts.insert(call_at + 1, retirement.clone());
    let mut shifted = site.clone();
    shifted.top += 1;
    refuse("duplicate-callee-retirement", &duplicate, &shifted);

    // Early clear of the binding identity instead of the callee identity.
    let mut early = original.clone();
    let vm::Stmt::Let {
        expr: vm::Expr::Call(_, _, args, _, _),
        ..
    } = &mut early.stmts[call_at + 1]
    else {
        unreachable!("real retirement let");
    };
    args[2] = vm::Expr::Int(binding_entry);
    refuse("early-binding-clear", &early, &site);

    // Extra unknown interposition, here a binding write, before the store.
    let mut interposed = original.clone();
    interposed.stmts.insert(
        call_at + 1,
        vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: binding,
            expr: vm::Expr::Var(binding),
            line: 1,
        },
    );
    let mut shifted = site.clone();
    shifted.top += 1;
    refuse("interposed-binding-write", &interposed, &shifted);

    // A Result arm must not repeat the callee retirement.
    let mut doubled = original.clone();
    let vm::Stmt::IfElse { then_branch, .. } = &mut doubled.stmts[call_at] else {
        unreachable!("real call guard");
    };
    let mut injected = false;
    for candidate in then_branch.iter_mut() {
        let vm::Stmt::Assign {
            expr: vm::Expr::ResultMatch { ok_expr, .. },
            ..
        } = candidate
        else {
            continue;
        };
        let vm::Expr::Block { stmts, .. } = &mut **ok_expr else {
            continue;
        };
        stmts.push(retirement.clone());
        injected = true;
        break;
    }
    assert!(injected, "real Ok arm of the dispatch ResultMatch");
    refuse("arm-duplicate-callee-retirement", &doubled, &site);

    // The retirement must stay the canonical three-slot clear, never the
    // generic four-argument root write.
    let mut wrong_helper = original.clone();
    let vm::Stmt::Let {
        expr: vm::Expr::Call(target, types, args, None, None),
        ..
    } = &mut wrong_helper.stmts[call_at + 1]
    else {
        unreachable!("real retirement let");
    };
    assert_eq!(*target, clear, "retirement uses the canonical root clear");
    assert!(
        types.is_empty() && args.len() == 3,
        "canonical clear takes only carrier, frame and entry"
    );
    *target = root;
    refuse("retirement-via-generic-root-write", &wrong_helper, &site);

    eprintln!(
        "PRIMITIVE_RETIREMENT_NEGATIVE_CONTROLS result={result} callee={callee} call={call_at} store={}",
        site.top
    );
}

/// Initializer-result root controls: the canonical plain-name call initializer
/// publishes no Ok/Err result temp and leaves no canonical clear after the
/// binding refresh. Each mutant reintroduces exactly one of those retired root
/// writes on a clone of the same actual parsed IR.
#[test]
fn primitive_call_initializer_result_root_negative_controls() {
    use vm::SourcePlugin;
    let source = "var f=function(){\"use strict\";return 7;};var o=f();";
    let catalog = heap_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let original = JAVASCRIPT_ES5_PLUGIN
        .parse_source(source, &context)
        .expect("real initializer control source");
    let binding = heap_binding_index(&original, "o").expect("actual o binding");
    let root = heap_helper_index(&original, HEAP_ROOT_WRITE_NAME).expect("root helper");
    let clear = heap_helper_index(&original, HEAP_ROOT_CLEAR_NAME).expect("root clear helper");
    let sites = pending_store_site::sites(&original, binding, root).expect("bounded store sites");
    let site = pending_store_site::unique(&sites)
        .expect("unique primitive store")
        .clone();
    let (start, end) =
        heap_guarded_primitive_replay(&original, &site).expect("canonical initializer root");
    assert!(
        start + 1 < site.top,
        "callee snapshot and root precede the call"
    );
    assert_eq!(
        end,
        site.after_anchor(),
        "canonical binding store bounds without a result clear"
    );
    let call_at = site
        .top
        .checked_sub(2)
        .expect("real call guard precedes the store");
    let result = match &site.assigned {
        vm::Expr::Var(slot) => *slot,
        _ => unreachable!("actual primitive store value"),
    };
    let retirement = original.stmts[call_at + 1].clone();
    let callee_entry = {
        let vm::Stmt::Let {
            expr: vm::Expr::Call(target, types, args, None, None),
            ..
        } = &retirement
        else {
            unreachable!("real common retirement let");
        };
        assert_eq!(*target, clear, "retirement uses the canonical root clear");
        assert!(
            types.is_empty() && args.len() == 3,
            "canonical clear takes only carrier, frame and entry"
        );
        assert!(matches!(args[0], vm::Expr::Var(_)) && matches!(args[1], vm::Expr::Var(_)));
        match &args[2] {
            vm::Expr::Int(entry) => *entry,
            _ => unreachable!("actual callee retirement root entry"),
        }
    };
    let binding_entry = pending_store_site::root_entry(&site.refresh, root, binding)
        .expect("actual binding refresh root");
    let fresh_entry = binding_entry.max(callee_entry) + 1000;
    let refuse = |label: &str, ir: &vm::FrontendIr| {
        assert!(
            heap_guarded_primitive_replay(ir, &site).is_err(),
            "{label} must be refused"
        );
        eprintln!("PRIMITIVE_INITIALIZER_ROOT_NEGATIVE {label} refused");
    };

    // Canonical positive witness: neither arm publishes the private result
    // root, and the replay accepts the initializer with no trailing clear.
    let publications = {
        let vm::Stmt::IfElse { then_branch, .. } = &original.stmts[call_at] else {
            unreachable!("real call guard");
        };
        let mut arms = None;
        for candidate in then_branch {
            let vm::Stmt::Assign {
                expr:
                    vm::Expr::ResultMatch {
                        ok_expr, err_expr, ..
                    },
                ..
            } = candidate
            else {
                continue;
            };
            let vm::Expr::Block { stmts: ok, .. } = &**ok_expr else {
                continue;
            };
            let vm::Expr::Block { stmts: err, .. } = &**err_expr else {
                continue;
            };
            arms = Some((ok.as_slice(), err.as_slice()));
            break;
        }
        let (ok, err) = arms.expect("real ResultMatch arms");
        ok.iter()
            .chain(err.iter())
            .filter(|stmt| pending_store_site::root_entry(stmt, root, result).is_some())
            .count()
    };
    assert_eq!(
        publications, 0,
        "canonical initializer publishes no Ok/Err result root"
    );

    // Reintroduced Ok result publication: a generic four-argument RootWrite of
    // the private result returns with the actual carrier and frame, changing
    // only index, entry and value.
    let mut republished = original.clone();
    let vm::Stmt::IfElse { then_branch, .. } = &mut republished.stmts[call_at] else {
        unreachable!("real call guard");
    };
    let mut injected = false;
    for candidate in then_branch.iter_mut() {
        let vm::Stmt::Assign {
            expr: vm::Expr::ResultMatch { ok_expr, .. },
            ..
        } = candidate
        else {
            continue;
        };
        let vm::Expr::Block { stmts, .. } = &mut **ok_expr else {
            continue;
        };
        let mut publication = site.refresh.clone();
        let vm::Stmt::Let {
            index,
            expr: vm::Expr::Call(_, _, args, _, _),
            ..
        } = &mut publication
        else {
            unreachable!("real binding refresh let");
        };
        *index = result;
        args[2] = vm::Expr::Int(fresh_entry);
        args[3] = vm::Expr::Var(result);
        stmts.push(publication);
        injected = true;
        break;
    }
    assert!(injected, "real Ok arm of the dispatch ResultMatch");
    refuse("arm-result-republication", &republished);

    // Reintroduced leftover result-temp clear: one canonical three-slot clear
    // for the private result slot after the binding refresh.
    let mut cleared = original.clone();
    let mut leaked = retirement.clone();
    let vm::Stmt::Let {
        index,
        expr: vm::Expr::Call(_, _, args, _, _),
        ..
    } = &mut leaked
    else {
        unreachable!("real common retirement let");
    };
    *index = result;
    args[2] = vm::Expr::Int(fresh_entry);
    cleared.stmts.insert(site.after_anchor(), leaked);
    refuse("leftover-result-clear", &cleared);
}

/// Top-level temporaries already dominate the repeat loop. Locals inside
/// the original guarded Result arms must retain their original declarations:
/// their first execution does not dominate another loop iteration.
fn heap_replay_assign(stmt: &mut vm::Stmt) {
    if let vm::Stmt::Let {
        index, expr, line, ..
    } = stmt
    {
        *stmt = vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: *index,
            expr: expr.clone(),
            line: *line,
        };
    }
}

fn append_heap_history_driver(ir: &mut vm::FrontendIr, iters: u64) -> Result<(), vm::ParseError> {
    let make = heap_helper_index(ir, HEAP_MAKE_NAME)?;
    let length = heap_helper_index(ir, HEAP_LENGTH_NAME)?;
    let root_enter = heap_helper_index(ir, HEAP_ROOT_ENTER_NAME)?;
    let max_source_capacity = ir
        .functions
        .iter()
        .filter(|function| function.name.starts_with("__compat_es5_fn_"))
        .filter_map(|function| ir.function_impls.get(&function.index))
        .flat_map(|implementation| &implementation.body_stmts)
        .filter_map(|stmt| match stmt {
            vm::Stmt::Let {
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } if *target == root_enter => match args.get(1) {
                Some(vm::Expr::Int(capacity)) => Some(*capacity),
                _ => None,
            },
            _ => None,
        })
        .max()
        .ok_or_else(|| {
            heap_rejection("history requires the actual compiled source frame".to_owned())
        })?;
    let derived_slack = 3 + 64 * 7 + 32 * (3 + 5 + 7) + 4 + 3 * max_source_capacity;
    eprintln!("P7_HISTORY_IR source_capacity={max_source_capacity} derived_slack={derived_slack}");
    if derived_slack as f64 > HEAP_HISTORY_BOUND_CELLS {
        return Err(heap_rejection(format!(
            "source-derived history bound {derived_slack} exceeds original1024"
        )));
    }
    let mut carriers = Vec::new();
    for stmt in &ir.stmts {
        if let vm::Stmt::Let {
            index,
            declared_schema: None,
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == make
            && args.len() == 1
        {
            carriers.push(*index);
        }
    }
    let [carrier] = carriers.as_slice() else {
        return Err(heap_rejection(format!(
            "heap fixture requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    let carrier = *carrier;
    // The real `o` binding: a top-level `Let` whose value is the dispatched
    // call block. Every inner statement must be a plain `Let` so the loop
    // can replay the identical emission code with `Assign`s on the same
    // slots (balanced stacks on loop edges, no new sites).
    let binding = ir
        .local_bindings
        .iter()
        .rfind(|(name, _)| name == "o")
        .map(|(_, slot)| *slot)
        .ok_or_else(|| heap_rejection("heap fixture requires the real `o` binding".to_owned()))?;
    // General Call effects place the actual store and handoff inside a
    // Normal guard. Flatten only a read-only analysis view; replay retains
    // every original guard and Result arm in the live IR.
    let mut flat = ir.clone();
    let mut owners = Vec::new();
    let mut nodes = Vec::new();
    for (owner, statement) in ir.stmts.iter().enumerate() {
        let mut tree = Vec::new();
        heap_tree_stmts(std::slice::from_ref(statement), &mut tree);
        for node in tree {
            owners.push(owner);
            nodes.push(node.clone());
        }
    }
    flat.stmts = nodes;
    let boxed_at = flat
        .stmts
        .iter()
        .position(|stmt| matches!(stmt,vm::Stmt::Assign{index,..}if *index==binding))
        .ok_or_else(|| {
            heap_rejection("heap fixture requires the real guarded `o` store".to_owned())
        })?;
    let (start, end) = heap_call_replay_bounds(&flat, boxed_at)?;
    let run_start = owners[start];
    let suffix_start = owners[end - 1] + 1;
    let mut replay = ir.stmts[run_start..suffix_start].to_vec();
    for statement in &mut replay {
        heap_replay_assign(statement);
    }
    let mut tree = Vec::new();
    heap_tree_stmts(&replay, &mut tree);
    if replay
        .iter()
        .any(|stmt| matches!(stmt, vm::Stmt::Let { .. }))
    {
        return Err(heap_rejection(
            "history replay left a declaration instead of an actual assignment".to_owned(),
        ));
    }
    let suffix = ir.stmts.split_off(suffix_start);
    let mut fresh = || -> Result<u16, vm::ParseError> {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection("heap fixture slot space exhausted".to_owned()))?;
        ir.locals += 1;
        Ok(slot)
    };
    let iter = fresh()?;
    let observed = fresh()?;
    ir.stmts.push(vm::Stmt::Let {
        index: iter,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::While {
        condition: vm::Expr::Lt(
            Box::new(vm::Expr::Div(
                Box::new(vm::Expr::Var(iter)),
                Box::new(vm::Expr::Float(1.0)),
            )),
            Box::new(vm::Expr::Float(iters as f64)),
        ),
        body: {
            let mut body = replay;
            body.push(vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: iter,
                expr: vm::Expr::Add(Box::new(vm::Expr::Var(iter)), Box::new(vm::Expr::Int(1))),
                line: 1,
            });
            body
        },
        line: 1,
    });
    ir.stmts.extend(suffix);
    for name in ["r", "old", "bt", "bf", "av"] {
        let slot = heap_binding_index(ir, name)?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(slot),
            line: 1,
        });
    }
    let mut ctx = heap_cycle_ctx(ir)?;
    ctx.carrier = carrier;
    let st = heap_binding_index(ir, "st")?;
    let (_, st_slot, st_gen) = heap_cycle_identity(ir, &ctx, st, "history-string")?;
    let st_base = heap_cycle_base(ir, &ctx, carrier, st_slot, st_gen, "history-string")?;
    for offset in [2, 3, 4] {
        let raw = heap_cycle_read_number(
            ir,
            &ctx,
            carrier,
            vm::Expr::Add(
                Box::new(vm::Expr::Var(st_base)),
                Box::new(vm::Expr::Int(offset)),
            ),
            "history-string",
        )?;
        heap_cycle_decode(ir, &ctx, raw, "history-string")?;
    }
    heap_cycle_collect(ir, &ctx, carrier, "history-final")?;
    for which in 0..=12 {
        heap_cycle_observe(ir, &ctx, carrier, which, 0, "history-final")?;
    }
    // Existing B1 length observation on the actual carrier: the warmed
    // history bound compares these lengths, never a synthetic allocator.
    ir.stmts.push(vm::Stmt::Let {
        index: observed,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: vm::Expr::Call(length, Vec::new(), vec![vm::Expr::Var(carrier)], None, None),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(observed),
        line: 1,
    });
    Ok(())
}

impl vm::SourcePlugin for HeapHistoryPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-history"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        append_heap_history_driver(&mut ir, self.iters)?;
        Ok(ir)
    }
}

#[derive(Debug)]
struct HeapHistoryEvidence {
    guest: Vec<i64>,
    backing: f64,
    capacity: usize,
    capture_entries: usize,
    capture_capacity: usize,
    identity_entries: usize,
    identity_capacity: usize,
}

fn heap_history_run(iters: u64, native: bool) -> HeapHistoryEvidence {
    heap_history_run_source(HEAP_HISTORY_SOURCE, iters, native)
}

fn heap_history_run_source(source: &str, iters: u64, native: bool) -> HeapHistoryEvidence {
    let plugin: &'static HeapHistoryPlugin = Box::leak(Box::new(HeapHistoryPlugin { iters }));
    let catalog = heap_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-history")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .expect("real history source must compile");
    let mut machine = compilation.into_vm().expect("certified history VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("initialize B1");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("bind production");
    machine.set_fuel(1_000_000_000);
    if native {
        eprintln!("P2_HEAP_NATIVE_PHASE compile_start iters={iters}");
        let started = std::time::Instant::now();
        machine
            .compile_aot()
            .expect("real history native compilation");
        eprintln!(
            "P2_HEAP_NATIVE_PHASE compile_done iters={iters} elapsed={:?}",
            started.elapsed()
        );
    }
    let run_started = std::time::Instant::now();
    if native {
        eprintln!("P2_HEAP_NATIVE_PHASE run_start iters={iters}");
    }
    assert_eq!(
        machine.run().expect("real history must execute"),
        VmStatus::Halted
    );
    if native {
        eprintln!(
            "P2_HEAP_NATIVE_PHASE run_done iters={iters} elapsed={:?}",
            run_started.elapsed()
        );
        let count = machine.aot_exec_count();
        let info = machine.dump_aot_info();
        eprintln!("P2_HEAP_HISTORY_NATIVE iters={iters} count={count}\n{info}");
        assert!(count > 0 && info.contains("lowering=ssa"));
    }
    // Five actual source consumer values, st slot/gen, UTF16 length/units,
    // thirteen independent guest observations and the real B1 length.
    let stack = machine.stack();
    assert!(stack.len() >= 24, "complete history readback: {stack:?}");
    let tail = &stack[stack.len() - 24..];
    assert_eq!(
        heap_box_number(&tail[0]),
        7.0,
        "latest returned object payload"
    );
    assert_eq!(
        heap_box_number(&tail[1]),
        7.0,
        "old surviving alias payload"
    );
    for (index, flag) in [(2, true), (3, false)] {
        assert_eq!(heap_box_tag(&tail[index]), 2);
        let Value::Map(fields) = &tail[index] else {
            unreachable!()
        };
        assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(flag)));
    }
    assert_eq!(
        heap_box_number(&tail[4]),
        8.0,
        "globally retained array payload"
    );
    assert_eq!(
        heap_stale_ints(&tail[7..10]),
        [2, 0xD800, 0],
        "exact UTF16/NUL after history"
    );
    let guest = heap_stale_ints(&tail[10..23]);
    let backing = match tail[23] {
        Value::Float(n) => n,
        Value::Int(n) => n as f64,
        ref other => panic!("B1 length {other:?}"),
    };
    assert_eq!(guest[3] as f64, backing);
    assert_eq!(guest[10], guest[3]);
    assert_eq!(
        guest[0] + guest[1] + guest[8] + guest[11],
        guest[2],
        "all directory states accounted"
    );
    assert_eq!((guest[1], guest[8], guest[6]), (0, 0, 0));
    assert!(guest[11] > 0 && guest[5] > 0 && guest[12] > 0);
    let capture = machine
        .capture_certification_stats()
        .expect("capture metadata");
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("independent callable metadata");
    assert!(capture.valid);
    assert_eq!(machine.host_context().resource_count(), 1);
    let b1 = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("B1 metadata");
    assert_eq!((b1.pending, b1.registered), (0, 1));
    let capacity = b1.reserved_bytes / std::mem::size_of::<Value>();
    assert!(capacity >= backing as usize);
    let evidence = HeapHistoryEvidence {
        guest,
        backing,
        capacity,
        capture_entries: capture.entries,
        capture_capacity: capture.capacity,
        identity_entries: identity.entries,
        identity_capacity: identity.capacity,
    };
    eprintln!(
        "P2_HEAP_HISTORY_EVIDENCE iters={iters} native={native} evidence={evidence:?} b1={b1:?} capture={capture:?} identity={identity:?}"
    );
    machine.reset_for_reuse().expect("reset real history VM");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    assert!(
        !machine
            .capture_certification_stats()
            .expect("revoked facts")
            .valid
    );
    evidence
}

fn heap_assert_history_bound(short: HeapHistoryEvidence, long: HeapHistoryEvidence) {
    // Original 1024-cell differential ceiling is retained. Capacity uses the
    // B1 Vec growth factor on that same layout bound, not a global heap cap.
    assert!(long.backing <= short.backing + HEAP_HISTORY_BOUND_CELLS);
    assert!(long.capacity <= 2 * (short.backing as usize + HEAP_HISTORY_BOUND_CELLS as usize));
    for which in [0, 1, 4, 7, 8, 12] {
        assert_eq!(
            long.guest[which], short.guest[which],
            "history-invariant selector {which}"
        );
    }
    assert!(long.guest[2] <= short.guest[2] + 64);
    assert!(long.guest[9] <= short.guest[9] + 1);
    assert!(long.guest[5] > short.guest[5]);
    assert_eq!(
        (long.capture_entries, long.capture_capacity),
        (short.capture_entries, short.capture_capacity)
    );
    assert_eq!(
        (long.identity_entries, long.identity_capacity),
        (short.identity_entries, short.identity_capacity)
    );
}

#[test]
fn p2_heap_history_reuses_backing_across_factory_calls() {
    heap_assert_history_bound(heap_history_run(64, false), heap_history_run(4096, false));
}

#[test]
fn p4_argument_history_retains_aliases_and_reclaims_unused_extras() {
    let source = "var keep={n:7,t:true,f:false,s:\"\\uD800\\0\"};var alias=keep;var a=[7,8];var f=function(x){\"use strict\";var local=[9];return x;};var o=f({n:7},\"unused\");var r=o.n;var old=alias.n;var st=alias.s;var bt=alias.t;var bf=alias.f;var av=a[1];";
    heap_assert_history_bound(
        heap_history_run_source(source, 64, false),
        heap_history_run_source(source, 4096, false),
    );
}

#[test]
fn p2_heap_history_reuses_backing_native_ssa() {
    // One native compilation per finite command. The separate native64
    // fixture preserves the former warm phase; its complete receipt is
    // compared independently with this native256 receipt by the recorder.
    heap_assert_history_bound(heap_history_run(64, false), heap_history_run(256, true));
}

#[test]
fn helper_heap_history_native_warm64() {
    // Extracted warm phase, not an additional language feature or business
    // count. All actual payload/UTF16/B1/Core assertions execute in run.
    heap_history_run(64, true);
}

// ---- Safepoint readback harness (test-only IR instrumentation, labeled) ----
// Parses real admitted source through the production plugin, then appends
// plain value reads of the named source bindings plus genuine
// `__compat_es5_heap_observe` counter observations. Every allocation in
// the source is production; only the trailing readback is fixture IR.

#[test]
fn array_computed_string_key_retains_captured_receiver_across_collection() {
    let baseline = "function pressure(){var i=0;while(i<20){({a:1});i++;}}var a=[1,2];var key='hasOwnProperty';var ok=a[(a=[],key)]('1');var rebound=a.length===0;";
    let pressure = "function pressure(){var i=0;while(i<20){({a:1});i++;}}var a=[1,2];var key='hasOwnProperty';var ok=a[(a=[],pressure(),key)]('1');var rebound=a.length===0;";
    // The old Array has no source-binding alias after a=[]; only the prepared
    // receiver can retain it during key evaluation. Only the trailing readback
    // is test-only IR. All source allocation, collection, Get and dispatch are
    // production operations with the original GC interval and fresh fuel.
    let reads = vec!["ok".to_owned(), "rebound".to_owned()];
    eprintln!("ARRAY_STRING_KEY_GC_SOURCE baseline {baseline}");
    let (base_values, base_gc, _, _) = heap_readback_run(baseline, reads.clone());
    eprintln!("ARRAY_STRING_KEY_GC_HALTED baseline gc={base_gc}");
    eprintln!("ARRAY_STRING_KEY_GC_SOURCE pressure {pressure}");
    let (pressure_values, pressure_gc, _, _) = heap_readback_run(pressure, reads);
    eprintln!("ARRAY_STRING_KEY_GC_HALTED pressure gc={pressure_gc}");
    for values in [&base_values, &pressure_values] {
        assert_eq!(values.len(), 2);
        for value in values {
            assert_eq!(heap_box_tag(value), 2);
            let Value::Map(fields) = value else { unreachable!() };
            assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
        }
    }
    assert!(pressure_gc > base_gc, "key evaluation must add a real collection: baseline={base_gc} pressure={pressure_gc}");
    eprintln!("ARRAY_STRING_KEY_COLLECTION baseline_gc={base_gc} pressure_gc={pressure_gc} captured_receiver_own=true rebound_empty=true");
}

#[test]
fn has_own_property_primitive_lookup_survives_real_collection() {
    let baseline = "function id(x){return x;}var s=id('ab');var n=id(1);var b=id(true);var stringOwn=s.hasOwnProperty('0');var numberOwn=n.hasOwnProperty('0');var booleanOwn=b.hasOwnProperty('0');";
    let pressure = "function id(x){return x;}var s=id('ab');var n=id(1);var b=id(true);var i=0;while(i<20){({a:1});i++;}var stringOwn=s.hasOwnProperty('0');var numberOwn=n.hasOwnProperty('0');var booleanOwn=b.hasOwnProperty('0');";
    // Reuse the existing test-only trailing observer. Source receivers exist
    // before pressure; every prototype lookup and public call occurs afterward.
    let reads = vec!["stringOwn".to_owned(), "numberOwn".to_owned(), "booleanOwn".to_owned()];
    let (base_values, base_gc, _, _) = heap_readback_run(baseline, reads.clone());
    let (pressure_values, pressure_gc, _, _) = heap_readback_run(pressure, reads);
    for values in [&base_values, &pressure_values] {
        assert_eq!(values.len(), 3);
        for (index, expected) in [(0, true), (1, false), (2, false)] {
            assert_eq!(heap_box_tag(&values[index]), 2);
            let Value::Map(fields) = &values[index] else { unreachable!() };
            assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(expected)));
        }
    }
    assert!(pressure_gc > base_gc, "pressure must add a real collection before primitive lookup: baseline={base_gc} pressure={pressure_gc}");
    eprintln!("PRIMITIVE_PROTOTYPE_COLLECTION baseline_gc={base_gc} pressure_gc={pressure_gc} string_own=true number_own=false boolean_own=false");
}

#[test]
fn regexp_test_private_owner_survives_real_collection() {
    let baseline = "var r=/a/;var hit=r.test(\"ba\");var miss=r.test(\"b\");var o={test:r.test};var caught=0;try{o.test(\"a\");}catch(e){caught=1;}";
    let pressure = "var r=/a/;var i=0;while(i<20){({a:1});i++;}var hit=r.test(\"ba\");var miss=r.test(\"b\");var o={test:r.test};var caught=0;try{o.test(\"a\");}catch(e){caught=1;}";
    // Existing test-only trailing observation only; every source allocation,
    // Get, method call, matcher and collection remains production behavior.
    // The retained r is created before the pressure region and called after it.
    let reads = vec!["hit".to_owned(), "miss".to_owned(), "caught".to_owned()];
    let (base_values, base_gc, _, _) = heap_readback_run(baseline, reads.clone());
    let (pressure_values, pressure_gc, _, _) = heap_readback_run(pressure, reads);
    for values in [&base_values, &pressure_values] {
        assert_eq!(values.len(), 3);
        for (index, expected) in [(0, true), (1, false)] {
            assert_eq!(heap_box_tag(&values[index]), 2);
            let Value::Map(fields) = &values[index] else { unreachable!() };
            assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(expected)));
        }
        assert_eq!(heap_box_number(&values[2]), 1.0, "copied method must reject a plain receiver");
    }
    assert!(pressure_gc > base_gc, "pressure must add a real collection after retaining r: baseline={base_gc} pressure={pressure_gc}");
    eprintln!("REGEXP_PRIVATE_OWNER_COLLECTION baseline_gc={base_gc} pressure_gc={pressure_gc} post_collection_hit=true miss=false plain_receiver_caught=1");
}

#[test]
fn new_error_after_callee_throw_does_not_allocate() {
    let baseline = "var keep=new Error(\"boom\");var caught=0;function fail(){throw 7;}try{fail();0;}catch(e){caught=e;}keep.message;";
    let skipped = "var keep=new Error(\"boom\");var caught=0;function fail(){throw 7;}try{fail();new Error(\"boom\");}catch(e){caught=e;}keep.message;";
    // Both scripts allocate the same live Error before the throw, demanding
    // the same realm. Only the unreachable expression differs. Root-frame
    // capacities may differ statically, so do not compare backing lengths.
    let (base_values, base_gc, base_live, _) = heap_readback_run(baseline, vec!["caught".to_owned()]);
    let (skip_values, skip_gc, skip_live, _) = heap_readback_run(skipped, vec!["caught".to_owned()]);
    assert_eq!(heap_box_number(&base_values[0]), 7.0);
    assert_eq!(heap_box_number(&skip_values[0]), 7.0);
    assert_eq!((skip_gc, skip_live), (base_gc, base_live), "unreachable Error must not allocate key/message nodes after callee throw");
}

const HEAP_OBSERVE_NAME: &str = "__compat_es5_heap_observe";

/// Observe selectors: 5=collections, 0=live_nodes, 3=heap_end.
const HEAP_SAFEPOINT_OBSERVES: [i64; 3] = [5, 0, 3];

struct HeapReadbackPlugin {
    reads: Vec<String>,
}

fn heap_binding_index(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.local_bindings
        .iter()
        .rfind(|(binding, _)| binding == name)
        .map(|(_, slot)| *slot)
        .ok_or_else(|| heap_rejection(format!("heap fixture requires binding `{name}'")))
}

impl vm::SourcePlugin for HeapReadbackPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-readback"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        for name in &self.reads {
            let slot = heap_binding_index(&ir, name)?;
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(slot),
                line: 1,
            });
        }
        let observe = heap_helper_index(&ir, HEAP_OBSERVE_NAME)?;
        let mut carriers = Vec::new();
        for stmt in &ir.stmts {
            if let vm::Stmt::Let {
                index,
                declared_schema: None,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } = stmt
                && *target == heap_helper_index(&ir, HEAP_MAKE_NAME)?
                && args.len() == 1
            {
                carriers.push(*index);
            }
        }
        let [carrier] = carriers.as_slice() else {
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let mut fresh = || -> Result<u16, vm::ParseError> {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection("heap fixture slot space exhausted".to_owned()))?;
            ir.locals += 1;
            Ok(slot)
        };
        for which in HEAP_SAFEPOINT_OBSERVES {
            let out = fresh()?;
            ir.stmts.push(vm::Stmt::Let {
                index: out,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: vm::Expr::Call(
                    observe,
                    Vec::new(),
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Int(which),
                        vm::Expr::Int(0),
                    ],
                    None,
                    None,
                ),
                line: 1,
            });
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(out),
                line: 1,
            });
        }
        Ok(ir)
    }
}

fn heap_readback_run(source: &str, reads: Vec<String>) -> (Vec<Value>, i64, i64, i64) {
    let plugin: &'static HeapReadbackPlugin = Box::leak(Box::new(HeapReadbackPlugin {
        reads: reads.clone(),
    }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-readback")),
        options,
    )
    .expect("safepoint compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(10_000_000);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let status = machine.run();
    let fuel_after = machine.get_fuel();
    match status {
        Ok(VmStatus::Halted) => {}
        Ok(status) => {
            // Default-off diagnostic (only on an unexpected non-halt): report
            // where the faithful program actually stopped without weakening
            // the Halted assertion below.
            let tail: Vec<&Value> = machine.stack().iter().rev().take(8).collect();
            eprintln!(
                "SAFEPOINT NONHALT status={status:?} fuel_after={fuel_after:?} ip={} stack_len={} frames={:?} stack_tail={tail:?}",
                machine.ip(),
                machine.stack().len(),
                machine.execution_frames(),
            );
            panic!("safepoint program should halt: {status:?}");
        }
        Err(error) => panic!(
            "SAFEPOINT RUN-FAIL ip={} error={error:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        ),
    }
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let stack = machine.stack().to_vec();
    assert!(
        stack.len() >= reads.len() + HEAP_SAFEPOINT_OBSERVES.len(),
        "binding readback plus observations"
    );
    let values = stack[stack.len() - reads.len() - HEAP_SAFEPOINT_OBSERVES.len()
        ..stack.len() - HEAP_SAFEPOINT_OBSERVES.len()]
        .to_vec();
    let tail = &stack[stack.len() - HEAP_SAFEPOINT_OBSERVES.len()..];
    let counter = |v: &Value| match v {
        Value::Int(n) => *n,
        Value::Float(n) => *n as i64,
        other => panic!("observe must return Int, got {other:?}"),
    };
    (
        values,
        counter(&tail[0]),
        counter(&tail[1]),
        counter(&tail[2]),
    )
}

fn heap_box_tag(v: &Value) -> i64 {
    if let Value::Map(fields) = v
        && let Some(Value::Int(tag)) = fields.get(&Value::string("tag"))
    {
        return *tag;
    }
    panic!("expected a boxed JsValue, got {v:?}");
}

fn heap_box_number(v: &Value) -> f64 {
    assert_eq!(heap_box_tag(v), 3, "expected NUMBER, got {v:?}");
    if let Value::Map(fields) = v {
        return match fields.get(&Value::string("number")) {
            Some(Value::Float(n)) => *n,
            Some(Value::Int(n)) => *n as f64,
            other => panic!("NUMBER must carry a numeric payload, got {other:?}"),
        };
    }
    unreachable!();
}

#[test]
fn p2_heap_safepoint_object_values_across_collects() {
    // 61 production allocations inside one object literal (40 keys, 10
    // nested objects with their keys, the containing object) plus 50 key
    // allocations across the readback sequence force repeated interval
    // collections mid-literal and mid-reads. Every key/value/receiver root
    // must survive; payloads must read back exactly.
    let mut source = String::from("var s = \"hi\"; var o = {");
    for i in 0..10 {
        source.push_str(&format!("k{i}: {{v: {i}}}, "));
    }
    for i in 10..40 {
        source.push_str(&format!("k{i}: {i}, "));
    }
    source.push_str("}; ");
    let mut reads = vec!["s".to_owned()];
    let mut expected: Vec<f64> = Vec::new();
    for i in 0..10 {
        source.push_str(&format!("var t{i} = o.k{i}.v; "));
        reads.push(format!("t{i}"));
        expected.push(i as f64);
    }
    for i in 10..40 {
        source.push_str(&format!("var t{i} = o.k{i}; "));
        reads.push(format!("t{i}"));
        expected.push(i as f64);
    }
    let (values, collections, live, heap_end) = heap_readback_run(&source, reads);
    eprintln!("P2_SAFEPOINT_OBJECT collections={collections} live={live} heap_end={heap_end}");
    assert!(
        collections >= 2,
        "literal plus reads must span collects, got {collections}"
    );
    assert_eq!(
        heap_box_tag(&values[0]),
        4,
        "string node must survive, got {:?}",
        values[0]
    );
    for (position, want) in expected.iter().enumerate() {
        let got = heap_box_number(&values[position + 1]);
        assert_eq!(got, *want, "readback t{position} must be {want}, got {got}");
    }
}

/// Diagnostic-only census (ignored by default): measures how much fuel the
/// faithful object-literal/readback program actually needs without touching
/// the fixed 10m gate or its Halted assertion.
fn safepoint_object_program(keys: usize, reads: usize) -> (String, Vec<String>) {
    let mut source = String::from("var s = \"hi\"; var o = {");
    for i in 0..keys {
        if i < 10 {
            source.push_str(&format!("k{i}: {{v: {i}}}, "));
        } else {
            source.push_str(&format!("k{i}: {i}, "));
        }
    }
    source.push_str("}; ");
    let mut names = Vec::new();
    for i in 0..reads {
        if i < 10 {
            source.push_str(&format!("var t{i} = o.k{i}.v; "));
        } else {
            source.push_str(&format!("var t{i} = o.k{i}; "));
        }
        names.push(format!("t{i}"));
    }
    (source, names)
}

fn safepoint_diag_run(source: &str, reads: Vec<String>, fuel: u64) -> (String, Option<u64>, usize) {
    let plugin: &'static HeapReadbackPlugin = Box::leak(Box::new(HeapReadbackPlugin { reads }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-readback")),
        options,
    )
    .expect("diagnostic compilation should succeed");
    let mut machine = compilation.into_vm().expect("diagnostic certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(fuel);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("diagnostic owned arrays");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("diagnostic adapters");
    let status = format!("{:?}", machine.run());
    (status, machine.get_fuel(), machine.ip())
}

#[test]
#[ignore = "diagnostic census: object-literal/readback safepoint fuel scaling"]
fn p2_safepoint_object_fuel_census_diagnostic() {
    for keys in [10usize, 20, 40] {
        let (source, names) = safepoint_object_program(keys, 0);
        let (status, fuel, ip) = safepoint_diag_run(&source, names, 10_000_000);
        eprintln!(
            "SAFEPOINT_CENSUS literal keys={keys} status={status} fuel_after={fuel:?} ip={ip}"
        );
    }
    let (source, names) = safepoint_object_program(40, 40);
    let (status, fuel, ip) = safepoint_diag_run(&source, names.clone(), 10_000_000);
    eprintln!("SAFEPOINT_CENSUS_FULL10 status={status} fuel_after={fuel:?} ip={ip}");
    let (status, fuel, ip) = safepoint_diag_run(&source, names, 24_000_000);
    eprintln!(
        "SAFEPOINT_CENSUS_FULL24 status={status} fuel_after={fuel:?} ip={ip} fuel_used={:?}",
        fuel.map(|left| 24_000_000 - left)
    );
    // Position probe: one read at the first row versus one read at the last
    // row of the same 40-property literal isolates the linear row scan.
    let (literal, _) = safepoint_object_program(40, 0);
    for key in ["k0", "k39"] {
        let probe = format!("{literal}o.{key};");
        let (status, fuel, ip) = safepoint_diag_run(&probe, Vec::new(), 24_000_000);
        eprintln!(
            "SAFEPOINT_CENSUS_READ key={key} status={status} fuel_after={fuel:?} ip={ip} fuel_used={:?}",
            fuel.map(|left| 24_000_000 - left)
        );
    }
}

#[test]
fn p2_heap_safepoint_bare_call_results_across_collects() {
    // 41 production allocations (one function plus 40 fresh objects
    // returned through real dispatch/Completion handoff) force an
    // interval collection mid-sequence. Every caller-pinned result,
    // including the earliest, must stay a valid OBJECT afterwards.
    let mut source = String::from("var f = function() { \"use strict\"; return {}; }; ");
    let mut reads = Vec::new();
    for i in 0..40 {
        source.push_str(&format!("var o{i} = f(); "));
        reads.push(format!("o{i}"));
    }
    let (values, collections, live, heap_end) = heap_readback_run(&source, reads);
    eprintln!("P2_SAFEPOINT_CALLS collections={collections} live={live} heap_end={heap_end}");
    assert!(
        collections >= 1,
        "40 calls must span a collect, got {collections}"
    );
    assert!(live >= 41, "function plus 40 results stay live, got {live}");
    for (position, value) in values.iter().enumerate() {
        assert_eq!(
            heap_box_tag(value),
            5,
            "result o{position} must stay OBJECT, got {value:?}"
        );
    }
}

#[test]
fn p2_heap_safepoint_method_this_across_collects() {
    // Receiver, key, callee, `this` and the nested member read inside the
    // body stay rooted while 16 method calls (2 key allocations each plus
    // the 4-node fixture) force an interval collection mid-sequence.
    let mut source =
        String::from("var o = {n: 7, m: function() { \"use strict\"; return this.n; }}; ");
    let mut reads = Vec::new();
    for i in 0..16 {
        source.push_str(&format!("var r{i} = o.m(); "));
        reads.push(format!("r{i}"));
    }
    let (values, collections, live, heap_end) = heap_readback_run(&source, reads);
    eprintln!("P2_SAFEPOINT_METHOD collections={collections} live={live} heap_end={heap_end}");
    assert!(
        collections >= 1,
        "16 method calls must span a collect, got {collections}"
    );
    for (position, value) in values.iter().enumerate() {
        let got = heap_box_number(value);
        assert_eq!(got, 7.0, "method result r{position} must be 7, got {got}");
    }
}

// ---- Stale-generation harness (test-only IR instrumentation, labeled) ----
// Single production allocation, then in-fixture: stash the live box,
// replay the real binding emission (unrooting the first node), run a real
// collection, replay once more (reusing the freed slot with a bumped
// generation), publish the old/new slot+generation evidence, and finally
// dereference either the stale or the fresh box through the production
// `__compat_es5_checked_base` gate. Stale must refuse; fresh must work.

const HEAP_DECODE_INT_NAME: &str = "__compat_es5_decode_int";
const HEAP_CHECKED_BASE_NAME: &str = "__compat_es5_checked_base";
const HEAP_COLLECT_NAME: &str = "__compat_es5_heap_collect";
const HEAP_NODE_BEGIN_NAME: &str = "__compat_es5_node_begin";
const HEAP_NODE_PUBLISH_NAME: &str = "__compat_es5_node_publish";
const HEAP_ROOT_ENTER_NAME: &str = "__compat_es5_root_enter";
const HEAP_ROOT_WRITE_NAME: &str = "__compat_es5_root_write";
const HEAP_ROOT_CLEAR_NAME: &str = "__compat_es5_root_clear";

struct HeapStalePlugin {
    binding: String,
    probe_stale: bool,
}

fn heap_field_get(get: u16, binding: u16, field: &str) -> vm::Expr {
    vm::Expr::Call(
        get,
        Vec::new(),
        vec![vm::Expr::Var(binding), vm::Expr::String(field.to_owned())],
        None,
        None,
    )
}

fn heap_as_number(expr: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(expr), Box::new(vm::Expr::Float(1.0)))
}

fn heap_guest_call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, Vec::new(), args, None, None)
}

/// Slots that carry a source-level binding. A publication run ends at the
/// previous statement that writes one of these, so replaying a store can never
/// re-run a neighbouring source statement.
fn heap_source_slots(ir: &vm::FrontendIr) -> Vec<u16> {
    ir.local_bindings.iter().map(|(_, slot)| *slot).collect()
}

/// The real activation frame that owns one store's roots.
fn heap_frame_slot(
    ir: &vm::FrontendIr,
    store: usize,
    root_enter: u16,
) -> Result<u16, vm::ParseError> {
    ir.stmts[..store]
        .iter()
        .rev()
        .find_map(|stmt| match stmt {
            vm::Stmt::Let {
                index,
                expr: vm::Expr::Call(target, _, _, _, _),
                ..
            } if *target == root_enter => Some(*index),
            _ => None,
        })
        .ok_or_else(|| heap_rejection("actual activation frame".to_owned()))
}

/// The real publication run behind one hoisted store. The current emitter
/// reserves every key node first, initializes the containing node through the
/// existing cell writer, publishes it into the active frame entry and hands the
/// published Primitive to the store temp. Only that contiguous run of internal
/// `Let`/`Assign` steps is replayed: a statement that writes a source binding,
/// or any other form, is the boundary, so a replay never re-runs a neighbouring
/// source statement and never adds an allocation site.
fn heap_publication_start(
    ir: &vm::FrontendIr,
    store: usize,
    node_begin: u16,
    root_enter: u16,
) -> Result<usize, vm::ParseError> {
    let node_publish = heap_helper_index(ir, HEAP_NODE_PUBLISH_NAME)?;
    let frame = heap_frame_slot(ir, store, root_enter)?;
    let sources = heap_source_slots(ir);
    let vm::Stmt::Assign { expr: stored, .. } = &ir.stmts[store] else {
        return Err(heap_rejection(
            "heap fixture requires the real binding store".to_owned(),
        ));
    };
    let vm::Expr::Var(value_slot) = stored else {
        return Err(heap_rejection(format!(
            "heap fixture requires the published binding value, got {stored:?}"
        )));
    };
    let mut start = store;
    while start > 0 {
        let internal = match &ir.stmts[start - 1] {
            vm::Stmt::Let { index, .. }
            | vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index,
                ..
            } => !sources.contains(index),
            _ => false,
        };
        if !internal {
            break;
        }
        start -= 1;
    }
    if start == store {
        return Err(heap_rejection(
            "heap fixture requires a replayable publication run".to_owned(),
        ));
    }
    let mut begin_at = None;
    let mut published = None;
    for (at, stmt) in ir.stmts[start..store].iter().enumerate() {
        match stmt {
            vm::Stmt::Let {
                index,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } => {
                if *target == node_begin {
                    begin_at = Some(at);
                }
                if *target == node_publish {
                    if !matches!(args.get(4), Some(vm::Expr::Var(framed)) if *framed == frame) {
                        return Err(heap_rejection(
                            "node publish must target the actual activation frame".to_owned(),
                        ));
                    }
                    published = Some((at, *index));
                }
            }
            vm::Stmt::Let { .. }
            | vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                ..
            } => {}
            other => {
                return Err(heap_rejection(format!(
                    "heap fixture replays only Let bindings, got {other:?}"
                )));
            }
        }
    }
    let begin_at = begin_at.ok_or_else(|| {
        heap_rejection("heap fixture replay must span a node reservation".to_owned())
    })?;
    let (publish_at, published_slot) = published
        .ok_or_else(|| heap_rejection("heap fixture replay must span a node publish".to_owned()))?;
    if begin_at > publish_at {
        return Err(heap_rejection(
            "node reservation must precede its publish".to_owned(),
        ));
    }
    // The stored value must be that publish result, directly or through the
    // single handoff temp the emitter assigns before the store.
    let direct = *value_slot == published_slot;
    let handed_off = ir.stmts[start..store].iter().any(|stmt| {
        matches!(stmt,
            vm::Stmt::Assign {kind: vm::AssignmentKind::Set, index, expr: vm::Expr::Var(source), ..}
            if *index == *value_slot && *source == published_slot
        )
    });
    if !direct && !handed_off {
        return Err(heap_rejection(
            "stored binding value must be the published node".to_owned(),
        ));
    }
    Ok(start)
}

fn heap_replay_binding(
    ir: &mut vm::FrontendIr,
    binding: u16,
    node_begin: u16,
    root_enter: u16,
    root_write: u16,
) -> Result<(), vm::ParseError> {
    let boxed_at = ir
        .stmts
        .iter()
        .position(|stmt| matches!(stmt, vm::Stmt::Assign { index, .. } if *index == binding))
        .ok_or_else(|| heap_rejection("heap fixture requires the real binding store".to_owned()))?;
    let publish_expr = match &ir.stmts[boxed_at] {
        vm::Stmt::Assign { expr, .. } => expr.clone(),
        other => {
            return Err(heap_rejection(format!(
                "heap fixture requires the replayable binding store, got {other:?}"
            )));
        }
    };
    // Hoisted bindings are predeclared, then stored by Assign. The store's value
    // is produced by the whole real publication run, so replay that run on the
    // same slots, then the store and its production root-write ack.
    let run_start = heap_publication_start(ir, boxed_at, node_begin, root_enter)?;
    let run: Vec<vm::Stmt> = ir.stmts[run_start..boxed_at].to_vec();
    let ack = ir.stmts.get(boxed_at + 1).cloned();
    for stmt in &run {
        match stmt {
            vm::Stmt::Let {
                index, expr, line, ..
            } => ir.stmts.push(vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: *index,
                expr: expr.clone(),
                line: *line,
            }),
            vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                ..
            } => ir.stmts.push(stmt.clone()),
            other => {
                return Err(heap_rejection(format!(
                    "heap fixture replays only Let bindings, got {other:?}"
                )));
            }
        }
    }
    ir.stmts.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: binding,
        expr: publish_expr,
        line: 1,
    });
    if let Some(vm::Stmt::Let {
        index, expr, line, ..
    }) = ack
        && let vm::Expr::Call(target, _, _, _, _) = &expr
        && *target == root_write
    {
        ir.stmts.push(vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line,
        });
    }
    Ok(())
}

impl vm::SourcePlugin for HeapStalePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-stale"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let get = vm::BuiltinFunction::Get.call_index();
        let make = heap_helper_index(&ir, HEAP_MAKE_NAME)?;
        let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)?;
        let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)?;
        let collect = heap_helper_index(&ir, HEAP_COLLECT_NAME)?;
        let node_begin = heap_helper_index(&ir, HEAP_NODE_BEGIN_NAME)?;
        let root_enter = heap_helper_index(&ir, HEAP_ROOT_ENTER_NAME)?;
        let root_write = heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME)?;
        let mut carriers = Vec::new();
        for stmt in &ir.stmts {
            if let vm::Stmt::Let {
                index,
                declared_schema: None,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } = stmt
                && *target == make
                && args.len() == 1
            {
                carriers.push(*index);
            }
        }
        let [carrier] = carriers.as_slice() else {
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let binding = heap_binding_index(&ir, &self.binding)?;
        // Reserve all fixture slots up front: no closure borrows the IR.
        let (old_slot, old_gen, ack, new_slot, new_gen) = {
            let mut take = || -> Result<u16, vm::ParseError> {
                let slot = u16::try_from(ir.locals)
                    .map_err(|_| heap_rejection("heap fixture slot space exhausted".to_owned()))?;
                ir.locals += 1;
                Ok(slot)
            };
            (take()?, take()?, take()?, take()?, take()?)
        };
        let stash = |ir: &mut vm::FrontendIr, out: u16, field: &str| {
            ir.stmts.push(vm::Stmt::Let {
                index: out,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(
                    decode,
                    vec![heap_as_number(heap_field_get(get, binding, field))],
                ),
                line: 1,
            });
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(out),
                line: 1,
            });
        };
        // Stash the live box identity in plain (unrooted) diagnostic locals.
        stash(&mut ir, old_slot, "ref_slot");
        stash(&mut ir, old_gen, "generation");
        // First replay unroots the original node; the explicit collection
        // frees it; the second replay reuses the freed slot with a bumped
        // generation. The entry now names the newest box throughout.
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(collect, vec![vm::Expr::Var(carrier)]),
            line: 1,
        });
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        stash(&mut ir, new_slot, "ref_slot");
        stash(&mut ir, new_gen, "generation");
        let base = {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection("heap fixture slot space exhausted".to_owned()))?;
            ir.locals += 1;
            slot
        };
        let (probe_slot, probe_gen) = if self.probe_stale {
            (old_slot, old_gen)
        } else {
            (new_slot, new_gen)
        };
        ir.stmts.push(vm::Stmt::Let {
            index: base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(probe_slot),
                    vm::Expr::Var(probe_gen),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(base),
            line: 1,
        });
        Ok(ir)
    }
}

fn heap_stale_run(
    source: &str,
    binding: &str,
    probe_stale: bool,
) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapStalePlugin = Box::leak(Box::new(HeapStalePlugin {
        binding: binding.to_owned(),
        probe_stale,
    }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-stale")),
        options,
    )
    .expect("stale compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(10_000_000);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "stale program should halt");
            None
        }
        Err(error) => Some(error),
    };
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    (machine.stack().to_vec(), error)
}

fn heap_stale_ints(stack: &[Value]) -> Vec<i64> {
    stack
        .iter()
        .map(|v| match v {
            Value::Int(n) => *n,
            Value::Float(n) => *n as i64,
            other => panic!("stale evidence must be Int, got {other:?}"),
        })
        .collect()
}

/// Stale refusal plus slot-reuse evidence: the run must fail exactly at the
/// stale dereference with the full four-cell evidence below it proving the
/// slot was reused with a bumped generation. The failing gate leaves its
/// false condition on the stack, which locates the refusal at the probe.
fn heap_expect_stale_refusal(source: &str, binding: &str, expected_slot: i64) {
    let (stack, error) = heap_stale_run(source, binding, true);
    let error = error.expect("stale dereference must refuse");
    eprintln!("P2_STALE_REFUSE {binding} error={error:?} stack={stack:?}");
    assert!(
        format!("{error:?}").contains("assertion failed"),
        "refusal must come from the checked gate, got {error:?}"
    );
    assert_eq!(
        stack.len(),
        5,
        "refusal must leave evidence plus the false gate, got {stack:?}"
    );
    let evidence = heap_stale_ints(&stack[..4]);
    assert!(
        matches!(stack[4], Value::Bool(false)),
        "refusal must leave the false gate condition, got {:?}",
        stack[4]
    );
    let (old_slot, old_gen, new_slot, new_gen) =
        (evidence[0], evidence[1], evidence[2], evidence[3]);
    assert_eq!(
        new_slot, old_slot,
        "freed slot must be reused, got {evidence:?}"
    );
    assert_eq!(
        new_gen,
        old_gen + 1,
        "reuse must bump the generation, got {evidence:?}"
    );
    assert_eq!(
        old_slot, expected_slot,
        "first node must live in the expected slot, got {evidence:?}"
    );
}

/// Fresh dereference after the identical reuse: must halt with the same
/// slot-reuse evidence plus a valid physical base past the fixed prefix.
fn heap_expect_fresh_success(source: &str, binding: &str, expected_slot: i64) {
    let (stack, error) = heap_stale_run(source, binding, false);
    assert!(
        error.is_none(),
        "fresh dereference must succeed, got {error:?}"
    );
    let evidence = heap_stale_ints(&stack);
    assert_eq!(
        evidence.len(),
        5,
        "success must leave evidence plus base, got {evidence:?}"
    );
    let (old_slot, old_gen, new_slot, new_gen, base) = (
        evidence[0],
        evidence[1],
        evidence[2],
        evidence[3],
        evidence[4],
    );
    eprintln!("P2_STALE_FRESH {binding} evidence={evidence:?}");
    assert_eq!(
        new_slot, old_slot,
        "freed slot must be reused, got {evidence:?}"
    );
    assert_eq!(
        new_gen,
        old_gen + 1,
        "reuse must bump the generation, got {evidence:?}"
    );
    assert_eq!(
        old_slot, expected_slot,
        "first node must live in the expected slot, got {evidence:?}"
    );
    assert!(
        base >= 80,
        "fresh base must clear the fixed prefix, got {base}"
    );
}

#[test]
fn p2_heap_stale_array_refuses_and_fresh_works() {
    heap_expect_stale_refusal("var a = [1];", "a", 0);
    heap_expect_fresh_success("var a = [1];", "a", 0);
}

#[test]
fn p2_heap_stale_string_refuses_and_fresh_works() {
    heap_expect_stale_refusal("var s = \"hi\";", "s", 0);
    heap_expect_fresh_success("var s = \"hi\";", "s", 0);
}

#[test]
fn p2_heap_stale_object_refuses_and_fresh_works() {
    heap_expect_stale_refusal("var o = {};", "o", HEAP_INTRINSIC_NODES);
    heap_expect_fresh_success("var o = {};", "o", HEAP_INTRINSIC_NODES);
}

#[test]
fn p2_heap_stale_function_refuses_and_fresh_works() {
    heap_expect_stale_refusal(
        "var f = function() { \"use strict\"; return 1; };",
        "f",
        HEAP_INTRINSIC_NODES,
    );
    heap_expect_fresh_success(
        "var f = function() { \"use strict\"; return 1; };",
        "f",
        HEAP_INTRINSIC_NODES,
    );
}

// ---- Stale-key harness: the stored row key_gen must not be ignored ----
// `var o = {k: 1}` replays through the same unroot/collect/reuse cycle;
// the forged Reference naming the ORIGINAL key generation must refuse in
// `__compat_es5_ref_get` while the same slot with the CURRENT generation
// reads back 1. Only key_slot/key_gen travel in the forged value; the base
// stays the live object and strictness stays false.

const HEAP_READ_NAME: &str = "__compat_ref_read";
const HEAP_REF_GET_NAME: &str = "__compat_es5_ref_get";

struct HeapKeyPlugin {
    probe_stale: bool,
    corrupt_stored: bool,
}

fn heap_object_row_key(
    ir: &mut vm::FrontendIr,
    read: u16,
    decode: u16,
    carrier: u16,
    base: u16,
    cell: i64,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let raw = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: raw,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: heap_guest_call(
            read,
            vec![
                vm::Expr::Var(carrier),
                heap_as_number(vm::Expr::Add(
                    Box::new(vm::Expr::Var(base)),
                    Box::new(vm::Expr::Int(5 + cell)),
                )),
            ],
        ),
        line: 1,
    });
    let out = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(decode, vec![vm::Expr::Var(raw)]),
        line: 1,
    });
    Ok(out)
}

impl vm::SourcePlugin for HeapKeyPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-key"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let get = vm::BuiltinFunction::Get.call_index();
        let map_new = vm::BuiltinFunction::MapNew.call_index();
        let set = vm::BuiltinFunction::Set.call_index();
        let make = heap_helper_index(&ir, HEAP_MAKE_NAME)?;
        let read = heap_helper_index(&ir, HEAP_READ_NAME)?;
        let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)?;
        let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)?;
        let collect = heap_helper_index(&ir, HEAP_COLLECT_NAME)?;
        let node_begin = heap_helper_index(&ir, HEAP_NODE_BEGIN_NAME)?;
        let root_enter = heap_helper_index(&ir, HEAP_ROOT_ENTER_NAME)?;
        let root_write = heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME)?;
        let ref_get = heap_helper_index(&ir, HEAP_REF_GET_NAME)?;
        let mut carriers = Vec::new();
        for stmt in &ir.stmts {
            if let vm::Stmt::Let {
                index,
                declared_schema: None,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } = stmt
                && *target == make
                && args.len() == 1
            {
                carriers.push(*index);
            }
        }
        let [carrier] = carriers.as_slice() else {
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let binding = heap_binding_index(&ir, "o")?;
        let (base_old, ack, base_new, forged, got) = {
            let mut take = || -> Result<u16, vm::ParseError> {
                let slot = u16::try_from(ir.locals)
                    .map_err(|_| heap_rejection("heap key slot space exhausted".to_owned()))?;
                ir.locals += 1;
                Ok(slot)
            };
            (take()?, take()?, take()?, take()?, take()?)
        };
        // Whole-box copy before any field read, mirroring the production
        // receiver copy: later field moves must never poison whole uses.
        // Taken after the final replay so it names the newest live box.
        let box_copy = {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection("heap key slot space exhausted".to_owned()))?;
            ir.locals += 1;
            slot
        };
        // Live base plus the stored row key pair, pushed as evidence.
        ir.stmts.push(vm::Stmt::Let {
            index: base_old,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    heap_field_get(get, binding, "ref_slot"),
                    heap_field_get(get, binding, "generation"),
                ],
            ),
            line: 1,
        });
        let key_slot_old = heap_object_row_key(&mut ir, read, decode, carrier, base_old, 0, "key")?;
        let key_gen_old = heap_object_row_key(&mut ir, read, decode, carrier, base_old, 1, "key")?;
        for slot in [key_slot_old, key_gen_old] {
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(slot),
                line: 1,
            });
        }
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(collect, vec![vm::Expr::Var(carrier)]),
            line: 1,
        });
        // Valid free-list ordering for the two reclaimed rows: key0 then
        // object1, so same-kind slot reuse isolates the generation check.
        let mut key_ctx = heap_cycle_ctx(&ir)?;
        key_ctx.carrier = carrier;
        let page_raw =
            heap_cycle_read_number(&mut ir, &key_ctx, carrier, vm::Expr::Int(16), "key-order")?;
        let page = heap_cycle_take(&mut ir, "key-order")?;
        ir.stmts.push(vm::Stmt::Let {
            index: page,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(decode, vec![vm::Expr::Var(page_raw)]),
            line: 1,
        });
        for (address, link) in [
            (vm::Expr::Int(5), 1),
            (
                vm::Expr::Add(Box::new(vm::Expr::Var(page)), Box::new(vm::Expr::Int(9))),
                2,
            ),
            (
                vm::Expr::Add(Box::new(vm::Expr::Var(page)), Box::new(vm::Expr::Int(16))),
                0,
            ),
        ] {
            heap_cycle_write(
                &mut ir,
                &key_ctx,
                carrier,
                address,
                vm::Expr::Float(link as f64),
                "key-order",
            )?;
        }
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        ir.stmts.push(vm::Stmt::Let {
            index: box_copy,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Primitive".to_owned(),
                Vec::new(),
            )),
            expr: vm::Expr::Var(binding),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Let {
            index: base_new,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    heap_field_get(get, binding, "ref_slot"),
                    heap_field_get(get, binding, "generation"),
                ],
            ),
            line: 1,
        });
        let key_slot_new = heap_object_row_key(&mut ir, read, decode, carrier, base_new, 0, "key")?;
        let key_gen_new = heap_object_row_key(&mut ir, read, decode, carrier, base_new, 1, "key")?;
        for slot in [key_slot_new, key_gen_new] {
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(slot),
                line: 1,
            });
        }
        if self.corrupt_stored {
            // The Reference below names the fresh live key correctly. Only
            // the object's stored key generation is stale in this control.
            heap_cycle_write(
                &mut ir,
                &key_ctx,
                carrier,
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(base_new)),
                    Box::new(vm::Expr::Int(6)),
                ),
                heap_as_number(vm::Expr::Var(key_gen_old)),
                "stored-key-generation",
            )?;
        }
        // Forge the Reference with either the original or the current key
        // generation; the slot travels from the matching read.
        let (key_slot, key_gen) = if self.probe_stale {
            (key_slot_old, key_gen_old)
        } else {
            (key_slot_new, key_gen_new)
        };
        let mut reference = heap_guest_call(map_new, Vec::new());
        for (field, value) in [
            ("kind", vm::Expr::Int(0)),
            ("base", vm::Expr::Var(box_copy)),
            ("key_slot", vm::Expr::Var(key_slot)),
            ("key_generation", vm::Expr::Var(key_gen)),
            ("strict", vm::Expr::Bool(false)),
        ] {
            reference = heap_guest_call(
                set,
                vec![reference, vm::Expr::String(field.to_owned()), value],
            );
        }
        ir.stmts.push(vm::Stmt::Let {
            index: forged,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Reference".to_owned(),
                Vec::new(),
            )),
            expr: reference,
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Let {
            index: got,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Primitive".to_owned(),
                Vec::new(),
            )),
            expr: heap_guest_call(ref_get, vec![vm::Expr::Var(carrier), vm::Expr::Var(forged)]),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(got),
            line: 1,
        });
        Ok(ir)
    }
}

fn heap_key_run(probe_stale: bool) -> (Vec<Value>, Option<vm::VmError>) {
    heap_key_run_variant(probe_stale, false)
}

fn heap_key_run_variant(
    probe_stale: bool,
    corrupt_stored: bool,
) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapKeyPlugin = Box::leak(Box::new(HeapKeyPlugin {
        probe_stale,
        corrupt_stored,
    }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var o = {k: 1};",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-key")),
        options,
    )
    .expect("key compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "key program should halt");
            None
        }
        Err(error) => Some(error),
    };
    (machine.stack().to_vec(), error)
}

#[test]
fn p2_heap_stale_key_refuses_and_fresh_reads() {
    let (stack, error) = heap_key_run(true);
    let error = error.expect("stale key generation must refuse");
    eprintln!("P2_STALE_KEY_REFUSE error={error:?} stack={stack:?}");
    assert!(
        format!("{error:?}").contains("assertion failed"),
        "key refusal must come from the checked gate, got {error:?}"
    );
    assert_eq!(
        stack.len(),
        5,
        "key refusal must leave key evidence plus the false gate, got {stack:?}"
    );
    assert!(
        matches!(stack[4], Value::Bool(false)),
        "key refusal must leave the false gate condition, got {:?}",
        stack[4]
    );
    let (stack, error) = heap_key_run(false);
    assert!(
        error.is_none(),
        "current key generation must read, got {error:?}"
    );
    let value = stack.last().expect("ref_get must leave the value");
    eprintln!("P2_STALE_KEY_FRESH stack={stack:?}");
    assert_eq!(
        heap_box_number(value),
        1.0,
        "current key must read back 1, got {value:?}"
    );
}

#[test]
fn p2_heap_stored_key_generation_is_independently_checked() {
    let (stack, error) = heap_key_run_variant(false, true);
    let error = error.expect("correct fresh Reference cannot authenticate a stale stored row key");
    assert!(format!("{error:?}").contains("assertion failed"));
    let evidence = heap_stale_ints(&stack[..4]);
    assert_eq!(evidence, [0, 1, 0, 2]);
    assert_eq!(stack.last(), Some(&Value::Bool(false)));
    eprintln!("P2_STORED_KEY_STALE error={error:?} evidence={evidence:?}");
}

// ---- Wrong-kind harness: kind gates are distinct from generation gates ----
// Mirrors the production numeric-array read gate bit for bit (live base,
// kind cell, 17-bit decode, assert against KIND_ARRAY) but presents a live
// OBJECT node. The gate must refuse with the true kind as evidence; the
// same gate against KIND_OBJECT must pass. A generation failure would show
// a different evidence shape, so this distinguishes the two checks.

struct HeapKindPlugin {
    expect_kind: i64,
}

impl vm::SourcePlugin for HeapKindPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-kind"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let assert_index = vm::BuiltinFunction::Assert.call_index();
        let make = heap_helper_index(&ir, HEAP_MAKE_NAME)?;
        let read = heap_helper_index(&ir, HEAP_READ_NAME)?;
        let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)?;
        let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)?;
        let mut carriers = Vec::new();
        for stmt in &ir.stmts {
            if let vm::Stmt::Let {
                index,
                declared_schema: None,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } = stmt
                && *target == make
                && args.len() == 1
            {
                carriers.push(*index);
            }
        }
        let [carrier] = carriers.as_slice() else {
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let binding = heap_binding_index(&ir, "o")?;
        let get = vm::BuiltinFunction::Get.call_index();
        let take = |ir: &mut vm::FrontendIr, tag: &str| -> Result<u16, vm::ParseError> {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
            ir.locals += 1;
            Ok(slot)
        };
        let base = take(&mut ir, "kind")?;
        ir.stmts.push(vm::Stmt::Let {
            index: base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    heap_field_get(get, binding, "ref_slot"),
                    heap_field_get(get, binding, "generation"),
                ],
            ),
            line: 1,
        });
        let kind_num = take(&mut ir, "kind")?;
        ir.stmts.push(vm::Stmt::Let {
            index: kind_num,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: heap_guest_call(
                read,
                vec![vm::Expr::Var(carrier), heap_as_number(vm::Expr::Var(base))],
            ),
            line: 1,
        });
        let kind = take(&mut ir, "kind")?;
        ir.stmts.push(vm::Stmt::Let {
            index: kind,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(decode, vec![vm::Expr::Var(kind_num)]),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(kind),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(
                assert_index,
                vec![vm::Expr::Eq(
                    Box::new(vm::Expr::Var(kind)),
                    Box::new(vm::Expr::Int(self.expect_kind)),
                )],
            ),
            line: 1,
        });
        Ok(ir)
    }
}

fn heap_kind_run(expect_kind: i64) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapKindPlugin = Box::leak(Box::new(HeapKindPlugin { expect_kind }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var o = {};",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-kind")),
        options,
    )
    .expect("kind compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "kind program should halt");
            None
        }
        Err(error) => Some(error),
    };
    (machine.stack().to_vec(), error)
}

#[test]
fn p2_heap_kind_gate_refuses_object_as_array() {
    // KIND_ARRAY = 1 against a live OBJECT node (kind 3): must refuse with
    // the true kind pushed as evidence plus the false gate.
    let (stack, error) = heap_kind_run(1);
    let error = error.expect("object presented as array must refuse");
    eprintln!("P2_KIND_REFUSE error={error:?} stack={stack:?}");
    assert!(
        format!("{error:?}").contains("assertion failed"),
        "kind refusal must come from the gate, got {error:?}"
    );
    assert_eq!(
        stack.len(),
        2,
        "kind refusal must leave the kind plus the false gate, got {stack:?}"
    );
    assert!(
        matches!(stack[0], Value::Int(3)),
        "refused node must report OBJECT kind 3, got {:?}",
        stack[0]
    );
    assert!(
        matches!(stack[1], Value::Bool(false)),
        "kind refusal must leave the false gate condition, got {:?}",
        stack[1]
    );
    // KIND_OBJECT = 3 against the same node: must pass.
    let (stack, error) = heap_kind_run(3);
    assert!(
        error.is_none(),
        "object presented as object must pass, got {error:?}"
    );
    eprintln!("P2_KIND_FRESH stack={stack:?}");
    assert!(
        matches!(stack.last(), Some(Value::Int(3))),
        "passing gate must confirm kind 3, got {stack:?}"
    );
}

// ---- Max-generation retirement: sweep retires, never wraps ----
// A consistent ACTIVE max-generation row (slot0, proven first node) is
// unrooted by replay, swept by an explicit collection, then observed
// retired while the next allocation lands on a different slot. The slot-0
// fact is re-asserted in-run; a moved first node would fail loudly.

const HEAP_WRITE_NAME: &str = "__compat_ref_write";

struct HeapMaxgenPlugin;

impl vm::SourcePlugin for HeapMaxgenPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-maxgen"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let get = vm::BuiltinFunction::Get.call_index();
        let make = heap_helper_index(&ir, HEAP_MAKE_NAME)?;
        let read = heap_helper_index(&ir, HEAP_READ_NAME)?;
        let write = heap_helper_index(&ir, HEAP_WRITE_NAME)?;
        let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)?;
        let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)?;
        let collect = heap_helper_index(&ir, HEAP_COLLECT_NAME)?;
        let node_begin = heap_helper_index(&ir, HEAP_NODE_BEGIN_NAME)?;
        let root_enter = heap_helper_index(&ir, HEAP_ROOT_ENTER_NAME)?;
        let root_write = heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME)?;
        let observe = heap_helper_index(&ir, HEAP_OBSERVE_NAME)?;
        let mut carriers = Vec::new();
        for stmt in &ir.stmts {
            if let vm::Stmt::Let {
                index,
                declared_schema: None,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } = stmt
                && *target == make
                && args.len() == 1
            {
                carriers.push(*index);
            }
        }
        let [carrier] = carriers.as_slice() else {
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let binding = heap_binding_index(&ir, "a")?;
        let take = |ir: &mut vm::FrontendIr, tag: &str| -> Result<u16, vm::ParseError> {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
            ir.locals += 1;
            Ok(slot)
        };
        // Stash the first-node slot; slot0 is asserted in-test below.
        // Both identity fields are stashed (mirroring the stale fixture
        // shape the move checker accepts).
        let first = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: first,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                decode,
                vec![heap_as_number(heap_field_get(get, binding, "ref_slot"))],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(first),
            line: 1,
        });
        let first_gen = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: first_gen,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                decode,
                vec![heap_as_number(heap_field_get(get, binding, "generation"))],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(first_gen),
            line: 1,
        });
        // Keep slot0 active and rooted while making the diagnostic maximum
        // generation consistent across row, node header and actual root.
        let original_base = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: original_base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(first),
                    vm::Expr::Var(first_gen),
                ],
            ),
            line: 1,
        });
        // Overwrite the original row generation with u32::MAX. Slot0 lives
        // in directory page 0 row 0: row address = page_base + 3, and the
        // generation cell is one past the row start.
        let page_ptr = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: page_ptr,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: heap_guest_call(
                read,
                vec![vm::Expr::Var(carrier), heap_as_number(vm::Expr::Int(16))],
            ),
            line: 1,
        });
        let page_base = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: page_base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(decode, vec![vm::Expr::Var(page_ptr)]),
            line: 1,
        });
        let gen_cell = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: gen_cell,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: vm::Expr::Add(
                Box::new(vm::Expr::Var(page_base)),
                Box::new(vm::Expr::Int(3 + 1)),
            ),
            line: 1,
        });
        let wack = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: wack,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: heap_guest_call(
                write,
                vec![
                    vm::Expr::Var(carrier),
                    heap_as_number(vm::Expr::Var(gen_cell)),
                    heap_as_number(vm::Expr::Int(u32::MAX as i64)),
                ],
            ),
            line: 1,
        });
        let header_ack = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: header_ack,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: heap_guest_call(
                write,
                vec![
                    vm::Expr::Var(carrier),
                    heap_as_number(vm::Expr::Add(
                        Box::new(vm::Expr::Var(original_base)),
                        Box::new(vm::Expr::Int(1)),
                    )),
                    vm::Expr::Float(f64::from(u32::MAX)),
                ],
            ),
            line: 1,
        });
        let frame = ir
            .stmts
            .iter()
            .find_map(|stmt| match stmt {
                vm::Stmt::Let {
                    index,
                    expr: vm::Expr::Call(target, _, args, _, _),
                    ..
                } if *target == root_enter && matches!(args.get(1), Some(vm::Expr::Int(1))) => {
                    Some(*index)
                }
                _ => None,
            })
            .ok_or_else(|| {
                heap_rejection(
                    "consistent maxgen requires the one-entry actual source frame".to_owned(),
                )
            })?;
        let root_node = heap_helper_index(&ir, "__compat_es5_root_write_node")?;
        let rooted_ack = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: rooted_ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                root_node,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(frame),
                    vm::Expr::Int(0),
                    vm::Expr::Int(5),
                    vm::Expr::Var(first),
                    vm::Expr::Int(i64::from(u32::MAX)),
                ],
            ),
            line: 1,
        });
        let retained = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: retained,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(collect, vec![vm::Expr::Var(carrier)]),
            line: 1,
        });
        let retained_base = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: retained_base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(first),
                    vm::Expr::Int(i64::from(u32::MAX)),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(
                vm::BuiltinFunction::Assert.call_index(),
                vec![vm::Expr::Eq(
                    Box::new(vm::Expr::Var(original_base)),
                    Box::new(vm::Expr::Var(retained_base)),
                )],
            ),
            line: 1,
        });
        // A real replacement now clears the sole old root. The maxgen node
        // was live and checked above; only this subsequent sweep retires it.
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        // Sweep must retire the max-generation row: observe the retired
        // counter plus the row state/generation through the production
        // selector chain, then allocate once more.
        let ack = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(collect, vec![vm::Expr::Var(carrier)]),
            line: 1,
        });
        // which, slot-arg: 1=retired counter, 13=row state, 14=row gen.
        for (which, slot_arg) in [(1i64, 0i64), (13, 0), (14, 0)] {
            let out = take(&mut ir, "maxgen")?;
            ir.stmts.push(vm::Stmt::Let {
                index: out,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(
                    observe,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Int(which),
                        vm::Expr::Int(slot_arg),
                    ],
                ),
                line: 1,
            });
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(out),
                line: 1,
            });
        }
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        let second = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: second,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                decode,
                vec![heap_as_number(heap_field_get(get, binding, "ref_slot"))],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(second),
            line: 1,
        });
        let second_gen = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: second_gen,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                decode,
                vec![heap_as_number(heap_field_get(get, binding, "generation"))],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(second_gen),
            line: 1,
        });
        // Fresh dereference of the newest box must still work. Reuse the
        // stashed identity (mirroring the stale fixture) rather than
        // reading the same fields a third time.
        let base = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(second),
                    vm::Expr::Var(second_gen),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(base),
            line: 1,
        });
        Ok(ir)
    }
}

#[test]
fn p2_heap_maxgen_retires_without_wrap() {
    let plugin: &'static HeapMaxgenPlugin = Box::leak(Box::new(HeapMaxgenPlugin));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var a = [1];",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-maxgen")),
        options,
    )
    .expect("maxgen compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(10_000_000);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    match machine.run() {
        Ok(status) => assert_eq!(status, VmStatus::Halted, "maxgen program should halt"),
        Err(error) => panic!(
            "MAXGEN RUN-FAIL ip={} error={error:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        ),
    }
    let stack = machine.stack().to_vec();
    eprintln!("P2_MAXGEN stack={stack:?}");
    // first slot/gen, retired counter, row state, row generation, second
    // slot/gen, fresh base: eight trailing observations.
    assert!(stack.len() >= 8, "maxgen evidence plus base, got {stack:?}");
    let tail = &stack[stack.len() - 8..];
    let counter = |v: &Value| match v {
        Value::Int(n) => *n,
        Value::Float(n) => *n as i64,
        other => panic!("maxgen evidence must be Int, got {other:?}"),
    };
    let (first, first_gen, retired, row_state, row_gen, second, second_gen, base) = (
        counter(&tail[0]),
        counter(&tail[1]),
        counter(&tail[2]),
        counter(&tail[3]),
        counter(&tail[4]),
        counter(&tail[5]),
        counter(&tail[6]),
        counter(&tail[7]),
    );
    assert_eq!(first, 0, "first node must live in slot0, got {first}");
    assert_eq!(
        first_gen, 1,
        "first node must start at generation 1, got {first_gen}"
    );
    assert_eq!(
        retired, 1,
        "sweep must retire exactly the max-generation row, got {retired}"
    );
    assert_eq!(
        row_state, 4,
        "retired row must report state 4, got {row_state}"
    );
    assert_eq!(
        row_gen,
        u32::MAX as i64,
        "retired row must keep its generation without wrapping, got {row_gen}"
    );
    assert_ne!(
        second, first,
        "post-retirement allocation must avoid the retired slot"
    );
    assert!(
        second_gen >= 1,
        "replacement slot must carry a live generation, got {second_gen}"
    );
    assert!(
        base >= 80,
        "fresh base must clear the fixed prefix, got {base}"
    );
    assert!(!machine.jit_config().enabled, "maxgen VM must keep JIT off");
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    eprintln!(
        "P10_U32_MAXGEN_RESULT status=Halted fuel={:?} jit=off aot=0 jit_native=0",
        machine.get_fuel()
    );
    machine.reset_for_reuse().expect("reset maxgen VM");
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "maxgen VM must release the real owner on reset"
    );
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none(),
        "maxgen VM must release the owned array directory on reset"
    );
    eprintln!(
        "P10_U32_MAXGEN_CLEANUP resource_count=0 owned_arrays=none reset=1 post_reset_guest_runs=0"
    );
}

// ---- Number.prototype.toString radix rejection: root-frame rollback ----
// The rejection arm publishes two fixed Strings and one RangeError instance
// inside a two-entry root frame. Every one of those allocations can grow the
// heap, and a growth past the physical ceiling is a host fatal that never
// reaches the trailing `root_leave`. The arm preflights its whole worst-case
// extent against `HDR_HEAP_END` before entering the frame, so the fatal is
// raised while the active-root capacity is still at its entry value. This
// fixture inflates the watermark just below the ceiling, observes the
// capacity at the top of the arm, and re-reads the live header after the
// abort.

const RADIX_ROOT_ID: &str = "javascript-es5-radix-root-rejection";
const RADIX_FRAME_OBSERVATION: &str = "number-to-string radix rejection entry";
/// `PHYSICAL_CEILING` (65536) minus a margin large enough for every
/// allocation that precedes the rejection arm, but smaller than the arm's
/// preflighted worst case (532 cells, one directory page included).
const RADIX_INFLATED_HEAP_END: i64 = 65_336;
const RADIX_HEADER_HEAP_END: i64 = 1;
const RADIX_HEADER_ACTIVE_ROOT_CAPACITY: i64 = 9;

/// The rejection arm is the only statement vector in the dispatch helper that
/// enters a two-entry frame; return that vector so the observation lands in
/// front of the preflight and the frame entry.
fn radix_rejection_block<'a>(
    stmts: &'a mut Vec<vm::Stmt>,
    root_enter: u16,
) -> Option<&'a mut Vec<vm::Stmt>> {
    let is_rejection_block = stmts.iter().any(|stmt| {
        matches!(
            stmt,
            vm::Stmt::Assign { expr: vm::Expr::Call(id, _, args, _, _), .. }
                if *id == root_enter && matches!(args.get(1), Some(vm::Expr::Int(2)))
        )
    });
    if is_rejection_block {
        return Some(stmts);
    }
    for stmt in stmts.iter_mut() {
        match stmt {
            vm::Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                if let Some(block) = radix_rejection_block(then_branch, root_enter) {
                    return Some(block);
                }
                if let Some(block) = radix_rejection_block(else_branch, root_enter) {
                    return Some(block);
                }
            }
            vm::Stmt::While { body, .. } => {
                if let Some(block) = radix_rejection_block(body, root_enter) {
                    return Some(block);
                }
            }
            _ => {}
        }
    }
    None
}

struct RadixRootPlugin;

impl vm::SourcePlugin for RadixRootPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new(RADIX_ROOT_ID);
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let write = heap_helper_index(&ir, HEAP_WRITE_NAME)?;
        let observe = heap_helper_index(&ir, HEAP_OBSERVE_NAME)?;
        let root_enter = heap_helper_index(&ir, HEAP_ROOT_ENTER_NAME)?;
        let dispatch = heap_helper_index(&ir, "__compat_es5_dispatch")?;
        let implementation = ir
            .function_impls
            .get_mut(&dispatch)
            .expect("dispatch implementation");
        let carrier = implementation.param_slots[0];
        let block = radix_rejection_block(&mut implementation.body_stmts, root_enter).ok_or_else(
            || heap_rejection("radix fixture requires the two-entry rejection frame".to_owned()),
        )?;
        // Raise the watermark only on the rejection path, in front of the
        // preflight. Only the frontier cell moves; every page and free extent
        // stays below it, and the sequence that follows reads cells 1 and 9
        // only, so the backing extent is never touched past its live end.
        block.insert(
            0,
            vm::Stmt::Expr {
                expr: heap_guest_call(
                    write,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Float(RADIX_HEADER_HEAP_END as f64),
                        vm::Expr::Float(RADIX_INFLATED_HEAP_END as f64),
                    ],
                ),
                line: 1,
            },
        );
        block.insert(
            1,
            vm::Stmt::Expr {
                expr: vm::Expr::String(RADIX_FRAME_OBSERVATION.into()),
                line: 1,
            },
        );
        block.insert(
            2,
            vm::Stmt::Expr {
                expr: heap_guest_call(
                    observe,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Int(4),
                        vm::Expr::Int(0),
                    ],
                ),
                line: 1,
            },
        );
        block.insert(
            3,
            vm::Stmt::Expr {
                expr: heap_guest_call(
                    observe,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Int(3),
                        vm::Expr::Int(0),
                    ],
                ),
                line: 1,
            },
        );
        Ok(ir)
    }
}

thread_local! {
    static RADIX_BACKING: std::cell::RefCell<Option<Value>> =
        const { std::cell::RefCell::new(None) };
}
static RADIX_READ: std::sync::OnceLock<vm::StaticHostFunction> = std::sync::OnceLock::new();

fn radix_observed_read(
    machine: &mut vm::Vm,
    args: &[Value],
) -> vm::VmResult<vm::CallOutcome> {
    RADIX_BACKING.with(|slot| *slot.borrow_mut() = Some(args[0].clone()));
    (RADIX_READ.get().expect("captured arrays::read"))(machine, args)
}

fn radix_observed_read_descriptor() -> vm::HostFunctionDescriptor {
    use vm::HostAdapterDescriptor::*;
    let mut read = pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .descriptors()
        .into_iter()
        .find(|descriptor| descriptor.schema.name == "arrays::read")
        .expect("arrays::read descriptor");
    let (original, wrapper) = match read.adapter {
        Static(f) => (f, Static(radix_observed_read)),
        StaticStack(f) => (f, StaticStack(radix_observed_read)),
        StaticStackRuntimeOwned(f) => (f, StaticStackRuntimeOwned(radix_observed_read)),
        _ => panic!("arrays::read must have a VM-aware static adapter"),
    };
    RADIX_READ.get_or_init(|| original);
    read.adapter = wrapper;
    read
}

#[test]
fn number_to_string_radix_rejection_keeps_root_capacity() {
    static PLUGIN: RadixRootPlugin = RadixRootPlugin;
    type RadixDescriptor = fn() -> vm::HostFunctionDescriptor;
    static RADIX_FUNCTIONS: std::sync::OnceLock<Vec<RadixDescriptor>> = std::sync::OnceLock::new();
    let catalog = heap_catalog();
    let mut arrays = pd_vm_compat_frontends::runtime::array::typescript_array_host_module();
    arrays.functions = RADIX_FUNCTIONS.get_or_init(|| {
        arrays
            .functions
            .iter()
            .map(|&descriptor| {
                if descriptor().schema.name == "arrays::read" {
                    radix_observed_read_descriptor as RadixDescriptor
                } else {
                    descriptor
                }
            })
            .collect()
    });
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&PLUGIN)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "(5).toString(1);",
        vm::SourceFlavor::Plugin(vm::FrontendId::new(RADIX_ROOT_ID)),
        options,
    )
    .expect("radix rejection compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    for module in [
        arrays,
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("production adapters should match the effective catalog");
    }
    // `(5).toString(1)` truncates the radix; the success arm is compiled too.
    for builtin in [
        "math::floor",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
        "math::trunc",
        "math::copysign",
        "bytes::from_utf8",
    ] {
        registry
            .allow_builtin(builtin)
            .unwrap_or_else(|error| panic!("{builtin} must be allow-listable: {error:?}"));
    }
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");

    let error = match machine.run() {
        Err(error) => error,
        Ok(status) => panic!(
            "radix rejection run must abort, got status={status:?} fuel={:?} stack={:?}",
            machine.get_fuel(),
            machine.stack()
        ),
    };
    eprintln!(
        "RADIX_RUN ip={} error={error:?} fuel={:?} frames={:?} stack={:?}",
        machine.ip(),
        machine.get_fuel(),
        machine.execution_frames(),
        machine.stack()
    );
    assert!(
        matches!(&error, vm::VmError::HostError(message) if message == "assertion failed"),
        "{error:?}"
    );
    let stack = machine.stack().to_vec();
    let entry = stack
        .iter()
        .rposition(|value| *value == Value::string(RADIX_FRAME_OBSERVATION))
        .expect("the rejected radix must reach the observation at the frame entry");
    let Value::Int(before) = stack[entry + 1] else {
        panic!("missing entry root capacity: {:?}", stack[entry + 1]);
    };
    let Value::Int(heap_end) = stack[entry + 2] else {
        panic!("missing entry heap end: {:?}", stack[entry + 2]);
    };
    assert!(
        (RADIX_INFLATED_HEAP_END..65_536).contains(&heap_end),
        "the realm must enter the rejection arm just below the ceiling, got {heap_end}"
    );
    let backing = RADIX_BACKING
        .with(|slot| slot.borrow_mut().take())
        .expect("the live realm backing");
    let after = match (RADIX_READ.get().expect("captured arrays::read"))(
        &mut machine,
        &[backing, Value::Int(RADIX_HEADER_ACTIVE_ROOT_CAPACITY)],
    )
    .expect("read the live root capacity header")
    {
        vm::CallOutcome::Return(vm::CallReturn::One(Value::Int(value))) => value,
        vm::CallOutcome::Return(vm::CallReturn::One(Value::Float(value))) => value as i64,
        other => panic!("expected a numeric root capacity, got {other:?}"),
    };
    eprintln!(
        "NUMBER_PROTO_RADIX_ROOT heap_end={heap_end} before={before} after={after} error={error:?}"
    );
    assert_eq!(
        after, before,
        "a fatal in the radix rejection path must not leak active root capacity"
    );
    machine.reset_for_reuse().expect("reset the radix rejection VM");
}

// ---- Cyclic-graph harness (test-only IR instrumentation, labeled) ----
// Object value edges are forged with raw B1 writes into real managed rows
// (tag 5 plus the peer slot+generation), then exercised through the actual
// root helpers and the production collector. Self and mutual garbage must
// be swept with slot recycling and bounded live sets (the marker must
// terminate on cycles); retained cycles must survive with edges intact.

const HEAP_SET_PROTOTYPE_NAME: &str = "__compat_es5_set_prototype";
const HEAP_FORCED_BOOTSTRAP_PROBE: u8 = 6;

struct HeapCyclePlugin {
    /// 0 = self garbage, 1 = mutual retained, 2 = mutual garbage,
    /// 3 = function/environment/parent-environment graph,
    /// 4 = origin baseline probe (ordinary 69-node realm, self-garbage graph),
    /// 5 = origin URI-expanded probe (72-node realm, retained mutual a/b
    /// plus the single retained URI string r).
    /// 6 = forced bootstrap, with counters sampled before any source allocation.
    mode: u8,
}

struct HeapCycleCtx {
    carrier: u16,
    get: u16,
    read: u16,
    write: u16,
    decode: u16,
    checked_base: u16,
    collect: u16,
    observe: u16,
    node_begin: u16,
    root_enter: u16,
    root_write: u16,
}

/// Whole-box copy plus one field stash each; every box field is read at
/// most once per binding, mirroring production receiver discipline.
fn heap_cycle_take(ir: &mut vm::FrontendIr, tag: &str) -> Result<u16, vm::ParseError> {
    let slot = u16::try_from(ir.locals)
        .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
    ir.locals += 1;
    Ok(slot)
}

fn heap_cycle_identity(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    binding: u16,
    tag: &str,
) -> Result<(u16, u16, u16), vm::ParseError> {
    let boxed = heap_cycle_take(ir, tag)?;
    ir.stmts.push(vm::Stmt::Let {
        index: boxed,
        declared_schema: Some(vm::TypeSchema::Named(
            "__CompatEs5Primitive".to_owned(),
            Vec::new(),
        )),
        expr: vm::Expr::Var(binding),
        line: 1,
    });
    let slot = heap_cycle_take(ir, tag)?;
    ir.stmts.push(vm::Stmt::Let {
        index: slot,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.decode,
            vec![heap_as_number(heap_field_get(ctx.get, binding, "ref_slot"))],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(slot),
        line: 1,
    });
    let generation = heap_cycle_take(ir, tag)?;
    ir.stmts.push(vm::Stmt::Let {
        index: generation,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.decode,
            vec![heap_as_number(heap_field_get(
                ctx.get,
                binding,
                "generation",
            ))],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(generation),
        line: 1,
    });
    Ok((boxed, slot, generation))
}

fn heap_cycle_base(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    slot: u16,
    generation: u16,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let base = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: base,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.checked_base,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(slot),
                vm::Expr::Var(generation),
            ],
        ),
        line: 1,
    });
    Ok(base)
}

fn heap_cycle_write(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    addr: vm::Expr,
    payload: vm::Expr,
    tag: &str,
) -> Result<(), vm::ParseError> {
    let ack = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: heap_guest_call(
            ctx.write,
            vec![vm::Expr::Var(carrier), heap_as_number(addr), payload],
        ),
        line: 1,
    });
    Ok(())
}

fn heap_cycle_read_number(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    addr: vm::Expr,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let raw = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: raw,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: heap_guest_call(ctx.read, vec![vm::Expr::Var(carrier), heap_as_number(addr)]),
        line: 1,
    });
    Ok(raw)
}

fn heap_cycle_decode(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    raw: u16,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let out = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(ctx.decode, vec![vm::Expr::Var(raw)]),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(out),
        line: 1,
    });
    Ok(out)
}

fn heap_cycle_observe(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    which: i64,
    slot_arg: i64,
    tag: &str,
) -> Result<(), vm::ParseError> {
    let out = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.observe,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Int(which),
                vm::Expr::Int(slot_arg),
            ],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(out),
        line: 1,
    });
    Ok(())
}

/// Nonallocating typed charge snapshot kept in a local instead of the value
/// stack, so live-graph extent probes never disturb the exact phase windows.
/// This forwards to the same `heap_observe` helper the existing phases use.
fn heap_cycle_cell_snapshot(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    which: i64,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let out = heap_cycle_take(ir, tag)?;
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.observe,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Int(which),
                vm::Expr::Int(0),
            ],
        ),
        line: 1,
    });
    Ok(out)
}

/// Nonallocating read of the occupying raw block size stored immediately
/// before a live node's checked data base (`ROW_DATA_CELLS + raw header`).
/// The value stays in a local; no node, page or frame is reserved.
fn heap_cycle_node_raw_extent(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    base: u16,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let raw = heap_cycle_read_number(
        ir,
        ctx,
        ctx.carrier,
        vm::Expr::Add(
            Box::new(vm::Expr::Var(base)),
            Box::new(vm::Expr::Int(-3)),
        ),
        tag,
    )?;
    let out = heap_cycle_take(ir, tag)?;
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(ctx.decode, vec![vm::Expr::Var(raw)]),
        line: 1,
    });
    Ok(out)
}

/// Per-slot physical trace of the permanent bootstrap graph. Every slot in
/// `0..count` is read through OBSERVE_ROW_GENERATION (14) first, then the
/// checked directory base helper (`__compat_es5_checked_base`) with that real
/// generation, then its occupying raw block size (RAW_SIZE at
/// `base - RAW_HEADER_CELLS`) and node kind (data cell 0) are decoded into
/// typed locals. One nonallocating loop: no node, page, frame or root is
/// created, and nothing is marked, published or collected. Values reach the
/// host only through the ordinary expression stack under a dedicated marker.
fn heap_cycle_slot_trace(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    count: i64,
) -> Result<(), vm::ParseError> {
    let slot = heap_cycle_take(ir, "env-slot-extents")?;
    let generation = heap_cycle_take(ir, "env-slot-extents")?;
    let base = heap_cycle_take(ir, "env-slot-extents")?;
    let raw_number = heap_cycle_take(ir, "env-slot-extents")?;
    let raw_size = heap_cycle_take(ir, "env-slot-extents")?;
    let kind_number = heap_cycle_take(ir, "env-slot-extents")?;
    let kind = heap_cycle_take(ir, "env-slot-extents")?;
    ir.stmts.push(vm::Stmt::Let {
        index: slot,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::While {
        condition: vm::Expr::Lt(
            Box::new(vm::Expr::Var(slot)),
            Box::new(vm::Expr::Int(count)),
        ),
        body: vec![
            vm::Stmt::Let {
                index: generation,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(
                    ctx.observe,
                    vec![
                        vm::Expr::Var(ctx.carrier),
                        vm::Expr::Int(14),
                        vm::Expr::Var(slot),
                    ],
                ),
                line: 1,
            },
            vm::Stmt::Let {
                index: base,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(
                    ctx.checked_base,
                    vec![
                        vm::Expr::Var(ctx.carrier),
                        vm::Expr::Var(slot),
                        vm::Expr::Var(generation),
                    ],
                ),
                line: 1,
            },
            vm::Stmt::Let {
                index: raw_number,
                declared_schema: Some(vm::TypeSchema::Number),
                expr: heap_guest_call(
                    ctx.read,
                    vec![
                        vm::Expr::Var(ctx.carrier),
                        heap_as_number(vm::Expr::Add(
                            Box::new(vm::Expr::Var(base)),
                            Box::new(vm::Expr::Int(-3)),
                        )),
                    ],
                ),
                line: 1,
            },
            vm::Stmt::Let {
                index: raw_size,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(ctx.decode, vec![vm::Expr::Var(raw_number)]),
                line: 1,
            },
            vm::Stmt::Let {
                index: kind_number,
                declared_schema: Some(vm::TypeSchema::Number),
                expr: heap_guest_call(
                    ctx.read,
                    vec![
                        vm::Expr::Var(ctx.carrier),
                        heap_as_number(vm::Expr::Var(base)),
                    ],
                ),
                line: 1,
            },
            vm::Stmt::Let {
                index: kind,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(ctx.decode, vec![vm::Expr::Var(kind_number)]),
                line: 1,
            },
            vm::Stmt::Expr {
                expr: vm::Expr::Var(slot),
                line: 1,
            },
            vm::Stmt::Expr {
                expr: vm::Expr::Var(generation),
                line: 1,
            },
            vm::Stmt::Expr {
                expr: vm::Expr::Var(raw_size),
                line: 1,
            },
            vm::Stmt::Expr {
                expr: vm::Expr::Var(kind),
                line: 1,
            },
            vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: slot,
                expr: vm::Expr::Add(
                    Box::new(vm::Expr::Var(slot)),
                    Box::new(vm::Expr::Int(1)),
                ),
                line: 1,
            },
        ],
        line: 1,
    });
    Ok(())
}

fn heap_cycle_collect(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    tag: &str,
) -> Result<(), vm::ParseError> {
    let ack = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(ctx.collect, vec![vm::Expr::Var(carrier)]),
        line: 1,
    });
    Ok(())
}

// Selected default manifest for these fixture sources (no Math, numeric
// globals, URI or JSON): 5 realm nodes, 9 Object statics, 8 fixed key Strings,
// 9 static name Strings, 3 Object.prototype method functions, 3
// Function.prototype method functions, those six methods' 6 name Strings, the
// empty packed Array prototype, the 3 private primitive prototypes, and
// String.prototype.charCodeAt plus its name String,
// String.prototype.charAt plus its name String,
// String.prototype.codePointAt plus its name String,
// String.prototype.concat plus its name String,
// String.prototype.substring plus its name String,
// String.prototype.slice plus its name String, and
// String.prototype.indexOf plus its name String,
// String.prototype.lastIndexOf plus its name String, and
// String.prototype.toString plus its name String,
// String.prototype.toLowerCase plus its name String, and
// String.prototype.toUpperCase plus its name String:
// 5 + 9 + 8 + 9 + 6 + 6 + 1 + 3 + 22 = 69 (bootstrap_node_count()). The
// bootstrap crosses two 32-allocation intervals, leaving 69 - 64 = 5
// committed allocations at the typed-bootstrap boundary. All 69 are
// permanent-rooted, so the bootstrap collections free none of them and the
// first user node still claims slot 69. The production trigger stays 32; this
// fixture does not reset it. URI-expanded sources add
// uri_calls::manifest(1) = 2 + 1 = 3 nodes (72 total). Default 69 already
// crosses the second interval at node 64; URI does not cross a third
// interval. Every earlier node already has a permanent root, and the stage
// key stays on the stage-8 root. Math, numeric-global and JSON selections
// carry their own manifest extra and are not used by these sources, so this
// default must not be reused there.
const HEAP_INTRINSIC_NODES: i64 = 71;
// Complete fresh node raw extents for that same default manifest, excluding
// system frames, the directory page and the realm prefix. Object-kind extents:
// six nodes with rows 4/2/2/12/1/1 (String prototype holds twelve rows)
// => 6*(3+5) + 7*22 = 202. Function-kind extents: twenty-eight nodes with rows
// 4+11+9+7+1*11 => 28*(3+10) + 7*42 = 658.
// `Function.prototype.apply` is the one prototype method allocated with two
// property-table rows (1/1/1/1/2/1 for the six methods); its arity is the
// independent function field, never a row count. charCodeAt, charAt,
// codePointAt, concat, substring, slice, indexOf, lastIndexOf, toString,
// toLowerCase and toUpperCase are the extra one-row function nodes.
// String extents: the 34 key/name Strings, including toLowerCase and
// toUpperCase at 6 + 11 each, => sum(6 + UTF-16 units) = 513.
// Empty packed Array prototype: raw 3 + Array header 5 = 8.
// 202 + 658 + 513 + 8 = 1381. This is the source-derived node-cell forecast,
// not a measured physical occupancy. The bootstrap collections keep all 69
// nodes, so the existing inventory compares this forecast at the user
// boundary. URI-expanded
// sources add manifest(1) = 56 node cells (22 object + 14 String + 20
// function); no fixture below uses this constant for the URI realm.
const HEAP_INTRINSIC_NODE_CELLS: i64 = 1419;

/// Forge one object value edge: row value cells become (packed OBJECT WEC, 0.0,
/// flag 0, peer slot, peer generation). The peer must be live with a
/// current generation at collection time.
fn heap_cycle_forge_edge(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    base: u16,
    peer_slot: u16,
    peer_gen: u16,
    tag: &str,
) -> Result<(), vm::ParseError> {
    let cell = |offset: i64| {
        vm::Expr::Add(
            Box::new(vm::Expr::Var(base)),
            Box::new(vm::Expr::Int(5 + offset)),
        )
    };
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        cell(2),
        heap_as_number(vm::Expr::Int(16 * 5 + 7)),
        tag,
    )?;
    heap_cycle_write(ir, ctx, carrier, cell(3), vm::Expr::Float(0.0), tag)?;
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        cell(4),
        heap_as_number(vm::Expr::Int(0)),
        tag,
    )?;
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        cell(5),
        heap_as_number(vm::Expr::Var(peer_slot)),
        tag,
    )?;
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        cell(6),
        heap_as_number(vm::Expr::Var(peer_gen)),
        tag,
    )?;
    Ok(())
}

fn heap_cycle_ctx(ir: &vm::FrontendIr) -> Result<HeapCycleCtx, vm::ParseError> {
    Ok(HeapCycleCtx {
        carrier: 0,
        get: vm::BuiltinFunction::Get.call_index(),
        read: heap_helper_index(ir, HEAP_READ_NAME)?,
        write: heap_helper_index(ir, HEAP_WRITE_NAME)?,
        decode: heap_helper_index(ir, HEAP_DECODE_INT_NAME)?,
        checked_base: heap_helper_index(ir, HEAP_CHECKED_BASE_NAME)?,
        collect: heap_helper_index(ir, HEAP_COLLECT_NAME)?,
        observe: heap_helper_index(ir, HEAP_OBSERVE_NAME)?,
        node_begin: heap_helper_index(ir, HEAP_NODE_BEGIN_NAME)?,
        root_enter: heap_helper_index(ir, HEAP_ROOT_ENTER_NAME)?,
        root_write: heap_helper_index(ir, HEAP_ROOT_WRITE_NAME)?,
    })
}

fn heap_cycle_carrier(ir: &vm::FrontendIr) -> Result<u16, vm::ParseError> {
    let make = heap_helper_index(ir, HEAP_MAKE_NAME)?;
    let mut carriers = Vec::new();
    for stmt in &ir.stmts {
        if let vm::Stmt::Let {
            index,
            declared_schema: None,
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == make
            && args.len() == 1
        {
            carriers.push(*index);
        }
    }
    let [carrier] = carriers.as_slice() else {
        return Err(heap_rejection(format!(
            "heap fixture requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    Ok(*carrier)
}

impl vm::SourcePlugin for HeapCyclePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-cycle"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let mut ctx = heap_cycle_ctx(&ir)?;
        let carrier = heap_cycle_carrier(&ir)?;
        ctx.carrier = carrier;
        if self.mode == HEAP_FORCED_BOOTSTRAP_PROBE {
            let sink = heap_cycle_take(&mut ir, "forced bootstrap")?;
            let begin = ir.function_impls.get_mut(&ctx.node_begin).unwrap();
            let carrier = begin.param_slots[0];
            begin.body_stmts.insert(
                0,
                vm::Stmt::Let {
                    index: sink,
                    declared_schema: Some(vm::TypeSchema::Number),
                    expr: heap_guest_call(
                        ctx.write,
                        vec![vm::Expr::Var(carrier), vm::Expr::Float(10.0), vm::Expr::Float(32.0)],
                    ),
                    line: 1,
                },
            );
        }
        // Existing interval-origin subtraction, moved to after the unique
        // typed bootstrap call and before the first user allocation. Permanent
        // nodes stay charged; this does not change the production threshold.
        // Origin probes (modes 4/5) record the raw bootstrap counters before
        // calibration for the RED proof. Mode 3 also reads the raw
        // pre-Environment boundary counters; other modes keep the exact old
        // stack.
        relocate_cycle_interval(&mut ir, &ctx, carrier, self.mode)?;
        match self.mode {
            0 => heap_cycle_self_garbage(&mut ir, &ctx, carrier)?,
            1 => heap_cycle_mutual(&mut ir, &ctx, carrier, true)?,
            3 => heap_environment_cycle(&mut ir, &ctx)?,
            4 => heap_cycle_self_garbage(&mut ir, &ctx, carrier)?,
            5 => {
                heap_cycle_mutual(&mut ir, &ctx, carrier, true)?;
                heap_cycle_uri_string_proof(&mut ir, &ctx, carrier)?;
            }
            HEAP_FORCED_BOOTSTRAP_PROBE => {}
            _ => heap_cycle_mutual(&mut ir, &ctx, carrier, false)?,
        }
        Ok(ir)
    }
}

/// Move the existing interval-origin subtraction to immediately after the
/// unique top-level typed bootstrap call, before the first user allocation.
/// Production collection threshold, charging, and tracing are unchanged: this
/// only restores the fixture origin now that permanent bootstrap nodes are
/// allocated before the old late calibration.
///
/// Mode 3 also takes one read-only counter panel at that exact boundary. The
/// panel is what separates the permanent-node charge from the later source
/// Function/Environment charges: NODE_CELLS there is the complete bootstrap
/// charge, before the script body allocates anything.
fn relocate_cycle_interval(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    mode: u8,
) -> Result<(), vm::ParseError> {
    let probe_origin = matches!(mode, 4 | 5 | HEAP_FORCED_BOOTSTRAP_PROBE);
    let mut indexes = Vec::new();
    for function in &ir.functions {
        if function.name == "__compat_es5_intrinsic_bootstrap" {
            indexes.push(function.index);
        }
    }
    let [bootstrap] = indexes.as_slice() else {
        return Err(heap_rejection(format!(
            "heap fixture requires exactly one bootstrap declaration, found {}",
            indexes.len()
        )));
    };
    let bootstrap = *bootstrap;
    let mut hits = Vec::new();
    for (index, stmt) in ir.stmts.iter().enumerate() {
        if let vm::Stmt::Let {
            declared_schema: Some(_),
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == bootstrap
            && args.len() == 1
        {
            hits.push(index);
        }
    }
    let [at] = hits.as_slice() else {
        return Err(heap_rejection(format!(
            "heap fixture requires exactly one top-level typed bootstrap call, found {}",
            hits.len()
        )));
    };
    let at = *at;
    let early_user = ir.stmts.iter().take(at + 1).any(|stmt| {
        matches!(
            stmt,
            vm::Stmt::Let {
                expr: vm::Expr::Call(target, _, _, _, _),
                ..
            } if *target == ctx.node_begin
        ) || matches!(
            stmt,
            vm::Stmt::Expr {
                expr: vm::Expr::Call(target, _, _, _, _),
                ..
            } if *target == ctx.node_begin
        )
    });
    if early_user {
        return Err(heap_rejection(
            "bootstrap call is not before the first user node allocation".into(),
        ));
    }
    let origin = ir.stmts.len();
    // Mode 3: one read-only counter panel at the raw typed-bootstrap
    // boundary, before the script body allocates source Function f. Every
    // selector is a global header counter (slot ignored) as declared in
    // `src/javascript/heap.rs`: 7 NODE_CELLS, 0 LIVE_NODES, 3 HEAP_END,
    // 2 NEXT_SLOT, 11 FREE_SLOTS, 1 RETIRED_SLOTS, 5 COLLECTIONS and
    // 8 PENDING_NODES. Each value lives in a typed local and is pushed under
    // its own marker, so the exact ids/extents/retained/swept windows keep
    // their shapes; nothing is allocated, rooted, reserved or collected.
    if mode == 3 {
        let mut panel = Vec::new();
        for which in [7i64, 0, 3, 2, 11, 1, 5, 8] {
            panel.push(heap_cycle_cell_snapshot(ir, ctx, which, "env-bootstrap")?);
        }
        for slot in panel {
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(slot),
                line: 1,
            });
        }
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("env-bootstrap".to_owned()),
            line: 1,
        });
        // Same nonallocating boundary, second read-only panel: the real
        // generation and occupied raw extent of every permanent slot, so the
        // 1381-cell nominal manifest can be compared against the physical
        // blocks. The constant is a source-derived forecast; the inventory
        // measures the actual sum.
        heap_cycle_slot_trace(ir, ctx, HEAP_INTRINSIC_NODES)?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("env-slot-extents".to_owned()),
            line: 1,
        });
    }
    // Origin probes record the raw bootstrap counters before calibration at
    // the exact typed-bootstrap boundary. Existing modes skip these observes
    // to keep the exact old stack shape.
    if probe_origin {
        for which in [6i64, 5, 0, 8, 12] {
            heap_cycle_observe(ir, ctx, carrier, which, 0, "origin-before")?;
        }
    }
    // Phase-origin zero: the user interval starts at 0 after the unique typed
    // bootstrap, regardless of how many permanent nodes the bootstrap charged
    // or whether it already collected. No subtraction of HEAP_INTRINSIC_NODES.
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        vm::Expr::Int(10),
        heap_as_number(vm::Expr::Int(0)),
        "bootstrap interval",
    )?;
    if probe_origin {
        heap_cycle_observe(ir, ctx, carrier, 6, 0, "origin-after")?;
    }
    let inserted: Vec<_> = ir.stmts.drain(origin..).collect();
    ir.stmts.splice(at + 1..at + 1, inserted);
    eprintln!(
        "P2_CYCLE_CALIBRATION probe_origin={probe_origin} mode={mode} \
         bootstrap_fn={bootstrap} call_stmt={at}"
    );
    Ok(())
}

/// Self-edge garbage: forge s->s, unroot by replay, collect, observe the
/// freed slot set, replay into the recycled slots, and prove the fresh box
/// still dereferences.
fn heap_cycle_self_garbage(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
) -> Result<(), vm::ParseError> {
    let binding = heap_binding_index(ir, "s")?;
    let (_boxed, slot, generation) = heap_cycle_identity(ir, ctx, binding, "cycle")?;
    let base = heap_cycle_base(ir, ctx, carrier, slot, generation, "cycle")?;
    heap_cycle_forge_edge(ir, ctx, carrier, base, slot, generation, "cycle")?;
    heap_replay_binding(ir, binding, ctx.node_begin, ctx.root_enter, ctx.root_write)?;
    heap_cycle_collect(ir, ctx, carrier, "cycle")?;
    // free slots, next slot, live nodes, collections.
    for which in [11i64, 2, 0, 5] {
        heap_cycle_observe(ir, ctx, carrier, which, 0, "cycle")?;
    }
    heap_replay_binding(ir, binding, ctx.node_begin, ctx.root_enter, ctx.root_write)?;
    // Re-stash the recycled identity: the new box may live in a swapped
    // slot, so the fresh dereference must use current values.
    let (_boxed2, slot2, generation2) = heap_cycle_identity(ir, ctx, binding, "cycle")?;
    for which in [2i64, 0, 11] {
        heap_cycle_observe(ir, ctx, carrier, which, 0, "cycle")?;
    }
    let base2 = heap_cycle_base(ir, ctx, carrier, slot2, generation2, "cycle")?;
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(base2),
        line: 1,
    });
    Ok(())
}

/// Mutual edges: forge a<->b, then either keep both rooted (retained) or
/// unroot both by replay and sweep the cyclic garbage (collected).
fn heap_cycle_mutual(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    retained: bool,
) -> Result<(), vm::ParseError> {
    let a_binding = heap_binding_index(ir, "a")?;
    let b_binding = heap_binding_index(ir, "b")?;
    let (_a_boxed, a_slot, a_gen) = heap_cycle_identity(ir, ctx, a_binding, "cycle")?;
    let (_b_boxed, b_slot, b_gen) = heap_cycle_identity(ir, ctx, b_binding, "cycle")?;
    let a_base = heap_cycle_base(ir, ctx, carrier, a_slot, a_gen, "cycle")?;
    let b_base = heap_cycle_base(ir, ctx, carrier, b_slot, b_gen, "cycle")?;
    heap_cycle_forge_edge(ir, ctx, carrier, a_base, b_slot, b_gen, "cycle")?;
    heap_cycle_forge_edge(ir, ctx, carrier, b_base, a_slot, a_gen, "cycle")?;
    if !retained {
        heap_replay_binding(
            ir,
            a_binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
        heap_replay_binding(
            ir,
            b_binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
    }
    heap_cycle_collect(ir, ctx, carrier, "cycle")?;
    // free slots, next slot, live nodes, collections.
    for which in [11i64, 2, 0, 5] {
        heap_cycle_observe(ir, ctx, carrier, which, 0, "cycle")?;
    }
    if retained {
        // Edge cells must still name the live peers; both bases must still
        // resolve through the production gate.
        for base in [a_base, b_base] {
            for cell in [10i64, 11] {
                let raw = heap_cycle_read_number(
                    ir,
                    ctx,
                    carrier,
                    vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(cell))),
                    "cycle",
                )?;
                heap_cycle_decode(ir, ctx, raw, "cycle")?;
            }
        }
        let ba = heap_cycle_base(ir, ctx, carrier, a_slot, a_gen, "cycle")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(ba),
            line: 1,
        });
        let bb = heap_cycle_base(ir, ctx, carrier, b_slot, b_gen, "cycle")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(bb),
            line: 1,
        });
    } else {
        heap_replay_binding(
            ir,
            a_binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
        heap_replay_binding(
            ir,
            b_binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
        for which in [2i64, 0, 11] {
            heap_cycle_observe(ir, ctx, carrier, which, 0, "cycle")?;
        }
        let (_a_boxed2, a_slot2, a_gen2) = heap_cycle_identity(ir, ctx, a_binding, "cycle")?;
        let (_b_boxed2, b_slot2, b_gen2) = heap_cycle_identity(ir, ctx, b_binding, "cycle")?;
        let ba = heap_cycle_base(ir, ctx, carrier, a_slot2, a_gen2, "cycle")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(ba),
            line: 1,
        });
        let bb = heap_cycle_base(ir, ctx, carrier, b_slot2, b_gen2, "cycle")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(bb),
            line: 1,
        });
    }
    Ok(())
}

/// Mode-5 post-collection proof that the retained URI string `r` is exactly
/// the `encodeURIComponent('A B')` result. Runs strictly after the real
/// user-phase collection on the same VM: the current `ref_slot`/`generation`
/// are re-read from the live binding and validated through the production
/// `checked_base` gate before any payload cell is touched. The boxed value
/// tag must equal `representation::STRING` (4), the managed node kind at
/// `base+0` must equal `heap::KIND_STRING` (2), the length at `base+2` must be
/// 5, and the five UTF-16 units at `base+3..base+7` must read back `A%20B`.
/// Every new observation is an Int appended at the tail (indices 20..30);
/// no allocation, root entry, guest conversion, getter call, or collection
/// happens during inspection.
fn heap_cycle_uri_string_proof(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
) -> Result<(), vm::ParseError> {
    let r_binding = heap_binding_index(ir, "r")?;
    let (_r_boxed, r_slot, r_gen) = heap_cycle_identity(ir, ctx, r_binding, "uri-string")?;
    let r_tag = heap_cycle_take(ir, "uri-string")?;
    ir.stmts.push(vm::Stmt::Let {
        index: r_tag,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.decode,
            vec![heap_as_number(heap_field_get(ctx.get, r_binding, "tag"))],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(r_tag),
        line: 1,
    });
    let r_base = heap_cycle_base(ir, ctx, carrier, r_slot, r_gen, "uri-string")?;
    for offset in [0i64, 2] {
        let raw = heap_cycle_read_number(
            ir,
            ctx,
            carrier,
            vm::Expr::Add(
                Box::new(vm::Expr::Var(r_base)),
                Box::new(vm::Expr::Int(offset)),
            ),
            "uri-string",
        )?;
        heap_cycle_decode(ir, ctx, raw, "uri-string")?;
    }
    for unit in 0i64..5 {
        let raw = heap_cycle_read_number(
            ir,
            ctx,
            carrier,
            vm::Expr::Add(
                Box::new(vm::Expr::Var(r_base)),
                Box::new(vm::Expr::Int(3 + unit)),
            ),
            "uri-string",
        )?;
        heap_cycle_decode(ir, ctx, raw, "uri-string")?;
    }
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(r_base),
        line: 1,
    });
    Ok(())
}

fn heap_cycle_run(source: &str, mode: u8) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapCyclePlugin = Box::leak(Box::new(HeapCyclePlugin { mode }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-cycle")),
        options,
    )
    .expect("cycle compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(if mode == HEAP_FORCED_BOOTSTRAP_PROBE {
        44_000_000
    } else {
        10_000_000
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "cycle program should halt");
            None
        }
        Err(error) => Some(error),
    };
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    (machine.stack().to_vec(), error)
}

fn heap_cycle_ints(stack: &[Value]) -> Vec<i64> {
    stack
        .iter()
        .map(|v| match v {
            Value::Int(n) => *n,
            Value::Float(n) => *n as i64,
            other => panic!("cycle evidence must be Int, got {other:?}"),
        })
        .collect()
}

#[test]
fn p2_heap_forced_gc_during_bootstrap() {
    let (stack, error) = heap_cycle_run("var s = {k: 0};", HEAP_FORCED_BOOTSTRAP_PROBE);
    assert!(error.is_none(), "forced bootstrap must halt, got {error:?}");
    let counters = heap_cycle_ints(&stack[..4]);
    let (since, collections, live, pending) = (counters[0], counters[1], counters[2], counters[3]);
    eprintln!(
        "P2_FORCED_BOOTSTRAP since={since} collections={collections} live={live} pending={pending}"
    );
    assert!(live > 32, "the full realm must be initialized");
    assert_eq!(
        (since, collections, pending),
        (1, live, 0),
        "each real bootstrap allocation must collect before publishing its live node"
    );
}

#[test]
fn p2_heap_cycle_self_garbage_is_swept() {
    // User slots start after the permanent manifest: key at
    // HEAP_INTRINSIC_NODES, obj at +1, replay key/obj at +2/+3. The self-edge
    // garbage (first pair) must be swept (free 2, next stays +4, live drops to
    // the replay pair); the recycled slots back the fresh box afterwards.
    let (stack, error) = heap_cycle_run("var s = {k: 0};", 0);
    assert!(
        error.is_none(),
        "self-cycle collection must halt, got {error:?}"
    );
    eprintln!("P2_CYCLE_SELF stack={stack:?}");
    let v = heap_cycle_ints(&stack);
    assert_eq!(v.len(), 12, "self-cycle evidence shape, got {v:?}");
    let (old_slot, old_gen, free, next, live, coll, new_slot, new_gen, next2, live2, free2, base) = (
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8], v[9], v[10], v[11],
    );
    assert_eq!(
        (free, next, live, coll),
        (2, HEAP_INTRINSIC_NODES + 4, HEAP_INTRINSIC_NODES + 2, 3),
        "self garbage must be swept after the two bootstrap collections, got {v:?}"
    );
    assert_eq!(
        (next2, live2, free2),
        (HEAP_INTRINSIC_NODES + 4, HEAP_INTRINSIC_NODES + 4, 0),
        "recycled slots must back the fresh nodes, got {v:?}"
    );
    assert!(
        (HEAP_INTRINSIC_NODES..HEAP_INTRINSIC_NODES + 2).contains(&new_slot)
            && (HEAP_INTRINSIC_NODES..HEAP_INTRINSIC_NODES + 2).contains(&old_slot),
        "cycle slots stay in the low directory, got {v:?}"
    );
    assert!(
        base >= 80,
        "fresh base must clear the fixed prefix, got {base}"
    );
    assert!(
        new_gen >= 1 && old_gen >= 1,
        "generations stay live, got {v:?}"
    );
}

#[test]
fn p2_heap_cycle_mutual_retained_survives() {
    // User slots start after the permanent manifest: a-key at
    // HEAP_INTRINSIC_NODES, a-obj at +1, b-key at +2, b-obj at +3. Nothing is
    // unrooted: the collection must free nothing and both forged edges must
    // still name the live peers afterwards.
    let (stack, error) = heap_cycle_run("var a = {k: 0}; var b = {k: 0};", 1);
    assert!(
        error.is_none(),
        "retained-cycle collection must halt, got {error:?}"
    );
    eprintln!("P2_CYCLE_RETAINED stack={stack:?}");
    let v = heap_cycle_ints(&stack);
    assert_eq!(v.len(), 14, "retained-cycle evidence shape, got {v:?}");
    let (a_slot, a_gen, b_slot, b_gen, free, next, live, coll) =
        (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]);
    assert_eq!(
        (free, next, live, coll),
        (0, HEAP_INTRINSIC_NODES + 4, HEAP_INTRINSIC_NODES + 4, 3),
        "retained graph must survive intact, got {v:?}"
    );
    assert_eq!(
        (v[8], v[9]),
        (b_slot, b_gen),
        "a must still edge to live b, got {v:?}"
    );
    assert_eq!(
        (v[10], v[11]),
        (a_slot, a_gen),
        "b must still edge to live a, got {v:?}"
    );
    assert!(
        v[12] >= 80 && v[13] >= 80,
        "both bases must still resolve, got {v:?}"
    );
}

#[test]
fn p2_heap_cycle_mutual_garbage_is_swept() {
    // Both old rows unrooted by replay: the collection must free all four
    // original slots (free 4, next stays 8, live drops to the replay set);
    // the second replay round recycles without fresh claims.
    let (stack, error) = heap_cycle_run("var a = {k: 0}; var b = {k: 0};", 2);
    assert!(
        error.is_none(),
        "mutual-garbage collection must halt, got {error:?}"
    );
    eprintln!("P2_CYCLE_GARBAGE stack={stack:?}");
    let v = heap_cycle_ints(&stack);
    assert_eq!(v.len(), 17, "mutual-garbage evidence shape, got {v:?}");
    let (free, next, live, coll) = (v[4], v[5], v[6], v[7]);
    assert_eq!(
        (free, next, live, coll),
        (4, HEAP_INTRINSIC_NODES + 8, HEAP_INTRINSIC_NODES + 4, 3),
        "mutual garbage must be swept, got {v:?}"
    );
    let (next2, live2, free2) = (v[8], v[9], v[10]);
    assert_eq!(
        (next2, free2),
        (HEAP_INTRINSIC_NODES + 8, 0),
        "recycled slots must back the fresh nodes, got {v:?}"
    );
    assert_eq!(
        live2,
        HEAP_INTRINSIC_NODES + 8,
        "both generations stay charged until the next sweep, got {v:?}"
    );
    assert!(
        v[15] >= 80 && v[16] >= 80,
        "fresh bases must resolve, got {v:?}"
    );
}

// ---- Origin-interval probes (test-only, same plugin/calibration) ----

fn heap_origin_bootstrap_counters(source: &str) -> [i64; 4] {
    use vm::SourcePlugin;

    let catalog = heap_catalog();
    let ir = JAVASCRIPT_ES5_PLUGIN
        .parse_source(
            source,
            &vm::SourceFrontendContext {
                source_id: 0,
                original_path: None,
                phase: vm::compiler::ParsePhase::Standalone,
                host_api_catalog: Some(&catalog),
                imports: &[],
            },
        )
        .expect("origin source must lower through the production plugin");
    let bootstrap = heap_helper_index(&ir, "__compat_es5_intrinsic_bootstrap")
        .expect("typed bootstrap helper");
    let allocate = heap_helper_index(&ir, "__compat_es5_object_allocate")
        .expect("intrinsic object and function allocator");
    let begin = heap_helper_index(&ir, HEAP_NODE_BEGIN_NAME).expect("node reservation helper");
    let count_allocations = |function, targets: &[u16]| {
        ir.function_impls[&function]
            .body_stmts
            .iter()
            .filter(|stmt| {
                matches!(stmt, vm::Stmt::Assign { expr: vm::Expr::Call(target, ..), .. }
                    if targets.contains(target))
            })
            .count() as i64
    };
    // The typed bootstrap emits a straight-line ledger for the selected realm.
    // Strings and the packed Array prototype reserve directly; each Object or
    // Function shell delegates one reservation to the allocator. All these
    // nodes stay rooted. Count allocation sites, not measured heap counters.
    assert_eq!(count_allocations(allocate, &[begin]), 1);
    let nodes = count_allocations(bootstrap, &[begin, allocate]);
    assert!(nodes > 0, "typed bootstrap must allocate permanent nodes");
    // Collection is checked before the next allocation (33rd, 65th, ...),
    // not after committing the 32nd. The final commit remains in the residue.
    const INTERVAL: i64 = 32;
    let collections = (nodes - 1) / INTERVAL;
    [nodes - collections * INTERVAL, collections, nodes, 0]
}

#[test]
fn p2_heap_cycle_origin_baseline_proves_zero_interval() {
    let source = "var s = {k: 0};";
    let expected = heap_origin_bootstrap_counters(source);
    let [_, bootstrap_collections, bootstrap_nodes, _] = expected;
    let (stack, error) = heap_cycle_run(source, 4);
    assert!(
        error.is_none(),
        "baseline origin probe must halt, got {error:?}"
    );
    eprintln!("P2_CYCLE_ORIGIN_BASELINE stack={stack:?}");
    let v = heap_cycle_ints(&stack);
    assert_eq!(v.len(), 18, "baseline origin evidence shape, got {v:?}");
    let (since_before, coll_before, live_before, pending_before, _roots_before, since_after) =
        (v[0], v[1], v[2], v[3], v[4], v[5]);
    eprintln!(
        "P2_CYCLE_ORIGIN_BASELINE before_since={since_before} before_coll={coll_before} before_live={live_before} pending={pending_before} after_since={since_after}"
    );
    assert_eq!(
        [since_before, coll_before, live_before, pending_before],
        expected,
        "bootstrap counters must match the node ledger, got {v:?}"
    );
    assert_eq!(
        since_after, 0,
        "baseline interval origin must be zero, got {v:?}"
    );
    // User phase reuses the exact self-garbage assertions, shifted by the six
    // origin observations.
    let (free, next, live, coll) = (v[8], v[9], v[10], v[11]);
    assert_eq!(
        (free, next, live, coll),
        (2, bootstrap_nodes + 4, bootstrap_nodes + 2, bootstrap_collections + 1),
        "baseline user garbage must be swept, got {v:?}"
    );
    let (next2, live2, free2) = (v[14], v[15], v[16]);
    assert_eq!(
        (next2, live2, free2),
        (bootstrap_nodes + 4, bootstrap_nodes + 4, 0),
        "baseline recycled slots must back the fresh nodes, got {v:?}"
    );
    assert!(
        (bootstrap_nodes..bootstrap_nodes + 2).contains(&v[6])
            && (bootstrap_nodes..bootstrap_nodes + 2).contains(&v[12]),
        "baseline cycle slots stay in the low directory, got {v:?}"
    );
    assert!(
        v[17] >= 80,
        "baseline fresh base must clear prefix, got {v:?}"
    );
}

#[test]
fn p2_heap_cycle_origin_uri_expanded_requires_zero_interval() {
    // The retained mutual graph and URI string are read back after the sweep.
    let source = "var a={k:0};var b={k:0};var r=encodeURIComponent('A B');";
    let expected = heap_origin_bootstrap_counters(source);
    let [_, bootstrap_collections, bootstrap_nodes, _] = expected;
    let (stack, error) = heap_cycle_run(source, 5);
    // RED proof must preserve raw counters even when the VM does not halt:
    // print stack/error before any halting assertion.
    eprintln!("P2_CYCLE_ORIGIN_URI stack={stack:?} error={error:?}");
    // Check typed counters even if the later user phase does not halt. The
    // origin write must still set only the interval counter to zero.
    assert_eq!(
        stack.get(..4),
        Some(expected.map(Value::Int).as_slice()),
        "URI bootstrap counters must match the node ledger, got {stack:?}"
    );
    assert_eq!(
        stack[5].clone(),
        vm::Value::Int(0),
        "URI interval origin must be zero, got {stack:?}"
    );
    assert!(
        error.is_none(),
        "URI-expanded origin probe must halt, got {error:?} stack={stack:?}"
    );
    let v = heap_cycle_ints(&stack);
    eprintln!("P2_CYCLE_ORIGIN_URI raw_len={} raw={v:?}", v.len());
    assert_eq!(
        v.len(),
        31,
        "URI origin evidence shape (20 prefix plus 11 r-proof tail), got {v:?}"
    );
    let (since_before, coll_before, live_before, pending_before, _roots_before, since_after) =
        (v[0], v[1], v[2], v[3], v[4], v[5]);
    eprintln!(
        "P2_CYCLE_ORIGIN_URI before_since={since_before} before_coll={coll_before} before_live={live_before} pending={pending_before} after_since={since_after}"
    );
    // The user phase reserves six nodes: two key/object pairs and two
    // strings. One transient literal is reclaimed by exactly one collection.
    let (a_slot, a_gen, b_slot, b_gen, free, next, live, coll) =
        (v[6], v[7], v[8], v[9], v[10], v[11], v[12], v[13]);
    assert_eq!(
        (free, next, live, coll),
        (1, bootstrap_nodes + 6, bootstrap_nodes + 5, bootstrap_collections + 1),
        "URI retained graph must survive with one transient reclaimed, got {v:?}"
    );
    assert_eq!(
        coll - coll_before,
        1,
        "URI user phase must collect exactly once, got {v:?}"
    );
    assert_eq!(
        (v[14], v[15]),
        (b_slot, b_gen),
        "a must still edge to live b, got {v:?}"
    );
    assert_eq!(
        (v[16], v[17]),
        (a_slot, a_gen),
        "b must still edge to live a, got {v:?}"
    );
    assert!(
        v[18] >= 80 && v[19] >= 80,
        "both bases must still resolve, got {v:?}"
    );
    // Post-collection proof that the retained `r` is exactly 'A%20B': the
    // current slot/gen re-read from the live binding (20, 21), the boxed
    // value tag `representation::STRING` = 4 (22), the managed node kind
    // `heap::KIND_STRING` = 2 at base+0 (23), the length 5 at base+2 (24),
    // and the five UTF-16 units `A`, `%`, `2`, `0`, `B` at base+3..base+7
    // (25..29); the checked base itself must still resolve past the fixed
    // prefix (30). Indices 0..20 above are unchanged.
    let (r_slot, r_gen, r_tag, r_kind, r_len) = (v[20], v[21], v[22], v[23], v[24]);
    eprintln!(
        "P2_CYCLE_ORIGIN_URI_R slot={r_slot} gen={r_gen} tag={r_tag} kind={r_kind} len={r_len} units={:?} base={}",
        &v[25..30],
        v[30]
    );
    assert!(
        r_gen >= 1,
        "retained r generation must stay live, got {v:?}"
    );
    assert_eq!(
        r_tag, 4,
        "retained r boxed tag must be representation::STRING, got {v:?}"
    );
    assert_eq!(
        r_kind, 2,
        "retained r node kind must be heap::KIND_STRING, got {v:?}"
    );
    assert_eq!(r_len, 5, "retained r length must be 5 ('A%20B'), got {v:?}");
    assert_eq!(
        (v[25], v[26], v[27], v[28], v[29]),
        (0x41, 0x25, 0x32, 0x30, 0x42),
        "retained r units must read back 'A%20B', got {v:?}"
    );
    assert!(
        v[30] >= 80,
        "retained r base must clear the fixed prefix, got {v:?}"
    );
}

// ---- Prototype-link harness (test-only IR instrumentation, labeled) ----
// The production `__compat_es5_set_prototype` helper returns 1 on success
// and 0 on a refused cycle, leaving the old pair unchanged. One halted run
// covers a valid link, a refused self cycle, and a refused indirect cycle,
// with the stored pairs read back through the production checked gate.

struct HeapProtoPlugin;

fn heap_proto_stash(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    binding: u16,
    field: &str,
) -> Result<u16, vm::ParseError> {
    let out = heap_cycle_take(ir, "proto")?;
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.decode,
            vec![heap_as_number(heap_field_get(ctx.get, binding, field))],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(out),
        line: 1,
    });
    Ok(out)
}

fn heap_proto_link(
    ir: &mut vm::FrontendIr,
    set_prototype: u16,
    carrier: u16,
    obj_box: u16,
    proto_box: u16,
) -> Result<(), vm::ParseError> {
    let verdict = heap_cycle_take(ir, "proto")?;
    ir.stmts.push(vm::Stmt::Let {
        index: verdict,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            set_prototype,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(obj_box),
                vm::Expr::Var(proto_box),
            ],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(verdict),
        line: 1,
    });
    Ok(())
}

fn heap_proto_pair(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    slot: u16,
    generation: u16,
) -> Result<(), vm::ParseError> {
    let base = heap_cycle_base(ir, ctx, carrier, slot, generation, "proto")?;
    for cell in [3i64, 4] {
        let raw = heap_cycle_read_number(
            ir,
            ctx,
            carrier,
            vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(cell))),
            "proto",
        )?;
        heap_cycle_decode(ir, ctx, raw, "proto")?;
    }
    Ok(())
}

impl vm::SourcePlugin for HeapProtoPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-proto"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let mut ctx = heap_cycle_ctx(&ir)?;
        let carrier = heap_cycle_carrier(&ir)?;
        ctx.carrier = carrier;
        let set_prototype = heap_helper_index(&ir, HEAP_SET_PROTOTYPE_NAME)?;
        // Whole-box copies first; field reads follow per binding.
        let mut boxes = std::collections::HashMap::new();
        let mut bindings = std::collections::HashMap::new();
        for name in ["p", "o", "a", "b"] {
            let binding = heap_binding_index(&ir, name)?;
            let boxed = heap_cycle_take(&mut ir, "proto")?;
            ir.stmts.push(vm::Stmt::Let {
                index: boxed,
                declared_schema: Some(vm::TypeSchema::Named(
                    "__CompatEs5Primitive".to_owned(),
                    Vec::new(),
                )),
                expr: vm::Expr::Var(binding),
                line: 1,
            });
            bindings.insert(name, binding);
            boxes.insert(name, boxed);
        }
        // Identity stashes (each field read once), pushed as evidence.
        let mut identities = std::collections::HashMap::new();
        for name in ["p", "o", "a", "b"] {
            let binding = bindings[name];
            let slot = heap_proto_stash(&mut ir, &ctx, binding, "ref_slot")?;
            let generation = heap_proto_stash(&mut ir, &ctx, binding, "generation")?;
            identities.insert(name, (slot, generation));
        }
        heap_proto_link(&mut ir, set_prototype, carrier, boxes["o"], boxes["p"])?;
        let (o_slot, o_gen) = identities["o"];
        heap_proto_pair(&mut ir, &ctx, carrier, o_slot, o_gen)?;
        heap_proto_link(&mut ir, set_prototype, carrier, boxes["o"], boxes["o"])?;
        heap_proto_pair(&mut ir, &ctx, carrier, o_slot, o_gen)?;
        heap_proto_link(&mut ir, set_prototype, carrier, boxes["a"], boxes["b"])?;
        heap_proto_link(&mut ir, set_prototype, carrier, boxes["b"], boxes["a"])?;
        let (b_slot, b_gen) = identities["b"];
        heap_proto_pair(&mut ir, &ctx, carrier, b_slot, b_gen)?;
        let (a_slot, a_gen) = identities["a"];
        heap_proto_pair(&mut ir, &ctx, carrier, a_slot, a_gen)?;
        Ok(ir)
    }
}

#[test]
fn p2_heap_prototype_links_and_cycle_refusals() {
    let plugin: &'static HeapProtoPlugin = Box::leak(Box::new(HeapProtoPlugin));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var p = {}; var o = {}; var a = {}; var b = {};",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-proto")),
        options,
    )
    .expect("proto compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(10_000_000);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    match machine.run() {
        Ok(status) => assert_eq!(status, VmStatus::Halted, "proto program should halt"),
        Err(error) => panic!(
            "PROTO RUN-FAIL ip={} error={error:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        ),
    }
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let stack = machine.stack().to_vec();
    eprintln!("P2_PROTO stack={stack:?}");
    // identities p,o,a,b (8) then: r1, o-pair(2), r2, o-pair(2), r3, r4,
    // b-pair(2), a-pair(2): 8 + 12 = 20 trailing observations.
    assert!(stack.len() >= 20, "proto evidence shape, got {stack:?}");
    let tail = &stack[stack.len() - 20..];
    let counter = |v: &Value| match v {
        Value::Int(n) => *n,
        Value::Float(n) => *n as i64,
        other => panic!("proto evidence must be Int, got {other:?}"),
    };
    let v: Vec<i64> = tail.iter().map(counter).collect();
    let (p_slot, p_gen, o_slot, o_gen, a_slot, a_gen, b_slot, b_gen) =
        (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]);
    assert_eq!(v[8], 1, "valid prototype link must succeed, got {v:?}");
    assert_eq!(
        (v[9], v[10]),
        (p_slot, p_gen),
        "o must link to p, got {v:?}"
    );
    assert_eq!(v[11], 0, "self prototype cycle must refuse, got {v:?}");
    assert_eq!(
        (v[12], v[13]),
        (p_slot, p_gen),
        "refused self link must leave o unchanged, got {v:?}"
    );
    assert_eq!(v[14], 1, "valid a->b link must succeed, got {v:?}");
    assert_eq!(v[15], 0, "indirect prototype cycle must refuse, got {v:?}");
    assert_eq!(
        (v[16], v[17]),
        (0, 1), // the original Object.prototype pair
        "refused indirect link must leave b unchanged, got {v:?}"
    );
    assert_eq!(
        (v[18], v[19]),
        (b_slot, b_gen),
        "a->b link must stay intact, got {v:?}"
    );
    let _ = (o_slot, o_gen, a_slot, a_gen);
}

// Private IR interruption fixture, not source loop syntax: reserve a real
// unpublished array node, observe its charged state, then fail before any
// initialization/publication can expose it as a JS value.
struct HeapPendingPlugin {
    collect_pending: bool,
}

impl vm::SourcePlugin for HeapPendingPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-pending"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let mut ctx = heap_cycle_ctx(&ir)?;
        ctx.carrier = heap_cycle_carrier(&ir)?;
        for which in [0, 7, 8, 3] {
            heap_cycle_observe(&mut ir, &ctx, ctx.carrier, which, 0, "pending")?;
        }
        let allocation = heap_cycle_take(&mut ir, "pending")?;
        ir.stmts.push(vm::Stmt::Let {
            index: allocation,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Allocation".to_owned(),
                Vec::new(),
            )),
            expr: heap_guest_call(
                ctx.node_begin,
                vec![
                    vm::Expr::Var(ctx.carrier),
                    vm::Expr::Int(6),
                    vm::Expr::Int(1),
                ],
            ),
            line: 1,
        });
        for which in [0, 7, 8, 3] {
            heap_cycle_observe(&mut ir, &ctx, ctx.carrier, which, 0, "pending")?;
        }
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("pending-window-ready".to_owned()),
            line: 1,
        });
        if self.collect_pending {
            heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "pending")?;
        } else {
            ir.stmts.push(vm::Stmt::While {
                condition: vm::Expr::Bool(true),
                body: Vec::new(),
                line: 1,
            });
        }
        Ok(ir)
    }
}

fn heap_assert_pending_failure_reset(collect_pending: bool) {
    let plugin: &'static HeapPendingPlugin =
        Box::leak(Box::new(HeapPendingPlugin { collect_pending }));
    let catalog = heap_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var a=[1];a[0];",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-pending")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .expect("pending fixture must compile the real source and helpers");
    let mut machine = compilation.into_vm().expect("source-certified pending VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("initialize actual owned arrays");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("bind production adapters");
    machine.set_fuel(1_000_000);
    let outcome = machine.run();
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    if collect_pending {
        assert!(
            matches!(&outcome, Err(vm::VmError::HostError(message)) if message.contains("assertion failed")),
            "collect must refuse pending publication, got {outcome:?}"
        );
    } else {
        // This SDK translates its fuel interrupt into a Fuel yield. The
        // embedding aborts here and resets; it never resumes this opaque
        // partially initialized pending node as normal source execution.
        assert!(
            matches!(&outcome, Ok(VmStatus::Yielded)),
            "actual VM fuel must interrupt the pending window, got {outcome:?}"
        );
        assert_eq!(machine.last_yield_reason(), Some(vm::VmYieldReason::Fuel));
    }
    let phase = machine
        .stack()
        .iter()
        .position(|value| *value == Value::string("pending-window-ready"))
        .expect("failure must occur after the real pending allocation and observations");
    assert!(phase >= 8);
    let counters = heap_stale_ints(&machine.stack()[phase - 8..phase]);
    assert_eq!(counters.len(), 8);
    assert_eq!(
        (counters[0], counters[2]),
        (1, 0),
        "one published source array, no pending node"
    );
    assert_eq!(
        (counters[4], counters[6]),
        (1, 1),
        "pending is charged but not published"
    );
    assert_eq!(
        counters[5],
        counters[1] + 3 + 6,
        "full raw node extent remains charged"
    );
    assert!(
        counters[7] >= counters[3],
        "backing cannot disappear on fatal interruption"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("B1 directory remains until reset");
    assert_eq!(
        (stats.pending, stats.registered),
        (0, 1),
        "logical pending differs from B1 owner pending"
    );
    assert!(stats.reserved_bytes > 0);
    assert!(
        machine
            .capture_certification_stats()
            .expect("source facts")
            .valid
    );
    eprintln!(
        "P2_PENDING collect={collect_pending} outcome={outcome:?} reason={:?} counters={counters:?} bytes={}",
        machine.last_yield_reason(),
        stats.reserved_bytes
    );
    machine.reset_for_reuse().expect("reset fatal pending VM");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    assert!(
        !machine
            .capture_certification_stats()
            .expect("revoked source facts")
            .valid
    );
}

#[test]
fn p2_heap_pending_fuel_exhaustion_remains_charged_until_reset() {
    heap_assert_pending_failure_reset(false);
}

#[test]
fn p2_heap_pending_collect_refuses_until_reset() {
    heap_assert_pending_failure_reset(true);
}

struct HeapEnvironmentNode {
    slot: u16,
    generation: u16,
    base: u16,
}

// Internal graph fixture only: these are real kind-5 managed nodes, not
// source closures or an implementation of source EnvironmentRecords.
fn heap_environment_node(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    frame: u16,
    entry: i64,
    values: Vec<[vm::Expr; 5]>,
) -> Result<HeapEnvironmentNode, vm::ParseError> {
    let allocation = heap_cycle_take(ir, "environment")?;
    ir.stmts.push(vm::Stmt::Let {
        index: allocation,
        declared_schema: Some(vm::TypeSchema::Named(
            "__CompatEs5Allocation".to_owned(),
            Vec::new(),
        )),
        expr: heap_guest_call(
            ctx.node_begin,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Int(5 + 5 * values.len() as i64),
                vm::Expr::Int(5),
            ],
        ),
        line: 1,
    });
    let mut fields = Vec::new();
    for name in ["slot", "generation", "base"] {
        let field = heap_cycle_take(ir, "environment")?;
        ir.stmts.push(vm::Stmt::Let {
            index: field,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_field_get(ctx.get, allocation, name),
            line: 1,
        });
        fields.push(field);
    }
    let node = HeapEnvironmentNode {
        slot: fields[0],
        generation: fields[1],
        base: fields[2],
    };
    let address = |offset| {
        vm::Expr::Add(
            Box::new(vm::Expr::Var(node.base)),
            Box::new(vm::Expr::Int(offset)),
        )
    };
    for (offset, value) in [(2, values.len() as i64), (3, 0), (4, 0)] {
        heap_cycle_write(
            ir,
            ctx,
            ctx.carrier,
            address(offset),
            heap_as_number(vm::Expr::Int(value)),
            "environment",
        )?;
    }
    for (row, value) in values.into_iter().enumerate() {
        for (cell, scalar) in value.into_iter().enumerate() {
            heap_cycle_write(
                ir,
                ctx,
                ctx.carrier,
                address(5 + 5 * row as i64 + cell as i64),
                heap_as_number(scalar),
                "environment",
            )?;
        }
    }
    let publish = heap_helper_index(ir, "__compat_es5_node_publish_edge")?;
    let ack = heap_cycle_take(ir, "environment")?;
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            publish,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Var(node.slot),
                vm::Expr::Var(node.generation),
                vm::Expr::Var(node.base),
                vm::Expr::Var(frame),
                vm::Expr::Int(entry),
                vm::Expr::Int(6),
            ],
        ),
        line: 1,
    });
    Ok(node)
}

fn heap_environment_cycle(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
) -> Result<(), vm::ParseError> {
    let function = heap_binding_index(ir, "f")?;
    let (_, fslot, fgen) = heap_cycle_identity(ir, ctx, function, "environment")?;
    let fbase = heap_cycle_base(ir, ctx, ctx.carrier, fslot, fgen, "environment")?;
    // Nonallocating boundary extents, all kept in locals: the typed
    // node-cell charge before this fixture's graph exists (permanent
    // bootstrap plus source Function f), the heap top, and f's own occupied
    // raw block. The allocator may charge a reused unsplittable tail, so the
    // requested extent and the occupying extent are separate facts.
    let cells_before_graph = heap_cycle_cell_snapshot(ir, ctx, 7, "environment-extent")?;
    let heap_end_before_graph = heap_cycle_cell_snapshot(ir, ctx, 3, "environment-extent")?;
    let f_extent = heap_cycle_node_raw_extent(ir, ctx, fbase, "environment-extent")?;
    let frame = heap_cycle_take(ir, "environment")?;
    ir.stmts.push(vm::Stmt::Let {
        index: frame,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.root_enter,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(2)],
        ),
        line: 1,
    });
    let child = heap_environment_node(ir, ctx, frame, 0, Vec::new())?;
    let cells_after_child = heap_cycle_cell_snapshot(ir, ctx, 7, "environment-extent")?;
    let parent = heap_environment_node(
        ir,
        ctx,
        frame,
        1,
        vec![
            [
                vm::Expr::Int(5),
                vm::Expr::Float(0.0),
                vm::Expr::Int(0),
                vm::Expr::Var(fslot),
                vm::Expr::Var(fgen),
            ],
            [
                vm::Expr::Int(3),
                vm::Expr::Float(7.0),
                vm::Expr::Int(0),
                vm::Expr::Int(0),
                vm::Expr::Int(0),
            ],
        ],
    )?;
    let cells_after_parent = heap_cycle_cell_snapshot(ir, ctx, 7, "environment-extent")?;
    // f -> child.env -> parent.env -> f, plus a primitive entry with no edge.
    for (base, offset, value) in [
        (fbase, 6, child.slot),
        (fbase, 7, child.generation),
        (child.base, 3, parent.slot),
        (child.base, 4, parent.generation),
    ] {
        heap_cycle_write(
            ir,
            ctx,
            ctx.carrier,
            vm::Expr::Add(
                Box::new(vm::Expr::Var(base)),
                Box::new(vm::Expr::Int(offset)),
            ),
            heap_as_number(vm::Expr::Var(value)),
            "environment",
        )?;
    }
    // Drop every direct environment root: only the source function keeps
    // this graph alive, so the following collect proves transitive tracing.
    let leave = heap_helper_index(ir, "__compat_es5_root_leave")?;
    let ack = heap_cycle_take(ir, "environment")?;
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            leave,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Var(frame)],
        ),
        line: 1,
    });
    for slot in [
        fslot,
        fgen,
        child.slot,
        child.generation,
        parent.slot,
        parent.generation,
    ] {
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(slot),
            line: 1,
        });
    }
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::String("env-identities".to_owned()),
        line: 1,
    });
    // Boundary extents for the typed bootstrap and the two source-node
    // reservations. `env-extents` is a separate phase so the existing
    // ids/retained/swept windows keep their exact shapes.
    for slot in [
        cells_before_graph,
        f_extent,
        cells_after_child,
        cells_after_parent,
        heap_end_before_graph,
    ] {
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(slot),
            line: 1,
        });
    }
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::String("env-extents".to_owned()),
        line: 1,
    });
    heap_cycle_collect(ir, ctx, ctx.carrier, "environment")?;
    for which in [0, 7, 11, 8] {
        heap_cycle_observe(ir, ctx, ctx.carrier, which, 0, "environment")?;
    }
    for (base, offsets) in [
        (fbase, vec![6, 7, 5]),
        (child.base, vec![3, 4]),
        (parent.base, vec![8, 9, 11]),
    ] {
        for offset in offsets {
            let raw = heap_cycle_read_number(
                ir,
                ctx,
                ctx.carrier,
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(base)),
                    Box::new(vm::Expr::Int(offset)),
                ),
                "environment",
            )?;
            heap_cycle_decode(ir, ctx, raw, "environment")?;
        }
    }
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::String("env-retained".to_owned()),
        line: 1,
    });
    heap_replay_binding(ir, function, ctx.node_begin, ctx.root_enter, ctx.root_write)?;
    heap_cycle_collect(ir, ctx, ctx.carrier, "environment")?;
    for which in [0, 11, 7] {
        heap_cycle_observe(ir, ctx, ctx.carrier, which, 0, "environment")?;
    }
    for slot in [fslot, child.slot, parent.slot] {
        let state = heap_cycle_take(ir, "environment")?;
        ir.stmts.push(vm::Stmt::Let {
            index: state,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                ctx.observe,
                vec![
                    vm::Expr::Var(ctx.carrier),
                    vm::Expr::Int(13),
                    vm::Expr::Var(slot),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(state),
            line: 1,
        });
    }
    let (_, fresh_slot, fresh_gen) = heap_cycle_identity(ir, ctx, function, "environment")?;
    let fresh_base = heap_cycle_base(ir, ctx, ctx.carrier, fresh_slot, fresh_gen, "environment")?;
    let code = heap_cycle_read_number(
        ir,
        ctx,
        ctx.carrier,
        vm::Expr::Add(
            Box::new(vm::Expr::Var(fresh_base)),
            Box::new(vm::Expr::Int(5)),
        ),
        "environment",
    )?;
    heap_cycle_decode(ir, ctx, code, "environment")?;
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::String("env-swept".to_owned()),
        line: 1,
    });
    Ok(())
}

#[test]
fn p2_heap_function_environment_parent_cycle_traces_and_sweeps() {
    let (stack, error) = heap_cycle_run("var f=function(){\"use strict\";return 7;};", 3);
    // Full pre-assertion host observation: the parent must see every pushed
    // value and the VM error before any exact raw-cell expectation fires.
    eprintln!("P2_ENV_RAW error={error:?} stack={stack:?}");
    assert!(
        error.is_none(),
        "real environment helpers must complete, got {error:?}"
    );
    let phase = |marker: &str, count: usize| {
        let end = stack
            .iter()
            .position(|value| *value == Value::string(marker))
            .expect("environment phase reached");
        assert!(end >= count);
        heap_stale_ints(&stack[end - count..end])
    };
    let ids = phase("env-identities", 6);
    let kept = phase("env-retained", 12);
    let swept = phase("env-swept", 9);
    eprintln!("P2_ENV_GRAPH ids={ids:?} retained={kept:?} swept={swept:?}");
    // which 7 is OBSERVE_NODE_CELLS (header offset HDR_NODE_CELLS: the typed
    // node-cell charge); which 3 is OBSERVE_HEAP_END (header offset
    // HDR_HEAP_END: the allocation cursor in cells), both verified in
    // `src/javascript/heap.rs` selector constants. f_extent is the occupying
    // raw block size in cells decoded from the RAW_SIZE header cell at
    // fbase - RAW_HEADER_CELLS. Push order matches the emitter order above.
    let extents = phase("env-extents", 5);
    eprintln!(
        "P2_ENV_EXTENTS node_cells_before_graph={} f_raw_block_cells={} \
         node_cells_after_child={} node_cells_after_parent={} \
         heap_end_cells_before_graph={}",
        extents[0], extents[1], extents[2], extents[3], extents[4]
    );
    // Independent pre-Environment boundary phase: taken once at the unique
    // typed bootstrap call, before source Function f exists. Field order
    // matches `relocate_cycle_interval`: NODE_CELLS, LIVE_NODES, HEAP_END,
    // NEXT_SLOT, FREE_SLOTS, RETIRED_SLOTS, COLLECTIONS, PENDING_NODES. This
    // only measures; no exact assertion below consumes it.
    let boundary = phase("env-bootstrap", 8);
    eprintln!(
        "P2_ENV_BOOTSTRAP node_cells={} live_nodes={} heap_end_cells={} \
         next_slot={} free_slots={} retired_slots={} collections={} pending_nodes={}",
        boundary[0],
        boundary[1],
        boundary[2],
        boundary[3],
        boundary[4],
        boundary[5],
        boundary[6],
        boundary[7]
    );
    // Per-slot physical trace of the same typed-bootstrap boundary. Four
    // values per permanent slot (slot, real generation, occupied RAW_SIZE,
    // node kind) were read through the row-generation selector and
    // `__compat_es5_checked_base`; this only prints, it never asserts or
    // feeds an expectation.
    let slot_extents = phase(
        "env-slot-extents",
        4 * usize::try_from(HEAP_INTRINSIC_NODES).expect("permanent slot count"),
    );
    let mut raw_sum = 0i64;
    for entry in slot_extents.chunks_exact(4) {
        let (slot, generation, raw_cells, kind) = (entry[0], entry[1], entry[2], entry[3]);
        eprintln!(
            "P2_ENV_SLOT slot={slot} generation={generation} raw_cells={raw_cells} kind={kind}"
        );
        raw_sum += raw_cells;
    }
    eprintln!(
        "P2_ENV_SLOT_TOTAL slots={} raw_cells_sum={raw_sum} node_cells={}",
        slot_extents.len() / 4,
        boundary[0]
    );
    // Source Function10 + length row7 + raw3, unchanged environment
    // extents, and the permanently rooted 69-node/1381-cell bootstrap graph.
    assert_eq!(
        &kept[..4],
        &[
            HEAP_INTRINSIC_NODES + 3,
            HEAP_INTRINSIC_NODE_CELLS + 10 + 7 + 3 + 5 + 3 + 15 + 3,
            0,
            0
        ]
    );
    assert_eq!(
        &kept[4..6],
        &ids[2..4],
        "function must keep the child environment"
    );
    assert_eq!(
        &kept[7..9],
        &ids[4..6],
        "child must keep its parent environment"
    );
    assert_eq!(
        &kept[9..11],
        &ids[..2],
        "parent boxed entry must retain the cycle edge"
    );
    assert_eq!(kept[11], 7, "primitive environment payload stays intact");
    assert_eq!(
        &swept[..6],
        &[
            HEAP_INTRINSIC_NODES + 1,
            3,
            HEAP_INTRINSIC_NODE_CELLS + 10 + 7 + 3,
            3,
            3,
            3
        ],
        "unrooted cyclic component must be swept"
    );
    assert_ne!(
        swept[6], ids[0],
        "fresh function allocated while the old component was live"
    );
    assert!(swept[7] >= 1);
    assert_eq!(
        swept[8], kept[6],
        "source code identity survives instance replacement"
    );
}

struct HeapPageProbe {
    corrupt: bool,
}

impl vm::SourcePlugin for HeapPageProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-page-probe"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let mut ctx = heap_cycle_ctx(&ir)?;
        let carrier = heap_cycle_carrier(&ir)?;
        ctx.carrier = carrier;
        if self.corrupt {
            let raw =
                heap_cycle_read_number(&mut ir, &ctx, carrier, vm::Expr::Int(16), "page-corrupt")?;
            let page = heap_cycle_decode(&mut ir, &ctx, raw, "page-corrupt")?;
            heap_cycle_write(
                &mut ir,
                &ctx,
                carrier,
                vm::Expr::Add(Box::new(vm::Expr::Var(page)), Box::new(vm::Expr::Int(3))),
                vm::Expr::Float(9.0),
                "page-corrupt",
            )?;
            heap_cycle_collect(&mut ir, &ctx, carrier, "page-corrupt")?;
        } else {
            heap_cycle_collect(&mut ir, &ctx, carrier, "page1")?;
            for (which, slot) in [(2, 0), (5, 0), (9, 0), (13, 64), (14, 64), (13, 0)] {
                heap_cycle_observe(&mut ir, &ctx, carrier, which, slot, "page1")?;
            }
        }
        Ok(ir)
    }
}

fn heap_page_probe_run(source: &str, corrupt: bool) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapPageProbe = Box::leak(Box::new(HeapPageProbe { corrupt }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-page-probe")),
        options,
    )
    .expect("page probe compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(10_000_000);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "page probe should halt");
            None
        }
        Err(error) => Some(error),
    };
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    (machine.stack().to_vec(), error)
}

#[test]
fn p2_heap_page1_gray_lookup_traces_live_row() {
    let (stack, error) = heap_page_probe_run(
        "var o0={v:1};var o1={v:1};var o2={v:1};var o3={v:1};var o4={v:1};var o5={v:1};var o6={v:1};var o7={v:1};var o8={v:1};var o9={v:1};var o10={v:1};var o11={v:1};var o12={v:1};var o13={v:1};var o14={v:1};var o15={v:1};var o16={v:1};var o17={v:1};var o18={v:1};var o19={v:1};var o20={v:1};var o21={v:1};var o22={v:1};var o23={v:1};var o24={v:1};var o25={v:1};var o26={v:1};var o27={v:1};var o28={v:1};var o29={v:1};var o30={v:1};var o31={v:1};var o32={v:1};var o33={v:1};var o34={v:1};var o35={v:1};var o36={v:1};var o37={v:1};var o38={v:1};var o39={v:1};",
        false,
    );
    assert!(
        error.is_none(),
        "page1 collect must halt, got {error:?} stack={stack:?}"
    );
    let values = heap_cycle_ints(&stack);
    eprintln!("P2_PAGE1 values={values:?}");
    assert_eq!(values.len(), 6, "page1 observes, got {values:?}");
    assert!(
        values[0] >= 64,
        "next slot must reach page 1, got {values:?}"
    );
    assert!(values[1] >= 1, "collector must have run, got {values:?}");
    assert!(
        values[2] >= 2,
        "directory must contain page 1, got {values:?}"
    );
    assert_eq!(
        values[3], 2,
        "slot 64 must stay active through the collector, got {values:?}"
    );
    assert!(values[4] >= 1, "slot 64 generation, got {values:?}");
    assert_eq!(
        values[5], 2,
        "page 0 row 0 must stay active, got {values:?}"
    );
}

#[test]
fn p2_heap_collect_rejects_illegal_row_state() {
    let (stack, error) = heap_page_probe_run("var a = {k: 0};", true);
    let error = error.expect("illegal row state must fail closed in collect");
    eprintln!("P2_PAGE_CORRUPT error={error:?} stack={stack:?}");
    assert!(
        format!("{error:?}").contains("assertion failed"),
        "cached collector must keep the row-state guard, got {error:?}"
    );
}

// ---- Exact checked decoders: one compiled case, one root guest run ----
// Retired method: the old harness bootstrapped one VM, refuelled it and
// invoked the exported probe repeatedly. Each case below instead compiles the
// real production source through the real JS plugin, appends ONE call to the
// selected actual private helper with that case's literal input plus one
// readback of its result, and lets the certified root run halt or refuse
// exactly once. The semantic Int/u32 value-domain oracles from the retired
// table are preserved unchanged, including the shared lower/integer
// boundaries, the six case, the u32 upper fields, fractions, non-finite,
// negative and out-of-range refusals, and the bounded Int wrapper's own
// `MAX_DECODE_INT` domain for raw Int inputs.

const HEAP_DECODE_U32_NAME: &str = "__compat_es5_decode_u32";
const HEAP_DECODE_CASE_ID: &str = "javascript-es5-heap-decode-single-entry";
const HEAP_DECODE_CASE_SOURCE: &str = "var o = {};";

#[derive(Clone, Copy)]
enum HeapDecodeHelper {
    Int,
    U32,
}

impl HeapDecodeHelper {
    fn name(self) -> &'static str {
        match self {
            HeapDecodeHelper::Int => HEAP_DECODE_INT_NAME,
            HeapDecodeHelper::U32 => HEAP_DECODE_U32_NAME,
        }
    }

    fn label(self) -> &'static str {
        match self {
            HeapDecodeHelper::Int => "int",
            HeapDecodeHelper::U32 => "u32",
        }
    }
}

/// The literal handed to the helper exactly once: a source-representable
/// Float (including real -0, NaN and infinities) or a raw Int that proves the
/// bounded wrapper's own domain instead of the Core builtin's full i64 one.
#[derive(Clone, Copy)]
enum HeapDecodeInput {
    Float(f64),
    RawInt(i64),
}

impl HeapDecodeInput {
    fn expr(self) -> vm::Expr {
        match self {
            HeapDecodeInput::Float(value) => vm::Expr::Float(value),
            HeapDecodeInput::RawInt(value) => vm::Expr::Int(value),
        }
    }

    fn kind(self) -> &'static str {
        match self {
            HeapDecodeInput::Float(_) => "float",
            HeapDecodeInput::RawInt(_) => "int",
        }
    }

    fn text(self) -> String {
        match self {
            HeapDecodeInput::Float(value) => {
                format!("Float({value:?}) sign_neg={}", value.is_sign_negative())
            }
            HeapDecodeInput::RawInt(value) => format!("Int({value})"),
        }
    }
}

#[derive(Clone, Copy)]
enum HeapDecodeExpect {
    /// Halted with exactly this Int value, never a Float or loose Number.
    Int(i64),
    /// The existing private checked-gate HostError, not any other failure.
    Gate,
}

#[derive(Clone, Copy)]
struct HeapDecodeCase {
    helper: HeapDecodeHelper,
    label: &'static str,
    input: HeapDecodeInput,
    expect: HeapDecodeExpect,
}

const fn heap_decode_expected(
    helper: HeapDecodeHelper,
    label: &'static str,
    input: f64,
    expected: i64,
) -> HeapDecodeCase {
    HeapDecodeCase {
        helper,
        label,
        input: HeapDecodeInput::Float(input),
        expect: HeapDecodeExpect::Int(expected),
    }
}

const fn heap_decode_refusal(
    helper: HeapDecodeHelper,
    label: &'static str,
    input: f64,
) -> HeapDecodeCase {
    HeapDecodeCase {
        helper,
        label,
        input: HeapDecodeInput::Float(input),
        expect: HeapDecodeExpect::Gate,
    }
}

const fn heap_decode_raw_int(
    label: &'static str,
    input: i64,
    expect: HeapDecodeExpect,
) -> HeapDecodeCase {
    HeapDecodeCase {
        helper: HeapDecodeHelper::Int,
        label,
        input: HeapDecodeInput::RawInt(input),
        expect,
    }
}

/// Test-only fixture: runs the production JS plugin first, then appends one
/// root-level call to the selected actual private helper and one readback of
/// the declared Int result. No exported probe, no replacement helper, and no
/// second entry point: the certified root run is the only execution.
struct HeapDecodeCasePlugin {
    helper: &'static str,
    input: HeapDecodeInput,
}

impl vm::SourcePlugin for HeapDecodeCasePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new(HEAP_DECODE_CASE_ID);
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let helper = heap_helper_index(&ir, self.helper)?;
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection("decode single-entry slot space exhausted".to_owned()))?;
        ir.locals += 1;
        ir.stmts.push(vm::Stmt::Let {
            index: slot,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(helper, vec![self.input.expr()]),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(slot),
            line: 1,
        });
        Ok(ir)
    }
}

fn heap_decode_single_entry_run(cases: &[HeapDecodeCase]) {
    let cases = cases.to_vec();
    let result = std::thread::Builder::new()
        .name("heap-decode-single-entry".to_owned())
        .stack_size(33_554_432)
        .spawn(move || {
            for case in &cases {
                heap_decode_single_entry_case(*case);
            }
        })
        .expect("decode single-entry thread");
    result
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
}

fn heap_decode_single_entry_case(case: HeapDecodeCase) {
    let helper = case.helper.label();
    let label = case.label;
    let kind = case.input.kind();
    let input = case.input.text();
    let plugin: &'static HeapDecodeCasePlugin = Box::leak(Box::new(HeapDecodeCasePlugin {
        helper: case.helper.name(),
        input: case.input,
    }));
    eprintln!(
        "P2_DECODE_STAGE_SOURCE helper={helper} label={label} kind={kind} input={input} frontend_parses=1 core_compiles=0 guest_runs=0"
    );
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        HEAP_DECODE_CASE_SOURCE,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(HEAP_DECODE_CASE_ID)),
        options,
    )
    .expect("decode single-entry compilation should succeed");
    eprintln!(
        "P2_DECODE_STAGE_CORE helper={helper} label={label} kind={kind} input={input} frontend_parses=1 core_compiles=1 guest_runs=0"
    );
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    machine.set_fuel(10_000_000);
    eprintln!(
        "P2_DECODE_STAGE_RUN helper={helper} label={label} kind={kind} input={input} vms=1 guest_runs=1 fuel=10000000"
    );
    let status = machine.run();
    let got = match (status, case.expect) {
        (Ok(VmStatus::Halted), HeapDecodeExpect::Int(expected)) => {
            let stack = machine.stack().to_vec();
            let value = stack
                .last()
                .cloned()
                .unwrap_or_else(|| panic!("{helper} {label} input={input} must publish one Int"));
            assert_eq!(
                value,
                Value::Int(expected),
                "{helper} {label} input={input} must return the exact Int({expected})"
            );
            format!("{value:?}")
        }
        (Ok(VmStatus::Halted), HeapDecodeExpect::Gate) => panic!(
            "{helper} {label} input={input} must refuse at the checked gate, got Halted"
        ),
        (Ok(status), HeapDecodeExpect::Gate) => panic!(
            "{helper} {label} input={input} must refuse at the checked gate, got {status:?}"
        ),
        (Err(error), HeapDecodeExpect::Gate) => {
            assert!(
                heap_decode_gate_failed(&error),
                "{helper} {label} input={input} must fail at the checked gate, got {error:?}"
            );
            format!("{error:?}")
        }
        (Ok(status), HeapDecodeExpect::Int(expected)) => panic!(
            "{helper} {label} input={input} must halt with Int({expected}), got {status:?}"
        ),
        (Err(error), HeapDecodeExpect::Int(expected)) => panic!(
            "{helper} {label} input={input} must return Int({expected}), got {error:?}"
        ),
    };
    assert!(
        !machine.jit_config().enabled,
        "{helper} {label} single-entry VM must keep JIT off"
    );
    assert_eq!(
        machine.aot_exec_count(),
        0,
        "{helper} {label} single-entry VM must not execute any AOT artifact"
    );
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "{helper} {label} single-entry VM must not execute any native artifact"
    );
    eprintln!(
        "P2_DECODE_CASE_RESULT helper={helper} label={label} kind={kind} input={input} got={got} status={} frontend_parses=1 core_compiles=1 vms=1 guest_runs=1 fuel={:?} jit=off aot=0 jit_native=0",
        match case.expect {
            HeapDecodeExpect::Int(_) => "halted",
            HeapDecodeExpect::Gate => "refused",
        },
        machine.get_fuel()
    );
    machine.reset_for_reuse().expect("reset decode single-entry VM");
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "{helper} {label} must release the real owner on reset"
    );
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none(),
        "{helper} {label} must release the owned array directory on reset"
    );
    eprintln!(
        "P2_DECODE_CASE_CLEANUP helper={helper} label={label} resource_count=0 owned_arrays=none reset=1 post_reset_guest_runs=0"
    );
}

fn heap_decode_gate_failed(error: &vm::VmError) -> bool {
    match error {
        vm::VmError::HostError(message) => {
            message.contains("assertion failed")
                && !message.contains("TypeError")
                && message != "uncaught JavaScript throw"
        }
        _ => false,
    }
}

// Each selector below owns at most two complete single-entry cases. Each case
// is three stages: one real frontend parse, one Core compile, and one fresh
// certified VM (one fuel assignment immediately before one root run). A
// selector therefore runs at most six stages. The preserved oracle table is
// split across named bounded selectors instead of one refuelled loop.

#[test]
fn p2_heap_decode_single_entry_zero_one_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "zero", 0.0, 0),
        heap_decode_expected(HeapDecodeHelper::U32, "zero", 0.0, 0),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_zero_one_part2() {
    const NEG_ZERO: f64 = -0.0;
    assert!(NEG_ZERO.is_sign_negative(), "fixture must pass real -0");
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "negzero", NEG_ZERO, 0),
        heap_decode_expected(HeapDecodeHelper::U32, "negzero", NEG_ZERO, 0),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_zero_one_part3() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "one", 1.0, 1),
        heap_decode_expected(HeapDecodeHelper::U32, "one", 1.0, 1),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_two_four_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "two", 2.0, 2),
        heap_decode_expected(HeapDecodeHelper::U32, "two", 2.0, 2),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_two_four_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "three", 3.0, 3),
        heap_decode_expected(HeapDecodeHelper::U32, "three", 3.0, 3),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_two_four_part3() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "four", 4.0, 4),
        heap_decode_expected(HeapDecodeHelper::U32, "four", 4.0, 4),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_five_eight_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "five", 5.0, 5),
        heap_decode_expected(HeapDecodeHelper::U32, "five", 5.0, 5),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_five_eight_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "seven", 7.0, 7),
        heap_decode_expected(HeapDecodeHelper::U32, "seven", 7.0, 7),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_five_eight_part3() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "eight", 8.0, 8),
        heap_decode_expected(HeapDecodeHelper::U32, "eight", 8.0, 8),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_fifteen_u8max_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "fifteen", 15.0, 15),
        heap_decode_expected(HeapDecodeHelper::U32, "fifteen", 15.0, 15),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_fifteen_u8max_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "sixteen", 16.0, 16),
        heap_decode_expected(HeapDecodeHelper::U32, "sixteen", 16.0, 16),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_fifteen_u8max_part3() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "u8max", 255.0, 255),
        heap_decode_expected(HeapDecodeHelper::U32, "u8max", 255.0, 255),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_plus_u16_int_max_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "u8max+1", 256.0, 256),
        heap_decode_expected(HeapDecodeHelper::U32, "u8max+1", 256.0, 256),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_plus_u16_int_max_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "u16max", 65535.0, 65535),
        heap_decode_expected(HeapDecodeHelper::U32, "u16max", 65535.0, 65535),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_plus_u16_int_max_part3() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "int-max", 65536.0, 65536),
        heap_decode_expected(HeapDecodeHelper::U32, "int-max", 65536.0, 65536),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_six_u32_upper_fields_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(HeapDecodeHelper::Int, "six", 6.0, 6),
        heap_decode_expected(HeapDecodeHelper::U32, "int-overflow", 65537.0, 65537),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_six_u32_upper_fields_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_expected(
            HeapDecodeHelper::U32,
            "i32-sign-bit",
            2147483648.0,
            2147483648,
        ),
        heap_decode_expected(HeapDecodeHelper::U32, "u32-max", 4294967295.0, 4294967295),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_fraction_refusals_low_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "half", 0.5),
        heap_decode_refusal(HeapDecodeHelper::U32, "half", 0.5),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_fraction_refusals_low_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "two-half", 2.5),
        heap_decode_refusal(HeapDecodeHelper::U32, "two-half", 2.5),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_fraction_refusals_high_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "seven-half", 7.5),
        heap_decode_refusal(HeapDecodeHelper::U32, "seven-half", 7.5),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_fraction_refusals_high_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "fifteen-half", 15.5),
        heap_decode_refusal(HeapDecodeHelper::U32, "fifteen-half", 15.5),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_nonfinite_refusals_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "nan", f64::NAN),
        heap_decode_refusal(HeapDecodeHelper::U32, "nan", f64::NAN),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_nonfinite_refusals_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "pos-inf", f64::INFINITY),
        heap_decode_refusal(HeapDecodeHelper::U32, "pos-inf", f64::INFINITY),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_nonfinite_refusals_part3() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "neg-inf", f64::NEG_INFINITY),
        heap_decode_refusal(HeapDecodeHelper::U32, "neg-inf", f64::NEG_INFINITY),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_negative_overflow_refusals_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "negative", -1.0),
        heap_decode_refusal(HeapDecodeHelper::U32, "negative", -1.0),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_negative_overflow_refusals_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "u32-overflow", 4294967296.0),
        heap_decode_refusal(HeapDecodeHelper::U32, "u32-overflow", 4294967296.0),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_int_upper_refusals_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "int-overflow", 65537.0),
        heap_decode_refusal(HeapDecodeHelper::Int, "i32-sign-bit", 2147483648.0),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_int_upper_refusals_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_refusal(HeapDecodeHelper::Int, "u32-max", 4294967295.0),
    ]);
}

/// The accepted Core builtin keeps the full i64 identity, so the Compat
/// wrapper must prove its own bounded domain still applies to raw Int inputs:
/// the inclusive `MAX_DECODE_INT` ceiling succeeds and everything outside the
/// nonnegative bounded domain refuses before the conversion.
#[test]
fn p2_heap_decode_single_entry_raw_int_domain_part1() {
    heap_decode_single_entry_run(&[
        heap_decode_raw_int("raw-zero", 0, HeapDecodeExpect::Int(0)),
        heap_decode_raw_int("raw-int-max", 65536, HeapDecodeExpect::Int(65536)),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_raw_int_domain_part2() {
    heap_decode_single_entry_run(&[
        heap_decode_raw_int("raw-negative", -1, HeapDecodeExpect::Gate),
        heap_decode_raw_int("raw-overflow", 65537, HeapDecodeExpect::Gate),
    ]);
}

#[test]
fn p2_heap_decode_single_entry_raw_int_domain_part3() {
    heap_decode_single_entry_run(&[
        heap_decode_raw_int("raw-i64-max", i64::MAX, HeapDecodeExpect::Gate),
    ]);
}

/// The accepted Core conversion keeps the full i64 identity, so the UInt32
/// wrapper must prove the whole `0..=4294967295` domain on raw Int inputs
/// itself: both inclusive endpoints convert unchanged and everything outside
/// the domain refuses before the conversion.
#[test]
fn p2_heap_decode_single_entry_raw_u32_domain_part1() {
    heap_decode_single_entry_run(&[
        HeapDecodeCase {
            helper: HeapDecodeHelper::U32,
            label: "raw-u32-zero",
            input: HeapDecodeInput::RawInt(0),
            expect: HeapDecodeExpect::Int(0),
        },
        HeapDecodeCase {
            helper: HeapDecodeHelper::U32,
            label: "raw-u32-max",
            input: HeapDecodeInput::RawInt(4_294_967_295),
            expect: HeapDecodeExpect::Int(4_294_967_295),
        },
    ]);
}

#[test]
fn p2_heap_decode_single_entry_raw_u32_domain_part2() {
    heap_decode_single_entry_run(&[
        HeapDecodeCase {
            helper: HeapDecodeHelper::U32,
            label: "raw-u32-negative",
            input: HeapDecodeInput::RawInt(-1),
            expect: HeapDecodeExpect::Gate,
        },
        HeapDecodeCase {
            helper: HeapDecodeHelper::U32,
            label: "raw-u32-overflow",
            input: HeapDecodeInput::RawInt(4_294_967_296),
            expect: HeapDecodeExpect::Gate,
        },
    ]);
}

// ---- U32 decode: one guarded Core conversion (test-only structural IR) ----
// Current-target regression witness for the accepted `__compat_es5_decode_u32`
// contract: the normalized Number parameter keeps the original closed bounds,
// the redundant modulo-zero assert is absent, then exactly ONE Core
// `CheckedIntFromNumber` call converts the ORIGINAL parameter. The helper body
// is four statements with exactly two Assert calls. The retired 32-bit
// subtract/add digit codec and its 0/1 fast path stay absent. One real
// production parse supplies the real helper body; no Core program is compiled,
// no VM is created and no guest code runs.

#[test]
fn p2_heap_decode_u32_core_conversion_contract() {
    use vm::SourcePlugin;

    /// One local traversal shared by the two narrow scans below: the existing
    /// flatteners collect statements, not arbitrary sub-expressions, so this
    /// reaches the conversion call wherever the real emitter nests it.
    fn heap_decode_u32_scan_expr(expr: &vm::Expr, visit: &mut dyn FnMut(&vm::Expr)) {
        visit(expr);
        match expr {
            vm::Expr::Call(_, _, args, _, _)
            | vm::Expr::LocalCall(_, _, args, _)
            | vm::Expr::ClosureCall(_, args) => {
                for arg in args {
                    heap_decode_u32_scan_expr(arg, &mut *visit);
                }
            }
            vm::Expr::Block { stmts, expr } => {
                heap_decode_u32_scan_stmts(stmts, &mut *visit);
                heap_decode_u32_scan_expr(expr, &mut *visit);
            }
            vm::Expr::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                heap_decode_u32_scan_expr(condition, &mut *visit);
                heap_decode_u32_scan_expr(then_expr, &mut *visit);
                heap_decode_u32_scan_expr(else_expr, &mut *visit);
            }
            vm::Expr::ResultMatch {
                value,
                ok_expr,
                err_expr,
                ..
            } => {
                heap_decode_u32_scan_expr(value, &mut *visit);
                heap_decode_u32_scan_expr(ok_expr, &mut *visit);
                heap_decode_u32_scan_expr(err_expr, &mut *visit);
            }
            vm::Expr::ResultTry { value, .. } => heap_decode_u32_scan_expr(value, &mut *visit),
            vm::Expr::Add(left, right)
            | vm::Expr::Sub(left, right)
            | vm::Expr::Mul(left, right)
            | vm::Expr::Div(left, right)
            | vm::Expr::Mod(left, right)
            | vm::Expr::And(left, right)
            | vm::Expr::Or(left, right)
            | vm::Expr::Eq(left, right)
            | vm::Expr::Lt(left, right)
            | vm::Expr::Gt(left, right) => {
                heap_decode_u32_scan_expr(left, &mut *visit);
                heap_decode_u32_scan_expr(right, &mut *visit);
            }
            vm::Expr::Neg(inner) | vm::Expr::Not(inner) => {
                heap_decode_u32_scan_expr(inner, &mut *visit)
            }
            _ => {}
        }
    }

    fn heap_decode_u32_scan_stmts(stmts: &[vm::Stmt], visit: &mut dyn FnMut(&vm::Expr)) {
        for stmt in stmts {
            match stmt {
                vm::Stmt::Let { expr, .. }
                | vm::Stmt::Assign { expr, .. }
                | vm::Stmt::Expr { expr, .. } => heap_decode_u32_scan_expr(expr, &mut *visit),
                vm::Stmt::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    heap_decode_u32_scan_expr(condition, &mut *visit);
                    heap_decode_u32_scan_stmts(then_branch, &mut *visit);
                    heap_decode_u32_scan_stmts(else_branch, &mut *visit);
                }
                vm::Stmt::For {
                    init,
                    condition,
                    post,
                    body,
                    ..
                } => {
                    heap_decode_u32_scan_stmts(std::slice::from_ref(init.as_ref()), &mut *visit);
                    heap_decode_u32_scan_expr(condition, &mut *visit);
                    heap_decode_u32_scan_stmts(std::slice::from_ref(post.as_ref()), &mut *visit);
                    heap_decode_u32_scan_stmts(body, &mut *visit);
                }
                vm::Stmt::While {
                    condition, body, ..
                } => {
                    heap_decode_u32_scan_expr(condition, &mut *visit);
                    heap_decode_u32_scan_stmts(body, &mut *visit);
                }
                _ => {}
            }
        }
    }

    fn heap_decode_u32_slot(expr: &vm::Expr) -> Option<u16> {
        match expr {
            vm::Expr::Var(slot) => Some(*slot),
            _ => None,
        }
    }

    /// The single Bool argument of the real `assert` builtin statement, if `stmt`
    /// is one.
    fn heap_decode_u32_assert_condition<'a>(
        stmt: &'a vm::Stmt,
        assert_index: u16,
    ) -> Option<&'a vm::Expr> {
        match stmt {
            vm::Stmt::Expr {
                expr: vm::Expr::Call(index, type_args, args, resolution, semantic),
                ..
            } if *index == assert_index
                && type_args.is_empty()
                && args.len() == 1
                && resolution.is_none()
                && semantic.is_none() =>
            {
                Some(&args[0])
            }
            _ => None,
        }
    }

    let catalog = heap_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    eprintln!(
        "P10_U32_CORE_CONVERSION_STAGE helper=u32 frontend_parses=1 core_compiles=0 guest_runs=0"
    );
    let ir = JAVASCRIPT_ES5_PLUGIN
        .parse_source(HEAP_DECODE_CASE_SOURCE, &context)
        .expect("real U32 decoder source must parse once through the production JS plugin");
    let helper_index = heap_helper_index(&ir, HEAP_DECODE_U32_NAME)
        .expect("production plugin must declare the private U32 decoder");
    let decl = ir
        .functions
        .iter()
        .find(|function| function.index == helper_index)
        .expect("U32 decoder declaration must exist in the flat table");

    // ABI first: the accepted helper identity is unchanged by the conversion.
    assert!(!decl.exported, "U32 decoder must stay private");
    assert_eq!(decl.arity, 1, "U32 decoder keeps one parameter");
    assert_eq!(
        decl.arg_schemas,
        vec![Some(vm::TypeSchema::Number)],
        "U32 decoder parameter domain stays Number"
    );
    assert_eq!(
        decl.return_schema,
        Some(vm::TypeSchema::Int),
        "U32 decoder result schema stays Int"
    );
    assert_eq!(
        decl.return_type,
        vm::ValueType::Int,
        "U32 decoder result type stays Int"
    );

    let helper = ir
        .function_impls
        .get(&decl.index)
        .expect("production U32 decoder must carry its real implementation");
    assert_eq!(helper.param_slots.len(), 1, "one real parameter slot");
    assert!(
        helper.capture_copies.is_empty(),
        "U32 decoder captures no context"
    );
    let param_x = helper.param_slots[0];
    let vm::Expr::Var(decoded_slot) = &helper.body_expr else {
        panic!("U32 decoder body must stay Var(decoded)");
    };
    let decoded = *decoded_slot;

    // Derive the producer's own temporaries from the real body instead of
    // assuming flat indices: the normalizing Let names `remaining`, and the
    // parameter/remaining/decoded slots stay contiguous, exactly as emitted.
    let vm::Stmt::Let {
        index: remaining_slot,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: normalizing,
        ..
    } = &helper.body_stmts[0]
    else {
        panic!("U32 decoder must open by normalizing the parameter to Number");
    };
    let remaining = *remaining_slot;
    assert_eq!(
        remaining,
        param_x + 1,
        "remaining slot follows the parameter slot"
    );
    assert_eq!(decoded, remaining + 1, "decoded slot follows remaining");
    let vm::Expr::Div(normalized_value, normalized_unit) = normalizing else {
        panic!("parameter normalization must be the divide-by-one shape");
    };
    assert_eq!(
        heap_decode_u32_slot(normalized_value),
        Some(param_x),
        "normalization divides the original parameter"
    );
    assert!(matches!(normalized_unit.as_ref(), vm::Expr::Float(unit) if *unit == 1.0));

    // Count the real Core conversion in that helper's body plus body_expr.
    let conversion = vm::BuiltinFunction::CheckedIntFromNumber.call_index();
    let assert_builtin = vm::BuiltinFunction::Assert.call_index();
    let mut builtin_calls = 0usize;
    let mut assert_calls = 0usize;
    let mut retired_arithmetic = 0usize;
    let mut modulo_ops = 0usize;
    {
        let mut visit = |expr: &vm::Expr| match expr {
            vm::Expr::Call(index, _, _, _, _) if *index == conversion => builtin_calls += 1,
            vm::Expr::Call(index, _, _, _, _) if *index == assert_builtin => assert_calls += 1,
            vm::Expr::Sub(..) | vm::Expr::Add(..) => retired_arithmetic += 1,
            vm::Expr::Mod(..) => modulo_ops += 1,
            _ => {}
        };
        heap_decode_u32_scan_stmts(&helper.body_stmts, &mut visit);
        heap_decode_u32_scan_expr(&helper.body_expr, &mut visit);
    }
    eprintln!(
        "P10_U32_CORE_CONVERSION_IR builtin_calls={builtin_calls} assert_calls={assert_calls} modulo_ops={modulo_ops} body_stmts={}",
        helper.body_stmts.len()
    );
    assert_eq!(
        builtin_calls, 1,
        "U32 decoder must convert through CheckedIntFromNumber exactly once"
    );

    // Current-target shape: normalize, two range guards, one conversion.
    assert_eq!(
        helper.body_stmts.len(),
        4,
        "normalize, two range guards, one conversion; no digit decomposition"
    );
    assert_eq!(
        modulo_ops, 0,
        "redundant modulo-zero assert and every Mod in the helper must be gone"
    );
    assert_eq!(
        assert_calls, 2,
        "U32 decoder keeps exactly two Assert calls"
    );
    assert_eq!(
        retired_arithmetic, 0,
        "retired 32-bit subtract/add digit accumulation must be gone"
    );
    assert!(
        !helper
            .body_stmts
            .iter()
            .any(|stmt| matches!(stmt, vm::Stmt::IfElse { .. })),
        "no per-bit IfElse or 0/1 fast path may remain"
    );

    let vm::Stmt::Let {
        index: number_slot,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: number_expr,
        ..
    } = &helper.body_stmts[0]
    else {
        panic!("first statement must bind the normalized Number");
    };
    assert_eq!(*number_slot, remaining);
    let vm::Expr::Div(number_value, number_unit) = number_expr else {
        panic!("normalized Number must be divide-by-one");
    };
    assert_eq!(heap_decode_u32_slot(number_value), Some(param_x));
    assert!(matches!(number_unit.as_ref(), vm::Expr::Float(unit) if *unit == 1.0));

    let lower = heap_decode_u32_assert_condition(&helper.body_stmts[1], assert_builtin)
        .expect("original nonnegative guard must stay an assert");
    let vm::Expr::Not(lower_inner) = lower else {
        panic!("lower guard must stay Not(Lt(remaining, 0))");
    };
    let vm::Expr::Lt(lower_value, lower_floor) = lower_inner.as_ref() else {
        panic!("lower guard must stay a strict comparison");
    };
    assert_eq!(heap_decode_u32_slot(lower_value), Some(remaining));
    assert!(matches!(lower_floor.as_ref(), vm::Expr::Float(floor) if *floor == 0.0));

    let upper = heap_decode_u32_assert_condition(&helper.body_stmts[2], assert_builtin)
        .expect("original closed upper guard must stay an assert");
    let vm::Expr::Or(upper_below, upper_equal) = upper else {
        panic!("upper guard must stay Or(Lt, Eq), never a NaN-admitting !Lt(MAX)");
    };
    let vm::Expr::Lt(upper_below_value, upper_below_max) = upper_below.as_ref() else {
        panic!("upper guard keeps its strict lower arm");
    };
    assert_eq!(heap_decode_u32_slot(upper_below_value), Some(remaining));
    assert!(matches!(upper_below_max.as_ref(), vm::Expr::Float(max) if *max == 4_294_967_295.0));
    let vm::Expr::Eq(upper_equal_value, upper_equal_max) = upper_equal.as_ref() else {
        panic!("upper guard keeps its inclusive arm");
    };
    assert_eq!(heap_decode_u32_slot(upper_equal_value), Some(remaining));
    assert!(matches!(upper_equal_max.as_ref(), vm::Expr::Float(max) if *max == 4_294_967_295.0));

    let vm::Stmt::Let {
        index: convert_slot,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: convert_expr,
        ..
    } = &helper.body_stmts[3]
    else {
        panic!("fourth statement must bind the converted Int");
    };
    assert_eq!(*convert_slot, decoded);
    let vm::Expr::Call(
        convert_target,
        convert_types,
        convert_args,
        convert_resolution,
        convert_semantic,
    ) = convert_expr
    else {
        panic!("decoded Int must come from the Core conversion call");
    };
    assert_eq!(*convert_target, conversion);
    assert!(
        convert_types.is_empty(),
        "conversion carries no type arguments"
    );
    assert!(convert_resolution.is_none() && convert_semantic.is_none());
    let [vm::Expr::Var(convert_arg)] = convert_args.as_slice() else {
        panic!("conversion must take exactly the original parameter slot");
    };
    assert_eq!(
        *convert_arg, param_x,
        "conversion reads the original parameter, never normalized remaining"
    );
    assert!(matches!(&helper.body_expr, vm::Expr::Var(slot) if *slot == decoded));
}

// ---- Pending-throw tail proof (test-only structural IR witness, labeled) ----
// Top-level script statements after an uncaught callee throw are still
// emitted by lower_script_list: only the Error object body is wrapped in
// the pending-normal gate, while the key/message UTF-16 nodes stay
// unconditional (the constructor emits them before the guarded region). A
// halted-VM counter comparison cannot observe this: the uncaught throw
// aborts before any trailing readback could run, so this witness compiles
// (and never executes) the real admitted source through the production
// plugin and counts __compat_es5_node_begin calls in the actual lowered IR
// after the real fail() dispatch, tracking real Block/IfElse nesting. A
// begin under a Var-conditioned IfElse (the pending-normal gate shape)
// counts as guarded; anything else counts as unguarded. The TEST-ONLY
// plugin below returns the production IR unchanged: the only intervention
// is compile-without-run, labeled here because the top-level uncaught
// throw makes a halted readback unreachable.

static HEAP_THROW_TAIL_TOTAL: AtomicUsize = AtomicUsize::new(usize::MAX);
static HEAP_THROW_TAIL_UNGUARDED: AtomicUsize = AtomicUsize::new(usize::MAX);
static HEAP_THROW_TAIL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct HeapThrowTailPlugin {
    target_name: &'static str,
}

impl vm::SourcePlugin for HeapThrowTailPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-throw-tail"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let (total, unguarded) = heap_throw_tail_scan(&ir, self.target_name)?;
        HEAP_THROW_TAIL_TOTAL.store(total, Ordering::SeqCst);
        HEAP_THROW_TAIL_UNGUARDED.store(unguarded, Ordering::SeqCst);
        Ok(ir)
    }
}

/// Narrow guard-tracking walk over the trailing top-level statements. It
/// mirrors the existing heap_tree_stmts/heap_expr_stmts nesting exactly
/// (Let/Assign/Expr plus Stmt::IfElse; Block/IfElse/ResultMatch/ResultTry/
/// Call) and only adds the Var-gate flag, so no general diagnostic
/// subsystem is duplicated.
fn heap_throw_tail_scan_stmt(
    stmt: &vm::Stmt,
    guarded: bool,
    node_begin: u16,
    counts: &mut (usize, usize),
) {
    match stmt {
        vm::Stmt::Let { expr, .. } | vm::Stmt::Assign { expr, .. } | vm::Stmt::Expr { expr, .. } => {
            heap_throw_tail_scan_expr(expr, guarded, node_begin, counts);
        }
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            heap_throw_tail_scan_expr(condition, guarded, node_begin, counts);
            let guarded = guarded || matches!(condition, vm::Expr::Var(_));
            for branch in then_branch {
                heap_throw_tail_scan_stmt(branch, guarded, node_begin, counts);
            }
            for branch in else_branch {
                heap_throw_tail_scan_stmt(branch, guarded, node_begin, counts);
            }
        }
        _ => {}
    }
}

fn heap_throw_tail_scan_expr(
    expr: &vm::Expr,
    guarded: bool,
    node_begin: u16,
    counts: &mut (usize, usize),
) {
    match expr {
        vm::Expr::Block { stmts, expr } => {
            for stmt in stmts {
                heap_throw_tail_scan_stmt(stmt, guarded, node_begin, counts);
            }
            heap_throw_tail_scan_expr(expr, guarded, node_begin, counts);
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            heap_throw_tail_scan_expr(condition, guarded, node_begin, counts);
            let guarded = guarded || matches!(condition.as_ref(), vm::Expr::Var(_));
            heap_throw_tail_scan_expr(then_expr, guarded, node_begin, counts);
            heap_throw_tail_scan_expr(else_expr, guarded, node_begin, counts);
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            heap_throw_tail_scan_expr(value, guarded, node_begin, counts);
            heap_throw_tail_scan_expr(ok_expr, guarded, node_begin, counts);
            heap_throw_tail_scan_expr(err_expr, guarded, node_begin, counts);
        }
        vm::Expr::ResultTry { value, .. } => {
            heap_throw_tail_scan_expr(value, guarded, node_begin, counts);
        }
        vm::Expr::Call(target, _, args, _, _) => {
            if *target == node_begin {
                counts.0 += 1;
                if !guarded {
                    counts.1 += 1;
                }
            }
            for arg in args {
                heap_throw_tail_scan_expr(arg, guarded, node_begin, counts);
            }
        }
        _ => {}
    }
}

/// Count (total, unguarded) __compat_es5_node_begin calls in the actual
/// lowered IR after the last real __compat_es5_dispatch statement (the end
/// of the throwing fail() call lowering). Helper identities come from the
/// real IR via the existing index helper; the boundary follows the actual
/// statement order with the existing flattener, never a string search.
fn heap_throw_tail_scan(ir: &vm::FrontendIr, target_name: &str) -> Result<(usize, usize), vm::ParseError> {
    let node_begin = heap_helper_index(ir, target_name)?;
    let dispatch = heap_helper_index(ir, "__compat_es5_dispatch")?;
    let mut boundary = None;
    for (at, stmt) in ir.stmts.iter().enumerate() {
        let mut flat = Vec::new();
        heap_tree_stmts(std::slice::from_ref(stmt), &mut flat);
        let dispatched = flat.iter().copied().any(|nested| {
            matches!(
                nested,
                vm::Stmt::Let {
                    expr: vm::Expr::Call(target, _, _, _, _),
                    ..
                }
                | vm::Stmt::Assign {
                    expr: vm::Expr::Call(target, _, _, _, _),
                    ..
                }
                | vm::Stmt::Expr {
                    expr: vm::Expr::Call(target, _, _, _, _),
                    ..
                } if *target == dispatch
            )
        });
        if dispatched {
            boundary = Some(at);
        }
    }
    let start = boundary
        .ok_or_else(|| {
            heap_rejection("throw-tail witness requires the actual fail() dispatch".to_owned())
        })?
        + 1;
    let mut counts = (0usize, 0usize);
    for stmt in &ir.stmts[start..] {
        heap_throw_tail_scan_stmt(stmt, false, node_begin, &mut counts);
    }
    Ok(counts)
}

/// Compile-only readback of the structural witness: the production IR is
/// returned unchanged and never executed (the top-level uncaught throw
/// cannot halt), so this only records what the real lowering emitted.
fn heap_throw_tail_counts(source: &str) -> (usize, usize) {
    heap_throw_tail_counts_for(source, HEAP_NODE_BEGIN_NAME)
}

fn heap_throw_tail_counts_for(source: &str, target_name: &'static str) -> (usize, usize) {
    let _lock = HEAP_THROW_TAIL_LOCK.lock().expect("serialize throw-tail observations");
    HEAP_THROW_TAIL_TOTAL.store(usize::MAX, Ordering::SeqCst);
    HEAP_THROW_TAIL_UNGUARDED.store(usize::MAX, Ordering::SeqCst);
    let plugin: &'static HeapThrowTailPlugin = Box::leak(Box::new(HeapThrowTailPlugin { target_name }));
    let catalog = heap_catalog();
    vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-throw-tail")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .expect("throw-tail witness must compile the real admitted source");
    (
        HEAP_THROW_TAIL_TOTAL.load(Ordering::SeqCst),
        HEAP_THROW_TAIL_UNGUARDED.load(Ordering::SeqCst),
    )
}

#[test]
fn new_error_after_top_level_throw_does_not_allocate() {
    let baseline = "var keep=new Error(\"boom\");function fail(){throw 7;}fail();0;";
    let skipped = "var keep=new Error(\"boom\");function fail(){throw 7;}fail();new Error(\"boom\");";
    // Both programs lower the same live Error, the same throwing call and
    // the same realm; only the final expression differs (inert literal vs
    // constructor). The existing try-controlled test stays untouched as the
    // passing control; this is the uncaught top-level RED.
    let (base_total, base_unguarded) = heap_throw_tail_counts(baseline);
    assert_eq!(
        (base_total, base_unguarded),
        (0, 0),
        "baseline trailing literal must lower no nodes"
    );
    let (skip_total, skip_unguarded) = heap_throw_tail_counts(skipped);
    assert!(
        skip_total > base_total,
        "witness must find the actual trailing constructor nodes, got total={skip_total}"
    );
    assert_eq!(
        skip_unguarded, base_unguarded,
        "trailing new Error must not allocate key/message nodes after the top-level callee throw"
    );
}

#[test]
fn regexp_test_value_after_top_level_throw_is_guarded() {
    let baseline = "var keep=/a/;function fail(){throw 7;}fail();0;";
    let skipped = "var keep=/a/;function fail(){throw 7;}fail();/a/;";
    // Test-only compile-without-run, unchanged production IR. Count the whole
    // allocating value helper, not node_begin hidden inside that helper body.
    // Like the Error witness, the generic Var-guard walk needs an independent
    // source review to confirm the actual normal slot and its true branch.
    let target = "__compat_es5_regexp_test_value";
    let baseline_counts = heap_throw_tail_counts_for(baseline, target);
    assert_eq!(baseline_counts, (0, 0));
    let (total, unguarded) = heap_throw_tail_counts_for(skipped, target);
    assert_eq!(total, 1, "find the real trailing RegExp value call");
    assert_eq!(unguarded, 0, "RegExp allocation after callee throw must be guarded");
}

// ---- Number-only root-frame metadata validation, one guest entry per case ----
// Each case compiles its own source, creates one fresh certified VM, and makes
// exactly one guest `run()`: the real realm bootstrap, one real
// `__compat_es5_root_enter`, the metadata cell writes and the real
// `__compat_es5_root_write` (or its internal-edge consumer) all execute in that
// single entry. No probe export, no host invocation of guest code, no resume.
// After the entry the actual destination cells are read through the existing
// production `arrays::snapshot` descriptor, using the real handle that the
// unchanged `arrays::read` adapter observed during the entry. The read wrapper
// only records the first argument and forwards to the original adapter.

const HEAP_ROOT_FRAME_SOURCE: &str = "var o = {};";
const HEAP_ROOT_FRAME_ID: &str = "javascript-es5-heap-root-frame-single-entry";
const HEAP_RAW_CLASS_OFFSET: i64 = 1;
const HEAP_FRAME_CAPACITY_OFFSET: i64 = 3;
const HEAP_FRAME_ENTRY_BASE: i64 = 4;
const HEAP_FRAME_ENTRY_CELLS: i64 = 3;
const HEAP_HDR_HEAP_END: i64 = 1;
const HEAP_CELL_BUDGET: i64 = 65536;
const HEAP_ROOT_CLASS: f64 = 3.0;
const HEAP_OBJECT_CATEGORY: i64 = 5;
const HEAP_ROOT_WRITE_NODE_NAME: &str = "__compat_es5_root_write_node";

thread_local! {
    static HEAP_FRAME_HANDLE: std::cell::RefCell<Option<Value>> =
        const { std::cell::RefCell::new(None::<Value>) };
}
static HEAP_FRAME_ORIGINAL_READ: std::sync::OnceLock<vm::StaticHostFunction> =
    std::sync::OnceLock::new();

fn heap_frame_capture_read(
    machine: &mut vm::Vm,
    args: &[Value],
) -> vm::VmResult<vm::CallOutcome> {
    if let Some(handle) = args.first() {
        HEAP_FRAME_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle.clone()));
    }
    HEAP_FRAME_ORIGINAL_READ
        .get()
        .expect("actual production arrays::read adapter")(machine, args)
}

fn heap_frame_snapshot(machine: &mut vm::Vm) -> Vec<Value> {
    let handle = HEAP_FRAME_HANDLE
        .with(|slot| slot.borrow().clone())
        .expect("the single entry must observe the actual carrier handle");
    let descriptor =
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
            .descriptors()
            .into_iter()
            .find(|descriptor| descriptor.schema.name == "arrays::snapshot")
            .expect("real arrays::snapshot descriptor");
    let outcome = match descriptor.adapter {
        vm::HostAdapterDescriptor::Static(f)
        | vm::HostAdapterDescriptor::StaticStack(f)
        | vm::HostAdapterDescriptor::StaticStackRuntimeOwned(f) => f(machine, &[handle]),
        _ => panic!("arrays::snapshot must keep a static VM-aware adapter"),
    }
    .expect("production snapshot of the still-open carrier payload");
    match outcome {
        vm::CallOutcome::Return(vm::CallReturn::One(Value::Array(values))) => values.to_vec(),
        other => panic!("arrays::snapshot must return one numeric array, got {other:?}"),
    }
}

fn heap_frame_registry() -> vm::HostFunctionRegistry {
    let mut descriptors = Vec::new();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        descriptors.extend(module.descriptors());
    }
    for descriptor in &mut descriptors {
        if descriptor.schema.name == "arrays::read" {
            descriptor.adapter = match descriptor.adapter {
                vm::HostAdapterDescriptor::Static(f) => {
                    HEAP_FRAME_ORIGINAL_READ.set(f).ok();
                    vm::HostAdapterDescriptor::Static(heap_frame_capture_read)
                }
                vm::HostAdapterDescriptor::StaticStack(f) => {
                    HEAP_FRAME_ORIGINAL_READ.set(f).ok();
                    vm::HostAdapterDescriptor::StaticStack(heap_frame_capture_read)
                }
                vm::HostAdapterDescriptor::StaticStackRuntimeOwned(f) => {
                    HEAP_FRAME_ORIGINAL_READ.set(f).ok();
                    vm::HostAdapterDescriptor::StaticStackRuntimeOwned(heap_frame_capture_read)
                }
                _ => panic!("arrays::read must keep a real VM-aware static adapter"),
            };
        }
    }
    let mut registry = vm::HostFunctionRegistry::restricted();
    vm::HostModuleDescriptor::install_descriptors(&mut registry, &descriptors)
        .expect("unchanged descriptor installation");
    for descriptor in &descriptors {
        registry.authorize_registered_builtin_import(&descriptor.schema.name);
    }
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
    registry
        .allow_builtin("math::copysign")
        .expect("math::copysign should be allow-listable");
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bytes::from_utf8 should be allow-listable");
    registry
}

#[derive(Clone)]
enum HeapFrameEnd {
    Keep,
    Exact(Value),
    FramePlusInt(i64),
    FramePlusHalf,
}

#[derive(Clone)]
struct HeapFrameCase {
    label: &'static str,
    capacity: i64,
    class: Value,
    cap: Value,
    end: HeapFrameEnd,
    entry: i64,
    node: bool,
}

fn heap_frame_case(
    label: &'static str,
    capacity: i64,
    class: Value,
    cap: Value,
    end: HeapFrameEnd,
    entry: i64,
) -> HeapFrameCase {
    HeapFrameCase {
        label,
        capacity,
        class,
        cap,
        end,
        entry,
        node: false,
    }
}

fn heap_frame_value_expr(value: &Value) -> vm::Expr {
    match value {
        Value::Int(value) => vm::Expr::Int(*value),
        Value::Float(value) => vm::Expr::Float(*value),
        other => panic!("root frame metadata must be Int or Float, got {other:?}"),
    }
}

fn heap_frame_push(ir: &mut vm::FrontendIr, expr: vm::Expr) {
    ir.stmts.push(vm::Stmt::Expr { expr, line: 1 });
}

fn heap_frame_emit(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    case: &HeapFrameCase,
    accept: bool,
) -> Result<(), vm::ParseError> {
    let carrier = ctx.carrier;
    let binding = heap_binding_index(ir, "o")?;
    let (boxed, slot, generation) = heap_cycle_identity(ir, ctx, binding, "root-frame")?;
    let frame = heap_cycle_take(ir, "root-frame")?;
    ir.stmts.push(vm::Stmt::Let {
        index: frame,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.root_enter,
            vec![vm::Expr::Var(carrier), vm::Expr::Int(case.capacity)],
        ),
        line: 1,
    });
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        vm::Expr::Add(
            Box::new(vm::Expr::Var(frame)),
            Box::new(vm::Expr::Int(HEAP_RAW_CLASS_OFFSET)),
        ),
        heap_frame_value_expr(&case.class),
        "root-frame-class",
    )?;
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        vm::Expr::Add(
            Box::new(vm::Expr::Var(frame)),
            Box::new(vm::Expr::Int(HEAP_FRAME_CAPACITY_OFFSET)),
        ),
        heap_frame_value_expr(&case.cap),
        "root-frame-capacity",
    )?;
    let saved_end = match &case.end {
        HeapFrameEnd::Keep => None,
        HeapFrameEnd::Exact(value) => {
            let saved = heap_cycle_read_number(
                ir,
                ctx,
                carrier,
                vm::Expr::Int(HEAP_HDR_HEAP_END),
                "root-frame-end-saved",
            )?;
            heap_cycle_write(
                ir,
                ctx,
                carrier,
                vm::Expr::Int(HEAP_HDR_HEAP_END),
                heap_frame_value_expr(value),
                "root-frame-end",
            )?;
            Some(saved)
        }
        HeapFrameEnd::FramePlusInt(delta) => {
            let saved = heap_cycle_read_number(
                ir,
                ctx,
                carrier,
                vm::Expr::Int(HEAP_HDR_HEAP_END),
                "root-frame-end-saved",
            )?;
            heap_cycle_write(
                ir,
                ctx,
                carrier,
                vm::Expr::Int(HEAP_HDR_HEAP_END),
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(frame)),
                    Box::new(vm::Expr::Int(*delta)),
                ),
                "root-frame-end",
            )?;
            Some(saved)
        }
        HeapFrameEnd::FramePlusHalf => {
            let saved = heap_cycle_read_number(
                ir,
                ctx,
                carrier,
                vm::Expr::Int(HEAP_HDR_HEAP_END),
                "root-frame-end-saved",
            )?;
            heap_cycle_write(
                ir,
                ctx,
                carrier,
                vm::Expr::Int(HEAP_HDR_HEAP_END),
                vm::Expr::Add(
                    Box::new(heap_as_number(vm::Expr::Var(frame))),
                    Box::new(vm::Expr::Float(0.5)),
                ),
                "root-frame-end",
            )?;
            Some(saved)
        }
    };
    heap_frame_push(ir, vm::Expr::String(case.label.to_owned()));
    heap_frame_push(ir, vm::Expr::Var(carrier));
    heap_frame_push(ir, vm::Expr::Var(frame));
    heap_frame_push(ir, vm::Expr::Int(case.entry));
    if accept {
        let ack = heap_cycle_take(ir, "root-frame")?;
        let call = if case.node {
            heap_guest_call(
                heap_helper_index(ir, HEAP_ROOT_WRITE_NODE_NAME)?,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(frame),
                    vm::Expr::Int(case.entry),
                    vm::Expr::Int(HEAP_OBJECT_CATEGORY),
                    vm::Expr::Var(slot),
                    vm::Expr::Var(generation),
                ],
            )
        } else {
            heap_guest_call(
                ctx.root_write,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(frame),
                    vm::Expr::Int(case.entry),
                    vm::Expr::Var(boxed),
                ],
            )
        };
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: call,
            line: 1,
        });
        heap_frame_push(ir, vm::Expr::Var(ack));
        for cell in 0..HEAP_FRAME_ENTRY_CELLS {
            let raw = heap_cycle_read_number(
                ir,
                ctx,
                carrier,
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(frame)),
                    Box::new(vm::Expr::Int(
                        HEAP_FRAME_ENTRY_BASE
                            + case.entry * HEAP_FRAME_ENTRY_CELLS
                            + cell,
                    )),
                ),
                "root-frame-written",
            )?;
            heap_frame_push(ir, vm::Expr::Var(raw));
        }
        heap_frame_push(ir, vm::Expr::Var(slot));
        heap_frame_push(ir, vm::Expr::Var(generation));
        if let Some(saved) = saved_end {
            heap_cycle_write(
                ir,
                ctx,
                carrier,
                vm::Expr::Int(HEAP_HDR_HEAP_END),
                vm::Expr::Var(saved),
                "root-frame-end-restore",
            )?;
        }
    } else {
        // The refusal target address is physical even when the entry lies
        // outside the allocated capacity: entry == capacity lands just past
        // the frame extent and entry == -1 lands in adjacent header cells.
        // Read the real cells instead of inferring a cleared pretriple.
        for cell in 0..HEAP_FRAME_ENTRY_CELLS {
            let raw = heap_cycle_read_number(
                ir,
                ctx,
                carrier,
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(frame)),
                    Box::new(vm::Expr::Int(
                        HEAP_FRAME_ENTRY_BASE
                            + case.entry * HEAP_FRAME_ENTRY_CELLS
                            + cell,
                    )),
                ),
                "root-frame-pre",
            )?;
            heap_frame_push(ir, vm::Expr::Var(raw));
        }
        let ack = heap_cycle_take(ir, "root-frame")?;
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                ctx.root_write,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(frame),
                    vm::Expr::Int(case.entry),
                    vm::Expr::Var(boxed),
                ],
            ),
            line: 1,
        });
    }
    Ok(())
}

struct HeapFramePlugin {
    cases: Vec<HeapFrameCase>,
    accept: bool,
}

impl vm::SourcePlugin for HeapFramePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new(HEAP_ROOT_FRAME_ID);
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let mut ctx = heap_cycle_ctx(&ir)?;
        ctx.carrier = heap_cycle_carrier(&ir)?;
        for case in &self.cases {
            heap_frame_emit(&mut ir, &ctx, case, self.accept)?;
        }
        Ok(ir)
    }
}

fn heap_frame_raw_eq(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Float(lhs), Value::Float(rhs)) => lhs.to_bits() == rhs.to_bits(),
        _ => lhs == rhs,
    }
}

fn heap_frame_int(value: &Value, what: &str) -> i64 {
    match value {
        Value::Int(value) => *value,
        other => panic!("{what} must be Int, got {other:?}"),
    }
}

fn heap_frame_run(cases: &[HeapFrameCase], accept: bool) {
    for case in cases {
        heap_frame_run_case(case, accept);
    }
}

fn heap_frame_run_case(case: &HeapFrameCase, accept: bool) {
    let host_cases = vec![case.clone()];
    let plugin: &'static HeapFramePlugin = Box::leak(Box::new(HeapFramePlugin {
        cases: vec![case.clone()],
        accept,
    }));
    let catalog = heap_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        HEAP_ROOT_FRAME_SOURCE,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(HEAP_ROOT_FRAME_ID)),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(catalog),
    )
    .expect("root frame single-entry compilation");
    let mut machine = compilation
        .into_vm()
        .expect("certified root frame VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(10_000_000);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("initialize B1");
    heap_frame_registry()
        .bind_vm_cached(&mut machine)
        .expect("production adapters bind to one fresh VM");
    let outcome = machine.run();
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let error = match (&outcome, accept) {
        (Ok(status), true) => {
            assert_eq!(*status, VmStatus::Halted, "acceptance entry must halt");
            None
        }
        (Ok(status), false) => panic!("refusal entry halted unexpectedly: {status:?}"),
        (Err(error), false) => Some(error),
        (Err(error), true) => panic!("acceptance entry failed: {error:?}"),
    };
    if let Some(error) = error {
        assert!(
            heap_decode_gate_failed(error),
            "refusal must fail at the checked metadata gate, got {error:?}"
        );
    }
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("B1 remains accounted");
    assert_eq!((stats.pending, stats.registered), (0, 1));
    let stack = machine.stack().to_vec();
    let snapshot = heap_frame_snapshot(&mut machine);
    for case in &host_cases {
        let need = if accept { 10 } else { 7 };
        let marker = stack
            .iter()
            .position(|value| *value == Value::string(case.label))
            .unwrap_or_else(|| panic!("witness marker {label} missing", label = case.label));
        assert!(
            stack.len() >= marker + need,
            "{label} witness group incomplete: marker={marker} len={}",
            stack.len(),
            label = case.label
        );
        let group = &stack[marker + 1..marker + need];
        assert!(
            matches!(&group[0], Value::Map(_)),
            "{label} must push the real carrier map",
            label = case.label
        );
        let frame = heap_frame_int(&group[1], "frame address");
        let entry = heap_frame_int(&group[2], "entry");
        assert_eq!(entry, case.entry, "{label} entry witness", label = case.label);
        let frame_at = usize::try_from(frame).expect("frame address fits usize");
        assert!(
            frame_at < snapshot.len(),
            "{label} frame address outside the actual backing array",
            label = case.label
        );
        assert!(
            heap_frame_raw_eq(
                &snapshot[(frame + HEAP_RAW_CLASS_OFFSET) as usize],
                &case.class
            ),
            "{label} actual raw class cell differs from the stamped encoding",
            label = case.label
        );
        assert!(
            heap_frame_raw_eq(
                &snapshot[(frame + HEAP_FRAME_CAPACITY_OFFSET) as usize],
                &case.cap
            ),
            "{label} actual raw capacity cell differs from the stamped encoding",
            label = case.label
        );
        let expected_end = match &case.end {
            HeapFrameEnd::Keep => None,
            HeapFrameEnd::Exact(value) => Some(value.clone()),
            HeapFrameEnd::FramePlusInt(delta) => Some(Value::Int(frame + *delta)),
            HeapFrameEnd::FramePlusHalf => Some(Value::Float(frame as f64 + 0.5)),
        };
        if let Some(expected) = expected_end {
            if !accept {
                assert!(
                    heap_frame_raw_eq(&snapshot[HEAP_HDR_HEAP_END as usize], &expected),
                    "{label} actual raw heap-end cell differs from the stamped encoding",
                    label = case.label
                );
            }
        }
        let base_i64 = frame + HEAP_FRAME_ENTRY_BASE + case.entry * HEAP_FRAME_ENTRY_CELLS;
        assert!(
            base_i64 >= 0,
            "{label} destination address underflowed",
            label = case.label
        );
        let base = usize::try_from(base_i64).expect("destination address fits usize");
        assert!(
            base + HEAP_FRAME_ENTRY_CELLS as usize <= snapshot.len(),
            "{label} destination outside the actual backing array",
            label = case.label
        );
        let destination = &snapshot[base..base + HEAP_FRAME_ENTRY_CELLS as usize];
        let pre: &[Value] = if accept { &group[4..7] } else { &group[3..6] };
        assert!(
            destination
                .iter()
                .zip(pre)
                .all(|(actual, witness)| heap_frame_raw_eq(actual, witness)),
            "{label} refusal/accept destination witness mismatch",
            label = case.label
        );
        if accept {
            assert_eq!(
                heap_frame_int(&group[3], "ack"),
                1,
                "{label} must acknowledge",
                label = case.label
            );
            let slot = heap_frame_int(&group[7], "ref_slot");
            let generation = heap_frame_int(&group[8], "generation");
            assert!(
                heap_frame_raw_eq(
                    &destination[0],
                    &Value::Float(HEAP_OBJECT_CATEGORY as f64)
                ) && heap_frame_raw_eq(&destination[1], &Value::Float(slot as f64))
                    && heap_frame_raw_eq(&destination[2], &Value::Float(generation as f64)),
                "{label} actual raw triple differs from the Float-encoded [5, ref_slot, generation]",
                label = case.label
            );
        } else if case.entry >= 0 && case.entry < case.capacity {
            // Only a genuinely allocated destination starts as a cleared
            // pretriple; adjacent metadata may hold Int/nonzero cells and is
            // proven by the bit-exact pre/post witness instead.
            assert!(
                destination.iter().all(|value| matches!(
                    value,
                    Value::Float(value) if *value == 0.0
                )),
                "{label} legal-entry destination must stay the cleared pretriple",
                label = case.label
            );
        }
        eprintln!(
            "ROOT_FRAME_ONE_ENTRY label={label} accept={accept} capacity={capacity} class_cell={:?} cap_cell={:?} entry={entry} frame={frame} compile=1 vm=1 guest_run=1 host_snapshot=1 read_intercept=1",
            snapshot[(frame + HEAP_RAW_CLASS_OFFSET) as usize],
            snapshot[(frame + HEAP_FRAME_CAPACITY_OFFSET) as usize],
            label = case.label,
            accept = accept,
            capacity = case.capacity,
            entry = case.entry,
            frame = frame,
        );
    }
    machine
        .reset_for_reuse()
        .expect("reset root frame single-entry VM");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    eprintln!(
        "ROOT_FRAME_RESET label={label} accept={accept} resource_count=0 reset=1 post_reset_guest_run=0 read_intercept=1",
        label = case.label,
        accept = accept,
    );
}

#[test]
fn root_frame_refuses_non_root_class_encodings() {
    heap_frame_run(
        &[
            heap_frame_case("class-int-two", 2, Value::Int(2), Value::Int(2), HeapFrameEnd::Keep, 0),
            heap_frame_case("class-float-3.5", 2, Value::Float(3.5), Value::Int(2), HeapFrameEnd::Keep, 0),
            heap_frame_case("class-nan", 2, Value::Float(f64::NAN), Value::Int(2), HeapFrameEnd::Keep, 0),
            heap_frame_case("class-pos-inf", 2, Value::Float(f64::INFINITY), Value::Int(2), HeapFrameEnd::Keep, 0),
            heap_frame_case("class-neg-inf", 2, Value::Float(f64::NEG_INFINITY), Value::Int(2), HeapFrameEnd::Keep, 0),
        ],
        false,
    );
}

#[test]
fn root_frame_refuses_fractional_signed_and_nonfinite_capacity() {
    heap_frame_run(
        &[
            heap_frame_case("capacity-half", 1, Value::Float(HEAP_ROOT_CLASS), Value::Float(0.5), HeapFrameEnd::Keep, 0),
            heap_frame_case("capacity-negative", 1, Value::Float(HEAP_ROOT_CLASS), Value::Int(-1), HeapFrameEnd::Keep, 0),
            heap_frame_case("capacity-nan", 1, Value::Float(HEAP_ROOT_CLASS), Value::Float(f64::NAN), HeapFrameEnd::Keep, 0),
            heap_frame_case("capacity-pos-inf", 1, Value::Float(HEAP_ROOT_CLASS), Value::Float(f64::INFINITY), HeapFrameEnd::Keep, 0),
            heap_frame_case("capacity-neg-inf", 1, Value::Float(HEAP_ROOT_CLASS), Value::Float(f64::NEG_INFINITY), HeapFrameEnd::Keep, 0),
        ],
        false,
    );
}

#[test]
fn root_frame_refuses_budget_edges_and_non_root_extremes() {
    heap_frame_run(
        &[
            heap_frame_case("capacity-over-budget", 1, Value::Float(HEAP_ROOT_CLASS), Value::Int(HEAP_CELL_BUDGET + 1), HeapFrameEnd::Keep, 0),
            heap_frame_case("class-int-negative", 2, Value::Int(-1), Value::Int(2), HeapFrameEnd::Keep, 0),
            heap_frame_case("class-int-budget", 2, Value::Int(HEAP_CELL_BUDGET), Value::Int(2), HeapFrameEnd::Keep, 0),
            heap_frame_case("end-int-over-budget", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Exact(Value::Int(HEAP_CELL_BUDGET + 1)), 0),
            heap_frame_case("end-int-negative", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Exact(Value::Int(-1)), 0),
        ],
        false,
    );
}

#[test]
fn root_frame_refuses_end_domain_boundaries() {
    heap_frame_run(
        &[
            heap_frame_case("end-fraction", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Exact(Value::Float(0.5)), 0),
            heap_frame_case("end-poszero", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Exact(Value::Float(0.0)), 0),
            heap_frame_case("end-negzero", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Exact(Value::Float(-0.0)), 0),
            heap_frame_case("end-equals-frame", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::FramePlusInt(0), 0),
            heap_frame_case("end-frame-plus-half", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::FramePlusHalf, 0),
            heap_frame_case("end-neg-inf", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Exact(Value::Float(f64::NEG_INFINITY)), 0),
        ],
        false,
    );
}

#[test]
fn root_frame_refuses_entry_and_signed_zero_capacity() {
    heap_frame_run(
        &[
            heap_frame_case("entry-equals-capacity", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Keep, 2),
            heap_frame_case("entry-negative", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Keep, -1),
            heap_frame_case("capacity-zero-entry-zero", 1, Value::Float(HEAP_ROOT_CLASS), Value::Int(0), HeapFrameEnd::Keep, 0),
            heap_frame_case("capacity-negzero-entry-zero", 1, Value::Float(HEAP_ROOT_CLASS), Value::Float(-0.0), HeapFrameEnd::Keep, 0),
            heap_frame_case("end-nan", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Exact(Value::Float(f64::NAN)), 0),
            heap_frame_case("end-pos-inf", 2, Value::Float(HEAP_ROOT_CLASS), Value::Int(2), HeapFrameEnd::Exact(Value::Float(f64::INFINITY)), 0),
        ],
        false,
    );
}

#[test]
fn root_frame_accepts_class_and_capacity_boundaries() {
    heap_frame_run(
        &[heap_frame_case(
            "class-int-three",
            2,
            Value::Int(3),
            Value::Int(2),
            HeapFrameEnd::Keep,
            1,
        )],
        true,
    );
    heap_frame_run(
        &[heap_frame_case(
            "class-float-three",
            2,
            Value::Float(HEAP_ROOT_CLASS),
            Value::Float(2.0),
            HeapFrameEnd::Keep,
            1,
        )],
        true,
    );
    heap_frame_run(
        &[heap_frame_case(
            "capacity-budget-entry-0",
            1,
            Value::Float(HEAP_ROOT_CLASS),
            Value::Int(HEAP_CELL_BUDGET),
            HeapFrameEnd::Keep,
            0,
        )],
        true,
    );
    heap_frame_run(
        &[heap_frame_case(
            "capacity-float-budget-entry-0",
            1,
            Value::Float(HEAP_ROOT_CLASS),
            Value::Float(HEAP_CELL_BUDGET as f64),
            HeapFrameEnd::Keep,
            0,
        )],
        true,
    );
    heap_frame_run(
        &[heap_frame_case(
            "capacity-16-last-entry",
            16,
            Value::Float(HEAP_ROOT_CLASS),
            Value::Int(16),
            HeapFrameEnd::Keep,
            15,
        )],
        true,
    );
}

#[test]
fn root_frame_accepts_end_and_edge_boundaries() {
    heap_frame_run(
        &[heap_frame_case(
            "end-float-budget",
            2,
            Value::Float(HEAP_ROOT_CLASS),
            Value::Int(2),
            HeapFrameEnd::Exact(Value::Float(HEAP_CELL_BUDGET as f64)),
            0,
        )],
        true,
    );
    heap_frame_run(
        &[heap_frame_case(
            "end-int-budget",
            2,
            Value::Float(HEAP_ROOT_CLASS),
            Value::Int(2),
            HeapFrameEnd::Exact(Value::Int(HEAP_CELL_BUDGET)),
            0,
        )],
        true,
    );
    heap_frame_run(
        &[heap_frame_case(
            "end-frame-plus-one",
            2,
            Value::Float(HEAP_ROOT_CLASS),
            Value::Int(2),
            HeapFrameEnd::FramePlusInt(1),
            0,
        )],
        true,
    );
    heap_frame_run(
        &[HeapFrameCase {
            label: "edge-root-write-node",
            capacity: 2,
            class: Value::Float(HEAP_ROOT_CLASS),
            cap: Value::Int(2),
            end: HeapFrameEnd::Keep,
            entry: 1,
            node: true,
        }],
        true,
    );
}

/// Current-production IR witness for the String gray-drain tail. The planned
/// H6 shape replaces the two unconditional tail assignments with one
/// `if (kind == KIND_STRING) { gray = g_next } else { rdn = read(13);
/// gray = decode(rdn) }`. This test parses one existing admitted heap source
/// through the real plugin; it does not run a VM and does not add a producer.
#[test]
fn p7_string_gray_tail_reuses_validated_next_program_structure() {
    use vm::SourcePlugin;

    // Local read-only IR views over the actual parsed helper body.
    fn p7_number(expr: &vm::Expr) -> &vm::Expr {
        if let vm::Expr::Div(inner, divisor) = expr
            && matches!(divisor.as_ref(), vm::Expr::Float(value) if *value == 1.0)
        {
            inner.as_ref()
        } else {
            expr
        }
    }

    fn p7_call<'a>(expr: &'a vm::Expr) -> Option<(u16, &'a [vm::Expr])> {
        match p7_number(expr) {
            vm::Expr::Call(index, type_args, args, None, None) if type_args.is_empty() => {
                Some((*index, args.as_slice()))
            }
            _ => None,
        }
    }

    fn p7_stmt_call<'a>(stmt: &'a vm::Stmt) -> Option<(u16, &'a [vm::Expr])> {
        match stmt {
            vm::Stmt::Let { expr, .. }
            | vm::Stmt::Assign { expr, .. }
            | vm::Stmt::Expr { expr, .. } => p7_call(expr),
            _ => None,
        }
    }

    fn p7_stmts<'a>(roots: &'a [vm::Stmt], out: &mut Vec<&'a vm::Stmt>) {
        let mut stack: Vec<&'a vm::Stmt> = roots.iter().collect();
        while let Some(stmt) = stack.pop() {
            out.push(stmt);
            match stmt {
                vm::Stmt::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    stack.extend(then_branch);
                    stack.extend(else_branch);
                }
                vm::Stmt::While { body, .. } => stack.extend(body),
                vm::Stmt::For {
                    init,
                    post,
                    body,
                    ..
                } => {
                    stack.push(init.as_ref());
                    stack.push(post.as_ref());
                    stack.extend(body);
                }
                _ => {}
            }
        }
    }

    fn p7_assert_condition<'a>(stmt: &'a vm::Stmt) -> Option<&'a vm::Expr> {
        let vm::Stmt::Expr {
            expr: vm::Expr::Call(index, type_args, args, None, None),
            ..
        } = stmt
        else {
            return None;
        };
        if *index != vm::BuiltinFunction::Assert.call_index()
            || !type_args.is_empty()
            || args.len() != 1
        {
            return None;
        }
        Some(&args[0])
    }

    fn p7_eq(expr: &vm::Expr) -> Option<(u16, i64)> {
        let vm::Expr::Eq(left, right) = expr else {
            return None;
        };
        let vm::Expr::Var(slot) = left.as_ref() else {
            return None;
        };
        let vm::Expr::Int(value) = right.as_ref() else {
            return None;
        };
        Some((*slot, *value))
    }

    let catalog = heap_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let ir = JAVASCRIPT_ES5_PLUGIN
        .parse_source(
            "var f=function(){\"use strict\";return 7;};var o=f();",
            &context,
        )
        .expect("real admitted heap source must parse through the production JS plugin");
    let collect_index = heap_helper_index(&ir, HEAP_COLLECT_NAME)
        .expect("production plugin must declare the heap collector helper");
    let collect = ir
        .function_impls
        .get(&collect_index)
        .expect("production collector must carry its real implementation");

    // Locate the actual Phase-3 gray drain by its real opening statement:
    // `s = gray - 1` inside the only While with that shape.
    let (gray_at, _s_slot, gray_slot) = collect
        .body_stmts
        .iter()
        .enumerate()
        .find_map(|(at, stmt)| {
            let vm::Stmt::While { body, .. } = stmt else {
                return None;
            };
            let vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: s,
                expr: vm::Expr::Sub(left, right),
                ..
            } = body.first()?
            else {
                return None;
            };
            let vm::Expr::Var(gray) = left.as_ref() else {
                return None;
            };
            if !matches!(right.as_ref(), vm::Expr::Int(1)) {
                return None;
            }
            Some((at, *s, *gray))
        })
        .expect("production collector must contain the actual gray-drain While");
    let vm::Stmt::While {
        body: gray_body, ..
    } = &collect.body_stmts[gray_at]
    else {
        unreachable!("the gray-drain locator selected a While");
    };

    // The validated gray-next pair is the real read/decode immediately after
    // the row lookup. All slot identities below are derived from this IR.
    let read_address = |expr: &vm::Expr| -> Option<(u16, u16, u16, i64)> {
        let (index, args) = p7_call(expr)?;
        if args.len() != 2 {
            return None;
        }
        let vm::Expr::Var(carrier) = &args[0] else {
            return None;
        };
        let vm::Expr::Div(address, divisor) = &args[1] else {
            return None;
        };
        if !matches!(divisor.as_ref(), vm::Expr::Float(value) if *value == 1.0) {
            return None;
        }
        let vm::Expr::Add(base, offset) = address.as_ref() else {
            return None;
        };
        let vm::Expr::Var(addr) = base.as_ref() else {
            return None;
        };
        let vm::Expr::Int(offset) = offset.as_ref() else {
            return None;
        };
        Some((index, *carrier, *addr, *offset))
    };
    let mut next_read = None;
    for (at, stmt) in gray_body.iter().enumerate() {
        let vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            ..
        } = stmt
        else {
            continue;
        };
        if let Some((read, call_carrier, addr, 5)) = read_address(expr) {
            next_read = Some((at, *index, read, call_carrier, addr));
            break;
        }
    }
    let (next_read_at, rdn_slot, read_idx, carrier, g_addr) = next_read
        .expect("collector gray body must read the row's ROW_GRAY_NEXT_PLUS_ONE cell");
    let decode_at = next_read_at + 1;
    let vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: g_next,
        expr: decode,
        ..
    } = &gray_body[decode_at]
    else {
        panic!("collector gray body must decode the validated gray-next cell immediately");
    };
    let (decode_idx, decode_args) = p7_call(decode)
        .expect("validated gray-next decode must be the actual production decoder call");
    assert_eq!(
        decode_args.len(),
        1,
        "gray-next decoder takes the raw cell only"
    );
    let vm::Expr::Var(decoded_rdn) = &decode_args[0] else {
        panic!("gray-next decoder must consume the shared raw-cell slot");
    };
    assert_eq!(
        *decoded_rdn, rdn_slot,
        "gray-next decode must consume the row-cell read"
    );
    let g_next = *g_next;

    // New H6 tail: one String-reuse IfElse. On current production the final
    // statement is still the unconditional Assign pair, so this is the RED.
    let tail_at = gray_body.len() - 1;
    let tail = &gray_body[tail_at];
    let vm::Stmt::IfElse {
        condition: tail_condition,
        then_branch: string_tail,
        else_branch: non_string_tail,
        ..
    } = tail
    else {
        panic!(
            "H6 requires the gray-drain tail to be one String-reuse IfElse; \
             current production still ends with the unconditional Assign pair"
        );
    };
    let (tail_kind, tail_value) = p7_eq(tail_condition)
        .expect("H6 String-reuse tail condition must be Eq(kind, KIND_STRING)");
    assert_eq!(
        tail_value, 2,
        "KIND_STRING is the closed private node-kind value 2"
    );
    assert_eq!(
        string_tail.len(),
        1,
        "String gray-drain tail reuses only the validated next"
    );
    let vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: reused_gray,
        expr: reused_expr,
        ..
    } = &string_tail[0]
    else {
        panic!("String gray-drain tail must be one plain gray = g_next assignment");
    };
    assert_eq!(
        *reused_gray, gray_slot,
        "String tail writes the drained gray local"
    );
    assert!(
        matches!(reused_expr, vm::Expr::Var(slot) if *slot == g_next),
        "String tail must reuse the validated g_next without another header read or decode: {reused_expr:?}"
    );
    assert_eq!(
        non_string_tail.len(),
        2,
        "nonString gray-drain tail keeps the fresh header read and decode"
    );
    let vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: fresh_rdn,
        expr: fresh_read,
        ..
    } = &non_string_tail[0]
    else {
        panic!("nonString gray-drain tail must refresh rdn from header cell 13");
    };
    assert_eq!(
        *fresh_rdn, rdn_slot,
        "nonString tail reuses the shared raw-cell slot"
    );
    let (fresh_read_idx, fresh_read_args) =
        p7_call(fresh_read).expect("nonString tail header refresh is a real read call");
    assert_eq!(
        fresh_read_idx, read_idx,
        "nonString tail must read through the actual production read helper"
    );
    assert_eq!(fresh_read_args.len(), 2, "read helper keeps carrier and address");
    let vm::Expr::Var(fresh_carrier) = &fresh_read_args[0] else {
        panic!("nonString header read must pass the actual carrier");
    };
    assert_eq!(*fresh_carrier, carrier, "nonString header carrier is unchanged");
    let vm::Expr::Div(fresh_addr, fresh_unit) = &fresh_read_args[1] else {
        panic!("nonString header address must stay Number-normalized");
    };
    assert!(
        matches!(fresh_addr.as_ref(), vm::Expr::Int(13)),
        "nonString tail must freshly read HDR_GRAY_HEAD_PLUS_ONE (13)"
    );
    assert!(
        matches!(fresh_unit.as_ref(), vm::Expr::Float(value) if *value == 1.0),
        "header read address keeps the canonical divide-by-one normalization"
    );
    let vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: fresh_gray,
        expr: fresh_decode,
        ..
    } = &non_string_tail[1]
    else {
        panic!("nonString gray-drain tail must decode the fresh header read");
    };
    assert_eq!(
        *fresh_gray, gray_slot,
        "nonString tail decodes back into the drained gray local"
    );
    let (fresh_decode_idx, fresh_decode_args) =
        p7_call(fresh_decode).expect("nonString tail header decode is a real decoder call");
    assert_eq!(
        fresh_decode_idx, decode_idx,
        "nonString tail must decode through the actual production Int decoder"
    );
    assert_eq!(
        fresh_decode_args.len(),
        1,
        "header decoder consumes the shared raw-cell slot only"
    );
    let vm::Expr::Var(fresh_decoded) = &fresh_decode_args[0] else {
        panic!("nonString header decode must consume the refreshed raw-cell slot");
    };
    assert_eq!(
        *fresh_decoded, rdn_slot,
        "nonString header decode must consume the fresh rdn write"
    );

    // The header write is unique, canonical and precedes the String trace.
    let mut header_write = None;
    for (at, stmt) in gray_body.iter().enumerate() {
        let vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            expr,
            ..
        } = stmt
        else {
            continue;
        };
        let Some((write_idx, args)) = p7_call(expr) else {
            continue;
        };
        if args.len() != 3 {
            continue;
        }
        let vm::Expr::Var(write_carrier) = &args[0] else {
            continue;
        };
        let vm::Expr::Div(address, unit) = &args[1] else {
            continue;
        };
        if !matches!(unit.as_ref(), vm::Expr::Float(value) if *value == 1.0) {
            continue;
        }
        let vm::Expr::Int(header) = address.as_ref() else {
            continue;
        };
        if *header != 13 {
            continue;
        }
        let vm::Expr::Div(payload, payload_unit) = &args[2] else {
            continue;
        };
        let vm::Expr::Var(payload_slot) = payload.as_ref() else {
            continue;
        };
        if !matches!(payload_unit.as_ref(), vm::Expr::Float(value) if *value == 1.0) {
            continue;
        }
        header_write = Some((at, write_idx, *write_carrier, *payload_slot));
        break;
    }
    let (write_at, write_idx, write_carrier, write_payload) = header_write
        .expect("collector gray drain must write HDR_GRAY_HEAD_PLUS_ONE exactly once");
    assert_eq!(
        write_carrier, carrier,
        "header write must use the actual carrier"
    );
    assert_eq!(
        write_payload, g_next,
        "header write payload must be as_number(validated g_next)"
    );
    assert!(
        decode_at < write_at,
        "validated g_next read/decode must precede the header write"
    );

    // Base/kind are read after the header refresh and before the trace branch.
    let mut base_read = None;
    for (at, stmt) in gray_body.iter().enumerate().skip(write_at + 1) {
        let vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            ..
        } = stmt
        else {
            continue;
        };
        if let Some((read, call_carrier, addr, 2)) = read_address(expr) {
            if read == read_idx && call_carrier == carrier && addr == g_addr {
                base_read = Some((at, *index));
                break;
            }
        }
    }
    let (base_read_at, base_rdn) = base_read
        .expect("collector gray drain must read ROW_PHYSICAL_BASE after the header write");
    assert_eq!(
        base_rdn, rdn_slot,
        "base read reuses the shared raw-cell slot"
    );
    let base_decode_at = base_read_at + 1;
    let vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: base,
        expr: base_decode,
        ..
    } = &gray_body[base_decode_at]
    else {
        panic!("base read must decode immediately through the actual Int decoder");
    };
    let (base_decode_idx, base_decode_args) =
        p7_call(base_decode).expect("base decode is a real decoder call");
    assert_eq!(base_decode_idx, decode_idx, "base decode keeps the Int decoder");
    assert_eq!(
        base_decode_args.len(),
        1,
        "base decoder consumes the shared raw-cell slot only"
    );
    let vm::Expr::Var(base_rdn_arg) = &base_decode_args[0] else {
        panic!("base decoder must consume the base read slot");
    };
    assert_eq!(*base_rdn_arg, rdn_slot, "base decoder consumes the base read");
    let base = *base;
    let kind_decode_at = base_decode_at + 1;
    let vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: kind,
        expr: kind_decode,
        ..
    } = &gray_body[kind_decode_at]
    else {
        panic!("collector must decode kind from the base cell immediately");
    };
    let (kind_decode_idx, kind_decode_args) =
        p7_call(kind_decode).expect("kind decode is a real decoder call");
    assert_eq!(kind_decode_idx, decode_idx, "kind decode keeps the Int decoder");
    assert_eq!(
        kind_decode_args.len(),
        1,
        "kind decoder takes the base-cell read only"
    );
    let (kind_read_idx, kind_read_args) =
        p7_call(&kind_decode_args[0]).expect("kind decode must consume a real base-cell read");
    assert_eq!(
        kind_read_idx, read_idx,
        "kind reads the base cell through the actual production read helper"
    );
    assert_eq!(kind_read_args.len(), 2, "kind read keeps carrier and address");
    let vm::Expr::Var(kind_carrier) = &kind_read_args[0] else {
        panic!("kind base-cell read must pass the actual carrier");
    };
    assert_eq!(*kind_carrier, carrier, "kind read carrier is unchanged");
    let vm::Expr::Div(kind_addr, kind_unit) = &kind_read_args[1] else {
        panic!("kind base-cell address must stay Number-normalized");
    };
    assert!(
        matches!(kind_addr.as_ref(), vm::Expr::Var(slot) if *slot == base),
        "kind read must address the decoded base local"
    );
    assert!(
        matches!(kind_unit.as_ref(), vm::Expr::Float(value) if *value == 1.0),
        "kind read address keeps canonical normalization"
    );
    let kind = *kind;
    assert_eq!(
        tail_kind, kind,
        "String-reuse tail must dispatch on the actual decoded kind"
    );

    // Closed 1..=7 kind domain gate remains before the trace dispatch.
    let mut domain_at = None;
    for (at, stmt) in gray_body.iter().enumerate().skip(kind_decode_at + 1) {
        let Some(condition) = p7_assert_condition(stmt) else {
            continue;
        };
        let vm::Expr::And(low, high) = condition else {
            continue;
        };
        let vm::Expr::Not(low_guard) = low.as_ref() else {
            continue;
        };
        let vm::Expr::Lt(low_kind, low_bound) = low_guard.as_ref() else {
            continue;
        };
        let vm::Expr::Not(high_guard) = high.as_ref() else {
            continue;
        };
        let vm::Expr::Gt(high_kind, high_bound) = high_guard.as_ref() else {
            continue;
        };
        let vm::Expr::Var(low_var) = low_kind.as_ref() else {
            continue;
        };
        let vm::Expr::Int(low_value) = low_bound.as_ref() else {
            continue;
        };
        let vm::Expr::Var(high_var) = high_kind.as_ref() else {
            continue;
        };
        let vm::Expr::Int(high_value) = high_bound.as_ref() else {
            continue;
        };
        if *low_var == kind
            && *low_value == 1
            && *high_var == kind
            && *high_value == 7
        {
            domain_at = Some(at);
            break;
        }
    }
    let domain_at =
        domain_at.expect("collector must gate kind to the closed 1..=7 domain");
    assert!(
        kind_decode_at < domain_at,
        "kind decode must precede its domain gate"
    );

    // The existing String trace branch stays empty; six nonString arms stay.
    let mut trace_at = None;
    for (at, stmt) in gray_body.iter().enumerate().skip(domain_at + 1) {
        let vm::Stmt::IfElse {
            condition,
            then_branch,
            ..
        } = stmt
        else {
            continue;
        };
        if then_branch.is_empty() && p7_eq(condition) == Some((kind, 2)) {
            trace_at = Some(at);
            break;
        }
    }
    let trace_at = trace_at.expect("String trace branch must exist before the tail");
    assert!(
        domain_at < trace_at,
        "kind domain gate must precede the trace dispatch"
    );
    assert!(
        trace_at < tail_at,
        "String-reuse tail must follow the trace dispatch"
    );
    let vm::Stmt::IfElse {
        then_branch: string_arm,
        else_branch: trace_else,
        ..
    } = &gray_body[trace_at]
    else {
        unreachable!("the trace locator selected an IfElse");
    };
    assert!(
        string_arm.is_empty(),
        "String trace arm must stay empty (strings carry no edges)"
    );
    let mut level: &[vm::Stmt] = trace_else.as_slice();
    let mut trace_conditions = Vec::new();
    let mut non_string_arms: Vec<&[vm::Stmt]> = Vec::new();
    loop {
        if let [vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        }] = level
        {
            let vm::Expr::Eq(left, right) = condition else {
                panic!("nonString trace dispatch must stay Eq(kind, arm)");
            };
            let vm::Expr::Var(cond_kind) = left.as_ref() else {
                panic!("nonString trace dispatch must compare the kind local");
            };
            let vm::Expr::Int(cond_value) = right.as_ref() else {
                panic!("nonString trace dispatch must compare against a kind constant");
            };
            assert_eq!(*cond_kind, kind, "trace arm must test the actual kind local");
            assert!(!then_branch.is_empty(), "every nonString arm stays populated");
            trace_conditions.push(*cond_value);
            non_string_arms.push(then_branch.as_slice());
            level = else_branch.as_slice();
        } else {
            non_string_arms.push(level);
            break;
        }
    }
    assert_eq!(
        trace_conditions,
        vec![1, 3, 4, 6, 7],
        "the nonString dispatch keeps array/object/function/tagged-array/bytes arms"
    );
    assert_eq!(
        non_string_arms.len(),
        6,
        "array, object, function, tagged-array, bytes and environment arms remain"
    );
    assert!(
        !non_string_arms.iter().any(|arm| arm.is_empty()),
        "all six nonString trace arms remain populated"
    );
    let env_condition = p7_assert_condition(&non_string_arms[5][0])
        .expect("environment arm keeps its own kind gate");
    assert_eq!(
        p7_eq(env_condition),
        Some((kind, 5)),
        "environment arm keeps the KIND_ENVIRONMENT gate"
    );

    // Marker calls keep their five-argument context in every nonString arm.
    let mut array_stmts = Vec::new();
    p7_stmts(non_string_arms[0], &mut array_stmts);
    let mut array_marks = 0usize;
    let mut mark_idx = None;
    let mut next_slot = None;
    for stmt in &array_stmts {
        let Some((call_index, args)) = p7_stmt_call(*stmt) else {
            continue;
        };
        if args.len() != 5 {
            continue;
        }
        let vm::Expr::Var(call_carrier) = &args[0] else {
            continue;
        };
        let vm::Expr::Var(call_next) = &args[4] else {
            continue;
        };
        if *call_carrier != carrier {
            continue;
        }
        array_marks += 1;
        mark_idx = Some(call_index);
        next_slot = Some(*call_next);
    }
    assert_eq!(
        array_marks, 1,
        "array trace arm keeps one five-argument marker call"
    );
    let mark_idx = mark_idx.expect("array trace arm must call the production marker");
    let next_slot = next_slot.expect("marker handoff must name the drain-next local");
    let mut arm_marks = Vec::new();
    for arm in &non_string_arms {
        let mut stmts = Vec::new();
        p7_stmts(*arm, &mut stmts);
        let mut count = 0usize;
        for stmt in &stmts {
            let Some((call_index, args)) = p7_stmt_call(*stmt) else {
                continue;
            };
            if call_index != mark_idx {
                continue;
            }
            assert_eq!(args.len(), 5, "marker keeps its five-argument context");
            let vm::Expr::Var(arg_carrier) = &args[0] else {
                panic!("marker must pass the actual carrier");
            };
            assert_eq!(*arg_carrier, carrier, "marker carrier identity is unchanged");
            let vm::Expr::Var(arg_next) = &args[4] else {
                panic!("marker must pass the drain-next local");
            };
            assert_eq!(*arg_next, next_slot, "marker next identity is unchanged");
            count += 1;
        }
        assert!(count >= 1, "every nonString arm must retain its marker calls");
        arm_marks.push(count);
    }
    assert_eq!(arm_marks[4], 1, "bytes trace only their prototype edge");
    assert_eq!(arm_marks.len(), 6, "all six nonString arms remain");

    // Object/function arms keep their real checked-base proofs; the common
    // gray segment never routes through checked_base.
    let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)
        .expect("production plugin must declare the checked-base helper");
    let count_checked = |arm: &[vm::Stmt]| -> usize {
        let mut stmts = Vec::new();
        p7_stmts(arm, &mut stmts);
        let mut count = 0usize;
        for stmt in &stmts {
            if let Some((index, args)) = p7_stmt_call(*stmt)
                && index == checked_base
                && args.len() == 3
            {
                count += 1;
            }
        }
        count
    };
    assert!(
        count_checked(non_string_arms[1]) >= 1,
        "object arm keeps its checked-base proofs"
    );
    assert!(
        count_checked(non_string_arms[2]) >= 1,
        "function arm keeps its checked-base proofs"
    );
    for arm in [non_string_arms[1], non_string_arms[2]] {
        let mut stmts = Vec::new();
        p7_stmts(arm, &mut stmts);
        for stmt in &stmts {
            let Some((index, args)) = p7_stmt_call(*stmt) else {
                continue;
            };
            if index != checked_base {
                continue;
            }
            assert_eq!(args.len(), 3, "checked-base keeps its three-slot context");
            let vm::Expr::Var(arg_carrier) = &args[0] else {
                panic!("checked-base must pass the actual carrier");
            };
            assert_eq!(*arg_carrier, carrier, "checked-base carrier identity is unchanged");
        }
    }
    let mut prefix_stmts = Vec::new();
    p7_stmts(&gray_body[..trace_at], &mut prefix_stmts);
    let mut pre_checked = 0usize;
    for stmt in &prefix_stmts {
        if let Some((index, _)) = p7_stmt_call(*stmt)
            && index == checked_base
        {
            pre_checked += 1;
        }
    }
    assert_eq!(
        pre_checked, 0,
        "the common gray segment must not route through checked_base"
    );

    // Exactly one header-13 read in the whole drain, only in the new tail else;
    // exactly one header-13 write, before the String trace.
    let mut flat = Vec::new();
    p7_stmts(gray_body, &mut flat);
    let mut header_reads = 0usize;
    let mut header_writes = 0usize;
    for stmt in &flat {
        let Some((index, args)) = p7_stmt_call(*stmt) else {
            continue;
        };
        if index == write_idx && args.len() == 3 {
            let vm::Expr::Var(call_carrier) = &args[0] else {
                continue;
            };
            let vm::Expr::Div(address, unit) = &args[1] else {
                continue;
            };
            if *call_carrier == carrier
                && matches!(unit.as_ref(), vm::Expr::Float(value) if *value == 1.0)
                && matches!(address.as_ref(), vm::Expr::Int(13))
            {
                header_writes += 1;
            }
        }
        if index == read_idx && args.len() == 2 {
            let vm::Expr::Var(call_carrier) = &args[0] else {
                continue;
            };
            let vm::Expr::Div(address, unit) = &args[1] else {
                continue;
            };
            if *call_carrier == carrier
                && matches!(unit.as_ref(), vm::Expr::Float(value) if *value == 1.0)
                && matches!(address.as_ref(), vm::Expr::Int(13))
            {
                header_reads += 1;
            }
        }
    }
    assert_eq!(
        header_writes, 1,
        "the gray drain must write header cell 13 exactly once"
    );
    assert_eq!(
        header_reads, 1,
        "the gray drain must read header cell 13 exactly once, only in the nonString tail"
    );
    assert!(
        write_at < trace_at,
        "header-13 write must precede the String trace arm"
    );
    let mut between_writes = 0usize;
    for stmt in &gray_body[write_at + 1..trace_at] {
        if let Some((index, _)) = p7_stmt_call(stmt)
            && index == write_idx
        {
            between_writes += 1;
        }
    }
    assert_eq!(
        between_writes, 0,
        "no write may run between the header refresh and the empty String trace arm"
    );

    // The nonzero page-pointer guard remains ahead of the actual row address.
    let mut addr_bind = None;
    for (at, stmt) in gray_body.iter().enumerate().take(next_read_at) {
        let vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr: vm::Expr::IfElse { condition, .. },
            ..
        } = stmt
        else {
            continue;
        };
        if *index != g_addr {
            continue;
        }
        let Some((page_ptr, zero)) = p7_eq(condition) else {
            continue;
        };
        if zero != 0 {
            continue;
        }
        addr_bind = Some((at, page_ptr));
        break;
    }
    let (addr_at, page_ptr) = addr_bind
        .expect("gray row lookup must bind g_addr through the page-pointer guard");
    let require = gray_body[..addr_at].iter().find_map(|stmt| {
        let condition = p7_assert_condition(stmt)?;
        let vm::Expr::Not(inner) = condition else {
            return None;
        };
        let vm::Expr::Eq(left, right) = inner.as_ref() else {
            return None;
        };
        let vm::Expr::Var(slot) = left.as_ref() else {
            return None;
        };
        let vm::Expr::Int(value) = right.as_ref() else {
            return None;
        };
        if *slot == page_ptr && *value == 0 {
            Some(())
        } else {
            None
        }
    });
    assert!(
        require.is_some(),
        "nonzero page-pointer guard must precede the guarded row address"
    );
    assert!(
        addr_at < next_read_at,
        "the actual row read must follow the guarded row address"
    );
}

/// Test-first IR witness for the Phase-4 sweep's row-state producer. The
/// sweep currently binds `st` through one unconditional decoder call; the
/// contract is that the existing `rdn = read(addr + ROW_STATE)` value feeds
/// the same phase-1 `IfElse(Eq(<rdn>, 2.0), Int(2), decode_int(<rdn>))`
/// shortcut while the row-mark read, class decode and sweep gate stay put.
/// The Float(2.0) label stays exact: the comparison keeps its physical-Float
/// spelling and no as_number projection is added, so a physical Int-valued 2
/// still reaches the original decoder. The witness is pointwise and does not assume phase 1
/// screened every row reaching the sweep. Parses one already-admitted heap
/// source through the real production plugin only: no VM/Core execution, no
/// observer plugin, no producer added here, and no new fast path in
/// `__compat_es5_decode_int`.
#[test]
fn p7_phase4_active_state_fastpath_program_structure() {
    use vm::SourcePlugin;

    // Read-only IR views over the actual parsed collector body.
    fn p7_projection(expr: &vm::Expr) -> &vm::Expr {
        if let vm::Expr::Div(inner, unit) = expr
            && matches!(unit.as_ref(), vm::Expr::Float(value) if *value == 1.0)
        {
            inner.as_ref()
        } else {
            expr
        }
    }
    fn p7_call<'a>(expr: &'a vm::Expr) -> Option<(u16, &'a [vm::Expr])> {
        match expr {
            vm::Expr::Call(index, type_args, args, None, None) if type_args.is_empty() => {
                Some((*index, args.as_slice()))
            }
            _ => None,
        }
    }
    /// The scratch keeps either spelling: Var or Div(Var, 1.0).
    fn p7_scratch(expr: &vm::Expr) -> Option<u16> {
        match p7_projection(expr) {
            vm::Expr::Var(slot) => Some(*slot),
            _ => None,
        }
    }
    /// A folded zero offset still names the row's own ROW_STATE cell.
    fn p7_address(expr: &vm::Expr) -> Option<(u16, i64)> {
        match p7_projection(expr) {
            vm::Expr::Var(base) => Some((*base, 0)),
            vm::Expr::Add(base, offset) => match (base.as_ref(), offset.as_ref()) {
                (vm::Expr::Var(base), vm::Expr::Int(offset)) => Some((*base, *offset)),
                _ => None,
            },
            _ => None,
        }
    }
    fn p7_read(expr: &vm::Expr, read: u16, carrier: u16) -> Option<(u16, i64)> {
        let (index, args) = p7_call(expr)?;
        if index != read
            || args.len() != 2
            || !matches!(&args[0], vm::Expr::Var(slot) if *slot == carrier)
        {
            return None;
        }
        p7_address(&args[1])
    }
    fn p7_decode_arg<'a>(expr: &'a vm::Expr, decode: u16) -> Option<&'a vm::Expr> {
        let (index, args) = p7_call(expr)?;
        if index == decode && args.len() == 1 {
            Some(&args[0])
        } else {
            None
        }
    }
    fn p7_eq_int(expr: &vm::Expr, slot: u16, value: i64) -> bool {
        matches!(
            expr,
            vm::Expr::Eq(left, right)
                if matches!(left.as_ref(), vm::Expr::Var(name) if *name == slot)
                    && matches!(right.as_ref(), vm::Expr::Int(stored) if *stored == value)
        )
    }
    /// Every statement list the collector nests in its loops/branches.
    fn p7_lists<'a>(body: &'a [vm::Stmt], out: &mut Vec<&'a [vm::Stmt]>) {
        out.push(body);
        for stmt in body {
            match stmt {
                vm::Stmt::While { body, .. } => p7_lists(body, out),
                vm::Stmt::For { body, .. } => p7_lists(body, out),
                vm::Stmt::IfElse { then_branch, else_branch, .. } => {
                    p7_lists(then_branch, out);
                    p7_lists(else_branch, out);
                }
                _ => {}
            }
        }
    }

    let catalog = heap_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let ir = JAVASCRIPT_ES5_PLUGIN
        .parse_source(
            "var f=function(){\"use strict\";return 7;};var o=f();",
            &context,
        )
        .expect("real admitted heap source must parse through the production JS plugin");
    let collect_index = heap_helper_index(&ir, HEAP_COLLECT_NAME)
        .expect("production plugin must declare the heap collector helper");
    let read = heap_helper_index(&ir, HEAP_READ_NAME)
        .expect("production plugin must declare the B1 read helper");
    let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)
        .expect("production plugin must declare the slot decoder");
    let collect = ir
        .function_impls
        .get(&collect_index)
        .expect("production collector must carry its real implementation");
    let carrier = collect.param_slots[0];

    let mut lists = Vec::new();
    p7_lists(&collect.body_stmts, &mut lists);
    // The phase-4 triple is the only adjacent `rdn = read(addr + ROW_STATE)`,
    // state producer, `cls = decode(read(addr + ROW_MARK))` run: the phase-1
    // producer is instead followed by its row-state domain assert.
    let mut phases = Vec::new();
    for (list_at, list) in lists.iter().enumerate() {
        for at in 0..list.len().saturating_sub(2) {
            let (
                vm::Stmt::Assign { kind: vm::AssignmentKind::Set, index: read_slot,
                    expr: state_read, .. },
                vm::Stmt::Assign { kind: vm::AssignmentKind::Set, index: producer_slot,
                    expr: _, .. },
                vm::Stmt::Assign { kind: vm::AssignmentKind::Set, index: cls_slot,
                    expr: cls_read, .. },
            ) = (&list[at], &list[at + 1], &list[at + 2]) else {
                continue;
            };
            let Some((state_addr, state_cell)) = p7_read(state_read, read, carrier) else {
                continue;
            };
            let Some(decoded_cls) = p7_decode_arg(cls_read, decode) else {
                continue;
            };
            let Some((cls_addr, cls_cell)) = p7_read(decoded_cls, read, carrier) else {
                continue;
            };
            if state_cell != 0 || state_addr != cls_addr || cls_cell != 4 {
                continue;
            }
            phases.push((list_at, at, *read_slot, *producer_slot, *cls_slot));
        }
    }
    assert_eq!(
        phases.len(), 1,
        "the collector must contain exactly one phase-4 read/state/class triple, got {phases:?}"
    );
    let (list_at, at, rdn, st, cls) = phases[0];
    let list = lists[list_at];

    // Target assert. Current production binds `st` with one unconditional
    // decoder call, so the original code fails here.
    let vm::Stmt::Assign { index: producer_slot, expr: producer, .. } = &list[at + 1] else {
        unreachable!("the phase-4 locator selected an Assign producer");
    };
    let vm::Expr::IfElse { condition, then_expr, else_expr } = producer else {
        panic!(
            "phase4 requires active-state shortcut; current producer is unconditional decode; \
             slot {st} still binds {producer:?}"
        )
    };
    assert_eq!(*producer_slot, st, "the producer writes the located state slot");
    let vm::Expr::Eq(condition_left, condition_right) = condition.as_ref() else {
        panic!("the active-state condition must compare the scratch with 2.0, got {condition:?}")
    };
    assert!(
        matches!(condition_right.as_ref(), vm::Expr::Float(value) if *value == 2.0),
        "the active-state condition compares Float(2.0) (STATE_ACTIVE), got {condition_right:?}"
    );
    assert!(
        matches!(then_expr.as_ref(), vm::Expr::Int(2)),
        "the exact active branch yields Int(2), got {then_expr:?}"
    );
    let Some(fallback) = p7_decode_arg(else_expr, decode) else {
        panic!("the non-active fallback keeps the original decoder call, got {else_expr:?}")
    };
    assert_eq!(
        p7_scratch(condition_left), Some(rdn),
        "the comparison reads the existing ROW_STATE scratch {rdn}, got {condition_left:?}"
    );
    assert_eq!(
        p7_scratch(fallback), Some(rdn),
        "the decoder fallback keeps the same scratch {rdn}, got {fallback:?}"
    );
    let declared = |slot: u16, schema: &vm::TypeSchema| {
        lists.iter().any(|list| list.iter().any(|stmt| {
            matches!(stmt, vm::Stmt::Let { index, declared_schema: Some(declared), .. }
                if *index == slot && declared == schema)
        }))
    };
    assert!(
        declared(rdn, &vm::TypeSchema::Number),
        "the state read stays bound to the pre-existing Number scratch {rdn}"
    );
    assert!(
        declared(st, &vm::TypeSchema::Int),
        "the state slot {st} stays the Int local"
    );
    assert!(
        ir.functions.iter().any(|function| {
            function.index == decode && function.return_schema == Some(vm::TypeSchema::Int)
        }),
        "the original slot decoder keeps its Int result"
    );
    // The phase-1 producer stays separate: exactly the phase-1 and phase-4
    // state slots are written through the active-state conditional.
    let conditional = lists.iter().copied().flatten().filter(|stmt| {
        matches!(stmt, vm::Stmt::Assign { index, expr: vm::Expr::IfElse { .. }, .. }
            if *index == st)
    }).count();
    assert_eq!(
        conditional, 2,
        "the phase-1 and phase-4 shortcuts stay two separate producers, got {conditional}"
    );
    // The outgoing class read and sweep gate are untouched.
    let Some(vm::Stmt::IfElse { condition: gate, .. }) = list.get(at + 3) else {
        panic!("the sweep gate must immediately follow the class read, got {:?}", list.get(at + 3))
    };
    let vm::Expr::And(lower, upper) = gate else {
        panic!("the sweep gate must conjoin active state and unmarked class, got {gate:?}")
    };
    assert!(
        p7_eq_int(lower.as_ref(), st, 2),
        "the sweep gate still tests st == STATE_ACTIVE(2), got {lower:?}"
    );
    assert!(
        p7_eq_int(upper.as_ref(), cls, 0),
        "the sweep gate still tests cls == 0 (unmarked), got {upper:?}"
    );
}
#[test]
fn number_constant_descriptor_survives_real_gc() {
    let reads = ["before", "after", "descriptor", "own"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let source = |count: u32| format!(
        "var alias=Number; var before=alias.MIN_VALUE; var i=0; while(i<{count}){{({{a:1}});i++;}} var after=alias['MIN_VALUE']; var d=Object.getOwnPropertyDescriptor(alias,'MIN_VALUE'); var descriptor=d.value; var own=alias.hasOwnProperty('MIN_VALUE') && d.writable===false && d.enumerable===false && d.configurable===false;"
    );
    let (_, baseline_gc, _, _) = heap_readback_run(&source(0), reads.clone());
    let (values, pressure_gc, _, _) = heap_readback_run(&source(40), reads);
    assert!(pressure_gc > baseline_gc, "baseline={baseline_gc} pressure={pressure_gc}");
    for value in &values[..3] {
        assert_eq!(heap_box_number(value).to_bits(), 1);
    }
    assert_eq!(heap_box_tag(&values[3]), 2);
    let Value::Map(fields) = &values[3] else { unreachable!() };
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
    eprintln!("NUMBER_CONSTANT_GC baseline={baseline_gc} pressure={pressure_gc}");
}
