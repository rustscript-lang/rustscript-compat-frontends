//! Math.min/max argument forwarding through Function.prototype.call/apply.
//! Each script contains one entry form only, so a second same-feature
//! occurrence cannot open the intrinsic gate for it.

use vm::{Value, VmStatus};

fn run_cookie(source: &str) -> bool {
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| {
            panic!("production compilation refused; source={source:?} error={error:?}")
        });
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    let status = machine.run().unwrap_or_else(|error| {
        panic!("guest run refused; source={source:?} error={error:?}")
    });
    assert_eq!(status, VmStatus::Halted, "source={source:?}");
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}; source={source:?}");
    };
    assert_eq!(
        fields.get(&Value::string("tag")),
        Some(&Value::Int(2)),
        "cookie must be a guest Boolean: {source:?} value={value:?}"
    );
    match fields.get(&Value::string("flag")) {
        Some(Value::Bool(flag)) => *flag,
        other => panic!("expected Boolean flag, got {other:?}: {source:?}"),
    }
}

/// The third numeric actual must decide. Truncation to the declared arity 2
/// would answer 2, and the intrinsic argument gate answers with TypeError
/// before dispatch.
#[test]
fn math_max_call_third_argument_decides() {
    let source = "Math.max.call(null,1,2,9)===9;";
    assert!(
        run_cookie(source),
        "Math.max.call must forward the decisive third actual"
    );
}

/// The third numeric actual must decide. Truncation to the declared arity 2
/// would answer 1.
#[test]
fn math_min_call_third_argument_decides() {
    let source = "Math.min.call(null,1,2,-9)===-9;";
    assert!(
        run_cookie(source),
        "Math.min.call must forward the decisive third actual"
    );
}

/// The third array element must decide. Truncation to the declared arity 2
/// would answer 2.
#[test]
fn math_max_apply_third_argument_decides() {
    let source = "Math.max.apply(null,[1,2,9])===9;";
    assert!(
        run_cookie(source),
        "Math.max.apply must expand the decisive third element"
    );
}

/// The third array element must decide. Truncation to the declared arity 2
/// would answer 1.
#[test]
fn math_min_apply_third_argument_decides() {
    let source = "Math.min.apply(null,[1,2,-9])===-9;";
    assert!(
        run_cookie(source),
        "Math.min.apply must expand the decisive third element"
    );
}

#[test]
fn math_max_call_empty_is_negative_infinity() {
    assert!(run_cookie("Math.max.call(null)===-Infinity;"));
}

#[test]
fn math_min_call_empty_is_positive_infinity() {
    assert!(run_cookie("Math.min.call(null)===Infinity;"));
}

/// Third actual selects +0. The first two are -0, so truncation would keep -0.
#[test]
fn math_max_call_third_actual_selects_positive_zero() {
    assert!(run_cookie("1/Math.max.call(null,-0,-0,0)===Infinity;"));
}

/// Third actual selects -0. The first two are +0, so truncation would keep +0.
#[test]
fn math_min_call_third_actual_selects_negative_zero() {
    assert!(run_cookie("1/Math.min.call(null,0,0,-0)===-Infinity;"));
}

/// Third actual is NaN, so the result must be NaN rather than 2.
#[test]
fn math_max_call_third_actual_is_nan() {
    assert!(run_cookie("var x=Math.max.call(null,1,2,NaN);x!==x;"));
}

#[test]
fn math_max_apply_empty_is_negative_infinity() {
    assert!(run_cookie("Math.max.apply(null,[])===-Infinity;"));
}

#[test]
fn math_min_apply_empty_is_positive_infinity() {
    assert!(run_cookie("Math.min.apply(null,[])===Infinity;"));
}

#[test]
fn math_max_apply_third_element_selects_positive_zero() {
    assert!(run_cookie("1/Math.max.apply(null,[-0,-0,0])===Infinity;"));
}

#[test]
fn math_min_apply_third_element_selects_negative_zero() {
    assert!(run_cookie("1/Math.min.apply(null,[0,0,-0])===-Infinity;"));
}

#[test]
fn math_max_apply_third_element_is_nan() {
    assert!(run_cookie("var x=Math.max.apply(null,[1,2,NaN]);x!==x;"));
}

/// Non-array apply stays on the pinned TypeError path. One `.apply` only.
#[test]
fn math_max_apply_array_like_is_type_error() {
    let source = "try{Math.max.apply(null,{length:3,0:1,1:2,2:9});}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "array-like apply must stay a TypeError");
}

/// The shared 65535 ceiling still refuses before expansion.
#[test]
fn math_max_apply_over_limit_is_type_error() {
    let source = "try{Math.max.apply(null,new Array(65536));}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "65536 apply arguments must stay a TypeError");
}

/// Ordinary numeric intrinsics keep the old extra-argument refusal.
#[test]
fn math_abs_call_extra_argument_stays_refused() {
    let source = "try{Math.abs.call(null,-3,9);}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "Math.abs.call must keep the old argument gate");
}

/// Object methods are not extrema and keep the old extra-argument refusal.
#[test]
fn object_to_string_call_extra_argument_stays_refused() {
    let source = "try{Object.prototype.toString.call({},1);}catch(e){e.name===\"TypeError\";}";
    assert!(
        run_cookie(source),
        "Object.prototype.toString.call must keep the old argument gate"
    );
}

