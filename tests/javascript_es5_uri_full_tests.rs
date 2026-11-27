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
                "URI123_SOURCE {}",
                serde_json::json!({"source":source,"expected_units":expected})
            );
            let observer = Box::leak(Box::new(object_observer::Observer {
                units: Some(expected.len()),
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "URI123_VM source={source:?} status={status:?} stack={:?}",
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
        "URI123_NODE {}",
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
fn full_uri123_exact_red() {
    string("encodeURI('A B');", &[65, 37, 50, 48, 66]);
}

fn execute(source: &str) -> vm::Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("URI123_SOURCE {}", serde_json::json!({"source": source}));
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
                "URI123_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "URI123_ROOT_FAILURE ip={} frames={:?}",
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
                "URI123_RESET source={source:?} pending=false resources=0 guest_invocations=1 native=0"
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
        "URI123_NODE {}",
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
        eprintln!("URI123_THROW_UNITS {}",serde_json::json!({"source":source,"expected_units":expected,"error":diagnostic,"full_stack":format!("{:?}",machine.stack()),"guest_invocations":1,"native":0}));
        reset(&mut machine);
    }).unwrap().join().unwrap();
}

fn yes(source: &str) {
    boxed(source, 2, 0.0, true);
}
#[test]
fn full_uri123_reserved_and_units() {
    for (source, expected) in [
        (
            "encodeURI(\"AZaz09-_.!~*'();/?:@&=+$,#% \");",
            "AZaz09-_.!~*'();/?:@&=+$,#%25%20",
        ),
        (
            "decodeURI('%2f%2F%3f%3F%23%25%41%7e');",
            "%2f%2F%3f%3F%23%A~",
        ),
        (
            "encodeURI('\\0Ж中\\uD83D\\uDE00');",
            "%00%D0%96%E4%B8%AD%F0%9F%98%80",
        ),
        ("decodeURI('%00%d0%96%e4%b8%ad%f0%9f%98%80');", "\0Ж中😀"),
        (
            "decodeURI('%0a%0d%E2%80%A8%E2%80%A9');",
            "\n\r\u{2028}\u{2029}",
        ),
        ("encodeURI('');", ""),
        ("decodeURI('');", ""),
        ("var s=false?'X':'A ';encodeURI(s+'B');", "A%20B"),
        ("var s=true?'%41':'X';decodeURI(s+'%20B');", "A B"),
        ("encodeURI('\\uD83D'+'\\uDE00');", "%F0%9F%98%80"),
    ] {
        text(source, expected);
    }
    yes(
        "encodeURIComponent(';/?:@&=+$,#')==='%3B%2F%3F%3A%40%26%3D%2B%24%2C%23'&&decodeURIComponent('%2f%3f%23')==='/?#'&&encodeURI(';/?:@&=+$,#')===';/?:@&=+$,#';",
    );
}
#[test]
fn full_uri123_decode_errors() {
    yes(
        "function f(s){try{decodeURI(s);}catch(e){return e.name==='URIError';}return false;}f('%')&&f('%0')&&f('%GG')&&f('%C0%AF')&&f('%E0%80%80')&&f('%E2%82')&&f('%ED%A0%80')&&f('%F4%90%80%80')&&f('%80')&&f('%F8%88%80%80%80');",
    );
}
#[test]
fn full_uri123_encode_errors() {
    yes(
        "function f(s){'use strict';try{encodeURI(s);}catch(e){return e.name==='URIError';}return false;}f('\\uD800')&&f('\\uDC00')&&f('x\\uD800x');",
    );
}
#[test]
fn full_uri123_error_prototypes() {
    yes(
        "var a;var b;var c;var t;var n=0;try{decodeURI('%');}catch(e){a=e;}finally{n=n+1;}try{encodeURI('\\uD800');}catch(e){b=e;}try{decodeURIComponent('%');}catch(e){c=e;}try{var x=null;x.y;}catch(e){t=e;}var p=Object.getPrototypeOf(a);n===1&&a!==b&&a!==c&&p===Object.getPrototypeOf(b)&&p===Object.getPrototypeOf(c)&&p!==Object.getPrototypeOf(t)&&p.name==='URIError'&&p.message===''&&Object.getPrototypeOf(p)===Object.getPrototypeOf(Object.getPrototypeOf(t));",
    );
}
#[test]
fn full_uri123_error_finally_payload() {
    text(
        "function f(){try{decodeURI('%');}catch(e){return e.name;}finally{var s=encodeURI('A B');}}f();",
        "URIError",
    );
}
#[test]
fn full_uri123_values_receivers() {
    yes(
        "var f=encodeURI;var d=decodeURI;var o={f:f,d:d};var p=Object.create(o);var b=p.f;f.length===1&&d.length===1&&f!==d&&Object.getPrototypeOf(f)===Object.getPrototypeOf(Object)&&o.f('A B')==='A%20B'&&p.d('%2f%41')==='%2fA'&&b('/')==='/';",
    );
}
#[test]
fn full_uri123_callee_argument_order() {
    yes(
        "function callee(o){o.n=o.n+1;return encodeURI;}function arg(o){o.n=o.n*10+2;return 'A B';}var o={n:0};var r=callee(o)(arg(o));o.n===12&&r==='A%20B';",
    );
}
#[test]
fn full_uri123_member_order() {
    yes(
        "var o={n:0,get f(){'use strict';this.n=this.n+1;return decodeURI;},get arg(){'use strict';this.n=this.n*10+2;return '%41';}};var r=o.f(o.arg);o.n===12&&r==='A';",
    );
}
#[test]
fn full_uri123_abrupt_callee() {
    yes(
        "function bad(){throw 'callee';}function arg(o){o.n=o.n+1;return 'x';}var o={n:0};var caught=false;try{bad()(arg(o));}catch(e){caught=e==='callee';}o.n===0&&caught;",
    );
}
#[test]
fn full_uri123_abrupt_argument() {
    yes(
        "var o={n:0,get arg(){'use strict';this.n=this.n+1;throw 'argument';}};var caught=false;try{encodeURI(o.arg);}catch(e){caught=e==='argument';}o.n===1&&caught;",
    );
}
#[test]
fn full_uri123_sloppy_and_own() {
    yes(
        "function sloppy(s){return encodeURI(s);}function own(s){'use strict';return decodeURI(s);}sloppy('A B')==='A%20B'&&own('%41')==='A'&&(function(){return decodeURI('%2f');})()==='%2f';",
    );
}
#[test]
fn full_uri123_inherited_strict() {
    yes("'use strict';function inherited(s){return encodeURI(s);}inherited('A B')==='A%20B';");
}
#[test]
fn full_uri123_prerequisites_indices() {
    for source in [
        "1;",
        "encodeURI('A B');",
        "decodeURI('%41');",
        "encodeURI('A B')==='A%20B'&&decodeURI('%41')==='A'&&encodeURIComponent('/')==='%2F'&&decodeURIComponent('%2f')==='/';",
        "function f(){var k=1.5;var o={'1.5':7};return o[k];}var o={get length(){return ('A'+true).length;}};('A'+1)==='A1'&&o.length===5&&f()===7&&Math.abs(-3)===3&&encodeURI('A '+null)==='A%20null'&&decodeURI('%2f')==='%2f';",
    ] {
        let ir = parse(source).unwrap();
        let count = |name| ir.functions.iter().filter(|f| f.name == name).count();
        let demand = source != "1;";
        for name in [
            "__compat_es5_uri_call",
            "__compat_es5_uri_kernel",
            "__compat_es5_managed_to_units",
            "__compat_es5_units_to_managed",
        ] {
            assert_eq!(count(name), usize::from(demand), "{source} {name}");
        }
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
                    assert!(slot < ir.locals, "{source}");
                }
            }
        }
        if demand {
            let adapter = helper(&ir, "__compat_es5_uri_call");
            let kernel = helper(&ir, "__compat_es5_uri_kernel");
            let to = helper(&ir, "__compat_es5_managed_to_units");
            let from = helper(&ir, "__compat_es5_units_to_managed");
            assert!(to < kernel && from < kernel && kernel < adapter);
            assert_eq!(
                ir.functions
                    .iter()
                    .find(|f| f.index == adapter)
                    .unwrap()
                    .arity,
                3
            );
            let dispatch = helper(&ir, "__compat_es5_dispatch");
            assert_eq!(
                ir.functions
                    .iter()
                    .find(|f| f.index == dispatch)
                    .unwrap()
                    .arity,
                4
            );
            assert!(
                call_indices(&format!("{:?}", ir.function_impls[&dispatch])).contains(&adapter)
            );
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
            if !funcs.is_empty() {
                assert_eq!(dispatch, funcs[0] + funcs.len() as u16);
                for f in &funcs {
                    assert_eq!(
                        ir.functions.iter().find(|v| v.index == *f).unwrap().arity,
                        5
                    );
                }
            }
            if let Some(first) = inits.first() {
                assert_eq!(*first, dispatch + 1);
            }
            eprintln!(
                "URI123_INDICES {}",
                serde_json::json!({"source":source,"adapter":adapter,"kernel":kernel,"to":to,"from":from,"dispatch":dispatch,"source_functions":funcs,"initializers":inits,"locals":ir.locals,"unique":ids.len()})
            );
        }
        if source.contains("&&") {
            yes(source);
        }
    }
}
const ORIGINALS: &[(&str, &str)] = &[
    (
        "S15.1.3.1_A4_T1",
        include_str!("fixtures/test262123/S15.1.3.1_A4_T1.js"),
    ),
    (
        "S15.1.3.1_A4_T2",
        include_str!("fixtures/test262123/S15.1.3.1_A4_T2.js"),
    ),
    (
        "S15.1.3.1_A4_T3",
        include_str!("fixtures/test262123/S15.1.3.1_A4_T3.js"),
    ),
    (
        "S15.1.3.1_A4_T4",
        include_str!("fixtures/test262123/S15.1.3.1_A4_T4.js"),
    ),
    (
        "S15.1.3.3_A4_T1",
        include_str!("fixtures/test262123/S15.1.3.3_A4_T1.js"),
    ),
    (
        "S15.1.3.3_A4_T3",
        include_str!("fixtures/test262123/S15.1.3.3_A4_T3.js"),
    ),
    (
        "S15.1.3.3_A4_T4",
        include_str!("fixtures/test262123/S15.1.3.3_A4_T4.js"),
    ),
];
fn file_case(id: &str, body: &str, expected: Option<&str>, original: bool) {
    let source = format!("function $ERROR(message){{throw message;}}\n{body}\n1;");
    let copy = source.clone();
    let id = id.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let path=std::env::temp_dir().join(format!("uri123-{id}-{}.js",std::process::id()));std::fs::write(&path,&copy).unwrap();
        let result=pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(&path);
        if original {
            let o=result.unwrap();assert_eq!(o.cookie,1);assert_eq!((o.aot_exec_count,o.jit_native_exec_count),(0,0));assert!(o.certification_valid_before_reset&&!o.certification_valid_after_reset);assert_eq!(o.resources_after_reset,0);assert!(o.owned_arrays_retired);
            eprintln!("URI123_FILE {}",serde_json::json!({"id":id,"source":copy,"observation":format!("{o:?}"),"guest_invocations":1,"upstream_original":true}));
        } else {
            let error=result.expect_err("actual guest throw");assert!(matches!(error,pd_vm_compat_frontends::javascript::corpus::CorpusFileError::Execution{..}));let diagnostic=format!("{error:?}");assert!(diagnostic.contains("uncaught JavaScript throw")&&!diagnostic.contains("Fuel"));
            eprintln!("URI123_FILE {}",serde_json::json!({"id":id,"source":copy,"error":diagnostic,"guest_invocations":1,"upstream_original":false}));
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
#[test]
fn full_uri123_original_decode_bodies() {
    for (id, body) in &ORIGINALS[..4] {
        file_case(id, body, None, true);
    }
}
#[test]
fn full_uri123_original_encode_bodies() {
    for (id, body) in &ORIGINALS[4..] {
        file_case(id, body, None, true);
    }
}
#[test]
fn full_uri123_original_failure_controls() {
    for i in [0, 4] {
        let (id, body) = ORIGINALS[i];
        file_case(
            &format!("{id}-sentinel"),
            &format!("{body}\n$ERROR('sentinel-{id}');"),
            Some(&format!("sentinel-{id}")),
            false,
        );
        const OLD: &str = "!== \"http://unipro.ru/0123456789\"";
        const NEW: &str = "!== \"http://unipro.ru/0123456789-mutant\"";
        assert_eq!(body.matches(OLD).count(), 1);
        let mutant = body.replacen(OLD, NEW, 1);
        assert_eq!(mutant.replace(NEW, OLD), body);
        eprintln!(
            "URI123_MUTATION {}",
            serde_json::json!({"id":id,"original_literal_comparison":OLD,"replacement":NEW,"single_RHS_expected_literal_only":true})
        );
        file_case(
            &format!("{id}-mutant"),
            &mutant,
            Some("#1: http://unipro.ru/0123456789"),
            false,
        );
    }
}
