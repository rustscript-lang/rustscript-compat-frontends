//! Owned-length `new Uint8Array(length)` interpreter receipts.
//!
//! The admitted route allocates a fresh kind-7 byte backing and a kind-8 view
//! directly from the typed-array helper: the ArrayBuffer constructor entry is
//! never called. Guest index errors stay catchable `RangeError`s, the
//! per-buffer byte quota stays a fatal host error, and the backing is rooted
//! across the view reservation so a real collection cannot reclaim it.

const PROBE_FUEL: u64 = 200_000_000;

use pd_vm_compat_frontends::{
    JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor, reference_array_effective_catalog,
};

fn with_big_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(body)
        .unwrap()
        .join()
        .unwrap()
}

/// One CLI-catalog run under the production compile/bind services. `Ok` is the
/// halted stack value, `Err` the exact refusal or run failure text.
fn run_outcome(source: &str) -> Result<vm::Value, String> {
    let source = source.to_owned();
    with_big_stack(move || {
        let mut machine =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                .unwrap_or_else(|error| panic!("source={source:?} {error:?}"));
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        machine.set_fuel(PROBE_FUEL);
        match machine.run() {
            Ok(vm::VmStatus::Halted) => Ok(machine
                .stack()
                .last()
                .cloned()
                .unwrap_or_else(|| panic!("source={source:?} halted with an empty stack"))),
            Ok(other) => Err(format!("{other:?}")),
            Err(error) => Err(format!("{error:?}")),
        }
    })
}

fn run_number(source: &str) -> f64 {
    match run_outcome(source) {
        Ok(vm::Value::Map(map)) => match map.get(&vm::Value::string("number")) {
            Some(vm::Value::Float(number)) => *number,
            other => panic!("source={source:?} non-number box {other:?}"),
        },
        Ok(other) => panic!("source={source:?} non-box value {other:?}"),
        Err(error) => panic!("source={source:?} must halt with a Number: {error}"),
    }
}

fn run_bool(source: &str) -> bool {
    match run_outcome(source) {
        Ok(vm::Value::Map(map)) => match map.get(&vm::Value::string("flag")) {
            Some(vm::Value::Bool(flag)) => *flag,
            other => panic!("source={source:?} non-boolean box {other:?}"),
        },
        Ok(other) => panic!("source={source:?} non-box value {other:?}"),
        Err(error) => panic!("source={source:?} must halt with a Boolean: {error}"),
    }
}

/// The catalog this suite must stay independent of: it carries no
/// `typescript::throw_error` adapter.
fn base_catalog() -> std::sync::Arc<vm::HostApiCatalog> {
    reference_array_effective_catalog()
}

fn parse_with_catalog(
    source: &str,
    catalog: &std::sync::Arc<vm::HostApiCatalog>,
) -> Result<(), String> {
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::ParsePhase::Standalone,
        host_api_catalog: Some(catalog),
        imports: &[],
    };
    vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, &context)
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

fn parse_without_throw_adapter(source: &str) -> Result<(), String> {
    parse_with_catalog(source, &base_catalog())
}

fn compile_without_throw_adapter(source: &str) -> Result<(), String> {
    let source = source.to_owned();
    with_big_stack(move || {
        vm::compile_source_with_flavor_and_options_with_capture_certification(
            &source,
            javascript_es5_flavor(),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
                .with_host_api_catalog(base_catalog()),
        )
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
    })
}