/// The existing Error.prototype.toString carve-out still admits extra arguments.
/// `Error` is not a value binding here, so the method is taken from an instance.
#[test]
fn error_to_string_call_extra_arguments_stay_admitted() {
    let source = "var e=new Error(\"boom\");e.toString.call(e,1,2)===\"Error: boom\";";
    assert!(run_cookie(source), "Error toString call carve-out must stay");
}

/// Function.prototype itself stays non-callable.
#[test]
fn function_prototype_call_stays_refused() {
    let source = "function f(){return 1;}var p=Object.getPrototypeOf(f);try{p.call(null,1);}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "Function.prototype.call must stay a TypeError");
}

/// A typed array is not a dense/tagged Array argument vector.
#[test]
fn math_min_apply_typed_array_is_type_error() {
    let source = "try{Math.min.apply(null,new Uint8Array(0));}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "typed-array apply must stay a TypeError");
}

// ---------------------------------------------------------------------------
// GC-lifetime witnesses for the extrema `.apply` argument snapshot.
//
// The extrema expansion copies every element of the source Array into a Core
// `Array<JsValue>`, which never traces guest payloads. A real collection that
// runs while the elements are converted therefore frees any later reference
// element that is no longer reachable from the source Array. The first
// element's `valueOf` below truncates the source Array and then executes one
// real `__compat_es5_heap_collect` before it returns, sampling the carrier's
// collection counter on both sides of that call. Without dynamic guest roots
// the later element is only reachable through the Core snapshot, so the
// collector retires it and the numeric dispatch reads a dead slot/generation.
// ---------------------------------------------------------------------------

use pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN;
use std::sync::Mutex;
use vm::{FrontendIr, SourceFrontendContext, SourceFrontendDescriptor};

const OBSERVE_LIVE_NODES: i64 = 0;
const OBSERVE_ACTIVE_ROOT_CAPACITY: i64 = 4;
const OBSERVE_COLLECTIONS: i64 = 5;
const OBSERVE_ACTIVE_ROOTS: i64 = 12;

const READOUTS_RESULT: &[&str] = &["result", "order", "built", "cleared"];

fn call_expr(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, Vec::new(), args, None, None)
}

fn helper_index(ir: &FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing guest helper {name}"))
        .index
}

fn fresh_slot(ir: &mut FrontendIr) -> u16 {
    let slot = u16::try_from(ir.locals).expect("observer slot");
    ir.locals += 1;
    slot
}

fn observe_expr(observe: u16, carrier: u16, which: i64) -> vm::Expr {
    call_expr(
        observe,
        vec![
            vm::Expr::Var(carrier),
            vm::Expr::Int(which),
            vm::Expr::Int(0),
        ],
    )
}

fn observe_let(ir: &mut FrontendIr, observe: u16, carrier: u16, which: i64) -> u16 {
    let slot = fresh_slot(ir);
    ir.stmts.push(vm::Stmt::Let {
        index: slot,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: observe_expr(observe, carrier, which),
        line: 1,
    });
    slot
}

fn push_slot(ir: &mut FrontendIr, slot: u16) {
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(slot),
        line: 1,
    });
}

fn walk_stmts(stmts: &[vm::Stmt], visit: &mut impl FnMut(&vm::Stmt)) {
    for stmt in stmts {
        visit(stmt);
        match stmt {
            vm::Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                walk_stmts(then_branch, visit);
                walk_stmts(else_branch, visit);
            }
            vm::Stmt::While { body, .. } | vm::Stmt::For { body, .. } => {
                walk_stmts(body, visit)
            }
            _ => {}
        }
    }
}

