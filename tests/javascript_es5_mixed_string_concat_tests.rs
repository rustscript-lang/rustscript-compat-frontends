//! Proved mixed String/Number additions through the public managed VM path.
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
                "MIXED119_SOURCE {}",
                serde_json::json!({"source":source,"expected_units":expected})
            );
            let observer = Box::leak(Box::new(object_observer::Observer {
                units: Some(expected.len()),
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "MIXED119_VM source={source:?} status={status:?} stack={:?}",
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
            eprintln!("MIXED119_SOURCE {}", serde_json::json!({"source": source}));
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
                "MIXED119_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "MIXED119_ROOT_FAILURE ip={} frames={:?}",
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
                "MIXED119_RESET source={source:?} pending=false resources=0 guest_invocations=1 native=0"
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
        "MIXED119_NODE {}",
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
        "MIXED119_NODE {}",
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
fn mixed119_exact_red() {
    string("'n='+1;", &[110, 61, 49]);
}

fn text(source: &str, expected: &str) {
    string(source, &expected.encode_utf16().collect::<Vec<_>>());
}
fn numeric_spellings(cases: &[(&str, &str)]) {
    for (number, spelling) in cases {
        text(&format!("'n='+({number});"), &format!("n={spelling}"));
        text(&format!("({number})+'n';"), &format!("{spelling}n"));
    }
}
#[test]
fn mixed119_numbers_special() {
    numeric_spellings(&[
        ("-0", "0"),
        ("0", "0"),
        ("NaN", "NaN"),
        ("Infinity", "Infinity"),
        ("-Infinity", "-Infinity"),
    ]);
}
#[test]
fn mixed119_numbers_boundaries() {
    numeric_spellings(&[
        ("1e-7", "1e-7"),
        ("1e-6", "0.000001"),
        ("1e20", "100000000000000000000"),
        ("1e21", "1e+21"),
    ]);
}
#[test]
fn mixed119_numbers_precision() {
    numeric_spellings(&[
        ("5e-324", "5e-324"),
        ("1.7976931348623157e308", "1.7976931348623157e+308"),
        ("9007199254740991", "9007199254740991"),
        ("9007199254740992", "9007199254740992"),
        ("1000000000000000100", "1000000000000000100"),
    ]);
}
#[test]
fn mixed119_retired_exact_sources() {
    for prefix in ["", "'A'+'B';"] {
        text(&format!("{prefix}'a'+1;"), "a1");
        text(&format!("{prefix}1+'a';"), "1a");
    }
}
#[test]
fn mixed119_flow_and_units() {
    for (source, expected) in [
        ("var n=2;var s='x';var alias=s;s='y';alias+n;", "x2"),
        ("var n=1;n=3;var s=true?'a':'b';s+(false?4:n);", "a3"),
        ("var s='a';(s='b')+(1+2);", "b3"),
        ("1+2+'x';", "3x"),
        ("'x'+1+2;", "x12"),
        ("function f(){return 'v='+2;}f();", "v=2"),
        ("function f(){'use strict';return 2+'v';}f();", "2v"),
        ("'use strict';function f(){return 'v='+2;}f();", "v=2"),
        ("(function(){return 2+'v';})();", "2v"),
        ("encodeURIComponent('x '+2);", "x%202"),
    ] {
        text(source, expected);
    }
    string(r#"'A\0\uD800'+2;"#, &[65, 0, 0xD800, 50]);
    string(r#"2+'\uDC00\0';"#, &[50, 0xDC00, 0]);
}
#[test]
fn mixed119_consumers_order_and_abrupt() {
    for source in [
        "('n='+2).length===3&&2+'n'==='2n';",
        "var n=0;var s=(n=n+1,'x')+(n=n+1);n===2&&s==='x2';",
        "var n=0;var s=(n=n+1)+(n=n+1,'x');n===2&&s==='1x';",
        "function fail(){throw 'left';}var n=0;var ok=false;try{(fail(),1)+(n=99,'x');}catch(e){ok=e==='left';}finally{n=n===0?1:99;}ok&&n===1;",
        "function fail(){throw 'left';}var n=0;var ok=false;try{(fail(),'x')+(n=99);}catch(e){ok=e==='left';}finally{n=n===0?1:99;}ok&&n===1;",
        "function fail(){throw 'right';}var n=0;var ok=false;try{(n=n+1,'x')+(fail(),1);}catch(e){ok=e==='right';}finally{n=n===1?2:99;}ok&&n===2;",
        "function fail(){throw 'right';}var n=0;var ok=false;try{(n=n+1)+(fail(),'x');}catch(e){ok=e==='right';}finally{n=n===1?2:99;}ok&&n===2;",
    ] {
        boxed(source, 2, 0.0, true);
    }
}
#[test]
fn mixed119_meaningful_refusals() {
    for prefix in ["", "'x'+1;"] {
        for (row, source) in [
            "({})+'x';",
            "(function(){})+'x';",
            "function f(n){return 'x'+n;}f(1);",
            "var n=true?1:'a';'x'+n;",
            "function f(o){return 'x'+o.n;}f({n:1});",
        ]
        .into_iter()
        .enumerate()
        {
            let source = format!("{prefix}{source}");
            if row >= 2 {
                text(&source, "x1");
                continue;
            }
            let error = parse(&source).expect_err("proved nonprimitive operand");
            assert!(
                error
                    .message
                    .contains("arithmetic currently requires numeric operands"),
                "{source} {error:?}"
            );
            eprintln!(
                "MIXED119_REFUSAL {}",
                serde_json::json!({"source":source,"error":format!("{error:?}"),"guest_invocations":0})
            );
        }
    }
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
#[test]
fn mixed119_shared_prerequisites_indices() {
    for source in [
        "'n='+1;",
        "function f(){var k=1.5;var o={'1.5':7};return o[k];}var o={get length(){'use strict';return ('A'+1).length;}};('A'+1)==='A1'&&o.length===2&&f()===7&&Math.abs(-3)===3&&encodeURIComponent('A '+2)==='A%202';",
    ] {
        let ir = parse(source).unwrap();
        let concat = helper(&ir, "__compat_es5_string_concat");
        let to = helper(&ir, "__compat_es5_managed_to_units");
        let from = helper(&ir, "__compat_es5_units_to_managed");
        let managed = helper(&ir, "__compat_es5_number_string_managed");
        let units = helper(&ir, "__compat_es5_number_string_units");
        assert!(to < managed && from < managed && units < managed && managed < concat);
        let ids: std::collections::HashSet<_> = ir.functions.iter().map(|f| f.index).collect();
        assert_eq!(ids.len(), ir.functions.len());
        let declaration = ir.functions.iter().find(|f| f.index == concat).unwrap();
        assert_eq!(declaration.arity, 5);
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
                    assert!(slot < ir.locals, "slot={slot} locals={}", ir.locals);
                }
            }
        }
        if source == "'n='+1;" {
            assert!(
                !ir.functions
                    .iter()
                    .any(|f| f.name == "__compat_es5_ref_get")
            );
            text(source, "n=1");
        } else {
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
                "MIXED119_INDEX_WINDOW {}",
                serde_json::json!({"source":source,"get":get,"length":length,"dispatch":dispatch,"source_functions":funcs,"initializers":inits})
            );
            boxed(source, 2, 0.0, true);
        }
        eprintln!(
            "MIXED119_PREREQUISITES {}",
            serde_json::json!({"source":source,"concat":concat,"to":to,"from":from,"managed":managed,"units":units,"locals":ir.locals,"unique_functions":ids.len()})
        );
    }
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
        eprintln!("MIXED119_THROW_UNITS {}",serde_json::json!({"source":source,"expected_units":expected,"error":diagnostic,"full_stack":format!("{:?}",machine.stack()),"guest_invocations":1,"native":0}));
        reset(&mut machine);
    }).unwrap().join().unwrap();
}

