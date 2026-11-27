//! Proved String + String through the public managed VM path.
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
                "CONCAT117_SOURCE {}",
                serde_json::json!({"source":source,"expected_units":expected})
            );
            let observer = Box::leak(Box::new(object_observer::Observer {
                units: Some(expected.len()),
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "CONCAT117_VM source={source:?} status={status:?} stack={:?}",
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
            eprintln!("CONCAT117_SOURCE {}", serde_json::json!({"source": source}));
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
                "CONCAT117_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "CONCAT117_ROOT_FAILURE ip={} frames={:?}",
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
                "CONCAT117_RESET source={source:?} pending=false resources=0 guest_invocations=1 native=0"
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
        "CONCAT117_NODE {}",
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

#[test]
fn concat117_exact_red() {
    string("'A'+'B';", &[65, 66]);
}

fn oracle_units(source: &str, expected: &[u16]) {
    let script = format!(
        "(()=>{{const source={};let v=(0,eval)(source);console.log(JSON.stringify({{kind:typeof v,units:Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))}}));}})();",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "CONCAT117_NODE {}",
        serde_json::json!({"source":source,"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)})
    );
    assert!(output.status.success());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"kind":"string","units":expected})
    );
}

#[test]
fn concat117_units_aliases() {
    for (source, expected) in [
        ("'a'+'b';", vec![97, 98]),
        ("''+'';", vec![]),
        ("''+'x';", vec![120]),
        ("'x'+'';", vec![120]),
        (
            r#"'a\0'+'\uD800x\uDC00';"#,
            vec![97, 0, 0xD800, 120, 0xDC00],
        ),
        (r#"'\uD800'+'\uDC00';"#, vec![0xD800, 0xDC00]),
        (
            "var s='A';var alias=s;var t=s+'B';s='C';alias+t+s;",
            vec![65, 65, 66, 67],
        ),
        (
            "var b=true;var s=b?'A':'X';var t=b?'B':'Y';s+t+'C';",
            vec![65, 66, 67],
        ),
        (
            "var b=false;var s=b?'A':'X';var t=b?'B':'Y';s+t;",
            vec![88, 89],
        ),
        ("var s='x';(s='A')+(s='B');", vec![65, 66]),
    ] {
        string(source, &expected);
    }
}

#[test]
fn concat117_contexts_consumers() {
    for source in [
        "function f(){'use strict';var s='A';return s+'B';}f()==='AB';",
        "'use strict';function f(){return 'A'+'B';}f()==='AB';",
        "function f(){var s='A';return s+'B';}f()==='AB';",
        "(function(){return 'A'+'B';})()==='AB';",
        "function f(){'use strict';return 'A'+'B';}f();var o={'\\141':7};o.a===7;",
        "('A'+'B').length===2&&('A'+'B')==='AB'&&('A'+'C')!=='AB';",
        "var s='A';var t=s+'B';s==='A'&&t==='AB';",
        "encodeURIComponent('A'+' B')==='A%20B';",
    ] {
        boxed(source, 2, 0.0, true);
    }
    boxed("('A'+'B').length+3;", 3, 5.0, false);
}

#[test]
fn concat117_once_order() {
    boxed(
        "var n=0;var s=(n=n+1,'A')+(n=n+1,n===2?'B':'X');n===2&&s==='AB';",
        2,
        0.0,
        true,
    );
    boxed(
        "var o={n:0};var s=(o.n=o.n+1,'A')+(o.n=o.n+1,o.n===2?'B':'X');o.n===2&&s==='AB';",
        2,
        0.0,
        true,
    );
}

#[test]
fn concat117_abrupt_prefixes() {
    for source in [
        "function fail(){throw 'left';}var n=0;var ok=false;try{(fail(),'x')+(n=99,'y');}catch(e){ok=e==='left';}finally{n=n===0?1:99;}ok&&n===1;",
        "function fail(){throw 'right';}var n=0;var ok=false;try{(n=n+1,'x')+(fail(),'y');}catch(e){ok=e==='right';}finally{n=n===1?2:99;}ok&&n===2;",
        "function f(){'use strict';function fail(){throw 'payload';}var n=0;var ok=false;try{(fail(),'x')+(n=99,'y');}catch(e){ok=e==='payload';}finally{n=n===0?1:99;}return ok&&n===1;}f();",
        "function f(){'use strict';function fail(){throw 'payload';}var n=0;var ok=false;try{(n=n+1,'x')+(fail(),'y');}catch(e){ok=e==='payload';}finally{n=n===1?2:99;}return ok&&n===2;}f();",
    ] {
        boxed(source, 2, 0.0, true);
    }
}

#[test]
fn concat117_boundaries_numeric() {
    for prefix in ["", "'A'+'B';"] {
        for (row, source) in [
            "({})+'a';",
            "function f(x){return x+'a';}f('b');",
            "function f(){return 'b';}f()+'a';",
            "function f(o){var x=o.x;return x+'a';}f({x:'b'});",
            "var s=true?'a':1;s+'b';",
            "function f(x){return x+1;}f(2);",
        ]
        .into_iter()
        .enumerate()
        {
            let source = format!("{prefix}{source}");
            if row != 0 {
                if row == 5 {
                    boxed(&source, 3, 3.0, false);
                    oracle(&source, serde_json::json!({"kind":"number","value":3}));
                } else {
                    let expected = if row == 4 { "ab" } else { "ba" };
                    string(&source, &expected.encode_utf16().collect::<Vec<_>>());
                    oracle_units(&source, &expected.encode_utf16().collect::<Vec<_>>());
                }
                continue;
            }
            let err = match parse(&source) {
                Ok(_) => panic!("unproved concat admitted: {source}"),
                Err(err) => err,
            };
            assert!(
                err.message
                    .contains("arithmetic currently requires numeric operands"),
                "{err:?}"
            );
            eprintln!(
                "CONCAT117_REFUSAL {}",
                serde_json::json!({"source":source,"error":format!("{err:?}"),"guest_invocations":0})
            );
        }
    }
    boxed("1+2;", 3, 3.0, false);
    boxed("-0+-0;", 3, -0.0, false);
    boxed("NaN===NaN;", 2, 0.0, false);
    boxed("0===-0;", 2, 0.0, true);
    boxed("function f(){var x=2;return x+1;}f();", 3, 3.0, false);
}

#[test]
fn concat117_original_a4_and_throw_controls() {
    const BODY: &str = include_str!("fixtures/test262117/S8.4_A4.js");
    assert_eq!(BODY.matches("__str.length !== 0").count(), 1);
    let mutant = BODY.replacen("__str.length !== 0", "__str.length !== 1", 1);
    let cases = [
        ("original", BODY.to_owned(), false),
        (
            "sentinel",
            format!("{BODY}\n$ERROR('sentinel'+'117');"),
            true,
        ),
        ("mutant", mutant, true),
    ];
    for (id, body, throws) in cases {
        let source = format!("function $ERROR(message) {{ throw message; }}\n{body}\n;1;\n");
        let source2 = source.clone();
        std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
            let path=std::env::temp_dir().join(format!("concat117-a4-{id}.js"));
            std::fs::write(&path,&source2).unwrap();
            let result=pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(&path);
            if throws {
                let error=result.expect_err("must execute diagnostic and throw");
                let text=format!("{error:?}");
                assert!(matches!(error, pd_vm_compat_frontends::javascript::corpus::CorpusFileError::Execution { .. }) && text.contains("uncaught JavaScript throw"), "{text}");
                assert!(!text.contains("Source")&&!text.contains("Fuel"),"{text}");
                eprintln!("CONCAT117_A4 {}",serde_json::json!({"id":id,"source":source2,"error":text,"guest_invocations":1,"upstream_original":false}));
            } else {
                let observation=result.unwrap();
                assert_eq!(observation.cookie,1);
                assert_eq!((observation.aot_exec_count,observation.jit_native_exec_count),(0,0));
                assert!(observation.certification_valid_before_reset&&!observation.certification_valid_after_reset);
                assert_eq!(observation.resources_after_reset,0);
                assert!(observation.owned_arrays_retired);
                eprintln!("CONCAT117_A4 {}",serde_json::json!({"id":id,"source":source2,"body":BODY,"observation":format!("{observation:?}"),"guest_invocations":1,"upstream_original":true}));
            }
        }).unwrap().join().unwrap();
        if throws {
            let value = if id == "sentinel" {
                "sentinel117"
            } else {
                "#1: var __str = \"\"; __str.length === 0. Actual: "
            };
            oracle(&source, serde_json::json!({"kind":"throw","value":value}));
            thrown_units(&source, &value.encode_utf16().collect::<Vec<_>>());
        } else {
            oracle(&source, serde_json::json!({"kind":"number","value":1}));
        }
    }
}

fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .index
}
fn call_indices(debug: &str) -> Vec<u16> {
    debug
        .match_indices("Call(")
        .filter_map(|(at, _)| debug[at + 5..].split(',').next()?.trim().parse().ok())
        .collect()
}