#[test]
fn u8_owned_length_basic_interpreter() {
    assert_eq!(run_number("new Uint8Array(4).length;"), 4.0);
    assert!(run_bool(
        r#"
        var a = new Uint8Array(4);
        var zeros = a[0] === 0 && a[1] === 0 && a[2] === 0 && a[3] === 0;
        var buffer = a.buffer;
        var attributes = a.length === 4 && a.byteLength === 4 &&
            a.byteOffset === 0 && buffer.byteLength === 4;
        var identity = a.buffer === buffer;
        var b = new Uint8Array(4);
        var independent = b.buffer !== a.buffer && b.buffer !== buffer;
        var alias = new Uint8Array(buffer);
        a[1] = 255;
        alias[2] = 7;
        var shared = alias.buffer === buffer && alias.length === 4 &&
            alias.byteLength === 4 && alias.byteOffset === 0 &&
            alias[1] === 255 && a[2] === 7;
        var prototype = Object.getPrototypeOf(a) === Uint8Array.prototype &&
            a.constructor === Uint8Array;
        zeros && attributes && identity && independent && shared && prototype;
    "#,
    ));
}

#[test]
fn u8_owned_length_conversion_and_order_interpreter() {
    assert!(run_bool(
        r#"
        function len(x) { return new Uint8Array(x).length; }
        function stop() { "use strict"; throw "early"; }
        function shadow(Uint8Array) {
            try { new Uint8Array(4); } catch (e) { return e.name; }
            return "none";
        }
        var hits = 0;
        if (len() === 0) hits = hits + 1;
        if (len(undefined) === 0) hits = hits + 1;
        if (len(null) === 0) hits = hits + 1;
        if (len(false) === 0) hits = hits + 1;
        if (len(true) === 1) hits = hits + 1;
        if (len(NaN) === 0) hits = hits + 1;
        if (len("abc") === 0) hits = hits + 1;
        if (len("4.9") === 4) hits = hits + 1;
        if (len(-0.9) === 0) hits = hits + 1;
        if (len(4.9) === 4) hits = hits + 1;
        if (len(32768.9) === 32768) hits = hits + 1;
        var conversions = hits === 11;

        var tail = { n: 0, valueOf: function(){ "use strict"; this.n = this.n + 1; return 9; } };
        var ignored = new Uint8Array(4, tail, tail, tail);
        var trailing = ignored.length === 4 && ignored.byteLength === 4 &&
            tail.n === 0;

        var tracker = { n: 0, mark: function(x){ "use strict"; this.n = this.n * 10 + x; return x; } };
        var ordered = new Uint8Array(tracker.mark(1), tracker.mark(2), tracker.mark(3));
        var evaluation = ordered.length === 1 && tracker.n === 123;

        var early = 0;
        var abrupt = "";
        try { new Uint8Array(stop(), (early = 1)); } catch (e) { abrupt = e; }
        var stopped = abrupt === "early" && early === 0;
        var later = "";
        try { new Uint8Array(2, stop()); } catch (e) { later = e; }
        var later_stopped = later === "early";

        var object = {
            n: 0,
            valueOf: function(){ "use strict"; this.n = this.n + 1; return 4; },
            toString: function(){ "use strict"; this.n = this.n + 1; return "4"; }
        };
        var object_error = 0;
        try { new Uint8Array(object); } catch (e) {
            if (e.name === "TypeError" && e.message === "unsupported object class") object_error = 1;
        }
        var fn = function(){};
        var function_error = 0;
        try { new Uint8Array(fn); } catch (e) {
            if (e.name === "TypeError" && e.message === "unsupported object class") function_error = 1;
        }
        var no_hooks = object.n === 0 && object_error === 1 && function_error === 1;

        var array_error = 0;
        try { new Uint8Array([1]); } catch (e) {
            if (e.name === "TypeError" && e.message === "unsupported object class") array_error = 1;
        }
        var view = new Uint8Array(2);
        var copy_error = 0;
        try { new Uint8Array(view); } catch (e) {
            if (e.name === "TypeError" && e.message === "unsupported object class") copy_error = 1;
        }
        var copy_refused = array_error === 1 && copy_error === 1;

        var C = Uint8Array;
        var aliased = new C(2);
        var alias_ok = aliased.length === 2 && aliased.byteLength === 2;
        var no_new = 0;
        try { C(2); } catch (e) {
            if (e.name === "TypeError" && e.message === "must be called with new") no_new = 1;
        }
        var plain_refused = no_new === 1;

        var shadowed = shadow({}) === "TypeError";

        conversions && trailing && evaluation && stopped && later_stopped &&
            no_hooks && copy_refused && alias_ok && plain_refused && shadowed;
    "#,
    ));
}

