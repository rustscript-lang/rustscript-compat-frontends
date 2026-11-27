//! Bounded Float remainder, public VM and independent Node observations.
fn boolean(source: &str, expected: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "REM131_SOURCE {}",
                serde_json::json!({"source":source,"expected":expected})
            );
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
            eprintln!("REM131_VM status={status:?} stack={:?}", machine.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let vm::Value::Map(map) = machine.stack().last().unwrap() else {
                panic!("boxed Boolean")
            };
            assert_eq!(map.len(), 5);
            for (name, value) in [
                ("tag", vm::Value::Int(2)),
                ("number", vm::Value::Float(0.0)),
                ("flag", vm::Value::Bool(expected)),
                ("ref_slot", vm::Value::Int(0)),
                ("generation", vm::Value::Int(0)),
            ] {
                assert_eq!(map.get(&vm::Value::string(name)), Some(&value), "{source}");
            }
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
            assert!(
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                    .is_none()
            );
            eprintln!("REM131_RESET resources=0 owned_retired=true cert=false native=0");
            oracle(
                &source,
                serde_json::json!({"kind":"boolean","value":expected}),
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
fn parse(source: &str) -> Result<vm::FrontendIr, vm::ParseError> {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    vm::SourcePlugin::parse_source(
        &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
        source,
        &vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        },
    )
}
fn oracle(source: &str, expected: serde_json::Value) {
    let script = format!(
        "const source={};try{{let v=(0,eval)(source);console.log(JSON.stringify({{kind:typeof v,value:v}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',value:String(e)}}));}}",
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
        "REM131_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version_rc":version.status.code(),"version_stdout":String::from_utf8_lossy(&version.stdout),"version_stderr":String::from_utf8_lossy(&version.stderr)})
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        expected
    );
}
fn scalar(source: &str, tag: i64, expected: f64) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "REM131_SOURCE {}",
                serde_json::json!({"source":source,"tag":tag,"expected":expected})
            );
            let mut m =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|e| panic!("source={source:?} guest_invocations=0 {e:?}"));
            m.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*m.jit_config()
            });
            m.set_fuel(10_000_000);
            if source == "5%2;" {
                let program=m.program();
                let mods:Vec<_>=program.type_map.as_ref().unwrap().operand_types.iter().filter(|(ip,_)|program.code[**ip]==vm::bytecode::OpCode::Mod as u8).collect();
                assert_eq!(mods.len(),1);
                assert_eq!(*mods[0].1,(vm::ValueType::Float,vm::ValueType::Float));
                eprintln!("REM131_MOD_INSTRUCTION source={source:?} operand_types={mods:?} disassembly={}",vm::vmbc::disassemble_program(program));
            }
            let status = m.run();
            eprintln!("REM131_VM status={status:?} stack={:?}", m.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let vm::Value::Map(b) = m.stack().last().unwrap() else {
                panic!("box")
            };
            assert_eq!(b.len(), 5);
            assert_eq!(b.get(&vm::Value::string("tag")), Some(&vm::Value::Int(tag)));
            let vm::Value::Float(n) = b.get(&vm::Value::string("number")).unwrap() else {
                panic!("number")
            };
            assert!(
                if expected.is_nan() {
                    n.is_nan()
                } else {
                    n.to_bits() == expected.to_bits()
                },
                "{source}: {n:?}"
            );
            for (k, v) in [
                ("flag", vm::Value::Bool(false)),
                ("ref_slot", vm::Value::Int(0)),
                ("generation", vm::Value::Int(0)),
            ] {
                assert_eq!(b.get(&vm::Value::string(k)), Some(&v));
            }
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            assert!(m.capture_certification_stats().unwrap().valid);
            m.reset_for_reuse().unwrap();
            if m.scope_reset_pending() {
                let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(matches!(
                    m.poll_reset_for_reuse(&mut cx),
                    std::task::Poll::Ready(Ok(()))
                ));
            }
            assert_eq!(m.host_context().resource_count(), 0);
            assert!(!m.capture_certification_stats().unwrap().valid);
            assert!(
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut m).is_none()
            );
            eprintln!("REM131_RESET resources=0 owned_retired=true cert=false native=0");
            oracle_scalar(&source, tag, expected);
        })
        .unwrap()
        .join()
        .unwrap();
}
fn run_vm(source: &str, f: fn(&mut vm::Vm)) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("REM131_WORKER source={source:?}");
            let mut m =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap();
            m.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*m.jit_config()
            });
            m.set_fuel(10_000_000);
            f(&mut m);
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
        })
        .unwrap()
        .join()
        .unwrap();
}
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
fn oracle_units_completion(source: &str, expected: Option<[u16; 3]>) {
    oracle_units(
        source,
        if let Some(u) = expected {
            serde_json::json!({"kind":"throw","units":u})
        } else {
            serde_json::json!({"kind":"number","value":1})
        },
    );
}
fn oracle_units(source: &str, expected: serde_json::Value) {
    let script = format!(
        "const source={};try{{let v=(0,eval)(source);console.log(JSON.stringify(typeof v==='string'?{{kind:'string',units:Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))}}:{{kind:typeof v,value:v}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',units:Array.from({{length:e.length}},(_,i)=>e.charCodeAt(i))}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let v = std::process::Command::new("node")
        .arg("--version")
        .output()
        .unwrap();
    let o = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "REM131_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":o.status.code(),"stdout":String::from_utf8_lossy(&o.stdout),"stderr":String::from_utf8_lossy(&o.stderr),"version_rc":v.status.code(),"version_stdout":String::from_utf8_lossy(&v.stdout),"version_stderr":String::from_utf8_lossy(&v.stderr)})
    );
    assert!(v.status.success() && o.status.success() && o.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(actual, expected);
}

