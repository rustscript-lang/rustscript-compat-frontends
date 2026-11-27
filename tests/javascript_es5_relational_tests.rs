//! Bounded relational operators through the actual public VM.
fn boolean(source: &str, expected: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "REL127_SOURCE {}",
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
            eprintln!("REL127_VM status={status:?} stack={:?}", machine.stack());
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
            eprintln!("REL127_RESET resources=0 owned_retired=true cert=false native=0");
            oracle(
                &source,
                serde_json::json!({"kind":"boolean","value":expected}),
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn relational127_numeric_red_green() {
    boolean("1<2;", true);
    for source in ["1>2;", "2<1;", "(0/0)<=1;", "1>=(0/0);"] {
        boolean(source, false);
    }
}
#[test]
fn relational127_numeric_no_pending_placeholder() {
    for source in ["1<2;", "1>2;", "1<=2;", "1>=2;"] {
        let ir = parse(source).unwrap();
        assert!(ir.functions.is_empty());
        let debug = format!("{:?}", ir.stmts);
        let needle = format!("Call({}, [], []", vm::BuiltinFunction::MapNew.call_index());
        // Two original Number boxes and exactly one result Boolean box.
        assert_eq!(debug.matches(&needle).count(), 3, "{source}: {debug}");
    }
}
#[test]
fn relational127_string_red_green() {
    boolean("'a'<'b';", true);
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
        "REL127_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version_rc":version.status.code(),"version_stdout":String::from_utf8_lossy(&version.stdout),"version_stderr":String::from_utf8_lossy(&version.stderr)})
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        expected
    );
}
fn relations(left: &str, right: &str, expected: [bool; 4]) {
    let source = ["<", ">", "<=", ">="]
        .into_iter()
        .zip(expected)
        .map(|(op, result)| format!("(({left}){op}({right}))==={result}"))
        .collect::<Vec<_>>()
        .join("&&")
        + ";";
    boolean(&source, true);
}
#[test]
fn relational127_numeric_finite() {
    for (l, r, e) in [
        ("1", "2", [true, false, true, false]),
        ("2", "1", [false, true, false, true]),
        ("2", "2", [false, false, true, true]),
        ("-1.25", "-0.5", [true, false, true, false]),
        ("0.25", "0.125", [false, true, false, true]),
        (
            "9007199254740991",
            "9007199254740992",
            [true, false, true, false],
        ),
        (
            "1.7976931348623157e308",
            "1.7976931348623157e308",
            [false, false, true, true],
        ),
    ] {
        relations(l, r, e);
    }
}
#[test]
fn relational127_numeric_nan_zeros_infinity() {
    for (l, r, e) in [
        ("0/0", "1", [false; 4]),
        ("1", "0/0", [false; 4]),
        ("0/0", "0/0", [false; 4]),
        ("-0", "0", [false, false, true, true]),
        ("0", "-0", [false, false, true, true]),
        ("1/0", "-1/0", [false, true, false, true]),
        ("-1/0", "1/0", [true, false, true, false]),
        ("1/0", "1/0", [false, false, true, true]),
        ("NaN", "Infinity", [false; 4]),
    ] {
        relations(l, r, e);
    }
}
#[test]
fn relational127_string_utf16() {
    for (l, r, e) in [
        ("''", "''", [false, false, true, true]),
        ("''", "'a'", [true, false, true, false]),
        ("'a'", "'aa'", [true, false, true, false]),
        ("'aa'", "'a'", [false, true, false, true]),
        ("'ab'", "'ac'", [true, false, true, false]),
        ("'a\\0b'", "'a\\0c'", [true, false, true, false]),
        ("'\\ud800'", "'\\ud801'", [true, false, true, false]),
        ("'\\udfff'", "'\\ue000'", [true, false, true, false]),
        ("'\\ud83d\\ude00'", "'\\ue000'", [true, false, true, false]),
        ("'\\uffff'", "'\\u0000'", [false, true, false, true]),
    ] {
        relations(l, r, e);
    }
}
#[test]
fn relational127_flow_loops_scopes() {
    for source in [
        "var a=1;var b=2;var c=(a=3)>b;var d=true?1:2;c&&d<=b;",
        "var s=true?'a':'b';var t=false?'d':'c';var u=s;u<t&&(s='c')>=t;",
        "('a'+'b')<('a'+'c');",
        "var i=0;var ok=true;while(i<3){ok=ok&&i>=0;i=i+1;}for(var j=0;j<=2;j=j+1){ok=ok&&j<3;}ok&&i===3&&j===3;",
        "function f(){var n=1;return n<2;}function g(){'use strict';var s='a';return s>='a';}f()&&g();",
        "'use strict';function f(){return 1<=2;}function g(){return 'b'>'a';}f()&&g();",
        "var s='a';try{throw 1;}catch(s){var n=1;n<2;}s<'b';",
        "function f(){try{return 'a'<'b';}finally{1;}}f();",
    ] {
        boolean(source, true);
    }
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
#[test]
fn relational127_refusals() {
    use object_observer as add_observer;
    fn add_vm(source: &str, observer: &'static dyn vm::SourcePlugin, id: &'static str) -> vm::Vm {
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

    for (row, source) in [
        "'1'<2;",
        "2<'1';",
        "true<2;",
        "null<2;",
        "undefined<2;",
        "({})<2;",
        "[]<2;",
        "function f(x){return x<2;}f(1);",
        "function f(){'use strict';try{throw 1;}catch(e){return e<2;}}f();",
        "var x=true?'a':1;x<'b';",
        "var x=1;while(x<2){x='a';}",
        "encodeURI('a')<'b';",
        "function f(){return 1;} 'a'+f();",
    ]
    .into_iter()
    .enumerate()
    {
        for prefix in ["", "'unrelated';"] {
            let source = format!("{prefix}{source}");
            if !matches!(row, 5 | 6 | 10 | 12) {
                boolean(&source, !matches!(row, 1 | 4));
                continue;
            }
            if row == 10 {
                static LOOP: add_observer::Observer = add_observer::Observer {
                    units: Some(1),
                    force_gc: false,
                    uncaught: false,
                    type_error: false,
                    realm_header: false,
                };
                std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
                    eprintln!("REL145_LEGACY_SOURCE source={source:?}");
                    let mut m = add_vm(&source, &LOOP, "javascript-es5-object-observer");
                    assert!(!m.jit_config().enabled);
                    assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
                    let status = m.run();
                    assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                    let at = m.stack().iter().position(|v| v == &vm::Value::string("P7_OBSERVED")).unwrap();
                    assert_eq!(&m.stack()[at+2..at+4], &[vm::Value::Float(1.0), vm::Value::Float(97.0)]);
                    assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
                    eprintln!("REL145_LEGACY_VM source={source:?} status=Halted aot=0 jit=0 units=[97]");
                    reset(&mut m);
                    oracle(&source, serde_json::json!({"kind":"string","value":"a"}));
                }).unwrap().join().unwrap();
                continue;
            }
            if row == 12 {
                static OBS: add_observer::Observer = add_observer::Observer {
                    units: Some(2),
                    force_gc: false,
                    uncaught: false,
                    type_error: false,
                    realm_header: false,
                };
                std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
                    let mut m=add_vm(&source,&OBS,"javascript-es5-object-observer");
                    assert!(!m.jit_config().enabled);assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));
                    let status=m.run();assert_eq!(status.unwrap(),vm::VmStatus::Halted);
                    let at=m.stack().iter().position(|v|v==&vm::Value::string("P7_OBSERVED")).unwrap();
                    assert_eq!(&m.stack()[at+2..at+5],&[vm::Value::Float(2.0),vm::Value::Float(97.0),vm::Value::Float(49.0)]);
                    assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));
                    eprintln!("ADD140_LEGACY_VM source={source:?} status=Halted aot=0 jit=0 jit_enabled=false units=[97,49]");
                    m.reset_for_reuse().unwrap();assert!(!m.scope_reset_pending());assert_eq!(m.host_context().resource_count(),0);
                    let script=format!("let v=(0,eval)({});console.log(JSON.stringify(Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))));",serde_json::to_string(&source).unwrap());
                    let o=std::process::Command::new("node").args(["-e",&script]).output().unwrap();assert!(o.status.success());assert_eq!(serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap(),serde_json::json!([97,49]));
                    eprintln!("ADD140_LEGACY_NODE {}",serde_json::json!({"source":source,"argv":["node","-e",script],"rc":o.status.code(),"stdout":String::from_utf8_lossy(&o.stdout)}));
                }).unwrap().join().unwrap();
                continue;
            }
            let err = parse(&source).expect_err("valid-source bounded profile refusal");
            eprintln!("REL127_REFUSED source={source:?} guest_invocations=0 error={err:?}");
            let diagnostic = format!("{err:?}");
            assert!(
                diagnostic.contains("numeric") || diagnostic.contains("Numeric"),
                "{diagnostic}"
            );
        }
    }
}
#[test]
fn relational127_guarded_numeric() {
    for source in [
        "function f(){return 1;}f()<2;",
        "var o={n:1};o.n<=2;",
        "function f(){return 2;}f()>=2;",
        "Math.abs(-2)>1;",
    ] {
        boolean(source, true);
    }
    let source =
        "function f(){return 'x';}var caught=false;try{f()<2;}catch(e){caught=true;}caught;"
            .to_owned();
    boolean(&source, false);
    for source in [
        "function f(){return {};}var caught=false;try{f()<2;}catch(e){caught=true;}caught;",
        "function f(){return [];}var caught=false;try{f()>=2;}catch(e){caught=true;}caught;",
    ] {
        let source = source.to_owned();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut m =
                    pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                        .unwrap();
                m.set_jit_config(vm::JitConfig {
                    enabled: false,
                    ..*m.jit_config()
                });
                m.set_fuel(10_000_000);
                let status = m.run();
                eprintln!("REL127_GUARD source={source:?} status={status:?}");
                assert!(
                    format!("{:?}", status.expect_err("actual Number guard fatal"))
                        .contains("assertion failed")
                );
                assert_eq!((m.jit_native_exec_count(), m.aot_exec_count()), (0, 0));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
#[test]
fn relational127_once_order() {
    for op in ["<", ">", "<=", ">="] {
        boolean(
            &format!("var n=0;var c=(n=n*10+1,1){op}(n=n*10+2,2);n===12;"),
            true,
        );
        boolean(
            &format!("var n=0;var c=(n=n*10+1,'a'){op}(n=n*10+2,'b');n===12;"),
            true,
        );
    }
}
#[test]
fn relational127_completion_abrupt() {
    for source in [
        "function fail(){throw 'left';}var n=0;var ok=false;try{(fail(),1)<(n=99,2);}catch(e){ok=e==='left';}finally{n=n===0?1:99;}ok&&n===1;",
        "function fail(){throw 'right';}var n=0;var ok=false;try{(n=n+1,1)>=(fail(),2);}catch(e){ok=e==='right';}finally{n=n===1?2:99;}ok&&n===2;",
        "function fail(){throw 'left';}var n=0;var ok=false;try{(fail(),'a')>(n=99,'b');}catch(e){ok=e==='left';}finally{n=n===0?1:99;}ok&&n===1;",
        "function fail(){throw 'right';}var n=0;var ok=false;try{(n=n+1,'a')<=(fail(),'b');}catch(e){ok=e==='right';}finally{n=n===1?2:99;}ok&&n===2;",
        "function bad(){return 'notNumber';}function fail(){throw 'right';}var ok=false;try{bad()<(fail(),2);}catch(e){ok=e==='right';}ok;",
        "function f(){function bad(){throw 'payload';}try{return (bad(),'a')<'b';}catch(e){return e==='payload';}finally{1;}}f();",
    ] {
        boolean(source, true);
    }
}
#[test]
fn relational127_demand_and_indices() {
    for source in ["1<2;", "1>2;", "1<=2;", "1>=2;", "(0,1)<(true?2:3);"] {
        let ir = parse(source).unwrap();
        assert!(
            ir.functions.is_empty(),
            "numeric literals stay B1-realm-free: {source}"
        );
    }
    // A shared script var already demands the old ordinary realm. A Numeric
    // relation must add no helper to that independently demanded realm.
    let numeric_var = parse("var x=1;x<2;").unwrap();
    let arithmetic_var = parse("var x=1;x+2;").unwrap();
    assert_eq!(
        format!("{:?}", numeric_var.functions),
        format!("{:?}", arithmetic_var.functions)
    );
    eprintln!("REL127_NUMERIC_VAR existing_realm_functions_unchanged=true");
    for source in [
        "'a'<'b';",
        "(true?'a':'b')<(false?'a':'c');",
        "function f(){return 'a'<'b';}f();",
        "var k=1.5;var o={'1.5':1};o[k];('a'+1)<'b';encodeURI('a');",
    ] {
        let ir = parse(source).unwrap();
        let selected = ir
            .functions
            .iter()
            .filter(|f| f.name == "__compat_es5_string_compare")
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 1);
        let indices = ir
            .functions
            .iter()
            .map(|f| f.index)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(indices.len(), ir.functions.len());
        let f = ir.function_impls.get(&selected[0].index).unwrap();
        assert_eq!(f.param_slots.len(), 3);
        assert!(f.param_slots.iter().all(|s| usize::from(*s) < ir.locals));
        if !source.contains('+') {
            for name in [
                "__compat_es5_managed_to_units",
                "__compat_es5_units_to_managed",
                "__compat_es5_number_string_managed",
                "__compat_es5_uri_call",
            ] {
                assert!(
                    !ir.functions.iter().any(|f| f.name == name),
                    "{source}: {name}"
                );
            }
        }
    }
    for source in [
        "1;",
        "'a';",
        "function f(){return 1;}f()<2;",
        "encodeURI('a');",
    ] {
        assert!(
            !parse(source)
                .unwrap()
                .functions
                .iter()
                .any(|f| f.name == "__compat_es5_string_compare")
        );
    }
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
fn let_(i: u16, t: vm::TypeSchema, e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index: i,
        declared_schema: Some(t),
        expr: e,
        line: 1,
    }
}
fn set(i: u16, e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: i,
        expr: e,
        line: 1,
    }
}
fn fresh(ir: &mut vm::FrontendIr) -> u16 {
    let i = u16::try_from(ir.locals).unwrap();
    ir.locals += 1;
    i
}

fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .index
}

// The observer changes only test IR; production comparison has no collection,
// allocation, bridge or publication frame. Force-GC is at node_begin entry.
struct RelationObserver(u8);
impl vm::SourcePlugin for RelationObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("relational127-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            cx,
        )?;
        let cmp = helper(&ir, "__compat_es5_string_compare");
        if self.0 == 1 {
            ir.function_impls
                .get_mut(&cmp)
                .unwrap()
                .body_stmts
                .insert(0, check(E::Bool(false)));
            eprintln!("REL127_ABRUPT_HELPER_MUTANT source={source:?} comparator_entry_fatal=true");
            return Ok(ir);
        }
        let begin = helper(&ir, "__compat_es5_node_begin");
        let write = helper(&ir, "__compat_ref_write");
        let ack = fresh(&mut ir);
        let c = ir.function_impls[&begin].param_slots[0];
        ir.function_impls
            .get_mut(&begin)
            .unwrap()
            .body_stmts
            .insert(
                0,
                let_(
                    ack,
                    T::Number,
                    call(write, vec![E::Var(c), E::Float(10.0), E::Float(32.0)]),
                ),
            );
        let checked = helper(&ir, "__compat_es5_checked_base");
        let read = helper(&ir, "__compat_ref_read");
        let decode = helper(&ir, "__compat_es5_decode_int");
        let params = ir.function_impls[&cmp].param_slots.clone();
        assert_eq!(params.len(), 3);
        let (c, l, r) = (params[0], params[1], params[2]);
        let original = &ir.function_impls[&cmp];
        let debug = format!("{original:?}");
        for name in [
            "__compat_es5_heap_collect",
            "__compat_es5_node_begin",
            "__compat_es5_root_enter",
            "__compat_es5_managed_to_units",
            "__compat_es5_units_to_managed",
        ] {
            if let Some(f) = ir.functions.iter().find(|f| f.name == name) {
                assert!(
                    !debug.contains(&format!("Call({},", f.index)),
                    "production helper invokes {name}"
                );
            }
        }
        let top = fresh(&mut ir);
        let cursor = fresh(&mut ir);
        let row = fresh(&mut ir);
        let lf = fresh(&mut ir);
        let rf = fresh(&mut ir);
        let lb = fresh(&mut ir);
        let rb = fresh(&mut ir);
        let rd = |a: E| call(decode, vec![num(call(read, vec![E::Var(c), num(a)]))]);
        let base = |v| {
            call(
                checked,
                vec![E::Var(c), get(v, "ref_slot"), get(v, "generation")],
            )
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
        let body = vec![
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
        ];
        let mut pre = vec![
            let_(top, T::Int, rd(E::Int(3))),
            let_(cursor, T::Int, E::Int(0)),
            let_(row, T::Int, E::Int(0)),
            let_(lf, T::Bool, E::Bool(false)),
            let_(rf, T::Bool, E::Bool(false)),
            let_(lb, T::Int, base(l)),
            let_(rb, T::Int, base(r)),
            check(E::Gt(Box::new(rd(E::Int(11))), Box::new(E::Int(0)))),
            S::While {
                condition: E::Lt(
                    Box::new(E::Var(cursor)),
                    Box::new(rd(add(E::Var(top), E::Int(3)))),
                ),
                body,
                line: 1,
            },
            check(E::Var(lf)),
            check(E::Var(rf)),
        ];
        for expr in [
            E::String("REL127_INPUT_ROOTS".into()),
            E::Var(l),
            E::Var(r),
            E::Var(top),
            rd(E::Int(11)),
            E::Var(lb),
            E::Var(rb),
        ] {
            pre.push(S::Expr { expr, line: 1 });
        }
        match self.0 {
            2 => pre.push(set(
                l,
                call(
                    vm::BuiltinFunction::Set.call_index(),
                    vec![
                        E::Var(l),
                        E::String("generation".into()),
                        add(get(l, "generation"), E::Int(1)),
                    ],
                ),
            )),
            3 => pre.push(S::Expr {
                expr: call(
                    write,
                    vec![
                        E::Var(c),
                        num(add(E::Var(lb), E::Int(2))),
                        E::Float(999999.0),
                    ],
                ),
                line: 1,
            }),
            4 => pre.push(S::Expr {
                expr: call(
                    write,
                    vec![
                        E::Var(c),
                        num(add(E::Var(lb), E::Int(3))),
                        E::Float(65536.0),
                    ],
                ),
                line: 1,
            }),
            5 => pre.push(S::Expr {
                expr: call(write, vec![E::Var(c), num(E::Var(lb)), E::Float(3.0)]),
                line: 1,
            }),
            _ => {}
        }
        // Insert after scalar declarations, immediately before original checks.
        let f = ir.function_impls.get_mut(&cmp).unwrap();
        let at = f
            .body_stmts
            .iter()
            .position(|s| matches!(s, S::Expr { .. }))
            .unwrap();
        f.body_stmts.splice(at..at, pre);
        eprintln!(
            "REL127_ROOT_OBSERVER source={source:?} mode={} input_rows_exact_tag_slot_generation=true rhs_node_begin_gc=true comparator_no_private_frame=true",
            self.0
        );
        Ok(ir)
    }
}
fn observed(source: &str, observer: &'static RelationObserver, success: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut m = probe_vm(&source, observer, "relational127-observer");
            let status = m.run();
            eprintln!(
                "REL127_OBSERVED source={source:?} status={status:?} stack={:?}",
                m.stack()
            );
            if success {
                assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                let vm::Value::Map(boxed) = m.stack().last().unwrap() else {
                    panic!("Boolean")
                };
                assert_eq!(
                    boxed.get(&vm::Value::string("flag")),
                    Some(&vm::Value::Bool(true))
                );
            } else {
                let text = format!("{:?}", status.expect_err("malformed input fatal"));
                assert!(
                    text.contains("assertion failed") && !text.contains("Fuel"),
                    "{text}"
                );
            }
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn relational127_managed_roots_rhs_gc_identity() {
    static OBS: RelationObserver = RelationObserver(0);
    for source in [
        "var key='a\\0\\ud800';var o={'a\\0\\ud800':7};var alias=key;var ok=key<('a\\0\\ud801'+'');ok&&key===alias&&o['a\\0\\ud800']===7;",
        "var s='same';s<=s;",
        "function f(){var a='a';return a>(''+ 'A');}f();",
    ] {
        observed(source, &OBS, true);
        oracle(source, serde_json::json!({"kind":"boolean","value":true}));
    }
}
#[test]
fn relational127_abrupt_comparator_not_entered() {
    static OBS: RelationObserver = RelationObserver(1);
    for source in [
        "function f(){throw 'L';}var n=0;var ok=false;try{(f(),'a')>(n=99,'b');}catch(e){ok=e==='L';}finally{n=n+1;}ok&&n===1;",
        "function f(){throw 'R';}var n=0;var ok=false;try{(n=n+1,'a')<=(f(),'b');}catch(e){ok=e==='R';}finally{n=n+1;}ok&&n===2;",
    ] {
        observed(source, &OBS, true);
        oracle(source, serde_json::json!({"kind":"boolean","value":true}));
    }
    observed("'a'<'b';", &OBS, false);
}
#[test]
fn relational127_malformed_string_guards() {
    static BAD: [RelationObserver; 4] = [
        RelationObserver(2),
        RelationObserver(3),
        RelationObserver(4),
        RelationObserver(5),
    ];
    for obs in &BAD {
        observed("var a='a';a<'b';", obs, false);
    }
}
#[test]
fn relational127_getters_order_once() {
    for op in ["<", ">", "<=", ">="] {
        boolean(
            &format!(
                "var o={{n:0,get l(){{'use strict';this.n=this.n*10+1;return 1;}},get r(){{'use strict';this.n=this.n*10+2;return 2;}}}};var b=o.l{op}o.r;o.n===12;"
            ),
            true,
        );
    }
}
#[test]
fn relational127_managed_throw_finally_gc() {
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(3),
        force_gc: true,
        uncaught: true,
        type_error: false,
        realm_header: false,
    };
    let source =
        "function f(){throw 'P\\0\\ud800';}try{(f(),'a')<'b';}finally{var t='garbage'+'!';}";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut m = probe_vm(source, &OBS, "javascript-es5-object-observer");
            let text = format!("{:?}", m.run().expect_err("uncaught payload"));
            assert!(text.contains("uncaught JavaScript throw"));
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
                "REL127_THROW_GC source={source:?} units=[80,0,55296] stack={:?}",
                m.stack()
            );
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn relational127_public_file_controls() {
    const BODY: &str =
        "var a=1<2;var b='\\ud83d\\ude00'<'\\ue000';if(a!==true||b!==true)$ERROR('M\\0\\udc00');";
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
            BODY.replacen("a!==true", "a!==false", 1),
            Some([77, 0, 0xdc00]),
        ),
    ] {
        let source = format!("function $ERROR(message){{throw message;}}{body}1;");
        let copy = source.clone();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let path =
                    std::env::temp_dir().join(format!("rel127-{id}-{}.js", std::process::id()));
                std::fs::write(&path, &copy).unwrap();
                let result =
                    pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(
                        &path,
                    );
                eprintln!("REL127_FILE id={id} source={copy:?} result={result:?} path={path:?}");
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
                        "REL127_FILE_THROW id={id} units={units:?} stack={:?}",
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
fn oracle_units_completion(source: &str, expected: Option<[u16; 3]>) {
    let script = format!(
        "const source={};try{{let v=(0,eval)(source);console.log(JSON.stringify({{kind:typeof v,value:v}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',units:Array.from({{length:e.length}},(_,i)=>e.charCodeAt(i))}}));}}",
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
        "REL127_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":o.status.code(),"stdout":String::from_utf8_lossy(&o.stdout),"stderr":String::from_utf8_lossy(&o.stderr),"version_rc":v.status.code(),"version_stdout":String::from_utf8_lossy(&v.stdout),"version_stderr":String::from_utf8_lossy(&v.stderr)})
    );
    assert!(v.status.success() && o.status.success() && o.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        actual,
        if let Some(u) = expected {
            serde_json::json!({"kind":"throw","units":u})
        } else {
            serde_json::json!({"kind":"number","value":1})
        }
    );
}