#[test]
fn concat117_helper_indices_and_dormancy() {
    let source = "function f(){var k=1.5;var o={'1.5':7};return o[k];}var o={get length(){'use strict';return ('A'+'B').length;}};('A'+'B')==='AB'&&o.length===2&&f()===7&&Math.abs(-3)===3&&encodeURIComponent('A'+' B')==='A%20B';";
    let ir = parse(source).unwrap();
    let concat = helper(&ir, "__compat_es5_string_concat");
    let to = helper(&ir, "__compat_es5_managed_to_units");
    let from = helper(&ir, "__compat_es5_units_to_managed");
    let get = helper(&ir, "__compat_es5_ref_get");
    let dispatch = helper(&ir, "__compat_es5_dispatch");
    let length = helper(&ir, "__compat_es5_string_length_get");
    let indices: std::collections::HashSet<_> = ir.functions.iter().map(|f| f.index).collect();
    assert_eq!(indices.len(), ir.functions.len());
    for name in [
        "__compat_es5_string_concat",
        "__compat_es5_managed_to_units",
        "__compat_es5_units_to_managed",
    ] {
        assert_eq!(ir.functions.iter().filter(|f| f.name == name).count(), 1);
    }
    let decl = ir.functions.iter().find(|f| f.index == concat).unwrap();
    assert_eq!(decl.arity, 5);
    assert_eq!(
        decl.arg_schemas[3..],
        [Some(vm::TypeSchema::Int), Some(vm::TypeSchema::Int)]
    );
    assert_eq!(
        decl.return_schema,
        Some(vm::TypeSchema::Named("__CompatEs5Primitive".into(), vec![]))
    );
    assert!(concat < get && concat < length && to < concat && from < concat);
    let calls = call_indices(&format!("{:?}", ir.function_impls[&concat]));
    assert_eq!(calls.iter().filter(|i| **i == to).count(), 2);
    assert_eq!(calls.iter().filter(|i| **i == from).count(), 1);
    assert!(call_indices(&format!("{:?}", ir.function_impls[&get])).contains(&dispatch));
    let mut functions: Vec<_> = ir
        .functions
        .iter()
        .filter(|f| f.name.starts_with("__compat_es5_fn_"))
        .map(|f| f.index)
        .collect();
    functions.sort();
    let mut inits: Vec<_> = ir
        .functions
        .iter()
        .filter(|f| f.name.starts_with("__compat_es5_utf16_init_"))
        .map(|f| f.index)
        .collect();
    inits.sort();
    assert!(!functions.is_empty() && !inits.is_empty());
    assert!(functions.iter().all(|i| *i > length && *i < dispatch));
    assert_eq!(inits[0], dispatch + 1);
    let dir = std::env::temp_dir().join("concat117-indices");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("functions.debug"), format!("{:#?}", ir.functions)).unwrap();
    std::fs::write(
        dir.join("concat.debug"),
        format!("{:#?}", ir.function_impls[&concat]),
    )
    .unwrap();
    eprintln!(
        "CONCAT117_INDICES {}",
        serde_json::json!({"source":source,"concat":concat,"to":to,"from":from,"get":get,"length":length,"dispatch":dispatch,"source_functions":functions,"initializers":inits,"unique":indices.len()})
    );
    boxed(source, 2, 0.0, true);
    for source in [
        "1+2;",
        "function f(){var x=2;return x+1;}f();",
        "'abc'.length;",
        "var s='AB';s;",
        "encodeURIComponent('x');",
    ] {
        let ir = parse(source).unwrap();
        assert!(
            !ir.functions
                .iter()
                .any(|f| f.name == "__compat_es5_string_concat")
        );
    }
    let ir = parse("'A'+'B';").unwrap();
    assert!(
        !ir.functions
            .iter()
            .any(|f| f.name == "__compat_es5_ref_get")
    );
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