fn oracle_scalar(source: &str, tag: i64, number: f64) {
    let script = format!(
        "const source={};let v=(0,eval)(source);console.log(JSON.stringify(typeof v==='undefined'?{{kind:'undefined'}}:{{kind:typeof v,number:String(v),negative_zero:Object.is(v,-0)}}));",
        serde_json::to_string(source).unwrap()
    );
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "REM131_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version_rc":version.status.code(),"version_stdout":String::from_utf8_lossy(&version.stdout),"version_stderr":String::from_utf8_lossy(&version.stderr)})
    );
    assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    if tag == 0 {
        assert_eq!(actual, serde_json::json!({"kind":"undefined"}));
    } else {
        assert_eq!(actual["kind"], "number");
        let text = actual["number"].as_str().unwrap();
        let expected_class = if number.is_nan() {
            "NaN"
        } else if number == f64::INFINITY {
            "Infinity"
        } else if number == f64::NEG_INFINITY {
            "-Infinity"
        } else {
            "finite"
        };
        let actual_class = match text {
            "NaN" => "NaN",
            "Infinity" => "Infinity",
            "-Infinity" => "-Infinity",
            _ => "finite",
        };
        assert_eq!(actual_class, expected_class);
        assert_eq!(
            actual["negative_zero"],
            number == 0.0 && number.is_sign_negative()
        );
        if expected_class == "finite" {
            let parsed: f64 = text.parse().unwrap();
            assert_eq!(
                parsed.to_bits(),
                if number == 0.0 {
                    0.0f64.to_bits()
                } else {
                    number.to_bits()
                }
            );
        }
    }
}

fn visit_expr(e: &mut vm::Expr, f: &mut impl FnMut(&mut vm::Expr)) {
    use vm::Expr as E;
    f(e);
    match e {
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
            visit_expr(a, f);
            visit_expr(b, f);
        }
        E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => visit_expr(a, f),
        E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) | E::ModuleCall(_, _, args, _) => {
            for a in args {
                visit_expr(a, f);
            }
        }
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_expr(condition, f);
            visit_expr(then_expr, f);
            visit_expr(else_expr, f);
        }
        E::Block { stmts, expr } => {
            visit_stmts(stmts, f);
            visit_expr(expr, f);
        }
        E::ResultConstruct { payload, .. } => visit_expr(payload, f),
        E::ResultQuery { value, .. } | E::ResultTry { value, .. } => visit_expr(value, f),
        E::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            visit_expr(value, f);
            visit_expr(ok_expr, f);
            visit_expr(err_expr, f);
        }
        E::OptionalGet { container, key, .. } => {
            visit_expr(container, f);
            visit_expr(key, f);
        }
        E::OptionUnwrapOr {
            value, fallback, ..
        } => {
            visit_expr(value, f);
            visit_expr(fallback, f);
        }
        E::Match {
            value,
            arms,
            default,
            ..
        } => {
            visit_expr(value, f);
            for (_, a) in arms {
                visit_expr(a, f);
            }
            visit_expr(default, f);
        }
        E::Closure(c) => visit_expr(&mut c.body, f),
        E::ClosureCall(c, args) => {
            visit_expr(&mut c.body, f);
            for a in args {
                visit_expr(a, f);
            }
        }
        E::Null
        | E::Int(_)
        | E::Float(_)
        | E::Bool(_)
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
fn visit_stmts(ss: &mut [vm::Stmt], f: &mut impl FnMut(&mut vm::Expr)) {
    use vm::Stmt as S;
    for s in ss {
        match s {
            S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                visit_expr(expr, f)
            }
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                visit_expr(condition, f);
                visit_stmts(then_branch, f);
                visit_stmts(else_branch, f);
            }
            S::While {
                condition, body, ..
            } => {
                visit_expr(condition, f);
                visit_stmts(body, f);
            }
            S::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                visit_stmts(std::slice::from_mut(init), f);
                visit_expr(condition, f);
                visit_stmts(std::slice::from_mut(post), f);
                visit_stmts(body, f);
            }
            S::ClosureLet { closure, .. } => visit_expr(&mut closure.body, f),
            S::Noop { .. }
            | S::FuncDecl { .. }
            | S::Break { .. }
            | S::Continue { .. }
            | S::Drop { .. } => {}
        }
    }
}