/// Compile-time shape certificate for the extrema `.apply` reservation:
/// exactly one `root_enter(carrier, capacity)`, `capacity = 4 + R`, one dynamic
/// `root_write` at entry `4 + count`, and the pre-dispatch
/// `assert(count == R)`. The fixed prologue keeps its three constant entries.
fn assert_extrema_root_shape(ir: &FrontendIr) {
    let apply = helper_index(ir, "__compat_es5_function_apply");
    let root_enter = helper_index(ir, "__compat_es5_root_enter");
    let root_write = helper_index(ir, "__compat_es5_root_write");
    let implementation = ir
        .function_impls
        .get(&apply)
        .expect("apply implementation");
    let mut reservation = None;
    let mut reservations = 0usize;
    for stmt in &implementation.body_stmts {
        if let vm::Stmt::Assign {
            index,
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == root_enter
        {
            reservations += 1;
            assert_eq!(args.len(), 2, "root_enter takes carrier and capacity");
            match args.get(1) {
                Some(vm::Expr::Var(capacity)) => reservation = Some(*capacity),
                other => panic!("extrema reservation must be dynamic, got {other:?}"),
            }
            let _ = index;
        }
    }
    assert_eq!(reservations, 1, "apply owns exactly one root reservation");
    let capacity = reservation.expect("dynamic reservation capacity");
    let mut reserved = None;
    walk_stmts(&implementation.body_stmts, &mut |stmt| {
        if let vm::Stmt::Assign { index, expr, .. } = stmt
            && *index == capacity
            && let vm::Expr::Add(left, right) = expr
            && matches!(left.as_ref(), vm::Expr::Int(4))
            && let vm::Expr::Var(slot) = right.as_ref()
        {
            reserved = Some(*slot);
        }
    });
    let reserved = reserved.expect("capacity is assigned 4 + R");
    let mut counter = None;
    let mut dynamic_writes = 0usize;
    walk_stmts(&implementation.body_stmts, &mut |stmt| {
        if let vm::Stmt::Assign {
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == root_write
            && args.len() == 4
            && let Some(vm::Expr::Add(left, right)) = args.get(2)
            && matches!(left.as_ref(), vm::Expr::Int(4))
            && let vm::Expr::Var(slot) = right.as_ref()
        {
            dynamic_writes += 1;
            counter = Some(*slot);
        }
    });
    assert_eq!(
        dynamic_writes, 1,
        "exactly one dynamic root write at 4 + count"
    );
    let counter = counter.expect("dynamic root entry is 4 + count");
    let mut guarded = false;
    walk_stmts(&implementation.body_stmts, &mut |stmt| {
        if let vm::Stmt::Expr {
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == vm::BuiltinFunction::Assert.call_index()
            && matches!(args.as_slice(), [vm::Expr::Eq(left, right)]
                if matches!(left.as_ref(), vm::Expr::Var(slot) if *slot == counter)
                && matches!(right.as_ref(), vm::Expr::Var(slot) if *slot == reserved))
        {
            guarded = true;
        }
    });
    assert!(
        guarded,
        "the expansion must assert written roots == R before dispatch"
    );
}

/// True when `stmts` (recursively) call `target`.
fn calls_helper(stmts: &[vm::Stmt], target: u16) -> bool {
    stmts.iter().any(|stmt| match stmt {
        vm::Stmt::Let { expr, .. } | vm::Stmt::Assign { expr, .. } | vm::Stmt::Expr { expr, .. } => {
            expr_calls_helper(expr, target)
        }
        vm::Stmt::IfElse {
            then_branch,
            else_branch,
            ..
        } => calls_helper(then_branch, target) || calls_helper(else_branch, target),
        vm::Stmt::While { body, .. } | vm::Stmt::For { body, .. } => calls_helper(body, target),
        _ => false,
    })
}

fn expr_calls_helper(expr: &vm::Expr, target: u16) -> bool {
    match expr {
        vm::Expr::Call(index, _, args, _, _) => {
            *index == target || args.iter().any(|arg| expr_calls_helper(arg, target))
        }
        vm::Expr::Block { stmts, expr } => {
            calls_helper(stmts, target) || expr_calls_helper(expr, target)
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            expr_calls_helper(value, target)
                || expr_calls_helper(ok_expr, target)
                || expr_calls_helper(err_expr, target)
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_calls_helper(condition, target)
                || expr_calls_helper(then_expr, target)
                || expr_calls_helper(else_expr, target)
        }
        _ => false,
    }
}

/// True when `expr` contains a dispatch call with a literal `null` thisArg.
/// The bind creation and the extrema `.apply` both use that shape; the test
/// takes the last such top-level statement, which is the apply.
fn dispatches_with_null_this(expr: &vm::Expr, dispatch: u16) -> bool {
    match expr {
        vm::Expr::Call(index, _, args, _, _) => {
            (*index == dispatch
                && args.len() == 4
                && matches!(args.get(2), Some(vm::Expr::Null)))
                || args
                    .iter()
                    .any(|arg| dispatches_with_null_this(arg, dispatch))
        }
        vm::Expr::Block { stmts, expr } => {
            stmts
                .iter()
                .any(|stmt| stmt_dispatches_with_null_this(stmt, dispatch))
                || dispatches_with_null_this(expr, dispatch)
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            dispatches_with_null_this(value, dispatch)
                || dispatches_with_null_this(ok_expr, dispatch)
                || dispatches_with_null_this(err_expr, dispatch)
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            dispatches_with_null_this(condition, dispatch)
                || dispatches_with_null_this(then_expr, dispatch)
                || dispatches_with_null_this(else_expr, dispatch)
        }
        _ => false,
    }
}

fn stmt_dispatches_with_null_this(stmt: &vm::Stmt, dispatch: u16) -> bool {
    match stmt {
        vm::Stmt::Let { expr, .. }
        | vm::Stmt::Assign { expr, .. }
        | vm::Stmt::Expr { expr, .. } => dispatches_with_null_this(expr, dispatch),
        vm::Stmt::IfElse {
            then_branch,
            else_branch,
            ..
        } => {
            then_branch
                .iter()
                .any(|stmt| stmt_dispatches_with_null_this(stmt, dispatch))
                || else_branch
                    .iter()
                    .any(|stmt| stmt_dispatches_with_null_this(stmt, dispatch))
        }
        vm::Stmt::While { body, .. } | vm::Stmt::For { body, .. } => body
            .iter()
            .any(|stmt| stmt_dispatches_with_null_this(stmt, dispatch)),
        _ => false,
    }
}

static WINDOW_LEDGER: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn window_ledger() -> Vec<String> {
    WINDOW_LEDGER.lock().expect("window ledger").clone()
}

struct ExtremaGcObserver {
    /// Insert the real collect window into the first source function (`fn_0`),
    /// whose body overwrites the source Array element, immediately before its
    /// completion binding. The body is structurally checked to contain the
    /// array element store, so the window cannot silently land in an
    /// unrelated function.
    collect_in_first_value_of: bool,
    /// Trailing collect after the extrema frame chain has returned, to show
    /// that only snapshot-kept objects become reclaimable.
    reclaim_probe: bool,
    /// Top-level bindings exposed as final stack values.
    readouts: &'static [&'static str],
}

impl ExtremaGcObserver {
    fn build(
        &self,
        source: &str,
        context: &SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let observe = helper_index(&ir, "__compat_es5_heap_observe");
        let make = helper_index(&ir, "__compat_ref_make");
        let carrier = ir
            .stmts
            .iter()
            .find_map(|stmt| match stmt {
                vm::Stmt::Let {
                    index,
                    expr: vm::Expr::Call(target, _, _, _, _),
                    ..
                } if *target == make => Some(*index),
                _ => None,
            })
            .expect("realm carrier");
        assert_extrema_root_shape(&ir);
        // The top-level apply dispatch statement is the last `var` binding
        // whose expression dispatches with a literal null thisArg (the bind
        // creation earlier in the same script also matches, the apply is
        // later). Sampling active root capacity immediately before it and at
        // script exit proves the whole frame chain was released.
        let dispatch = helper_index(&ir, "__compat_es5_dispatch");
        let apply_at = ir
            .stmts
            .iter()
            .enumerate()
            .filter_map(|(at, stmt)| match stmt {
                vm::Stmt::Let { expr, .. } if dispatches_with_null_this(expr, dispatch) => {
                    Some(at)
                }
                _ => None,
            })
            .last();
        let pre_capacity = apply_at.map(|at| {
            let slot = fresh_slot(&mut ir);
            ir.stmts.insert(
                at,
                vm::Stmt::Let {
                    index: slot,
                    declared_schema: Some(vm::TypeSchema::Int),
                    expr: observe_expr(observe, carrier, OBSERVE_ACTIVE_ROOT_CAPACITY),
                    line: 1,
                },
            );
            slot
        });
        if self.collect_in_first_value_of {
            let value_of = helper_index(&ir, "__compat_es5_fn_0");
            let truncate = helper_index(&ir, "__compat_es5_array_put");
            let collect = helper_index(&ir, "__compat_es5_heap_collect");
            let completion =
                vm::TypeSchema::Named("__CompatEs5Completion".to_owned(), Vec::new());
            let before = fresh_slot(&mut ir);
            let sink = fresh_slot(&mut ir);
            let after = fresh_slot(&mut ir);
            let implementation = ir
                .function_impls
                .get_mut(&value_of)
                .expect("first source function implementation");
            let function_carrier = implementation.param_slots[0];
            if !calls_helper(&implementation.body_stmts, truncate) {
                let names: Vec<String> = ir
                    .functions
                    .iter()
                    .filter(|function| {
                        calls_helper(&implementation.body_stmts, function.index)
                    })
                    .map(|function| function.name.clone())
                    .collect();
                panic!(
                    "__compat_es5_fn_0 must be the valueOf whose body overwrites the source Array element; calls={names:?}"
                );
            }
            let at = implementation
                .body_stmts
                .iter()
                .rposition(|stmt| {
                    matches!(
                        stmt,
                        vm::Stmt::Let {
                            declared_schema: Some(schema),
                            ..
                        } if *schema == completion
                    )
                })
                .expect("source completion binding");
            let body_len = implementation.body_stmts.len();
            implementation.body_stmts.splice(
                at..at,
                vec![
                    vm::Stmt::Let {
                        index: before,
                        declared_schema: Some(vm::TypeSchema::Int),
                        expr: observe_expr(observe, function_carrier, OBSERVE_COLLECTIONS),
                        line: 1,
                    },
                    vm::Stmt::Let {
                        index: sink,
                        declared_schema: Some(vm::TypeSchema::Int),
                        expr: call_expr(collect, vec![vm::Expr::Var(function_carrier)]),
                        line: 1,
                    },
                    vm::Stmt::Let {
                        index: after,
                        declared_schema: Some(vm::TypeSchema::Int),
                        expr: observe_expr(observe, function_carrier, OBSERVE_COLLECTIONS),
                        line: 1,
                    },
                    vm::Stmt::Expr {
                        expr: call_expr(
                            vm::BuiltinFunction::Assert.call_index(),
                            vec![vm::Expr::Eq(
                                Box::new(vm::Expr::Var(after)),
                                Box::new(vm::Expr::Add(
                                    Box::new(vm::Expr::Var(before)),
                                    Box::new(vm::Expr::Int(1)),
                                )),
                            )],
                        ),
                        line: 1,
                    },
                ],
            );
            WINDOW_LEDGER.lock().expect("window ledger").push(format!(
                "window=__compat_es5_fn_0 insert_at={at} body_len={body_len} \
                 before_slot={before} after_slot={after} assert=collections+1"
            ));
        }
        if self.reclaim_probe {
            let live_before = observe_let(&mut ir, observe, carrier, OBSERVE_LIVE_NODES);
            let collect = helper_index(&ir, "__compat_es5_heap_collect");
            let sink = fresh_slot(&mut ir);
            ir.stmts.push(vm::Stmt::Let {
                index: sink,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: call_expr(collect, vec![vm::Expr::Var(carrier)]),
                line: 1,
            });
            let live_after = observe_let(&mut ir, observe, carrier, OBSERVE_LIVE_NODES);
            push_slot(&mut ir, live_before);
            push_slot(&mut ir, live_after);
        }
        let end_collections = observe_let(&mut ir, observe, carrier, OBSERVE_COLLECTIONS);
        let end_capacity = observe_let(&mut ir, observe, carrier, OBSERVE_ACTIVE_ROOT_CAPACITY);
        let end_roots = observe_let(&mut ir, observe, carrier, OBSERVE_ACTIVE_ROOTS);
        push_slot(&mut ir, end_collections);
        push_slot(&mut ir, end_capacity);
        push_slot(&mut ir, end_roots);
        if let Some(pre_capacity) = pre_capacity {
            push_slot(&mut ir, pre_capacity);
        } else {
            let fallback = observe_let(&mut ir, observe, carrier, OBSERVE_ACTIVE_ROOT_CAPACITY);
            push_slot(&mut ir, fallback);
        }
        for name in self.readouts {
            let slot = ir
                .local_bindings
                .iter()
                .rfind(|(binding, _)| binding == name)
                .map(|(_, slot)| *slot)
                .unwrap_or_else(|| panic!("missing top-level binding {name}"));
            push_slot(&mut ir, slot);
        }
        Ok(ir)
    }
}

impl vm::SourcePlugin for ExtremaGcObserver {
    fn descriptor(&self) -> SourceFrontendDescriptor {
        // Keep the production frontend id: the compiler resolves the source
        // plugin by the flavor's frontend id, so a renamed descriptor would
        // only fail as `MissingFrontendPlugin`.
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }

    fn parse_source(
        &self,
        source: &str,
        context: &SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, vm::ParseError> {
        self.build(source, context)
    }
}

static GC_READOUTS: ExtremaGcObserver = ExtremaGcObserver {
    collect_in_first_value_of: true,
    reclaim_probe: false,
    readouts: READOUTS_RESULT,
};

static NO_GC_READOUTS: ExtremaGcObserver = ExtremaGcObserver {
    collect_in_first_value_of: false,
    reclaim_probe: true,
    readouts: READOUTS_RESULT,
};

static GC_THROW: ExtremaGcObserver = ExtremaGcObserver {
    collect_in_first_value_of: true,
    reclaim_probe: true,
    readouts: &["same", "order", "thirdRan", "cleared"],
};

static GC_NESTED: ExtremaGcObserver = ExtremaGcObserver {
    collect_in_first_value_of: true,
    reclaim_probe: false,
    readouts: &["result", "order", "inner", "cleared"],
};

fn install_cli_host_services(machine: &mut vm::Vm) {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let mut registry = vm::HostFunctionRegistry::new();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact host catalog entry");
    }
    for builtin in [
        "math::floor",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
    ] {
        registry
            .allow_builtin(builtin)
            .unwrap_or_else(|error| panic!("allow {builtin}: {error}"));
    }
    registry.bind_vm_cached(machine).expect("host registry bind");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned array initialization");
}

fn run_observed(
    source: &str,
    plugin: &'static dyn vm::SourcePlugin,
) -> (Result<VmStatus, vm::VmError>, Vec<Value>) {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog)
        .with_source_plugin(plugin);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        pd_vm_compat_frontends::javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("plugin compile refused: {error:?}\nsource={source}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    install_cli_host_services(&mut machine);
    machine.set_fuel(10_000_000);
    let status = machine.run();
    let stack = machine.stack().to_vec();
    (status, stack)
}

fn stack_number_at(stack: &[Value], at: usize) -> f64 {
    assert!(
        at < stack.len(),
        "stack has {} values, wanted index {at} from the front",
        stack.len()
    );
    match &stack[at] {
        Value::Int(value) => *value as f64,
        Value::Float(value) => *value,
        Value::Bool(value) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Value::Map(fields) => match fields.get(&Value::string("tag")) {
            Some(Value::Int(3)) => match fields.get(&Value::string("number")) {
                Some(Value::Float(value)) => *value,
                Some(Value::Int(value)) => *value as f64,
                other => panic!("boxed Number payload {other:?}"),
            },
            Some(Value::Int(2)) => match fields.get(&Value::string("flag")) {
                Some(Value::Bool(true)) => 1.0,
                Some(Value::Bool(false)) => 0.0,
                other => panic!("boxed Boolean flag {other:?}"),
            },
            other => panic!("unexpected boxed primitive tag {other:?}"),
        },
        other => panic!("expected a numeric stack value, got {other:?}"),
    }
}

fn stack_number(stack: &[Value], from_end: usize) -> f64 {
    assert!(
        from_end < stack.len(),
        "stack has {} values, wanted index {from_end} from the end",
        stack.len()
    );
    stack_number_at(stack, stack.len() - 1 - from_end)
}

fn halted_or_report(
    source: &str,
    status: Result<VmStatus, vm::VmError>,
    stack: &[Value],
) -> VmStatus {
    match status {
        Ok(status) => status,
        Err(error) => panic!(
            "guest run refused: {error:?}\nsource={source}\nstack={stack:?}\nwindow_ledger={:?}",
            window_ledger()
        ),
    }
}

fn eval_number(source: &str) -> f64 {
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("compile refused: {error:?}\nsource={source}"));
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    assert_eq!(
        machine.run().unwrap_or_else(|error| panic!(
            "guest run refused: {error:?}\nsource={source}"
        )),
        VmStatus::Halted
    );
    let stack = machine.stack();
    let last = stack
        .len()
        .checked_sub(1)
        .unwrap_or_else(|| panic!("no completion value for {source}"));
    stack_number_at(stack, last)
}

/// Source order: `shrink` is the first function expression, so the plugin's
/// `__compat_es5_fn_0` window lands exactly between its `s[1] = null`
/// overwrite and its return. `first.valueOf` is a bound product: source
/// functions are closed, so the array arrives as a bound parameter instead of
/// a free variable.
const EXTREMA_GC_SCRIPT_MAX: &str = r#"var shrink = function(s, f){ "use strict"; f.built = f.built + 1; s[1] = null; f.order = f.order * 10 + 1; return 2; };
var first = { order: 0, built: 0 };
var source = [ first, { link: first, valueOf: function(){ "use strict"; this.link.order = this.link.order * 10 + 2; return 8; } } ];
first.valueOf = shrink.bind(null, source, first);
var result = Math.max.apply(null, source);
var order = first.order;
var built = first.built;
var cleared = source[1] === null;
result;"#;

const EXTREMA_GC_SCRIPT_MIN: &str = r#"var shrink = function(s, f){ "use strict"; f.built = f.built + 1; s[1] = null; f.order = f.order * 10 + 1; return 2; };
var first = { order: 0, built: 0 };
var source = [ first, { link: first, valueOf: function(){ "use strict"; this.link.order = this.link.order * 10 + 2; return 8; } } ];
first.valueOf = shrink.bind(null, source, first);
var result = Math.min.apply(null, source);
var order = first.order;
var built = first.built;
var cleared = source[1] === null;
result;"#;

const EXTREMA_PRE_TRUNCATED_MAX: &str = r#"var first = { order: 0, built: 0, valueOf: function(){ "use strict"; this.built = this.built + 1; this.order = this.order * 10 + 1; return 2; } };
var source = [ first, { link: first, valueOf: function(){ "use strict"; this.link.order = this.link.order * 10 + 2; return 8; } } ];
source.length = 1;
var result = Math.max.apply(null, source);
var order = first.order;
var built = first.built;
var cleared = source[1] === null;
result;"#;

fn assert_extrema_gc_run(
    label: &str,
    source: &str,
    expected_result: f64,
    expected_order: f64,
    expected_cleared: f64,
    expects_collection: bool,
    status: Result<VmStatus, vm::VmError>,
    stack: &[Value],
) {
    assert_eq!(
        halted_or_report(source, status, stack),
        VmStatus::Halted,
        "{label}"
    );
    // Tail: script completion, end counters, pre-apply capacity, then
    // readouts in order [result, order, built, cleared].
    let cleared = stack_number(stack, 0);
    let built = stack_number(stack, 1);
    let order = stack_number(stack, 2);
    let result = stack_number(stack, 3);
    let pre_capacity = stack_number(stack, 4);
    let end_roots = stack_number(stack, 5);
    let end_capacity = stack_number(stack, 6);
    let end_collections = stack_number(stack, 7);
    eprintln!(
        "EXTREMA_APPLY_GC {label} result={result} expected={expected_result} order={order} \
         built={built} cleared={cleared} collections_end={end_collections} \
         capacity={pre_capacity}->{end_capacity} roots_end={end_roots} ledger={:?}",
        window_ledger()
    );
    assert_eq!(result, expected_result, "{label}: later element must convert");
    assert_eq!(order, expected_order, "{label}: element conversion order");
    assert_eq!(built, 1.0, "{label}: first valueOf runs exactly once");
    assert_eq!(cleared, expected_cleared, "{label}: source overwrite marker");
    if expects_collection {
        assert!(
            end_collections > 0.0,
            "{label}: the in-window collection must be counted (end={end_collections})"
        );
    }
    assert_eq!(
        end_capacity, pre_capacity,
        "{label}: active root capacity must return to its pre-apply value"
    );
}

#[test]
fn math_max_apply_gc_keeps_later_object() {
    let (status, stack) = run_observed(EXTREMA_GC_SCRIPT_MAX, &GC_READOUTS);
    assert_extrema_gc_run(
        "max",
        EXTREMA_GC_SCRIPT_MAX,
        8.0,
        12.0,
        1.0,
        true,
        status,
        &stack,
    );
}

#[test]
fn math_min_apply_gc_keeps_later_object() {
    let (status, stack) = run_observed(EXTREMA_GC_SCRIPT_MIN, &GC_READOUTS);
    assert_extrema_gc_run(
        "min",
        EXTREMA_GC_SCRIPT_MIN,
        2.0,
        12.0,
        1.0,
        true,
        status,
        &stack,
    );
}

/// Control 1: the same fixture without the injected collection returns the
/// original result and conversion order, so the failing category is GC, not
/// the fixture itself.
#[test]
fn math_max_apply_without_gc_keeps_original_result() {
    let (status, stack) = run_observed(EXTREMA_GC_SCRIPT_MAX, &NO_GC_READOUTS);
    assert_extrema_gc_run(
        "max-no-gc",
        EXTREMA_GC_SCRIPT_MAX,
        8.0,
        12.0,
        1.0,
        false,
        status,
        &stack,
    );
}

/// Control 2: overwriting the source element before apply yields a different
/// marker (only the first element) and result 2. This is what a dynamically
/// re-read source would answer instead of the snapshot's 8/order 12.
#[test]
fn math_max_apply_overwritten_source_answers_only_first_element() {
    let (status, stack) = run_observed(EXTREMA_PRE_TRUNCATED_MAX, &NO_GC_READOUTS);
    assert_extrema_gc_run(
        "max-pre-truncated",
        EXTREMA_PRE_TRUNCATED_MAX,
        2.0,
        1.0,
        0.0,
        false,
        status,
        &stack,
    );
}

/// Guest throw: after the first NaN conversion and the real collection, the
/// second element still converts and throws the identical object; the third
/// element is never converted. The frame chain then restores capacity, and a
/// collect after return reclaims the snapshot-only object.
#[test]
fn math_max_apply_gc_guest_throw_restores_frame_and_reclaims() {
    let source = r#"var shrink = function(s, f){ "use strict"; f.order = f.order * 10 + 1; s[1] = null; return NaN; };
var marker = { tag: 1 };
var first = { order: 0 };
var third = { order: 0, valueOf: function(){ "use strict"; this.order = this.order + 1; return 7; } };
var source = [ first, { link: first, boom: marker, valueOf: function(){ "use strict"; this.link.order = this.link.order * 10 + 2; throw this.boom; } }, third ];
first.valueOf = shrink.bind(null, source, first);
var same = false;
try { Math.max.apply(null, source); } catch (e) { same = e === marker; }
var order = first.order;
var thirdRan = third.order;
var cleared = source[1] === null;
same;"#;
    let (status, stack) = run_observed(source, &GC_THROW);
    assert_eq!(
        halted_or_report(source, status, &stack),
        VmStatus::Halted,
        "guest throw must be caught by the script"
    );
    // Tail: live_before, live_after, endCollections, endCapacity, endRoots,
    // preApplyCapacity, same, order, thirdRan, cleared.
    let cleared = stack_number(&stack, 0);
    let third_ran = stack_number(&stack, 1);
    let order = stack_number(&stack, 2);
    let same = stack_number(&stack, 3);
    let pre_capacity = stack_number(&stack, 4);
    let end_roots = stack_number(&stack, 5);
    let end_capacity = stack_number(&stack, 6);
    eprintln!(
        "EXTREMA_APPLY_GC_THROW same={same} order={order} third_ran={third_ran} \
         cleared={cleared} capacity={pre_capacity}->{end_capacity} \
         roots_end={end_roots} ledger={:?}",
        window_ledger()
    );
    assert_eq!(same, 1.0, "the identical thrown object must reach the catch");
    assert_eq!(order, 12.0, "second element must convert after the first NaN");
    assert_eq!(third_ran, 0.0, "the tail element must not convert after the throw");
    assert_eq!(cleared, 1.0, "the thrown element must be overwritten before the collect");
    assert_eq!(
        end_capacity, pre_capacity,
        "guest throw must release the whole extrema frame chain"
    );
    let live_after = stack_number(&stack, 8);
    let live_before = stack_number(&stack, 9);
    eprintln!("EXTREMA_APPLY_GC_RECLAIM live_before={live_before} live_after={live_after}");
    assert!(
        live_after < live_before,
        "post-return collect must reclaim the snapshot-only object \
         ({live_before}->{live_after})"
    );
}

/// Nested extrema apply: the inner `.apply` frame exits while the outer
/// `.apply` frame's dynamic roots are live; the real collection inside the
/// outer first valueOf must still keep the outer later element alive.
#[test]
fn math_min_apply_nested_keeps_outer_dynamic_roots() {
    let source = r#"var shrink = function(s, f){ "use strict";
var inner = Math.min.apply(null, [ { valueOf: function(){ "use strict"; return 9; } }, { valueOf: function(){ "use strict"; return 4; } } ]);
s[1] = null;
f.order = f.order * 10 + 1;
f.inner = inner;
return 5; };
var first = { order: 0, inner: 0 };
var source = [ first, { link: first, valueOf: function(){ "use strict"; this.link.order = this.link.order * 10 + 2; return 7; } } ];
first.valueOf = shrink.bind(null, source, first);
var result = Math.max.apply(null, source);
var order = first.order;
var inner = first.inner;
var cleared = source[1] === null;
result;"#;
    let (status, stack) = run_observed(source, &GC_NESTED);
    assert_eq!(
        halted_or_report(source, status, &stack),
        VmStatus::Halted,
        "nested extrema apply must halt"
    );
    let cleared = stack_number(&stack, 0);
    let inner = stack_number(&stack, 1);
    let order = stack_number(&stack, 2);
    let result = stack_number(&stack, 3);
    eprintln!(
        "EXTREMA_APPLY_GC_NESTED result={result} order={order} inner={inner} \
         cleared={cleared} ledger={:?}",
        window_ledger()
    );
    assert_eq!(inner, 4.0, "inner Math.min.apply must answer 4");
    assert_eq!(order, 12.0, "outer later element must still convert");
    assert_eq!(result, 7.0, "outer Math.max.apply must answer 7");
    assert_eq!(cleared, 1.0, "outer source overwrite must stay visible");
}