struct RootObserver {
    corruption: u8,
    abrupt: bool,
}
impl vm::SourcePlugin for RootObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("concat117-root-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let output = object_observer::Observer {
            units: if self.abrupt { None } else { Some(5) },
            force_gc: true,
            ..Default::default()
        };
        let mut ir = vm::SourcePlugin::parse_source(&output, source, context)?;
        let concat = helper(&ir, "__compat_es5_string_concat");
        let begin = helper(&ir, "__compat_es5_node_begin");
        let to = helper(&ir, "__compat_es5_managed_to_units");
        let from = helper(&ir, "__compat_es5_units_to_managed");
        let leave = helper(&ir, "__compat_es5_root_leave");
        let checked = helper(&ir, "__compat_es5_checked_base");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let read = helper(&ir, "__compat_ref_read");
        let write = helper(&ir, "__compat_ref_write");
        let decode = helper(&ir, "__compat_es5_decode_int");
        if self.abrupt {
            ir.function_impls
                .get_mut(&concat)
                .unwrap()
                .body_stmts
                .insert(0, check(E::Bool(false)));
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
                "CONCAT117_ABRUPT_OBSERVER source={source:?} helper_entry_fatal=true skipped_literal_sizes=[131,132] allocation_entry_fatal=true"
            );
            return Ok(ir);
        }
        let params = ir.function_impls[&concat].param_slots.clone();
        let (c, l, r, caller, entry) = (params[0], params[1], params[2], params[3], params[4]);
        let top = fresh(&mut ir);
        let lb = fresh(&mut ir);
        let rb = fresh(&mut ir);
        let before = fresh(&mut ir);
        let ack = fresh(&mut ir);
        let rd = |a: E| call(decode, vec![num(call(read, vec![E::Var(c), num(a)]))]);
        let base = |v| {
            call(
                checked,
                vec![E::Var(c), get(v, "ref_slot"), get(v, "generation")],
            )
        };
        let row = |frame: E, entry: E| {
            add(
                add(frame, E::Int(4)),
                E::Mul(Box::new(entry), Box::new(E::Int(3))),
            )
        };
        let pin = |frame: E, entry: E, v| {
            let a = row(frame, entry);
            vec![
                check(eq(rd(a.clone()), E::Int(4))),
                check(eq(rd(add(a.clone(), E::Int(1))), get(v, "ref_slot"))),
                check(eq(rd(add(a, E::Int(2))), get(v, "generation"))),
            ]
        };
        let mut pre = vec![
            let_(top, T::Int, rd(E::Int(3))),
            check(E::Not(Box::new(eq(E::Var(top), E::Var(caller))))),
            check(eq(rd(add(E::Var(top), E::Int(2))), E::Var(caller))),
            check(eq(rd(add(E::Var(top), E::Int(3))), E::Int(2))),
            let_(lb, T::Int, base(l)),
            let_(rb, T::Int, base(r)),
        ];
        pre.extend(pin(E::Var(top), E::Int(0), l));
        pre.extend(pin(E::Var(top), E::Int(1), r));
        pre.extend([
            let_(before, T::Int, rd(E::Int(11))),
            let_(ack, T::Int, call(collect, vec![E::Var(c)])),
            check(eq(rd(E::Int(11)), add(E::Var(before), E::Int(1)))),
            check(eq(base(l), E::Var(lb))),
            check(eq(base(r), E::Var(rb))),
        ]);
        match self.corruption {
            1 => pre.push(S::Expr {
                expr: call(write, vec![E::Var(c), num(E::Var(lb)), E::Float(3.0)]),
                line: 1,
            }),
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
                        num(add(E::Var(lb), E::Int(3))),
                        E::Float(65536.0),
                    ],
                ),
                line: 1,
            }),
            _ => {}
        }
        let f = ir.function_impls.get_mut(&concat).unwrap();
        let at = f
            .body_stmts
            .iter()
            .position(|s| matches!(s,S::Let{expr:E::Call(i,_,_,_,_),..} if *i==to))
            .unwrap();
        f.body_stmts.splice(at..at, pre);
        let (at, out) = f
            .body_stmts
            .iter()
            .enumerate()
            .find_map(|(at, s)| match s {
                S::Assign {
                    index,
                    expr: E::Call(i, _, _, _, _),
                    ..
                } if *i == from => Some((at, *index)),
                _ => None,
            })
            .unwrap();
        let mut post = vec![check(eq(rd(E::Int(3)), E::Var(top)))];
        post.extend(pin(E::Var(caller), E::Var(entry), out));
        post.push(check(E::Gt(Box::new(base(out)), Box::new(E::Int(0)))));
        f.body_stmts.splice(at + 1..at + 1, post);
        let leave_at = f
            .body_stmts
            .iter()
            .position(|s| matches!(s,S::Assign{expr:E::Call(i,_,_,_,_),..} if *i==leave))
            .unwrap();
        // No observer allocation or collection in the publish/leave/caller gap.
        assert!(
            f.body_stmts[at + 1..leave_at]
                .iter()
                .all(|s| !format!("{s:?}").contains(&format!("Call({collect},")))
        );
        eprintln!(
            "CONCAT117_ROOTS source={source:?} concat={concat} caller_frame_param={caller} caller_entry_param={entry} private_two_inputs_checked=true collection_before_bridge=true output_caller_row_checked_before_leave=true corruption={}",
            self.corruption
        );
        Ok(ir)
    }
}