#[test]
fn remainder131_source_red_green() {
    scalar("5%2;", 3, 1.0);
}
#[test]
fn remainder131_preexisting_conditional() {
    assert!(parse("1 ? 2 : 3;").is_ok());
}
#[test]
fn remainder131_ieee() {
    for (s, n) in [
        ("5%2;", 1.0),
        ("5%-2;", 1.0),
        ("-5%2;", -1.0),
        ("-5%-2;", -1.0),
        ("5.5%2;", 1.5),
        ("-5.5%2;", -1.5),
        ("5.5%-2;", 1.5),
        ("-5.5%-2;", -1.5),
        ("6.5%2.25;", 2.0),
        ("1%2;", 1.0),
        ("-1%2;", -1.0),
        ("0%2;", 0.0),
        ("-0%2;", -0.0),
        ("0%-2;", 0.0),
        ("-0%-2;", -0.0),
        ("6%2;", 0.0),
        ("-6%2;", -0.0),
        ("-6%-2;", -0.0),
        ("1%0;", f64::NAN),
        ("1%-0;", f64::NAN),
        ("0%0;", f64::NAN),
        ("-0%-0;", f64::NAN),
        ("NaN%2;", f64::NAN),
        ("2%NaN;", f64::NAN),
        ("NaN%NaN;", f64::NAN),
        ("Infinity%2;", f64::NAN),
        ("-Infinity%2;", f64::NAN),
        ("2%Infinity;", 2.0),
        ("-2%Infinity;", -2.0),
        ("2%-Infinity;", 2.0),
        ("-2%-Infinity;", -2.0),
        ("Infinity%Infinity;", f64::NAN),
        ("Infinity%-Infinity;", f64::NAN),
        ("-Infinity%Infinity;", f64::NAN),
        ("-Infinity%-Infinity;", f64::NAN),
        ("-0%Infinity;", -0.0),
        ("0%-Infinity;", 0.0),
        ("1e308%3;", 2.0),
        ("-1e308%3;", -2.0),
        ("1e-300%3e-301;", 1.0000000000000003e-301),
        ("5e-324%1e-323;", 5e-324),
        ("-5e-324%1e-323;", -5e-324),
        ("1e308%1e-308;", 3.498445546245627e-309),
    ] {
        scalar(s, 3, n);
    }
}
#[test]
fn remainder131_once_order_facts() {
    for s in [
        "var x=5;var n=x%(x=2);n===1&&x===2;",
        "var o={n:0,get a(){'use strict';this.n=this.n*10+1;return 5;},get b(){'use strict';this.n=this.n*10+2;return 2;}};var r=o.a%o.b;r===1&&o.n===12;",
        "function f(o,n){if(n===1){o.n=o.n*10+1;return 5;}o.n=o.n*10+2;return 2;}var o={n:0};var r=f(o,1)%f(o,2);r===1&&o.n===12;",
        "var x=0;var r=(x=x+1,5)%(x=x+1,2);r===1&&x===2;",
        "var x=5;var r=(true?x:7)%(false?3:2);r===1;",
        "var x=0;while(x<3){x=x+1;var n=(x+4)%2;}for(var j=0;j<2;j=j+1){n=(j+3)%2;}x===3&&j===2&&n===0;",
        "function f(){return 9%4;}f()===1;",
        "function f(){'use strict';return 9%4;}f()===1;",
        "'use strict';function f(){return 9%4;}f()===1;",
        "var o={get n(){return 7;}};o.n%3===1;",
        "+((17%5)%2)===0&&(void (7%3))===undefined;",
        "var n=1;n=(7%3)+2;n===3&&2*(7%3)===2;",
        "'n='+(7%3)==='n=1'&&(7%3)+'x'==='1x';",
    ] {
        boolean(s, true);
    }
}
#[test]
fn remainder131_hidden_managed_scanner() {
    for s in [
        "(true?('hidden',5):7)%2;",
        "5%(false?3:('hidden',2));",
        "(true?({},5):7)%2;",
        "(true?([1],5):7)%2;",
        "(true?((function(){return 5;})(),5):7)%2;",
    ] {
        scalar(s, 3, 1.0);
    }
}
#[test]
fn remainder131_retired_exact_sources() {
    boolean("false && (1%2);", false);
    boolean("true || (1%2);", true);
    scalar(
        "function f(){'use strict';return;false && (1%2);} f();",
        0,
        0.0,
    );
    scalar("void (1%2);", 0, 0.0);
    scalar("if(false){void (1%2);}", 0, 0.0);
}
#[test]
fn remainder131_numeric_refusals() {
    // ES5.1 12.5 returns Empty for a false If without else; 12.1 step 5
    // retains the preceding String. Modern Node instead returns Undefined.
    fn retained_string(source: &str) {
        static OBS: object_observer::Observer = object_observer::Observer {
            units: Some(9),
            force_gc: true,
            uncaught: false,
            type_error: false,
            realm_header: false,
        };
        let source = source.to_owned();
        std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
            let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
            let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
                &source, vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer")),
                vm::CompileSourceFileOptions::new().with_host_api_catalog(catalog.clone()).with_source_plugin(&OBS),
            ).unwrap();
            let mut m = issued.into_vm().unwrap();
            m.set_jit_config(vm::JitConfig { enabled: false, ..*m.jit_config() });
            assert!(!m.jit_config().enabled);
            let before = (m.aot_exec_count(), m.jit_native_exec_count());
            assert_eq!(before, (0, 0));
            pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(&mut m, Default::default()).unwrap();
            // Match the public JS CLI's registry, including the private parser
            // builtins demanded by a validated String operand in a skipped arm.
            let mut registry = vm::HostFunctionRegistry::new();
            for module in [
                pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
                pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
                pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
                pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
                pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
            ] { module.install_from_catalog(&mut registry, &catalog).unwrap(); }
            registry.bind_vm_cached(&mut m).unwrap();
            m.set_fuel(10_000_000);
            let status = m.run();
            let after = (m.aot_exec_count(), m.jit_native_exec_count());
            assert_eq!(after, (0, 0));
            assert!(!m.jit_config().enabled);
            eprintln!("TRANS138B_COMPLETION_VM source={source:?} status={status:?} before={before:?} after={after:?} stack={:?}", m.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let at = m.stack().iter().position(|v| *v == vm::Value::string("P7_OBSERVED")).unwrap();
            let vm::Value::Map(boxed) = &m.stack()[at + 1] else { panic!("String box") };
            assert_eq!(boxed.get(&vm::Value::string("tag")), Some(&vm::Value::Int(4)));
            let units: Vec<_> = "unrelated".encode_utf16().collect();
            assert_eq!(m.stack()[at + 2], vm::Value::Float(units.len() as f64));
            for (i, unit) in units.iter().enumerate() {
                assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*unit)));
            }
            reset(&mut m);
            oracle_units(&source, serde_json::json!({"kind":"undefined"}));
            eprintln!("LEGACY138B_COMPLETION_POLICY source={source:?} es51_string_units={units:?} modern_node=undefined rule=12.5-12.1");
        }).unwrap().join().unwrap();
    }

    for s in [
        "true%2;",
        "2%false;",
        "'1'%2;",
        "2%'1';",
        "null%2;",
        "2%null;",
        "undefined%2;",
        "2%undefined;",
        "({})%2;",
        "2%({});",
        "[]%2;",
        "2%[];",
        "function f(x){return x%2;}f(5);",
        "function f(){try{throw 5;}catch(e){return e%2;}}f();",
        "var x=true?'a':1;x%2;",
        "var x=1;void(x='a');x%2;",
        "false&&(1%true);",
        "true||(1%true);",
        "if(false){1%true;}",
        "function f(){return;1%true;}f();",
        "void (1%true);",
        "if(false){void (1%true);}",
    ] {
        for prefix in ["", "'unrelated';"] {
            let source = format!("{prefix}{s}");
            if !prefix.is_empty() && s.starts_with("if(false)") {
                retained_string(&source);
                continue;
            }
            match s {
                "true%2;"
                | "'1'%2;"
                | "function f(x){return x%2;}f(5);"
                | "function f(){try{throw 5;}catch(e){return e%2;}}f();" => scalar(&source, 3, 1.0),
                "2%'1';" | "null%2;" => scalar(&source, 3, 0.0),
                "2%false;"
                | "2%null;"
                | "undefined%2;"
                | "2%undefined;"
                | "var x=true?'a':1;x%2;"
                | "var x=1;void(x='a');x%2;" => scalar(&source, 3, f64::NAN),
                "false&&(1%true);" => boolean(&source, false),
                "true||(1%true);" => boolean(&source, true),
                "if(false){1%true;}"
                | "function f(){return;1%true;}f();"
                | "void (1%true);"
                | "if(false){void (1%true);}" => scalar(&source, 0, 0.0),
                _ => {
                    let e = parse(&source).expect_err("nonprimitive numeric refusal");
                    eprintln!("REM131_REFUSED source={source:?} guest_invocations=0 error={e:?}");
                    assert!(e.span.is_some());
                    assert!(format!("{e:?}").contains("numeric"));
                }
            }
        }
    }
}
#[test]
fn remainder131_guard_fatal() {
    for s in [
        "function f(){return 'x';}var caught=false;try{f()%2;}catch(e){caught=true;}caught;",
        "function f(){return true;}var caught=false;try{5%f();}catch(e){caught=true;}caught;",
    ] {
        boolean(s, false);
    }
    for s in [
        "function f(){return {};}var caught=false;try{f()%2;}catch(e){caught=true;}caught;",
        "function f(){return [1];}var caught=false;try{5%f();}catch(e){caught=true;}caught;",
    ] {
        run_vm(s, |m| {
            let status = m.run();
            eprintln!("REM131_FATAL status={status:?}");
            assert!(
                format!("{:?}", status.expect_err("actual nonNumber guard is fatal"))
                    .contains("assertion failed")
            );
        });
    }
}
#[test]
fn remainder131_completion_result_finally() {
    for s in [
        "function fail(){throw 'L';}var o={n:0,get r(){'use strict';this.n=this.n+1;return 2;}};var ok=false;try{fail()%o.r;}catch(e){ok=e==='L';}ok&&o.n===0;",
        "function bad(){return 'x';}function fail(){throw 'R';}var ok=false;try{bad()%fail();}catch(e){ok=e==='R';}ok;",
        "var o={get a(){return 'x';},get b(){throw 'R';}};var ok=false;try{o.a%o.b;}catch(e){ok=e==='R';}ok;",
        "function fail(){throw 'L';}var ok=false;var n=0;try{fail()%2;n=99;}catch(e){ok=e==='L';}finally{n=n+1;}ok&&n===1;",
        "function f(){function fail(){throw 'R';}try{return 5%fail();}finally{return 7;}}f()===7;",
        "function f(){try{return 5%2;}finally{throw 'F';}}var ok=false;try{f();}catch(e){ok=e==='F';}ok;",
        "function f(){function fail(){throw 'R';}try{return 5%fail();}catch(e){return e==='R';}finally{var s='a'+'b';}}f();",
        "function g(){function f(){return 5%2;}try{return f()%2;}finally{var s='a'+'b';}}g()===1;",
    ] {
        boolean(s, true);
    }
}
#[test]
fn remainder131_float_ir() {
    use vm::{Expr as E, Stmt as S, TypeSchema as T};
    let mut ir = parse("5%2;").unwrap();
    assert!(ir.functions.is_empty());
    let mut slots = None;
    visit_stmts(&mut ir.stmts, &mut |e| {
        if let E::Mod(a, b) = e {
            let (E::Var(a), E::Var(b)) = (a.as_ref(), b.as_ref()) else {
                panic!("bound operands")
            };
            assert!(slots.replace((*a, *b)).is_none());
        }
    });
    let (a, b) = slots.unwrap();
    let mut typed = Vec::new();
    visit_stmts(&mut ir.stmts, &mut |e| {
        if let E::Block { stmts, .. } = e {
            for s in stmts {
                if let S::Let {
                    index,
                    declared_schema: Some(t),
                    ..
                } = s
                    && (*index == a || *index == b)
                {
                    assert_eq!(*t, T::Float);
                    typed.push(*index);
                }
            }
        }
    });
    assert_eq!(typed, vec![a, b]);
    let text = format!("{:?}", ir.stmts);
    assert_eq!(
        text.matches(&format!(
            "Call({}, [], []",
            vm::BuiltinFunction::MapNew.call_index()
        ))
        .count(),
        3
    );
    let schema = format!("{:?}", ir.struct_schemas);
    assert!(schema.contains("\"number\": Float"));
    eprintln!("REM131_FLOAT_IR slots={slots:?} typed={typed:?} schema={schema} stmts={text}");
}