#[test]
fn mixed119_original_a3_and_throw_controls() {
    const BODY: &str = include_str!("fixtures/test262117/S8.4_A3.js");
    assert_eq!(BODY.matches("__str.length !== 8").count(), 1);
    for (id, body, expected) in [
        ("original", BODY.to_owned(), None),
        (
            "sentinel",
            format!("{BODY}\n$ERROR('sentinel'+8);"),
            Some("sentinel8"),
        ),
        (
            "mutant",
            BODY.replacen("__str.length !== 8", "__str.length !== 9", 1),
            Some("#1: var __str = \"ABCDEFGH\"; __str.length === 8. Actual: 8"),
        ),
    ] {
        let source = format!("function $ERROR(message) {{ throw message; }}\n{body}\n;1;\n");
        let copied = source.clone();
        std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
            let path=std::env::temp_dir().join(format!("mixed119-a3-{id}.js"));
            std::fs::write(&path,&copied).unwrap();
            let result=pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(&path);
            if expected.is_some() {
                let error=result.expect_err("actual diagnostic must throw");
                assert!(matches!(error,pd_vm_compat_frontends::javascript::corpus::CorpusFileError::Execution{..}));
                let text=format!("{error:?}");assert!(text.contains("uncaught JavaScript throw")&&!text.contains("Fuel"),"{text}");
                eprintln!("MIXED119_A3 {}",serde_json::json!({"id":id,"source":copied,"error":text,"guest_invocations":1,"upstream_original":false}));
            } else {
                let o=result.unwrap();assert_eq!(o.cookie,1);assert_eq!((o.aot_exec_count,o.jit_native_exec_count),(0,0));
                assert!(o.certification_valid_before_reset&&!o.certification_valid_after_reset);assert_eq!(o.resources_after_reset,0);assert!(o.owned_arrays_retired);
                eprintln!("MIXED119_A3 {}",serde_json::json!({"id":id,"source":copied,"original_body":BODY,"observation":format!("{o:?}"),"guest_invocations":1,"prospective_upstream_id":"S8.4_A3"}));
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
struct MixedRootObserver {
    abrupt: bool,
    units: Option<usize>,
}
impl vm::SourcePlugin for MixedRootObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("mixed119-root-observer");
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
        let managed = helper(&ir, "__compat_es5_number_string_managed");
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
                "MIXED119_ABRUPT_OBSERVER source={source:?} formatter_and_concat_entry_fatal=true skipped_literal_sizes=[131,132]"
            );
            return Ok(ir);
        }
        let checked = helper(&ir, "__compat_es5_checked_base");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let read = helper(&ir, "__compat_ref_read");
        let decode = helper(&ir, "__compat_es5_decode_int");
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
                    panic!("actual formatter ABI")
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
                    .expect("normal concat after formatter");
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
            "MIXED119_ROOTS source={source:?} actual_original_caller_row_found=true original_generation_across_formatter_gc=true converted_caller_row_checked=true converted_generation_across_concat_gc=true output_caller_row_checked_before_consumption=true protected_publication_gap_has_no_gc=true"
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
            let observer = Box::leak(Box::new(MixedRootObserver {
                abrupt,
                units: expected.as_ref().map(Vec::len),
            }));
            let mut machine = probe_vm(&source, observer, "mixed119-root-observer");
            let status = machine.run();
            eprintln!(
                "MIXED119_ROOT_VM source={source:?} abrupt={abrupt} status={status:?} stack={:?}",
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
fn mixed119_roots_publication_gc() {
    for (source, units) in [
        (
            r#"function f(){return 'A\0\uD800'+8;}f();"#,
            &[65, 0, 0xD800, 56][..],
        ),
        (
            r#"function f(){'use strict';return 8+'A\0\uDC00';}f();"#,
            &[56, 65, 0, 0xDC00][..],
        ),
    ] {
        observed(source, false, Some(units));
        oracle_units(source, units);
    }
}
#[test]
fn mixed119_abrupt_skips_formatter_concat_and_literals() {
    let l = "L".repeat(128);
    let r = "R".repeat(129);
    for source in [
 format!("function fail(){{throw 'left';}}var n=0;var ok=false;try{{(fail(),1)+(n=99,'{r}');}}catch(e){{ok=e==='left';}}finally{{n=n===0?1:99;}}ok&&n===1;"),
 format!("function fail(){{throw 'left';}}var n=0;var ok=false;try{{(fail(),'{l}')+(n=99);}}catch(e){{ok=e==='left';}}finally{{n=n===0?1:99;}}ok&&n===1;"),
 format!("function fail(){{throw 'right';}}var n=0;var ok=false;try{{(n=n+1)+(fail(),'{r}');}}catch(e){{ok=e==='right';}}finally{{n=n===1?2:99;}}ok&&n===2;"),
 "function fail(){throw 'right';}var n=0;var ok=false;try{(n=n+1,'x')+(fail(),1);}catch(e){ok=e==='right';}finally{n=n===1?2:99;}ok&&n===2;".to_owned(),
 ]{observed(&source,true,None);oracle(&source,serde_json::json!({"kind":"boolean","value":true}));}
}
#[test]
fn mixed119_managed_throw_finally_gc() {
    let source = "function f(){try{throw 'A'+8;}finally{var t=2+'B';}}var ok=false;try{f();}catch(e){ok=e==='A8';}ok;";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let obs = Box::leak(Box::new(object_observer::Observer {
                force_gc: true,
                ..Default::default()
            }));
            let mut machine = probe_vm(source, obs, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "MIXED119_FINALLY source={source:?} status={status:?} stack={:?}",
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
            let vm::Value::Map(v) = &machine.stack()[at + 1] else {
                panic!("box")
            };
            assert_eq!(
                v.get(&vm::Value::string("flag")),
                Some(&vm::Value::Bool(true))
            );
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
    oracle(source, serde_json::json!({"kind":"boolean","value":true}));
}

#[test]
fn mixed119_number_facts_and_dynamic_refusals() {
    for prefix in ["", "'A'+'B';"] {
        for (row, source) in [
            "function f(){return 'b';}f()+'a';",
            "function f(o){var x=o.x;return x+'a';}f({x:'b'});",
            "function f(){return 2;}'x'+f();",
            "function f(){return 2;}f()+'x';",
            "var o={x:2};'x'+o.x;",
            "var o={x:2};o.x+'x';",
        ]
        .into_iter()
        .enumerate()
        {
            let source = format!("{prefix}{source}");
            text(&source, ["ba", "ba", "x2", "2x", "x2", "2x"][row]);
        }
    }
    for (source, expected) in [
        ("'x'+(1+2);", "x3"),
        ("(1+2)+'x';", "3x"),
        ("'x'+'ABC'.length;", "x3"),
        ("'ABC'.length+'x';", "3x"),
        ("'x'+(-Math.abs(-2));", "x-2"),
        ("(-Math.abs(-2))+'x';", "-2x"),
    ] {
        text(source, expected);
    }
}
#[test]
fn mixed119_old_numeric_dynamic_guards_unchanged() {
    for source in ["function f(){return 2;}f()+1;", "var o={x:2};o.x+1;"] {
        boxed(source, 3, 3.0, false);
    }
    for source in ["function f(){return 'b';}f()+1;", "var o={x:'b'};o.x+1;"] {
        text(source, "b1");
    }
    // New runtime nonprimitive controls replace the retired String guards.
    for source in [
        "function f(){return {};}f()+1;",
        "function id(x){return x;}id([])+1;",
        "function f(){return function(){};}f()+1;",
    ] {
        std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
            let mut machine=pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
            machine.set_jit_config(vm::JitConfig{enabled:false,..*machine.jit_config()});machine.set_fuel(10_000_000);
            assert!(!machine.jit_config().enabled);assert_eq!((machine.aot_exec_count(),machine.jit_native_exec_count()),(0,0));
            let error=format!("{:?}",machine.run().expect_err("actual nonprimitive profile guard"));
            assert!(error.contains("assertion failed")&&!error.contains("Fuel"),"{error}");
            assert_eq!((machine.aot_exec_count(),machine.jit_native_exec_count()),(0,0));
            eprintln!("ADD140_REPLACEMENT_GUARD {}",serde_json::json!({"source":source,"error":error,"guest_invocations":1,"native":0}));reset(&mut machine);
        }).unwrap().join().unwrap();
    }
    // Characterize the existing builtin's NaN result; do not invent a new guard.
    let source = "Math.abs('bad')+1;";
    let vm::Value::Map(map) = execute(source) else {
        panic!("Number box")
    };
    assert_eq!(map.len(), 5);
    for (key, value) in [
        ("tag", vm::Value::Int(3)),
        ("flag", vm::Value::Bool(false)),
        ("ref_slot", vm::Value::Int(0)),
        ("generation", vm::Value::Int(0)),
    ] {
        assert_eq!(map.get(&vm::Value::string(key)), Some(&value));
    }
    assert!(
        matches!(map.get(&vm::Value::string("number")),Some(vm::Value::Float(n)) if n.is_nan())
    );
    oracle(
        &format!(
            "Number.isNaN((0,eval)({}));",
            serde_json::to_string(source).unwrap()
        ),
        serde_json::json!({"kind":"boolean","value":true}),
    );
}
