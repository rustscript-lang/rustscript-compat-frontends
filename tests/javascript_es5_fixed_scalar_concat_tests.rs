//! Proved fixed-scalar String additions through the public managed VM path.
#[path = "javascript_es5/object_observer.rs"]
mod object_observer;
fn reset(machine: &mut vm::Vm) {
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
    assert!(pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine).is_none());
}

fn probe_vm(source: &str, observer: &'static dyn vm::SourcePlugin, id: &'static str) -> vm::Vm {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(id)),
        vm::CompileSourceFileOptions::new()
            .with_host_api_catalog(catalog.clone())
            .with_source_plugin(observer),
    )
    .unwrap_or_else(|error| panic!("source={source:?} guest_invocations=0 {error:?}"));
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
        "math::abs",
        "math::acos",
        "math::asin",
        "math::atan",
        "math::atan2",
        "math::ceil",
        "math::cos",
        "math::exp",
        "math::ln",
        "math::sin",
        "math::tan",
        "math::trunc",
        "math::is_infinite",
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
    machine.set_fuel(10_000_000);
    machine
}

fn string(source: &str, expected: &[u16]) {
    let source = source.to_owned();
    let expected = expected.to_vec();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "FIXED121_SOURCE {}",
                serde_json::json!({"source":source,"expected_units":expected})
            );
            let observer = Box::leak(Box::new(object_observer::Observer {
                units: Some(expected.len()),
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "FIXED121_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let stack = machine.stack();
            let at = stack
                .iter()
                .position(|v| v == &vm::Value::string("P7_OBSERVED"))
                .unwrap();
            let vm::Value::Map(boxed) = &stack[at + 1] else {
                panic!("managed box")
            };
            assert_eq!(boxed.len(), 5);
            for (key, value) in [
                ("tag", vm::Value::Int(4)),
                ("number", vm::Value::Float(0.0)),
                ("flag", vm::Value::Bool(false)),
            ] {
                assert_eq!(boxed.get(&vm::Value::string(key)), Some(&value));
            }
            for key in ["ref_slot", "generation"] {
                assert!(
                    matches!(boxed.get(&vm::Value::string(key)),Some(vm::Value::Int(n)) if *n>0)
                );
            }
            assert_eq!(stack[at + 2], vm::Value::Float(expected.len() as f64));
            for (i, u) in expected.iter().enumerate() {
                assert_eq!(stack[at + 3 + i], vm::Value::Float(f64::from(*u)));
            }
            reset(&mut machine);
            oracle_units(&source, &expected);
        })
        .unwrap()
        .join()
        .unwrap();
}
fn execute(source: &str) -> vm::Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("FIXED121_SOURCE {}", serde_json::json!({"source": source}));
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!("source={source:?} guest_invocations=0 {error:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!(
                "FIXED121_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "FIXED121_ROOT_FAILURE ip={} frames={:?}",
                    machine.ip(),
                    machine.execution_frames()
                );
            }
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let result = machine.stack().last().unwrap().clone();
            reset(&mut machine);
            eprintln!(
                "FIXED121_RESET source={source:?} pending=false resources=0 guest_invocations=1 native=0"
            );
            result
        })
        .unwrap()
        .join()
        .unwrap()
}

fn boxed(source: &str, tag: i64, number: f64, flag: bool) {
    let result = execute(source);
    let vm::Value::Map(map) = result else {
        panic!("full box: {result:?}")
    };
    assert_eq!(map.len(), 5, "{source}");
    for (name, expected) in [
        ("tag", vm::Value::Int(tag)),
        ("number", vm::Value::Float(number)),
        ("flag", vm::Value::Bool(flag)),
        ("ref_slot", vm::Value::Int(0)),
        ("generation", vm::Value::Int(0)),
    ] {
        assert_eq!(
            map.get(&vm::Value::string(name)),
            Some(&expected),
            "{source}"
        );
    }
    if tag == 3 {
        let Some(vm::Value::Float(actual)) = map.get(&vm::Value::string("number")) else {
            panic!("number payload {source}")
        };
        assert_eq!(actual.to_bits(), number.to_bits(), "{source}");
    }
    let expected = match tag {
        0 => serde_json::json!({"kind": "undefined"}),
        2 => serde_json::json!({"kind": "boolean", "value": flag}),
        3 => serde_json::json!({"kind": "number", "value": number}),
        _ => unreachable!(),
    };
    oracle(source, expected);
}