// Mutate only the actual Mod inputs/result in test IR. The terminal status,
// not construction-time instrumentation text, proves marker entry or skip.
#[derive(Clone, Copy, Debug)]
enum Marker {
    Left,
    Right,
    Mod,
}
struct ModObserver(Marker);
impl vm::SourcePlugin for ModObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("remainder131-marker");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            cx,
        )?;
        let mut sites = Vec::new();
        visit_stmts(&mut ir.stmts, &mut |e| {
            if let E::Mod(a, b) = e {
                let (E::Var(a), E::Var(b)) = (a.as_ref(), b.as_ref()) else {
                    panic!("typed Mod vars")
                };
                sites.push((*a, *b));
            }
        });
        assert_eq!(sites.len(), 1);
        let (a, b) = sites[0];
        let fatal = |expr: E| E::Block {
            stmts: vec![S::Expr {
                expr: E::Call(
                    vm::BuiltinFunction::Assert.call_index(),
                    vec![],
                    vec![E::Bool(false)],
                    None,
                    None,
                ),
                line: 1,
            }],
            expr: Box::new(expr),
        };
        let mut changed = 0;
        fn projections(ss: &mut [S], slot: u16, changed: &mut usize, f: &impl Fn(E) -> E) {
            for s in ss {
                match s {
                    S::Let { index, expr, .. } if *index == slot => {
                        *expr = f(expr.clone());
                        *changed += 1;
                    }
                    S::IfElse {
                        then_branch,
                        else_branch,
                        ..
                    } => {
                        projections(then_branch, slot, changed, f);
                        projections(else_branch, slot, changed, f);
                    }
                    _ => {}
                }
            }
        }
        match self.0 {
            Marker::Mod => visit_stmts(&mut ir.stmts, &mut |e| {
                if changed == 0 && matches!(e, E::Mod(..)) {
                    *e = fatal(e.clone());
                    changed += 1;
                }
            }),
            Marker::Left | Marker::Right => {
                let slot = if matches!(self.0, Marker::Left) { a } else { b };
                visit_stmts(&mut ir.stmts, &mut |e| {
                    if let E::Block { stmts, .. } = e {
                        projections(stmts, slot, &mut changed, &fatal);
                    }
                });
            }
        }
        assert_eq!(changed, 1, "exact actual marker site");
        eprintln!(
            "REM131_MARKER_SELECTED source={source:?} marker={:?} sites={sites:?} mutations={changed}",
            self.0
        );
        Ok(ir)
    }
}
#[test]
fn remainder131_abrupt_projection_markers() {
    static L: ModObserver = ModObserver(Marker::Left);
    static R: ModObserver = ModObserver(Marker::Right);
    static M: ModObserver = ModObserver(Marker::Mod);
    for obs in [&L, &R, &M] {
        for (source, normal) in [
            (
                "function fail(){throw 'L';}var ok=false;try{fail()%2;}catch(e){ok=e==='L';}ok;",
                false,
            ),
            (
                "function bad(){return 'x';}function fail(){throw 'R';}var ok=false;try{bad()%fail();}catch(e){ok=e==='R';}ok;",
                false,
            ),
            (
                "var o={get a(){throw 'L';}};var ok=false;try{o.a%2;}catch(e){ok=e==='L';}ok;",
                false,
            ),
            ("function f(){return 5;}f()%2;", true),
            ("function f(){return '5';}f()%2;", true),
            ("function f(){return true;}f()%'2';", true),
        ] {
            std::thread::Builder::new().stack_size(33_554_432).spawn(move|| {
                let mut m=probe_vm(source,obs,"remainder131-marker");let status=m.run();
                eprintln!("REM131_MARKER_RESULT source={source:?} marker={:?} normal={normal} status={status:?}",obs.0);
                if normal {assert!(format!("{:?}",status.expect_err("normal marker entered")).contains("assertion failed"));}
                else {assert_eq!(status.unwrap(),vm::VmStatus::Halted);let vm::Value::Map(v)=m.stack().last().unwrap()else{panic!("bool")};assert_eq!(v.get(&vm::Value::string("flag")),Some(&vm::Value::Bool(true)));oracle(source,serde_json::json!({"kind":"boolean","value":true}));}
                assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));reset(&mut m);
            }).unwrap().join().unwrap();
        }
    }
}