fn roots(source: &str, observer: &'static RootObserver, success: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = probe_vm(&source, observer, "concat117-root-observer");
            let status = machine.run();
            eprintln!(
                "CONCAT117_ROOT_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if success {
                assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            } else {
                let diagnostic = format!("{:?}", status.expect_err("metadata fatal"));
                assert!(diagnostic.contains("assertion failed"), "{diagnostic}");
                assert!(!diagnostic.contains("Fuel"));
            }
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            if success && !observer.abrupt {
                let at = machine
                    .stack()
                    .iter()
                    .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                    .unwrap();
                assert_eq!(machine.stack()[at + 2], vm::Value::Float(5.0));
                for (i, u) in [65, 0, 0xD800, 0xDC00, 0xDC00].iter().enumerate() {
                    assert_eq!(machine.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
                }
            }
            if observer.abrupt {
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
            }
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn concat117_private_roots_caller_publication_gc() {
    static OBS: RootObserver = RootObserver {
        corruption: 0,
        abrupt: false,
    };
    let source = r#"function f(){'use strict';return 'A\0\uD800'+'\uDC00\uDC00';}var s=f();var t='X'+'Y';s;"#;
    roots(source, &OBS, true);
    oracle_units(source, &[65, 0, 0xD800, 0xDC00, 0xDC00]);
}
#[test]
fn concat117_malformed_bridge_controls() {
    static BAD: [RootObserver; 3] = [
        RootObserver {
            corruption: 1,
            abrupt: false,
        },
        RootObserver {
            corruption: 2,
            abrupt: false,
        },
        RootObserver {
            corruption: 3,
            abrupt: false,
        },
    ];
    for observer in &BAD {
        roots("'A'+'B';", observer, false);
    }
}
#[test]
fn concat117_abrupt_no_helper_no_skipped_allocation() {
    static OBS: RootObserver = RootObserver {
        corruption: 0,
        abrupt: true,
    };
    for source in [
        "function fail(){throw 'left';}var n=0;var ok=false;try{(fail(),'LLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLL')+(n=99,'RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR');}catch(e){ok=e==='left';}finally{n=n===0?1:99;}ok&&n===1;",
        "function fail(){throw 'right';}var n=0;var ok=false;try{(n=n+1,'x')+(fail(),'RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR');}catch(e){ok=e==='right';}finally{n=n===1?2:99;}ok&&n===2;",
    ] {
        roots(source, &OBS, true);
        oracle(source, serde_json::json!({"kind":"boolean","value":true}));
    }
}

#[test]
fn concat117_original65536_cap_attempt() {
    // This is the original public size, never a private lowered ceiling. Keep
    // setup/static/fuel failure distinct if the fixed budget stops us earlier.
    std::thread::Builder::new().stack_size(33_554_432).spawn(|| {
        let source = format!("var a='{}';var b=a+a;var c=b+b;c+'B';", "A".repeat(16384));
        let path=std::env::temp_dir().join("concat117-original65536.js");std::fs::write(&path,&source).unwrap();
        let result=pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source);
        match result {
            Err(error)=>eprintln!("CONCAT117_CAP {}",serde_json::json!({"source_file":path,"left":65536,"right":1,"total":65537,"original_ceiling":65536,"fuel":10000000,"phase":"compile","error":format!("{error:?}"),"sum_limit_witness":false})),
            Ok(mut machine)=>{
                machine.set_jit_config(vm::JitConfig{enabled:false,..*machine.jit_config()});machine.set_fuel(10_000_000);
                let status=machine.run();
                eprintln!("CONCAT117_CAP_STATUS status={status:?} ip={} frames={:?}",machine.ip(),machine.execution_frames());
                                assert!(!matches!(status, Ok(vm::VmStatus::Halted)), "oversized concat must not publish");
                if matches!(status, Ok(vm::VmStatus::Yielded)) {
                    assert_eq!(machine.last_yield_reason(),Some(vm::VmYieldReason::Fuel));
                    assert_eq!(machine.get_fuel(),Some(0));
                }
                let diagnostic=format!("{status:?}");
                eprintln!("CONCAT117_CAP {}",serde_json::json!({"source_file":path,"left":65536,"right":1,"total":65537,"original_ceiling":65536,"fuel":10000000,"phase":"run","error":diagnostic,"ip":machine.ip(),"frames":format!("{:?}",machine.execution_frames()),"sum_limit_witness":false,"classification":"actual fatal transport or original fuel exhaustion before oversized sum check; no sum-limit claim","yield_reason":format!("{:?}",machine.last_yield_reason()),"fuel_remaining":machine.get_fuel()}));
                assert_eq!((machine.aot_exec_count(),machine.jit_native_exec_count()),(0,0));reset(&mut machine);
            }
        }
    }).unwrap().join().unwrap();
}

fn thrown_units(source: &str, expected: &[u16]) {
    let source = source.to_owned();
    let expected = expected.to_vec();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let observer=Box::leak(Box::new(object_observer::Observer { units:Some(expected.len()),uncaught:true,..Default::default() }));
        let mut machine=probe_vm(&source,observer,"javascript-es5-object-observer");
        let status=machine.run();
        let diagnostic=format!("{:?}",status.expect_err("actual uncaught guest throw"));
        assert!(diagnostic.contains("uncaught JavaScript throw"),"{diagnostic}");
        assert_eq!((machine.aot_exec_count(),machine.jit_native_exec_count()),(0,0));
        let at=machine.stack().iter().position(|v|*v==vm::Value::string("P7_OBSERVED")).unwrap();
        let vm::Value::Map(boxed)=&machine.stack()[at+1] else {panic!("throw box")};
        assert_eq!(boxed.len(),5);
        for (key,value) in [("tag",vm::Value::Int(4)),("number",vm::Value::Float(0.0)),("flag",vm::Value::Bool(false))] { assert_eq!(boxed.get(&vm::Value::string(key)),Some(&value)); }
        for key in ["ref_slot","generation"] { assert!(matches!(boxed.get(&vm::Value::string(key)),Some(vm::Value::Int(n)) if *n>0)); }
        assert_eq!(machine.stack()[at+2],vm::Value::Float(expected.len() as f64));
        for (i,u) in expected.iter().enumerate(){assert_eq!(machine.stack()[at+3+i],vm::Value::Float(f64::from(*u)));}
        eprintln!("CONCAT117_THROW_UNITS {}",serde_json::json!({"source":source,"expected_units":expected,"error":diagnostic,"full_stack":format!("{:?}",machine.stack()),"guest_invocations":1,"native":0}));
        reset(&mut machine);
    }).unwrap().join().unwrap();
}

#[test]
fn concat117_managed_throw_finally_gc() {
    let source = "function f(){'use strict';try{throw 'A'+'B';}finally{var t='C'+'D';}}var ok=false;try{f();}catch(e){ok=e==='AB';}ok;";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let observer = Box::leak(Box::new(object_observer::Observer {
                force_gc: true,
                ..Default::default()
            }));
            let mut machine = probe_vm(source, observer, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "CONCAT117_MANAGED_THROW_GC source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let at = machine
                .stack()
                .iter()
                .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                .unwrap();
            let vm::Value::Map(boxed) = &machine.stack()[at + 1] else {
                panic!("catch result")
            };
            assert_eq!(
                boxed.get(&vm::Value::string("tag")),
                Some(&vm::Value::Int(2))
            );
            assert_eq!(
                boxed.get(&vm::Value::string("flag")),
                Some(&vm::Value::Bool(true))
            );
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
    oracle(source, serde_json::json!({"kind":"boolean","value":true}));
}