fn oracle(source: &str, expected: serde_json::Value) {
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    let script = format!(
        "(()=>{{const source={};try{{const v=(0,eval)(source);console.log(JSON.stringify(v===undefined?{{kind:'undefined'}}:{{kind:typeof v,value:v,...(typeof v==='number'?{{negative_zero:Object.is(v,-0)}}:{{}})}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',value:String(e)}}));}}}})();",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "FIXED121_NODE {}",
        serde_json::json!({
            "source": source,
            "version_rc": version.status.code(),
            "version": String::from_utf8_lossy(&version.stdout),
            "argv": ["timeout", "15", "node", "-e", &script],
            "rc": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr)
        })
    );
    assert!(
        version.status.success() && output.status.success(),
        "{source}"
    );
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    if expected["kind"] == "number" {
        assert_eq!(actual["kind"], "number", "{source}");
        assert_eq!(actual.as_object().unwrap().len(), 3, "{source}");
        let value = expected["value"].as_f64().unwrap();
        assert_eq!(
            actual["negative_zero"],
            value.is_sign_negative() && value == 0.0
        );
        assert_eq!(actual["value"].as_f64(), Some(value), "{source}");
    } else {
        assert_eq!(actual, expected, "{source}");
    }
}

fn parse(source: &str) -> Result<vm::FrontendIr, vm::ParseError> {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    vm::SourcePlugin::parse_source(
        &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
        source,
        &context,
    )
}

fn oracle_units(source: &str, expected: &[u16]) {
    let script = format!(
        "(()=>{{const source={};let v=(0,eval)(source);console.log(JSON.stringify({{kind:typeof v,units:Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))}}));}})();",
        serde_json::to_string(source).unwrap()
    );
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    assert!(version.status.success());
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "FIXED121_NODE {}",
        serde_json::json!({"source":source,"version_rc":version.status.code(),"version":String::from_utf8_lossy(&version.stdout),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)})
    );
    assert!(output.status.success());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"kind":"string","units":expected})
    );
}

#[test]
fn fixed121_exact_red() {
    string("''+true;", &[116, 114, 117, 101]);
}

fn text(source: &str, expected: &str) {
    string(source, &expected.encode_utf16().collect::<Vec<_>>());
}
fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    let matches: Vec<_> = ir.functions.iter().filter(|f| f.name == name).collect();
    assert_eq!(matches.len(), 1, "{name}");
    matches[0].index
}
fn call_indices(debug: &str) -> Vec<u16> {
    debug
        .match_indices("Call(")
        .filter_map(|(at, _)| debug[at + 5..].split(',').next()?.trim().parse().ok())
        .collect()
}
fn thrown_units(source: &str, expected: &[u16]) {
    let source = source.to_owned();
    let expected = expected.to_vec();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let observer=Box::leak(Box::new(object_observer::Observer{units:Some(expected.len()),uncaught:true,..Default::default()}));
        let mut machine=probe_vm(&source,observer,"javascript-es5-object-observer");
        let diagnostic=format!("{:?}",machine.run().expect_err("actual uncaught guest throw"));
        assert!(diagnostic.contains("uncaught JavaScript throw"),"{diagnostic}");
        assert_eq!((machine.aot_exec_count(),machine.jit_native_exec_count()),(0,0));
        let at=machine.stack().iter().position(|v|*v==vm::Value::string("P7_OBSERVED")).unwrap();
        let vm::Value::Map(boxed)=&machine.stack()[at+1] else {panic!("throw box")};assert_eq!(boxed.len(),5);
        for (key,value) in [("tag",vm::Value::Int(4)),("number",vm::Value::Float(0.0)),("flag",vm::Value::Bool(false))]{assert_eq!(boxed.get(&vm::Value::string(key)),Some(&value));}
        for key in ["ref_slot","generation"]{assert!(matches!(boxed.get(&vm::Value::string(key)),Some(vm::Value::Int(n)) if *n>0));}
        assert_eq!(machine.stack()[at+2],vm::Value::Float(expected.len() as f64));
        for (i,u) in expected.iter().enumerate(){assert_eq!(machine.stack()[at+3+i],vm::Value::Float(f64::from(*u)));}
        eprintln!("FIXED121_THROW_UNITS {}",serde_json::json!({"source":source,"expected_units":expected,"error":diagnostic,"full_stack":format!("{:?}",machine.stack()),"guest_invocations":1,"native":0}));
        reset(&mut machine);
    }).unwrap().join().unwrap();
}