/// Anti-control: packed arrays, pure scalar tagged arrays, holes beyond the
/// present extent, and empty arrays contribute no dynamic roots. A scalar or
/// hole vector longer than the realm root budget would fatally exceed 8192
/// roots if those tags were counted, so the 65535 expansion below is the
/// positive witness that R stays 0.
#[test]
fn math_max_apply_scalar_arity_stays_out_of_root_budget() {
    assert_eq!(eval_number("Math.max.apply(null,[1,2,9])===9;"), 1.0);
    // Tagged array with no reference entries: Number/Boolean/Null/Undefined.
    assert_eq!(
        eval_number("var x=Math.max.apply(null,[1,true,null,undefined,3]);x!==x;"),
        1.0
    );
    // Packed array grown beyond its physical present extent: the expansion
    // reads Undefined for the absent tail and must not scan it as roots.
    assert_eq!(
        eval_number("var a=[1,2];a.length=4;var x=Math.max.apply(null,a);x!==x;"),
        1.0
    );
    assert_eq!(
        eval_number("Math.max.apply(null,[])===-Infinity;"),
        1.0
    );
}

/// `new Array(65535)` has zero physical entries and is expanded as 65535
/// Undefined values. If holes were pre-scanned as roots the reservation would
/// exceed the realm budget before dispatch. The run must either answer NaN
/// (the full expansion) or be stopped by the fixed Core/heap resource ceiling;
/// that VM fatal is reported explicitly and is never the guest TypeError that
/// the 65536 admission boundary keeps.
#[test]
fn math_max_apply_65535_holes_stay_within_admitted_limit() {
    let source = "var a=new Array(65535);var x=Math.max.apply(null,a);x!==x&&a.length===65535;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("compile refused: {error:?}"));
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(200_000_000);
    match machine.run() {
        Ok(VmStatus::Halted) => {
            let stack = machine.stack();
            let last = stack
                .len()
                .checked_sub(1)
                .unwrap_or_else(|| panic!("no 65535 completion value"));
            assert_eq!(
                stack_number_at(stack, last),
                1.0,
                "65535 Undefined arguments must expand and answer NaN"
            );
            eprintln!("EXTREMA_65535_HOLES result=NaN roots=0");
        }
        Ok(VmStatus::Yielded) => {
            eprintln!(
                "EXTREMA_65535_HOLES_RESOURCE_LIMIT fuel=yielded; 65536 keeps the guest TypeError"
            );
        }
        Ok(other) => panic!("unexpected 65535 status {other:?}"),
        Err(error) => {
            // The 8193-hole witness above proves the scanning logic; this
            // branch only records the fixed Core/heap ceiling that the task
            // allows for the maximal vector. It is a VM fatal, not a guest
            // TypeError, and the 65536 admission test keeps that boundary.
            eprintln!(
                "EXTREMA_65535_HOLES_RESOURCE_LIMIT error={error:?}; 65536 keeps the guest TypeError"
            );
        }
    }
}