fn assert_host_quota(source: &str) {
    let error = run_outcome(source).expect_err("the owned byte quota must be fatal");
    assert!(
        error.contains("HostError(\"array buffer length exceeds host quota\")"),
        "source={source:?} must raise the fatal host quota, got {error}"
    );
}

fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .index
}

fn call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, vec![], args, None, None)
}

fn get(slot: u16, name: &str) -> vm::Expr {
    call(
        vm::BuiltinFunction::Get.call_index(),
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(slot))),
            vm::Expr::String(name.into()),
        ],
    )
}

fn num(e: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(e), Box::new(vm::Expr::Float(1.0)))
}

fn plus(a: vm::Expr, b: vm::Expr) -> vm::Expr {
    vm::Expr::Add(Box::new(a), Box::new(b))
}

fn eq(a: vm::Expr, b: vm::Expr) -> vm::Expr {
    vm::Expr::Eq(Box::new(num(a)), Box::new(num(b)))
}

fn check(e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Expr {
        expr: call(vm::BuiltinFunction::Assert.call_index(), vec![e]),
        line: 1,
    }
}

fn probe_vm_with_observer(
    source: &str,
    observer: &'static dyn vm::SourcePlugin,
    id: &'static str,
) -> vm::Vm {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(id)),
        vm::CompileSourceFileOptions::new()
            .with_host_api_catalog(catalog.clone())
            .with_source_plugin(observer),
    )
    .unwrap();
    let mut machine = issued.into_vm().unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .unwrap();
    let mut registry = vm::HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
    }
    for name in [
        "math::floor",
        "math::trunc",
        "math::copysign",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
        "bytes::from_utf8",
    ] {
        registry.allow_builtin(name).unwrap();
    }
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(PROBE_FUEL);
    machine
}

fn reset_machine(machine: &mut vm::Vm) {
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.reset_for_reuse().unwrap();
    if machine.scope_reset_pending() {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            machine.poll_reset_for_reuse(&mut cx),
            std::task::Poll::Ready(Ok(()))
        ));
    }
    assert!(!machine.scope_reset_pending());
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(!machine.capture_certification_stats().unwrap().valid);
}

/// Injects a real collection exactly when the kind-8 view node is reserved,
/// i.e. after the kind-7 backing was published and rooted but before the view
/// allocation exists. A lost backing root fails here, not in a later read.
struct OwnedGcObserver;