#[test]
fn remainder131_managed_roots_gc() {
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(3),
        force_gc: true,
        uncaught: false,
        type_error: false,
        realm_header: false,
    };
    static ALIAS: AliasRootObserver = AliasRootObserver;
    for source in [
        "function id(x,y){return x;}var k='P\\0\\ud800';var a=k;var n=id(k,'garbage'+'!');var r=(true?('garbage'+'!',5):7)%2;if(r!==1)throw 2;if(k!==a)throw 1;k;",
        "function f(x){var n=(true?('garbage'+'!',5):7)%2;var t='garbage'+'!';if(n!==1)throw 2;return x;}f('P\\0\\ud800');",
    ] {
        let source = source.to_owned();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut m = if source.contains("k!==a") {
                    probe_vm(&source, &ALIAS, "remainder131-alias-roots")
                } else {
                    probe_vm(&source, &OBS, "javascript-es5-object-observer")
                };
                let status = m.run();
                assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                let at = m
                    .stack()
                    .iter()
                    .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                    .unwrap();
                let vm::Value::Map(b) = &m.stack()[at + 1] else {
                    panic!("managed box")
                };
                assert_eq!(b.get(&vm::Value::string("tag")), Some(&vm::Value::Int(4)));
                for field in ["ref_slot", "generation"] {
                    let vm::Value::Int(n) = b.get(&vm::Value::string(field)).unwrap() else {
                        panic!("identity")
                    };
                    assert!(*n > 0);
                }
                assert_eq!(m.stack()[at + 2], vm::Value::Float(3.0));
                for (i, u) in [80, 0, 0xd800].iter().enumerate() {
                    assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
                }
                eprintln!(
                    "REM131_MANAGED_GC source={source:?} units=[80,0,55296] stack={:?}",
                    m.stack()
                );
                assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
                reset(&mut m);
                oracle_units(
                    &source,
                    serde_json::json!({"kind":"string","units":[80,0,55296]}),
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
#[test]
fn remainder131_managed_throw_finally_gc() {
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(3),
        force_gc: true,
        uncaught: true,
        type_error: false,
        realm_header: false,
    };
    let source = "function f(){throw 'P\\0\\ud800';}try{5%f();}finally{var t='garbage'+'!';}";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut m = probe_vm(source, &OBS, "javascript-es5-object-observer");
            assert!(
                format!("{:?}", m.run().expect_err("uncaught payload"))
                    .contains("uncaught JavaScript throw")
            );
            let at = m
                .stack()
                .iter()
                .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                .unwrap();
            assert_eq!(m.stack()[at + 2], vm::Value::Float(3.0));
            for (i, u) in [80, 0, 0xd800].iter().enumerate() {
                assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
            }
            eprintln!(
                "REM131_THROW_GC source={source:?} units=[80,0,55296] stack={:?}",
                m.stack()
            );
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
            oracle_units_completion(source, Some([80, 0, 0xd800]));
        })
        .unwrap()
        .join()
        .unwrap();
}
struct AliasRootObserver;
impl vm::SourcePlugin for AliasRootObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("remainder131-alias-roots");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let obs = object_observer::Observer {
            units: Some(3),
            force_gc: true,
            uncaught: false,
            type_error: false,
            realm_header: false,
        };
        let mut ir = vm::SourcePlugin::parse_source(&obs, source, cx)?;
        let h = |name: &str| ir.functions.iter().find(|f| f.name == name).unwrap().index;
        let cmp = h("__compat_es5_string_equal");
        let read = h("__compat_ref_read");
        let decode = h("__compat_es5_decode_int");
        let checked = h("__compat_es5_checked_base");
        let p = ir.function_impls[&cmp].param_slots.clone();
        assert_eq!(p.len(), 3);
        let (c, l, r) = (p[0], p[1], p[2]);
        let mut fresh = || {
            let s = u16::try_from(ir.locals).unwrap();
            ir.locals += 1;
            s
        };
        let (top, cursor, row, lf, rf, lb, rb) = (
            fresh(),
            fresh(),
            fresh(),
            fresh(),
            fresh(),
            fresh(),
            fresh(),
        );
        let call = |i, args| E::Call(i, vec![], args, None, None);
        let num = |e| E::Div(Box::new(e), Box::new(E::Float(1.0)));
        let add = |a, b| E::Add(Box::new(a), Box::new(b));
        let eq = |a, b| E::Eq(Box::new(a), Box::new(b));
        let get = |s, k: &str| {
            call(
                vm::BuiltinFunction::Get.call_index(),
                vec![E::Var(s), E::String(k.into())],
            )
        };
        let rd = |a| call(decode, vec![num(call(read, vec![E::Var(c), num(a)]))]);
        let check = |e| S::Expr {
            expr: call(vm::BuiltinFunction::Assert.call_index(), vec![e]),
            line: 1,
        };
        let set = |index, expr| S::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line: 1,
        };
        let let_ = |index, t, expr| S::Let {
            index,
            declared_schema: Some(t),
            expr,
            line: 1,
        };
        let matched = |v| {
            E::And(
                Box::new(eq(rd(E::Var(row)), E::Int(4))),
                Box::new(E::And(
                    Box::new(eq(rd(add(E::Var(row), E::Int(1))), get(v, "ref_slot"))),
                    Box::new(eq(rd(add(E::Var(row), E::Int(2))), get(v, "generation"))),
                )),
            )
        };
        let mut pre = vec![
            check(eq(get(l, "ref_slot"), get(r, "ref_slot"))),
            check(eq(get(l, "generation"), get(r, "generation"))),
            let_(top, T::Int, rd(E::Int(3))),
            let_(cursor, T::Int, E::Int(0)),
            let_(row, T::Int, E::Int(0)),
            let_(lf, T::Bool, E::Bool(false)),
            let_(rf, T::Bool, E::Bool(false)),
            let_(
                lb,
                T::Int,
                call(
                    checked,
                    vec![E::Var(c), get(l, "ref_slot"), get(l, "generation")],
                ),
            ),
            let_(
                rb,
                T::Int,
                call(
                    checked,
                    vec![E::Var(c), get(r, "ref_slot"), get(r, "generation")],
                ),
            ),
            check(E::Gt(Box::new(rd(E::Int(11))), Box::new(E::Int(0)))),
            S::While {
                condition: E::Lt(
                    Box::new(E::Var(cursor)),
                    Box::new(rd(add(E::Var(top), E::Int(3)))),
                ),
                body: vec![
                    set(
                        row,
                        add(
                            add(E::Var(top), E::Int(4)),
                            E::Mul(Box::new(E::Var(cursor)), Box::new(E::Int(3))),
                        ),
                    ),
                    set(lf, E::Or(Box::new(E::Var(lf)), Box::new(matched(l)))),
                    set(rf, E::Or(Box::new(E::Var(rf)), Box::new(matched(r)))),
                    set(cursor, add(E::Var(cursor), E::Int(1))),
                ],
                line: 1,
            },
            check(E::Var(lf)),
            check(E::Var(rf)),
            check(eq(E::Var(lb), E::Var(rb))),
        ];
        let f = ir.function_impls.get_mut(&cmp).unwrap();
        pre.append(&mut f.body_stmts);
        f.body_stmts = pre;
        eprintln!(
            "REM131_ROOT_ASSERTED source={source:?} injected_assertions=alias_identity,input_root_rows,checked_base,prior_collection"
        );
        Ok(ir)
    }
}