fn walk_stmts(ss: &mut [vm::Stmt], f: &mut impl FnMut(&mut Vec<vm::Stmt>)) {
    use vm::Stmt as S;
    for s in ss {
        match s {
            S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                walk_expr(expr, f)
            }
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                walk_expr(condition, f);
                walk_stmts(then_branch, f);
                walk_stmts(else_branch, f);
                f(then_branch);
                f(else_branch);
            }
            S::While {
                condition, body, ..
            } => {
                walk_expr(condition, f);
                walk_stmts(body, f);
            }
            S::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                walk_stmts(std::slice::from_mut(init), f);
                walk_expr(condition, f);
                walk_stmts(std::slice::from_mut(post), f);
                walk_stmts(body, f);
            }
            S::ClosureLet { closure, .. } => walk_expr(&mut closure.body, f),
            S::Noop { .. }
            | S::FuncDecl { .. }
            | S::Break { .. }
            | S::Continue { .. }
            | S::Drop { .. } => {}
        }
    }
}
fn walk_expr(e: &mut vm::Expr, f: &mut impl FnMut(&mut Vec<vm::Stmt>)) {
    use vm::Expr as E;
    match e {
        E::Block { stmts, expr } => {
            walk_stmts(stmts, f);
            walk_expr(expr, f);
            f(stmts);
        }
        E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) | E::ModuleCall(_, _, args, _) => {
            for a in args {
                walk_expr(a, f);
            }
        }
        E::Add(a, b)
        | E::Sub(a, b)
        | E::Mul(a, b)
        | E::Div(a, b)
        | E::Mod(a, b)
        | E::Eq(a, b)
        | E::Lt(a, b)
        | E::Gt(a, b)
        | E::And(a, b)
        | E::Or(a, b) => {
            walk_expr(a, f);
            walk_expr(b, f);
        }
        E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => walk_expr(a, f),
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, f);
            walk_expr(then_expr, f);
            walk_expr(else_expr, f);
        }
        E::ResultConstruct { payload, .. } => walk_expr(payload, f),
        E::ResultQuery { value, .. } | E::ResultTry { value, .. } => walk_expr(value, f),
        E::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            walk_expr(value, f);
            walk_expr(ok_expr, f);
            walk_expr(err_expr, f);
        }
        E::OptionalGet { container, key, .. } => {
            walk_expr(container, f);
            walk_expr(key, f);
        }
        E::OptionUnwrapOr {
            value, fallback, ..
        } => {
            walk_expr(value, f);
            walk_expr(fallback, f);
        }
        E::Match {
            value,
            arms,
            default,
            ..
        } => {
            walk_expr(value, f);
            for (_, a) in arms {
                walk_expr(a, f);
            }
            walk_expr(default, f);
        }
        E::Closure(c) => walk_expr(&mut c.body, f),
        E::ClosureCall(c, args) => {
            walk_expr(&mut c.body, f);
            for a in args {
                walk_expr(a, f);
            }
        }
        E::Null
        | E::Int(_)
        | E::Bool(_)
        | E::Float(_)
        | E::String(_)
        | E::Bytes(_)
        | E::Var(_)
        | E::MoveVar(_)
        | E::MoveField { .. }
        | E::MoveIndex { .. }
        | E::FunctionRef(..)
        | E::ModuleFunctionRef(..)
        | E::UnresolvedFunctionRef { .. } => {}
    }
}