impl vm::SourcePlugin for OwnedGcObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("u8-owned-length-gc");
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let node_begin = helper(&ir, "__compat_es5_node_begin");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let flag = u16::try_from(ir.locals).unwrap();
        ir.locals += 1;
        {
            let implementation = ir.function_impls.get_mut(&node_begin).unwrap();
            let carrier = implementation.param_slots[0];
            let kind = implementation.param_slots[2];
            implementation.body_stmts.splice(
                0..0,
                [
                    S::Let {
                        index: flag,
                        declared_schema: Some(T::Int),
                        expr: E::Int(0),
                        line: 1,
                    },
                    S::IfElse {
                        condition: vm::Expr::And(
                            Box::new(eq(E::Var(kind), E::Int(8))),
                            Box::new(eq(E::Var(flag), E::Int(0))),
                        ),
                        then_branch: vec![
                            S::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: flag,
                                expr: E::Int(1),
                                line: 1,
                            },
                            S::Expr {
                                expr: call(collect, vec![E::Var(carrier)]),
                                line: 1,
                            },
                        ],
                        else_branch: Vec::new(),
                        line: 1,
                    },
                ],
            );
        }

        let view_helper = helper(&ir, "__compat_es5_typed_array");
        let observe = helper(&ir, "__compat_es5_heap_observe");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let checked = helper(&ir, "__compat_es5_checked_base");
        let read = helper(&ir, "__compat_ref_read");
        let decode = helper(&ir, "__compat_es5_decode_int");
        let decode_u32 = helper(&ir, "__compat_es5_decode_u32");
        let before = u16::try_from(ir.locals).unwrap();
        ir.locals += 3;
        let (view, backing) = (before + 1, before + 2);
        let implementation = ir.function_impls.get_mut(&view_helper).unwrap();
        let [carrier, mode, receiver] = implementation.param_slots[..3] else {
            unreachable!()
        };
        let count = || call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]);
        let handle = |slot, generation| call(checked, vec![E::Var(carrier), slot, generation]);
        let view_base = || handle(get(receiver, "ref_slot"), get(receiver, "generation"));
        let read_cell = |base, offset| {
            call(
                read,
                vec![E::Var(carrier), num(plus(E::Var(base), E::Int(offset)))],
            )
        };
        let backing_base = || {
            handle(
                call(decode, vec![read_cell(view, 5)]),
                call(decode_u32, vec![read_cell(view, 6)]),
            )
        };
        let let_int = |index, expr| S::Let {
            index,
            declared_schema: Some(T::Int),
            expr,
            line: 1,
        };
        let mut assertions = vec![
            let_int(before, count()),
            let_int(view, view_base()),
            let_int(backing, backing_base()),
            S::Expr {
                expr: call(collect, vec![E::Var(carrier)]),
                line: 1,
            },
            check(eq(count(), plus(E::Var(before), E::Int(1)))),
            check(eq(view_base(), E::Var(view))),
            check(eq(backing_base(), E::Var(backing))),
            check(vm::Expr::Gt(
                Box::new(num(call(decode_u32, vec![read_cell(view, 1)]))),
                Box::new(E::Int(0)),
            )),
            check(eq(read_cell(view, 7), E::Int(0))),
            check(eq(read_cell(view, 8), E::Int(4))),
            check(eq(read_cell(view, 9), E::Int(1))),
            check(eq(read_cell(view, 10), E::Int(1))),
            check(eq(read_cell(backing, 5), E::Int(4))),
            check(eq(read_cell(backing, 6), E::Int(169))),
        ];
        for offset in 7..10 {
            assertions.push(check(eq(read_cell(backing, offset), E::Float(0.0))));
        }
        implementation.body_stmts.insert(
            0,
            S::IfElse {
                condition: eq(E::Var(mode), E::Int(330)),
                then_branch: assertions,
                else_branch: Vec::new(),
                line: 1,
            },
        );
        Ok(ir)
    }
}

/// Negative control for the rooting boundary: at the same pre-view point the
/// positive observer collects, this variant first clears the top root frame's
/// entry 0. The backing becomes unreachable and the construction must fail, so
/// the positive probe cannot pass without the publication root.
struct BrokenRootObserver;