#[test]
fn remainder131_public_file_controls() {
    const BODY: &str = "var a=5%2;var b=void (7%3);if(a!==1||b!==undefined)$ERROR('M\\0\\udc00');";
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(3),
        force_gc: false,
        uncaught: true,
        type_error: false,
        realm_header: false,
    };
    for (id, body, expected) in [
        ("positive", BODY.to_owned(), None),
        (
            "sentinel",
            format!("{BODY}$ERROR('S\\0\\ud800');"),
            Some([83, 0, 0xd800]),
        ),
        (
            "mutant",
            BODY.replacen("a!==1", "a!==2", 1),
            Some([77, 0, 0xdc00]),
        ),
    ] {
        let source = format!("function $ERROR(message){{throw message;}}{body}1;");
        let copy = source.clone();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let path =
                    std::env::temp_dir().join(format!("rem131-{id}-{}.js", std::process::id()));
                std::fs::write(&path, &copy).unwrap();
                let result =
                    pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(
                        &path,
                    );
                eprintln!("REM131_FILE id={id} source={copy:?} result={result:?} path={path:?}");
                if let Some(units) = expected {
                    let text = format!("{:?}", result.expect_err("guest throw"));
                    assert!(text.contains("uncaught JavaScript throw") && !text.contains("Fuel"));
                    let mut m = probe_vm(&copy, &OBS, "javascript-es5-object-observer");
                    let text = format!("{:?}", m.run().expect_err("exact managed payload"));
                    assert!(text.contains("uncaught JavaScript throw"));
                    let at = m
                        .stack()
                        .iter()
                        .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                        .unwrap();
                    assert_eq!(m.stack()[at + 2], vm::Value::Float(3.0));
                    for (i, u) in units.iter().enumerate() {
                        assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
                    }
                    eprintln!(
                        "REM131_FILE_THROW id={id} units={units:?} stack={:?}",
                        m.stack()
                    );
                    assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
                    reset(&mut m);
                } else {
                    let o = result.unwrap();
                    assert_eq!(o.cookie, 1);
                    assert_eq!((o.aot_exec_count, o.jit_native_exec_count), (0, 0));
                    assert!(
                        o.certification_valid_before_reset && !o.certification_valid_after_reset
                    );
                    assert_eq!(o.resources_after_reset, 0);
                    assert!(o.owned_arrays_retired);
                }
            })
            .unwrap()
            .join()
            .unwrap();
        oracle_units_completion(&source, expected);
    }
}