/// Holes above the realm root budget. The 8193-hole expansion exercises the
/// physical-prefix scan with `present == 0`, so a counted hole would overflow
/// the 8192-entry reservation before dispatch. Like the maximal vector this
/// expansion is bounded by the fixed Core conversion resource; the case is
/// recorded rather than claimed as a NaN success.
#[test]
fn math_max_apply_holes_above_root_budget_scan_counts_no_roots() {
    let source = "var a=new Array(8193);var x=Math.max.apply(null,a);x!==x&&a.length===8193;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("compile refused: {error:?}"));
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    match machine.run() {
        Ok(VmStatus::Halted) => {
            let stack = machine.stack();
            let last = stack
                .len()
                .checked_sub(1)
                .unwrap_or_else(|| panic!("no 8193 completion value"));
            assert_eq!(stack_number_at(stack, last), 1.0);
            eprintln!("EXTREMA_8193_HOLES result=NaN roots=0");
        }
        Ok(VmStatus::Yielded) => {
            eprintln!("EXTREMA_8193_HOLES_RESOURCE_LIMIT fuel=yielded; no guest TypeError");
        }
        Ok(other) => panic!("unexpected 8193 status {other:?}"),
        Err(error) => {
            eprintln!(
                "EXTREMA_8193_HOLES_RESOURCE_LIMIT error={error:?}; no guest TypeError at 8193"
            );
        }
    }
}