impl vm::SourcePlugin for BrokenRootObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("u8-owned-length-broken-root");
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let node_begin = helper(&ir, "__compat_es5_node_begin");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let read = helper(&ir, "__compat_ref_read");
        let write = helper(&ir, "__compat_ref_write");
        let decode = helper(&ir, "__compat_es5_decode_int");
        let flag = u16::try_from(ir.locals).unwrap();
        ir.locals += 2;
        let top = flag + 1;
        // Raw realm header `HDR_ROOT_TOP` and the root-frame entry stride.
        const ROOT_TOP: i64 = 3;
        const FRAME_FIXED_CELLS: i64 = 4;
        let implementation = ir.function_impls.get_mut(&node_begin).unwrap();
        let carrier = implementation.param_slots[0];
        let kind = implementation.param_slots[2];
        implementation.body_stmts.splice(
            0..0,
            [
                S::Let {
                    index: flag,
                    declared_schema: Some(T::Int),
                    expr: E::Int(0),
                    line: 1,
                },
                S::IfElse {
                    condition: vm::Expr::And(
                        Box::new(eq(E::Var(kind), E::Int(8))),
                        Box::new(eq(E::Var(flag), E::Int(0))),
                    ),
                    then_branch: vec![
                        S::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: flag,
                            expr: E::Int(1),
                            line: 1,
                        },
                        S::Let {
                            index: top,
                            declared_schema: Some(T::Int),
                            expr: call(
                                decode,
                                vec![call(read, vec![E::Var(carrier), E::Int(ROOT_TOP)])],
                            ),
                            line: 1,
                        },
                        S::Expr {
                            expr: call(
                                write,
                                vec![
                                    E::Var(carrier),
                                    num(plus(E::Var(top), E::Int(FRAME_FIXED_CELLS))),
                                    E::Float(0.0),
                                ],
                            ),
                            line: 1,
                        },
                        S::Expr {
                            expr: call(
                                write,
                                vec![
                                    E::Var(carrier),
                                    num(plus(E::Var(top), E::Int(FRAME_FIXED_CELLS + 1))),
                                    E::Float(0.0),
                                ],
                            ),
                            line: 1,
                        },
                        S::Expr {
                            expr: call(
                                write,
                                vec![
                                    E::Var(carrier),
                                    num(plus(E::Var(top), E::Int(FRAME_FIXED_CELLS + 2))),
                                    E::Float(0.0),
                                ],
                            ),
                            line: 1,
                        },
                        S::Expr {
                            expr: call(collect, vec![E::Var(carrier)]),
                            line: 1,
                        },
                    ],
                    else_branch: Vec::new(),
                    line: 1,
                },
            ],
        );
        Ok(ir)
    }
}

#[test]
fn u8_owned_length_gc_and_budget() {
    static OBS: OwnedGcObserver = OwnedGcObserver;
    let source = r#"
        var v = new Uint8Array(4);
        v[0] = 169;
        var n = v.length;
        v[1] = 84;
        n === 4 && v[0] === 169 && v[1] === 84 &&
            v[2] === 0 && v[3] === 0 && v.buffer.byteLength === 4;
    "#;
    with_big_stack(|| {
        let mut machine = probe_vm_with_observer(source, &OBS, "u8-owned-length-gc");
        assert_eq!(
            machine.run().expect("GC probe must halt"),
            vm::VmStatus::Halted
        );
        let Some(vm::Value::Map(map)) = machine.stack().last() else {
            panic!("missing Boolean box");
        };
        assert_eq!(
            map.get(&vm::Value::string("flag")),
            Some(&vm::Value::Bool(true))
        );
        let remaining = machine.get_fuel().expect("probe fuel was configured");
        let used = PROBE_FUEL - remaining;
        assert!(used > 0, "a run must consume fuel");
        // A reset revokes source certification, and owned allocation requires
        // it, so each boundary run gets a freshly certified VM. The program is
        // deterministic with JIT disabled, so N is exactly the first run's use.
        let mut exact = probe_vm_with_observer(source, &OBS, "u8-owned-length-gc");
        exact.set_fuel(used);
        assert_eq!(
            exact.run().expect("the exact N fuel budget must halt"),
            vm::VmStatus::Halted
        );
        let mut short = probe_vm_with_observer(source, &OBS, "u8-owned-length-gc");
        short.set_fuel(used - 1);
        assert_eq!(
            short.run().expect("N-1 fuel must yield, not fault"),
            vm::VmStatus::Yielded,
            "N-1 must be the exact fuel boundary"
        );

        u8_broken_root_probe(source);
    });
}

/// Marks the typed-array helper entry and the quota branch with live-node and
/// active-root-capacity observations. The fatal quota must raise before any
/// node or root frame is reserved, so both pairs stay bit-identical.
struct OwnedQuotaObserver;