// Test-only observers validate actual caller rows at allocation boundaries.
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
fn add(a: vm::Expr, b: vm::Expr) -> vm::Expr {
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
fn let_(index: u16, t: vm::TypeSchema, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index,
        declared_schema: Some(t),
        expr,
        line: 1,
    }
}
fn set(index: u16, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index,
        expr,
        line: 1,
    }
}
fn fresh_slot(next: &mut usize) -> u16 {
    let s = u16::try_from(*next).unwrap();
    *next += 1;
    s
}
struct FixedRootObserver {
    abrupt: bool,
    units: Option<usize>,
}
impl vm::SourcePlugin for FixedRootObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("fixed121-root-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let output = object_observer::Observer {
            units: self.units,
            force_gc: true,
            ..Default::default()
        };
        let mut ir = vm::SourcePlugin::parse_source(&output, source, context)?;
        let managed = helper(&ir, "__compat_es5_fixed_scalar_string_managed");
        let concat = helper(&ir, "__compat_es5_string_concat");
        if self.abrupt {
            for id in [managed, concat] {
                ir.function_impls
                    .get_mut(&id)
                    .unwrap()
                    .body_stmts
                    .insert(0, check(E::Bool(false)));
            }
            let begin = helper(&ir, "__compat_es5_node_begin");
            let size = ir.function_impls[&begin].param_slots[1];
            ir.function_impls
                .get_mut(&begin)
                .unwrap()
                .body_stmts
                .insert(
                    0,
                    check(E::And(
                        Box::new(E::Not(Box::new(eq(E::Var(size), E::Int(131))))),
                        Box::new(E::Not(Box::new(eq(E::Var(size), E::Int(132))))),
                    )),
                );
            eprintln!(
                "FIXED121_ABRUPT_OBSERVER source={source:?} fixed_producer_and_concat_entry_fatal=true skipped_literal_sizes=[131,132]"
            );
            return Ok(ir);
        }
        let read = helper(&ir, "__compat_ref_read");
        let decode = helper(&ir, "__compat_es5_decode_int");
        // Each root subject executes one exact site once. Actual output rows
        // start Undefined and become String: a second call would fail here.
        for (id, fi, ei) in [(managed, 2, 3), (concat, 3, 4)] {
            let p = ir.function_impls[&id].param_slots.clone();
            let row = add(
                add(E::Var(p[fi]), E::Int(4)),
                E::Mul(Box::new(E::Var(p[ei])), Box::new(E::Int(3))),
            );
            let tag = call(decode, vec![num(call(read, vec![E::Var(p[0]), num(row)]))]);
            ir.function_impls
                .get_mut(&id)
                .unwrap()
                .body_stmts
                .insert(0, check(eq(tag, E::Int(0))));
        }
        let checked = helper(&ir, "__compat_es5_checked_base");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let mut next = ir.locals;
        let mut observed = 0;
        let mut instrument = |ss: &mut Vec<S>| {
            let sites: Vec<_> = ss
                .iter()
                .enumerate()
                .filter_map(|(at, s)| match s {
                    S::Assign {
                        index,
                        expr: E::Call(i, _, a, _, _),
                        ..
                    } if *i == managed => Some((at, *index, a.clone())),
                    _ => None,
                })
                .collect();
            for (at, converted, args) in sites.into_iter().rev() {
                let [E::Var(c), _, E::Var(frame), E::Int(converted_entry)] = args.as_slice() else {
                    panic!("actual fixed_producer ABI")
                };
                let (c, frame, converted_entry) = (*c, *frame, *converted_entry);
                let (cat_at, returned, cat_args) = ss
                    .iter()
                    .enumerate()
                    .skip(at + 1)
                    .find_map(|(i, s)| match s {
                        S::Let {
                            index,
                            expr: E::Call(id, _, a, _, _),
                            ..
                        } if *id == concat => Some((i, *index, a.clone())),
                        _ => None,
                    })
                    .expect("normal concat after fixed_producer");
                let [E::Var(cc), E::Var(l), E::Var(r), E::Var(cf), E::Int(entry)] =
                    cat_args.as_slice()
                else {
                    panic!("actual concat ABI")
                };
                assert_eq!((*cc, *cf), (c, frame));
                let original = if *l == converted {
                    *r
                } else {
                    assert_eq!(*r, converted);
                    *l
                };
                let entry = *entry;
                let rd = |a: E| call(decode, vec![num(call(read, vec![E::Var(c), num(a)]))]);
                let base = |v| {
                    call(
                        checked,
                        vec![E::Var(c), get(v, "ref_slot"), get(v, "generation")],
                    )
                };
                let row = |e: E| {
                    add(
                        add(E::Var(frame), E::Int(4)),
                        E::Mul(Box::new(e), Box::new(E::Int(3))),
                    )
                };
                let pin = |v, e| {
                    let a = row(E::Int(e));
                    vec![
                        check(eq(rd(a.clone()), E::Int(4))),
                        check(eq(rd(add(a.clone(), E::Int(1))), get(v, "ref_slot"))),
                        check(eq(rd(add(a, E::Int(2))), get(v, "generation"))),
                    ]
                };
                let original_base = fresh_slot(&mut next);
                let converted_base = fresh_slot(&mut next);
                let cursor = fresh_slot(&mut next);
                let found = fresh_slot(&mut next);
                let ack = fresh_slot(&mut next);
                let epoch = fresh_slot(&mut next);
                let a = row(E::Var(cursor));
                let matches = E::And(
                    Box::new(eq(rd(a.clone()), E::Int(4))),
                    Box::new(E::And(
                        Box::new(eq(rd(add(a.clone(), E::Int(1))), get(original, "ref_slot"))),
                        Box::new(eq(rd(add(a, E::Int(2))), get(original, "generation"))),
                    )),
                );
                let before = vec![
                    let_(original_base, T::Int, base(original)),
                    let_(cursor, T::Int, E::Int(0)),
                    let_(found, T::Int, E::Int(0)),
                    check(eq(rd(E::Int(3)), E::Var(frame))),
                    S::While {
                        condition: E::Lt(
                            Box::new(E::Var(cursor)),
                            Box::new(rd(add(E::Var(frame), E::Int(3)))),
                        ),
                        body: vec![
                            S::IfElse {
                                condition: matches,
                                then_branch: vec![set(found, add(E::Var(found), E::Int(1)))],
                                else_branch: vec![],
                                line: 1,
                            },
                            set(cursor, add(E::Var(cursor), E::Int(1))),
                        ],
                        line: 1,
                    },
                    check(E::Gt(Box::new(E::Var(found)), Box::new(E::Int(0)))),
                    let_(epoch, T::Int, rd(E::Int(11))),
                    let_(ack, T::Int, call(collect, vec![E::Var(c)])),
                    check(eq(rd(E::Int(11)), add(E::Var(epoch), E::Int(1)))),
                    check(eq(base(original), E::Var(original_base))),
                ];
                let mut after = pin(converted, converted_entry);
                after.extend([
                    check(eq(base(original), E::Var(original_base))),
                    let_(converted_base, T::Int, base(converted)),
                    set(ack, call(collect, vec![E::Var(c)])),
                    check(eq(base(converted), E::Var(converted_base))),
                    check(eq(base(original), E::Var(original_base))),
                ]);
                let mut published = pin(returned, entry);
                published.extend([
                    check(E::Gt(Box::new(base(returned)), Box::new(E::Int(0)))),
                    check(eq(base(converted), E::Var(converted_base))),
                    check(eq(base(original), E::Var(original_base))),
                ]);
                // Read exact original units without allocating managed nodes.
                // These subjects use the same three-unit original in both directions.
                let surrogate = if source.contains("uDC00") {
                    0xDC00
                } else {
                    0xD800
                };
                for (offset, unit) in [65, 0, surrogate].into_iter().enumerate() {
                    let unchanged = check(eq(
                        rd(add(E::Var(original_base), E::Int(3 + offset as i64))),
                        E::Int(unit),
                    ));
                    after.push(unchanged.clone());
                    published.push(unchanged);
                }
                // Insert from the end; never allocate or collect after concat publication.
                ss.splice(cat_at + 1..cat_at + 1, published);
                ss.splice(at + 1..at + 1, after);
                ss.splice(at..at, before);
                observed += 1;
            }
        };
        walk_stmts(&mut ir.stmts, &mut instrument);
        instrument(&mut ir.stmts);
        for body in ir.function_impls.values_mut() {
            walk_stmts(&mut body.body_stmts, &mut instrument);
            instrument(&mut body.body_stmts);
            walk_expr(&mut body.body_expr, &mut instrument);
        }
        assert_eq!(observed, 1, "one exact mixed site per root subject");
        ir.locals = next;
        eprintln!(
            "FIXED121_ROOTS source={source:?} once_only_actual_Undefined_entry_gates=true original_units_unchanged=true actual_original_caller_row_found=true original_generation_across_fixed_producer_gc=true converted_caller_row_checked=true converted_generation_across_concat_gc=true output_caller_row_checked_before_consumption=true protected_publication_gap_has_no_gc=true"
        );
        Ok(ir)
    }
}
fn observed(source: &str, abrupt: bool, expected: Option<&[u16]>) {
    let source = source.to_owned();
    let expected = expected.map(|v| v.to_vec());
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let observer = Box::leak(Box::new(FixedRootObserver {
                abrupt,
                units: expected.as_ref().map(Vec::len),
            }));
            let mut machine = probe_vm(&source, observer, "fixed121-root-observer");
            let status = machine.run();
            eprintln!(
                "FIXED121_ROOT_VM source={source:?} abrupt={abrupt} status={status:?} stack={:?}",
                machine.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let at = machine
                .stack()
                .iter()
                .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                .unwrap();
            if let Some(expected) = expected {
                assert_eq!(
                    machine.stack()[at + 2],
                    vm::Value::Float(expected.len() as f64)
                );
                for (i, u) in expected.iter().enumerate() {
                    assert_eq!(machine.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
                }
            } else {
                let vm::Value::Map(v) = &machine.stack()[at + 1] else {
                    panic!("box")
                };
                assert_eq!(
                    v.get(&vm::Value::string("flag")),
                    Some(&vm::Value::Bool(true))
                );
            }
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn fixed121_kinds_and_directions() {
    for (scalar, spelling) in [
        ("true", "true"),
        ("false", "false"),
        ("null", "null"),
        ("undefined", "undefined"),
    ] {
        text(&format!("''+{scalar};"), spelling);
        text(&format!("{scalar}+'';"), spelling);
        text(&format!("'x'+{scalar};"), &format!("x{spelling}"));
        text(&format!("{scalar}+'x';"), &format!("{spelling}x"));
    }
}
#[test]
fn fixed121_exact_retirements() {
    for prefix in ["", "'A'+'B';"] {
        text(&format!("{prefix}true+'a';"), "truea");
        text(&format!("{prefix}'a'+null;"), "anull");
    }
    for prefix in ["", "'x'+1;"] {
        for (source, expected) in [
            ("'x'+true;", "xtrue"),
            ("false+'x';", "falsex"),
            ("'x'+null;", "xnull"),
            ("'x'+undefined;", "xundefined"),
        ] {
            text(&format!("{prefix}{source}"), expected);
        }
    }
}
#[test]
fn fixed121_flow_units_and_consumers() {
    for (source, expected) in [
        ("var b=false;var a=b;b=true;''+a;", "false"),
        ("var b=false;''+(b=true);", "true"),
        ("var b=false;''+(b?true:false);", "false"),
        ("var n=2;''+(n===2);", "true"),
        ("var u;var a=u;'x'+a;", "xundefined"),
        (
            "function f(){var undefined=false;return 'x'+undefined;}f();",
            "xfalse",
        ),
        (
            "function f(){var undefined=null;return undefined+'x';}f();",
            "nullx",
        ),
        (
            "function f(){var undefined=2;return 'x'+undefined;}f();",
            "x2",
        ),
        (
            "function f(){var undefined='z';return undefined+'x';}f();",
            "zx",
        ),
        ("'x'+true+null+undefined+2+'z';", "xtruenullundefined2z"),
        ("2+'x'+false;", "2xfalse"),
        ("true+('x'+2);", "truex2"),
        ("encodeURIComponent(' '+false);", "%20false"),
        ("decodeURIComponent(encodeURIComponent(null+' '));", "null "),
    ] {
        text(source, expected);
    }
    string(
        r#"'A\0\uD800'+false;"#,
        &[65, 0, 0xD800, 102, 97, 108, 115, 101],
    );
    string(
        r#"undefined+'\uDC00\0';"#,
        &[117, 110, 100, 101, 102, 105, 110, 101, 100, 0xDC00, 0],
    );
    for source in [
        "('x'+false).length===6&&(null+'x')==='nullx';",
        "var s='A';var a=s;var t=s+true;s===a&&s==='A'&&t==='Atrue';",
        "var n=0;var s=(n=n+1,'x')+(n=n+1,false);n===2&&s==='xfalse';",
        "var n=0;var s=(n=n+1,null)+(n=n+1,'x');n===2&&s==='nullx';",
        "var n=0;var s=(n=n+1,'x')+(n=n+1,undefined);n===2&&s==='xundefined';",
    ] {
        boxed(source, 2, 0.0, true);
    }
}
#[test]
fn fixed121_function_modes() {
    for (scalar, spelling) in [
        ("false", "false"),
        ("null", "null"),
        ("undefined", "undefined"),
    ] {
        for (prefix, directive) in [("", ""), ("", "'use strict';"), ("'use strict';", "")] {
            text(
                &format!("{prefix}function f(){{{directive}return 'x'+{scalar};}}f();"),
                &format!("x{spelling}"),
            );
            text(
                &format!("{prefix}function f(){{{directive}return {scalar}+'x';}}f();"),
                &format!("{spelling}x"),
            );
        }
        text(
            &format!("(function(){{return 'x'+{scalar};}})();"),
            &format!("x{spelling}"),
        );
        text(
            &format!("(function(){{'use strict';return {scalar}+'x';}})();"),
            &format!("{spelling}x"),
        );
    }
}
#[test]
fn fixed121_meaningful_refusals() {
    for prefix in ["", "'x'+true;"] {
        for (row, source) in [
            "false+1;",
            "true+1;",
            "null+null;",
            "undefined+1;",
            "'x'+({});",
            "[]+'x';",
            "(function(){})+'x';",
            "function f(x){return 'x'+x;}f(true);",
            "function f(undefined){return 'x'+undefined;}f(false);",
            "function f(){var undefined={};return 'x'+undefined;}f();",
            "var b=true?true:null;'x'+b;",
            "function f(){return false;}'x'+f();",
            "var o={b:true};'x'+o.b;",
        ]
        .into_iter()
        .enumerate()
        {
            let source = format!("{prefix}{source}");
            if row < 4 {
                let vm::Value::Map(b) = execute(&source) else {
                    panic!("Number box")
                };
                assert_eq!(b.get(&vm::Value::string("tag")), Some(&vm::Value::Int(3)));
                let vm::Value::Float(n) = b.get(&vm::Value::string("number")).unwrap() else {
                    panic!("Float")
                };
                let expected = [1.0, 2.0, 0.0, f64::NAN][row];
                assert!(if expected.is_nan() {
                    n.is_nan()
                } else {
                    n.to_bits() == expected.to_bits()
                });
                if row == 3 {
                    oracle(
                        &format!(
                            "Number.isNaN((0,eval)({}));",
                            serde_json::to_string(&source).unwrap()
                        ),
                        serde_json::json!({"kind":"boolean","value":true}),
                    );
                } else {
                    oracle(
                        &source,
                        serde_json::json!({"kind":"number","value":expected}),
                    );
                }
                continue;
            }
            if ![4, 5, 6, 9].contains(&row) {
                text(
                    &source,
                    if [8, 11].contains(&row) {
                        "xfalse"
                    } else {
                        "xtrue"
                    },
                );
                continue;
            }
            let error = parse(&source).expect_err("no generic coercion");
            eprintln!(
                "FIXED121_REFUSAL {}",
                serde_json::json!({"source":source,"error":format!("{error:?}"),"guest_invocations":0})
            );
            assert!(
                error
                    .message
                    .contains("arithmetic currently requires numeric operands"),
                "{source}: {error:?}"
            );
        }
    }
}
#[test]
fn fixed121_roots_publication_gc() {
    for (scalar, spelling) in [
        ("false", "false"),
        ("null", "null"),
        ("undefined", "undefined"),
    ] {
        for left in [true, false] {
            let source = if left {
                format!(r#"function f(){{return 'A\0\uD800'+{scalar};}}f();"#)
            } else {
                format!(r#"function f(){{'use strict';return {scalar}+'A\0\uDC00';}}f();"#)
            };
            let mut units = vec![65, 0, if left { 0xD800 } else { 0xDC00 }];
            if left {
                units.extend(spelling.encode_utf16());
            } else {
                let mut v = spelling.encode_utf16().collect::<Vec<_>>();
                v.extend(units);
                units = v;
            }
            observed(&source, false, Some(&units));
            oracle_units(&source, &units);
        }
    }
}
#[test]
fn fixed121_abrupt_skips_conversion_concat_and_siblings() {
    let l = "L".repeat(128);
    let r = "R".repeat(129);
    for scalar in ["false", "null", "undefined"] {
        for source in [
            format!(
                "function fail(){{throw 'left';}}var n=0;var ok=false;try{{(fail(),{scalar})+(n=99,'{r}');}}catch(e){{ok=e==='left';}}finally{{n=n===0?1:99;}}ok&&n===1;"
            ),
            format!(
                "function fail(){{throw 'left';}}var n=0;var ok=false;try{{(fail(),'{l}')+(n=99,{scalar});}}catch(e){{ok=e==='left';}}finally{{n=n===0?1:99;}}ok&&n===1;"
            ),
            format!(
                "function fail(){{throw 'right';}}var n=0;var ok=false;try{{(n=n+1,{scalar})+(fail(),'{r}');}}catch(e){{ok=e==='right';}}finally{{n=n===1?2:99;}}ok&&n===2;"
            ),
            format!(
                "function fail(){{throw 'right';}}var n=0;var ok=false;try{{(n=n+1,'x')+(fail(),{scalar});}}catch(e){{ok=e==='right';}}finally{{n=n===1?2:99;}}ok&&n===2;"
            ),
        ] {
            observed(&source, true, None);
            oracle(&source, serde_json::json!({"kind":"boolean","value":true}));
        }
    }
}
#[test]
fn fixed121_throw_finally_and_input_identity() {
    for (scalar, spelling) in [
        ("true", "true"),
        ("null", "null"),
        ("undefined", "undefined"),
    ] {
        let source = format!(
            "function f(){{try{{throw 'A'+{scalar};}}finally{{var t={scalar}+'B';}}}}var ok=false;try{{f();}}catch(e){{ok=e==='A{spelling}';}}ok;"
        );
        // The output observer collects after all guest execution, while catch
        // checks the exact payload preserved across the finally allocation.
        observed_finally(&source);
    }
}
fn observed_finally(source: &str) {
    let source = source.to_owned();
    let reference = source.clone();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let obs = Box::leak(Box::new(object_observer::Observer {
                force_gc: true,
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, obs, "javascript-es5-object-observer");
            assert_eq!(machine.run().unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let at = machine
                .stack()
                .iter()
                .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                .unwrap();
            let vm::Value::Map(v) = &machine.stack()[at + 1] else {
                panic!("box")
            };
            assert_eq!(
                v.get(&vm::Value::string("flag")),
                Some(&vm::Value::Bool(true))
            );
            eprintln!(
                "FIXED121_FINALLY source={source:?} stack={:?}",
                machine.stack()
            );
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
    oracle(
        &reference,
        serde_json::json!({"kind":"boolean","value":true}),
    );
}
#[test]
fn fixed121_public_file_controls() {
    const BODY: &str = "var a=''+true;var b=false+'';var c=''+null;var d=undefined+'';if(a!=='true'||b!=='false'||c!=='null'||d!=='undefined')$ERROR('fixed'+false);";
    for (id, body, expected) in [
        ("positive", BODY.to_owned(), None),
        (
            "sentinel",
            format!("{BODY}$ERROR('sentinel'+true);"),
            Some("sentineltrue"),
        ),
        (
            "mutant",
            BODY.replacen("a!=='true'", "a!=='false'", 1),
            Some("fixedfalse"),
        ),
    ] {
        let source = format!("function $ERROR(message){{throw message;}}{body}1;");
        let copy = source.clone();
        std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
            let path=std::env::temp_dir().join(format!("fixed121-{id}-{}.js",std::process::id()));std::fs::write(&path,&copy).unwrap();
            let result=pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(&path);
            std::fs::remove_file(&path).unwrap();
            if expected.is_some() {
                let error=result.expect_err("managed guest throw");assert!(matches!(error,pd_vm_compat_frontends::javascript::corpus::CorpusFileError::Execution{..}));
                let diagnostic=format!("{error:?}");assert!(diagnostic.contains("uncaught JavaScript throw")&&!diagnostic.contains("Fuel"));
                eprintln!("FIXED121_FILE {}",serde_json::json!({"id":id,"source":copy,"error":diagnostic,"guest_invocations":1,"upstream_original":false}));
            } else {
                let o=result.unwrap();assert_eq!(o.cookie,1);assert_eq!((o.aot_exec_count,o.jit_native_exec_count),(0,0));
                assert!(o.certification_valid_before_reset&&!o.certification_valid_after_reset);assert_eq!(o.resources_after_reset,0);assert!(o.owned_arrays_retired);
                eprintln!("FIXED121_FILE {}",serde_json::json!({"id":id,"source":copy,"observation":format!("{o:?}"),"guest_invocations":1,"upstream_original":false}));
            }
        }).unwrap().join().unwrap();
        if let Some(expected) = expected {
            thrown_units(&source, &expected.encode_utf16().collect::<Vec<_>>());
            oracle(
                &source,
                serde_json::json!({"kind":"throw","value":expected}),
            );
        } else {
            oracle(&source, serde_json::json!({"kind":"number","value":1}));
        }
    }
}
#[test]
fn fixed121_prerequisites_indices() {
    for source in [
        "'x'+false;",
        "function f(){var k=1.5;var o={'1.5':7};return o[k];}var o={get length(){'use strict';return ('A'+true).length;}};('A'+1)==='A1'&&o.length===5&&f()===7&&Math.abs(-3)===3&&encodeURIComponent('A '+null)==='A%20null'&&(undefined+'x')==='undefinedx';",
    ] {
        let ir = parse(source).unwrap();
        let fixed = helper(&ir, "__compat_es5_fixed_scalar_string_managed");
        let concat = helper(&ir, "__compat_es5_string_concat");
        let to = helper(&ir, "__compat_es5_managed_to_units");
        let from = helper(&ir, "__compat_es5_units_to_managed");
        assert!(to < fixed && from < fixed && fixed < concat);
        assert_eq!(
            ir.functions
                .iter()
                .find(|f| f.index == fixed)
                .unwrap()
                .arity,
            4
        );
        assert_eq!(
            ir.functions
                .iter()
                .find(|f| f.index == concat)
                .unwrap()
                .arity,
            5
        );
        let ids: std::collections::HashSet<_> = ir.functions.iter().map(|f| f.index).collect();
        assert_eq!(ids.len(), ir.functions.len());
        for body in ir.function_impls.values() {
            assert!(body.param_slots.iter().all(|s| usize::from(*s) < ir.locals));
            let debug = format!("{body:?}");
            for token in ["Var(", "MoveVar("] {
                for (at, _) in debug.match_indices(token) {
                    let slot: usize = debug[at + token.len()..]
                        .split(')')
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    assert!(slot < ir.locals);
                }
            }
        }
        if source == "'x'+false;" {
            for name in [
                "__compat_es5_number_string_managed",
                "__compat_es5_number_string_units",
                "__compat_es5_ref_get",
            ] {
                assert!(!ir.functions.iter().any(|f| f.name == name));
            }
            text(source, "xfalse");
        } else {
            let managed = helper(&ir, "__compat_es5_number_string_managed");
            let units = helper(&ir, "__compat_es5_number_string_units");
            assert!(units < managed && managed < fixed);
            let get = helper(&ir, "__compat_es5_ref_get");
            let dispatch = helper(&ir, "__compat_es5_dispatch");
            let length = helper(&ir, "__compat_es5_string_length_get");
            assert!(call_indices(&format!("{:?}", ir.function_impls[&get])).contains(&dispatch));
            let mut funcs: Vec<_> = ir
                .functions
                .iter()
                .filter(|f| f.name.starts_with("__compat_es5_fn_"))
                .map(|f| f.index)
                .collect();
            funcs.sort();
            let mut inits: Vec<_> = ir
                .functions
                .iter()
                .filter(|f| f.name.starts_with("__compat_es5_utf16_init_"))
                .map(|f| f.index)
                .collect();
            inits.sort();
            assert_eq!(funcs[0], length + 1 + 5);
            assert_eq!(dispatch, funcs[0] + funcs.len() as u16);
            assert_eq!(inits[0], dispatch + 1);
            eprintln!(
                "FIXED121_INDEX_WINDOW {}",
                serde_json::json!({"source":source,"managed":managed,"units":units,"get":get,"dispatch":dispatch,"length":length,"functions":funcs,"initializers":inits})
            );
            boxed(source, 2, 0.0, true);
        }
        eprintln!(
            "FIXED121_PREREQUISITES {}",
            serde_json::json!({"source":source,"fixed":fixed,"concat":concat,"to":to,"from":from,"locals":ir.locals,"unique_functions":ids.len()})
        );
    }
    for source in ["'A'+'B';", "'x'+1;", "1+'x';"] {
        assert!(
            !parse(source)
                .unwrap()
                .functions
                .iter()
                .any(|f| f.name == "__compat_es5_fixed_scalar_string_managed")
        );
    }
}