fn inject_quota_probe(statements: &mut Vec<vm::Stmt>, observe: u16, carrier: u16) -> bool {
    use vm::{Expr as E, Stmt as S};
    for index in 0..statements.len() {
        let found = match &mut statements[index] {
            S::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                inject_quota_probe(then_branch, observe, carrier)
                    || inject_quota_probe(else_branch, observe, carrier)
            }
            S::While { body, .. } => inject_quota_probe(body, observe, carrier),
            _ => false,
        };
        if found {
            return true;
        }
        let is_quota_call = matches!(
            &statements[index],
            S::Assign { expr: E::Call(_, _, args, _, _), .. }
                if args.iter().any(|arg| matches!(
                    arg,
                    E::String(message) if message == "array buffer length exceeds host quota"
                ))
        );
        if is_quota_call {
            statements.splice(
                index..index,
                [
                    S::Expr {
                        expr: E::String("U8_QUOTA".into()),
                        line: 1,
                    },
                    S::Expr {
                        expr: call(observe, vec![E::Var(carrier), E::Int(0), E::Int(0)]),
                        line: 1,
                    },
                    S::Expr {
                        expr: call(observe, vec![E::Var(carrier), E::Int(4), E::Int(0)]),
                        line: 1,
                    },
                ],
            );
            return true;
        }
    }
    false
}

impl vm::SourcePlugin for OwnedQuotaObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("u8-owned-length-quota");
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S};
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let view_helper = helper(&ir, "__compat_es5_typed_array");
        let observe = helper(&ir, "__compat_es5_heap_observe");
        let implementation = ir.function_impls.get_mut(&view_helper).unwrap();
        let carrier = implementation.param_slots[0];
        implementation.body_stmts.splice(
            0..0,
            [
                S::Expr {
                    expr: E::String("U8_ENTRY".into()),
                    line: 1,
                },
                S::Expr {
                    expr: call(observe, vec![E::Var(carrier), E::Int(0), E::Int(0)]),
                    line: 1,
                },
                S::Expr {
                    expr: call(observe, vec![E::Var(carrier), E::Int(4), E::Int(0)]),
                    line: 1,
                },
            ],
        );
        assert!(
            inject_quota_probe(&mut implementation.body_stmts, observe, carrier),
            "the quota adapter call must be present"
        );
        Ok(ir)
    }
}
#[test]
fn u8_owned_length_quota_and_route_interpreter() {
    assert!(run_bool(
        r#"
        var v = new Uint8Array(32768);
        v.length === 32768 && v.byteLength === 32768 &&
            v.buffer.byteLength === 32768 && v.byteOffset === 0 &&
            v[0] === 0 && v[32767] === 0;
    "#,
    ));
    // A catchable guest index error must win before the local byte quota:
    // 2^30 is outside JS_ToIndex, so the host never gets to allocate.
    assert!(run_bool(
        r#"
        var hit = 0;
        try { new Uint8Array(1073741824); } catch (e) {
            if (e.name === "RangeError" && e.message === "invalid array index") hit = 1;
        }
        hit === 1;
    "#,
    ));
    // The Script-level site and a function-local admitted alias both reach
    // the fatal quota; no guest catch may see the host error.
    assert_host_quota("try { new Uint8Array(32769); } catch (e) { var seen = 1; } seen;");
    // A function-local admitted alias inside nested guest catches: neither
    // inner nor outer catch may observe the fatal host error.
    assert_host_quota(
        "function f(){ var C = Uint8Array; try { try { new C(32769); } catch (e) { return 2; } } catch (e) { return 3; } return 1; } f();",
    );
    // The baseline catalog has no scalar adapter: the reachable fatal quota
    // must reject at compile time, while proven sizes and shadowed spellings
    // stay catalog-free.
    assert!(
        base_catalog()
            .functions_named("typescript::throw_error")
            .is_empty(),
        "the comparison catalog must not carry the adapter"
    );
    for source in [
        "function f(){ return new Uint8Array(4).buffer.byteLength; }",
        "function f(){ return new Uint8Array(4, 9, 9).length; }",
        "function f(Uint8Array, x){ try { new Uint8Array(x); } catch (e) { return e.name; } return \"none\"; }",
    ] {
        parse_without_throw_adapter(source)
            .unwrap_or_else(|error| panic!("{source} must stay catalog-free: {error}"));
        compile_without_throw_adapter(source)
            .unwrap_or_else(|error| panic!("{source} must stay catalog-free: {error}"));
    }
    let unproven = "function f(x){ return new Uint8Array(x).length; }";
    let error = parse_without_throw_adapter(unproven)
        .expect_err("an unproven owned length needs the fatal quota adapter");
    assert!(
        error.contains("typescript::throw_error"),
        "{unproven} must name the adapter: {error}"
    );
    let error = compile_without_throw_adapter(unproven)
        .expect_err("the full compile must refuse without the fatal quota adapter");
    assert!(
        error.contains("typescript::throw_error"),
        "{unproven} must name the adapter: {error}"
    );

    u8_quota_observer_probe();
}

#[test]
fn u8_owned_length_unknown_tail_keeps_the_owned_realm() {
    // The safe first actual still installs the ArrayBuffer realm for the
    // owned backing; the unknown tail is evaluated by the call argument
    // path but never converted by the owned-length branch.
    assert_eq!(
        run_number("function f(x){ return new Uint8Array(4, x).buffer.byteLength; } f(9);"),
        4.0
    );
    // With the adapter present, an unknown tail cannot hide the fatal quota
    // of a 32769 first actual from the guest catch: the owned branch never
    // reads the tail.
    assert_host_quota(
        "function f(x){ try { new Uint8Array(32769, x); } catch (e) { return 1; } return 0; } f(9);",
    );
}

fn u8_quota_observer_probe() {
    static QUOTA: OwnedQuotaObserver = OwnedQuotaObserver;
    with_big_stack(|| {
        let mut machine =
            probe_vm_with_observer("new Uint8Array(32769);", &QUOTA, "u8-owned-length-quota");
        let error = machine.run().expect_err("the owned byte quota is fatal");
        assert!(
            matches!(&error, vm::VmError::HostError(message)
            if message == "array buffer length exceeds host quota"),
            "{error:?}"
        );
        let stack = machine.stack();
        let entry = stack
            .iter()
            .rposition(|value| *value == vm::Value::string("U8_ENTRY"))
            .expect("typed-array helper entry observation");
        let quota = stack
            .iter()
            .rposition(|value| *value == vm::Value::string("U8_QUOTA"))
            .expect("quota branch observation");
        let observed = |at: usize| -> (i64, i64) {
            let vm::Value::Int(live) = stack[at + 1] else {
                panic!("missing live-node observation");
            };
            let vm::Value::Int(roots) = stack[at + 2] else {
                panic!("missing active-root-capacity observation");
            };
            (live, roots)
        };
        let before = observed(entry);
        let at_quota = observed(quota);
        assert_eq!(
            at_quota, before,
            "the fatal quota allocated a node or grew active root capacity"
        );
        reset_machine(&mut machine);
    });
}

fn u8_broken_root_probe(source: &str) {
    // Deleting the publication root at the same point must break the
    // construction: the forced collection frees the backing and the view
    // publication cannot resolve it.
    static BROKEN: BrokenRootObserver = BrokenRootObserver;
    let mut broken = probe_vm_with_observer(source, &BROKEN, "u8-owned-length-broken-root");
    let error = broken
        .run()
        .expect_err("a lost backing root must fail the construction");
    let text = format!("{error:?}");
    assert!(
        !text.contains("Fuel"),
        "the negative control must fail on the lost root, got {text}"
    );
}
