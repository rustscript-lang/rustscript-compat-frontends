//! Actual primitive arguments and standard URI arity through the public VM.
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
                "URI125_SOURCE {}",
                serde_json::json!({"source":source,"expected_units":expected})
            );
            let observer = Box::leak(Box::new(object_observer::Observer {
                units: Some(expected.len()),
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "URI125_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            let native = (machine.jit_native_exec_count(), machine.aot_exec_count());
            if !matches!(status, Ok(vm::VmStatus::Halted)) {
                eprintln!("URI125_RUNTIME_RED status={status:?} native={native:?}");
                assert_eq!(native, (0, 0));
                reset(&mut machine);
                eprintln!("URI125_RED_RESET resources=0 owned_retired=true cert_valid=false");
                panic!("actualruntimeStringtagassertion: {status:?}");
            }
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
        "URI125_NODE {}",
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
fn uri_primitive125_exact_red() {
    string("encodeURIComponent(1);", &[49]);
}

fn execute(source: &str) -> vm::Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("URI125_SOURCE {}", serde_json::json!({"source": source}));
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
                "URI125_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "URI125_ROOT_FAILURE ip={} frames={:?}",
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
                "URI125_RESET source={source:?} pending=false resources=0 guest_invocations=1 native=0"
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
        "URI125_NODE {}",
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
        eprintln!("URI125_THROW_UNITS {}",serde_json::json!({"source":source,"expected_units":expected,"error":diagnostic,"full_stack":format!("{:?}",machine.stack()),"guest_invocations":1,"native":0}));
        reset(&mut machine);
    }).unwrap().join().unwrap();
}

fn yes(source: &str) {
    boxed(source, 2, 0.0, true);
}

#[test]
fn uri_primitive125_old_alias4() {
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=encodeURIComponent;var caught=false;try{f(1);}catch(e){caught=true;}caught;","owner":"uri_public_unsupported_boundaries_type_alias"})
    );
    boxed(
        "var f=encodeURIComponent;var caught=false;try{f(1);}catch(e){caught=true;}caught;",
        2,
        0.0,
        false,
    );
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=encodeURIComponent;var caught=false;try{f(true);}catch(e){caught=true;}caught;","owner":"uri_public_unsupported_boundaries_type_alias"})
    );
    boxed(
        "var f=encodeURIComponent;var caught=false;try{f(true);}catch(e){caught=true;}caught;",
        2,
        0.0,
        false,
    );
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=encodeURIComponent;var caught=false;try{f(null);}catch(e){caught=true;}caught;","owner":"uri_public_unsupported_boundaries_type_alias"})
    );
    boxed(
        "var f=encodeURIComponent;var caught=false;try{f(null);}catch(e){caught=true;}caught;",
        2,
        0.0,
        false,
    );
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=encodeURIComponent;var caught=false;try{f(undefined);}catch(e){caught=true;}caught;","owner":"uri_public_unsupported_boundaries_type_alias"})
    );
    boxed(
        "var f=encodeURIComponent;var caught=false;try{f(undefined);}catch(e){caught=true;}caught;",
        2,
        0.0,
        false,
    );
    text("var f=encodeURIComponent;f(1);", "1");
    text("var f=encodeURIComponent;f(true);", "true");
    text("var f=encodeURIComponent;f(null);", "null");
    text("var f=encodeURIComponent;f(undefined);", "undefined");
}

#[test]
fn uri_primitive125_old_member2() {
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var o={f:decodeURIComponent};var copied=o.f;try{copied();}catch(e){true;}","owner":"uri_public_unsupported_boundaries_arity_member"})
    );
    text(
        "var o={f:decodeURIComponent};var copied=o.f;try{copied();}catch(e){true;}",
        "undefined",
    );
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var o={f:decodeURIComponent};var copied=o.f;try{copied('A','B');}catch(e){true;}","owner":"uri_public_unsupported_boundaries_arity_member"})
    );
    text(
        "var o={f:decodeURIComponent};var copied=o.f;try{copied('A','B');}catch(e){true;}",
        "A",
    );
}

#[test]
fn uri_primitive125_old_encode9() {
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"try{encodeURI(1);}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text("try{encodeURI(1);}catch(e){true;}", "1");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"try{encodeURI();}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text("try{encodeURI();}catch(e){true;}", "undefined");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"try{encodeURI('A','B');}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text("try{encodeURI('A','B');}catch(e){true;}", "A");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=encodeURI;try{f(1);}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text("var f=encodeURI;try{f(1);}catch(e){true;}", "1");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=encodeURI;try{f();}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text("var f=encodeURI;try{f();}catch(e){true;}", "undefined");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=encodeURI;try{f('A','B');}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text("var f=encodeURI;try{f('A','B');}catch(e){true;}", "A");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var o={f:encodeURI};try{o.f(1);}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text("var o={f:encodeURI};try{o.f(1);}catch(e){true;}", "1");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var o={f:encodeURI};try{o.f();}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text(
        "var o={f:encodeURI};try{o.f();}catch(e){true;}",
        "undefined",
    );
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var o={f:encodeURI};try{o.f('A','B');}catch(e){true;}","owner":"full_boundary/encodeURI"})
    );
    text("var o={f:encodeURI};try{o.f('A','B');}catch(e){true;}", "A");
}

#[test]
fn uri_primitive125_old_decode9() {
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"try{decodeURI(1);}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text("try{decodeURI(1);}catch(e){true;}", "1");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"try{decodeURI();}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text("try{decodeURI();}catch(e){true;}", "undefined");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"try{decodeURI('A','B');}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text("try{decodeURI('A','B');}catch(e){true;}", "A");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=decodeURI;try{f(1);}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text("var f=decodeURI;try{f(1);}catch(e){true;}", "1");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=decodeURI;try{f();}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text("var f=decodeURI;try{f();}catch(e){true;}", "undefined");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var f=decodeURI;try{f('A','B');}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text("var f=decodeURI;try{f('A','B');}catch(e){true;}", "A");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var o={f:decodeURI};try{o.f(1);}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text("var o={f:decodeURI};try{o.f(1);}catch(e){true;}", "1");
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var o={f:decodeURI};try{o.f();}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text(
        "var o={f:decodeURI};try{o.f();}catch(e){true;}",
        "undefined",
    );
    eprintln!(
        "URI125_EXACT_OLD {}",
        serde_json::json!({"source":"var o={f:decodeURI};try{o.f('A','B');}catch(e){true;}","owner":"full_boundary/decodeURI"})
    );
    text("var o={f:decodeURI};try{o.f('A','B');}catch(e){true;}", "A");
}

#[test]
fn uri_primitive125_tags_encode_component() {
    for (args, expected) in [
        ("'A'", "A"),
        ("1", "1"),
        ("true", "true"),
        ("false", "false"),
        ("null", "null"),
        ("undefined", "undefined"),
        ("u", "undefined"),
        ("", "undefined"),
        ("'A',{},function(){throw 'coerce';}", "A"),
    ] {
        text(&format!("var u;encodeURIComponent({args});"), expected);
    }
}

#[test]
fn uri_primitive125_tags_decode_component() {
    for (args, expected) in [
        ("'A'", "A"),
        ("1", "1"),
        ("true", "true"),
        ("false", "false"),
        ("null", "null"),
        ("undefined", "undefined"),
        ("u", "undefined"),
        ("", "undefined"),
        ("'A',{},function(){throw 'coerce';}", "A"),
    ] {
        text(&format!("var u;decodeURIComponent({args});"), expected);
    }
}

#[test]
fn uri_primitive125_tags_encode_full() {
    for (args, expected) in [
        ("'A'", "A"),
        ("1", "1"),
        ("true", "true"),
        ("false", "false"),
        ("null", "null"),
        ("undefined", "undefined"),
        ("u", "undefined"),
        ("", "undefined"),
        ("'A',{},function(){throw 'coerce';}", "A"),
    ] {
        text(&format!("var u;encodeURI({args});"), expected);
    }
}

#[test]
fn uri_primitive125_tags_decode_full() {
    for (args, expected) in [
        ("'A'", "A"),
        ("1", "1"),
        ("true", "true"),
        ("false", "false"),
        ("null", "null"),
        ("undefined", "undefined"),
        ("u", "undefined"),
        ("", "undefined"),
        ("'A',{},function(){throw 'coerce';}", "A"),
    ] {
        text(&format!("var u;decodeURI({args});"), expected);
    }
}

#[test]
fn uri_primitive125_numbers_component() {
    for (arg, expected) in [
        ("-0", "0"),
        ("NaN", "NaN"),
        ("Infinity", "Infinity"),
        ("-Infinity", "-Infinity"),
        ("1e-7", "1e-7"),
        ("1e-6", "0.000001"),
        ("1e20", "100000000000000000000"),
        ("1e21", "1e%2B21"),
        ("1.2345678901234567", "1.2345678901234567"),
        ("1000000000000000100", "1000000000000000100"),
    ] {
        text(&format!("encodeURIComponent({arg});"), expected);
    }
}

#[test]
fn uri_primitive125_numbers_full() {
    for (arg, expected) in [
        ("-0", "0"),
        ("NaN", "NaN"),
        ("Infinity", "Infinity"),
        ("-Infinity", "-Infinity"),
        ("1e-7", "1e-7"),
        ("1e-6", "0.000001"),
        ("1e20", "100000000000000000000"),
        ("1e21", "1e+21"),
        ("1.2345678901234567", "1.2345678901234567"),
        ("1000000000000000100", "1000000000000000100"),
    ] {
        text(&format!("encodeURI({arg});"), expected);
    }
}

#[test]
fn uri_primitive125_numbers_decode_component() {
    for (arg, expected) in [
        ("-0", "0"),
        ("NaN", "NaN"),
        ("Infinity", "Infinity"),
        ("-Infinity", "-Infinity"),
        ("1e-7", "1e-7"),
        ("1e-6", "0.000001"),
        ("1e20", "100000000000000000000"),
        ("1e21", "1e+21"),
        ("1.2345678901234567", "1.2345678901234567"),
        ("1000000000000000100", "1000000000000000100"),
    ] {
        text(&format!("decodeURIComponent({arg});"), expected);
    }
}

#[test]
fn uri_primitive125_numbers_decode_full() {
    for (arg, expected) in [
        ("-0", "0"),
        ("NaN", "NaN"),
        ("Infinity", "Infinity"),
        ("-Infinity", "-Infinity"),
        ("1e-7", "1e-7"),
        ("1e-6", "0.000001"),
        ("1e20", "100000000000000000000"),
        ("1e21", "1e+21"),
        ("1.2345678901234567", "1.2345678901234567"),
        ("1000000000000000100", "1000000000000000100"),
    ] {
        text(&format!("decodeURI({arg});"), expected);
    }
}

#[test]
fn uri_primitive125_actual_unknown_shadow_overwrite() {
    yes(
        "function f(g,x){return g(x);}var o={g:decodeURI,x:true};var saved=encodeURI;encodeURI=decodeURIComponent;function shadow(encodeURIComponent){return encodeURIComponent;}f(saved,7)==='7'&&f(o.g,o.x)==='true'&&encodeURI(1)==='1'&&saved.length===1&&encodeURIComponent.length===1&&decodeURIComponent.length===1&&decodeURI.length===1&&shadow(9)===9;",
    );
}
#[test]
fn uri_primitive125_order_once_ignored_extras() {
    yes(
        "var o={n:0};var extra={n:0,valueOf:function(){'use strict';this.n=this.n+1;throw 'coerce';},toString:function(){'use strict';this.n=this.n+1;throw 'coerce';}};function callee(o){o.n=o.n*10+1;return encodeURIComponent;}function first(o){o.n=o.n*10+2;return true;}function second(o,extra){o.n=o.n*10+3;return extra;}function third(o){o.n=o.n*10+4;return function(){throw 'ignored';};}var r=callee(o)(first(o),second(o,extra),third(o));o.n===1234&&extra.n===0&&r==='true';",
    );
}
#[test]
fn uri_primitive125_legacy_string_units() {
    text("encodeURI(';/?:@&=+$,#');", ";/?:@&=+$,#");
    text("encodeURIComponent('+');", "%2B");
    text("decodeURI('%2f%3F%23');", "%2f%3F%23");
    text("decodeURIComponent('%2b');", "+");
    text("decodeURI('%00%c3%a9%F0%9F%98%80');", "\0é😀");
}

#[test]
fn uri_primitive125_finite09_10_parity() {
    text("var f=encodeURIComponent;f('A B');", "A%20B");
    yes(
        "function f(){'use strict';try{decodeURIComponent('%');}catch(e){return e.name==='URIError';}}f();",
    );
}
#[test]
fn uri_primitive125_object_function_array_first() {
    for name in [
        "encodeURIComponent",
        "decodeURIComponent",
        "encodeURI",
        "decodeURI",
    ] {
        for value in [
            "{valueOf:function(){throw 'callback';},toString:function(){throw 'callback';}}",
            "function(){return 1;}",
            "[]",
        ] {
            let source = format!("var f={name};try{{f({value});}}catch(e){{true;}}");
            std::thread::Builder::new().stack_size(33_554_432).spawn(move|| {
                let mut m=pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source).unwrap();
                m.set_jit_config(vm::JitConfig{enabled:false,..*m.jit_config()});m.set_fuel(10_000_000);
                let status=m.run();eprintln!("URI125_BOUNDARY {}",serde_json::json!({"source":source,"status":format!("{status:?}"),"native":m.jit_native_exec_count(),"aot":m.aot_exec_count()}));
                assert!(format!("{:?}",status.expect_err("fatal profile, not guest catch")).contains("assertion failed"));assert_eq!((m.jit_native_exec_count(),m.aot_exec_count()),(0,0));reset(&mut m);
            }).unwrap().join().unwrap();
        }
    }
}

fn file_case(id: &str, body: &str, expected: Option<&str>, success: bool) {
    let source = format!("function $ERROR(message){{throw message;}}\n{body}\n1;");
    let copy = source.clone();
    let id = id.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let path=std::env::temp_dir().join(format!("uri125-{id}-{}.js",std::process::id()));std::fs::write(&path,&copy).unwrap();
        let result=pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(&path);
        if success {
            let o=result.unwrap();assert_eq!(o.cookie,1);assert_eq!((o.aot_exec_count,o.jit_native_exec_count),(0,0));assert!(o.certification_valid_before_reset&&!o.certification_valid_after_reset);assert_eq!(o.resources_after_reset,0);assert!(o.owned_arrays_retired);
            eprintln!("URI125_FILE {}",serde_json::json!({"id":id,"source":copy,"observation":format!("{o:?}"),"guest_invocations":1,"upstream_original":false,"completion_cookie_case":true}));
        } else {
            let error=result.expect_err("actual guest throw");assert!(matches!(error,pd_vm_compat_frontends::javascript::corpus::CorpusFileError::Execution{..}));let diagnostic=format!("{error:?}");assert!(diagnostic.contains("uncaught JavaScript throw")&&!diagnostic.contains("Fuel"));
            eprintln!("URI125_FILE {}",serde_json::json!({"id":id,"source":copy,"error":diagnostic,"guest_invocations":1,"upstream_original":false}));
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

const FILE_BODY: &str = "if(encodeURIComponent(1)!=='1')$ERROR('numeric');if(decodeURIComponent()!=='undefined')$ERROR('missing');if(encodeURI(true,{},function(){throw 'extra';})!=='true')$ERROR('extras');if(decodeURI(null)!=='null')$ERROR('null');if(encodeURI.length!==1)$ERROR('length');";
#[test]
fn uri_primitive125_public_file() {
    file_case("primitive", FILE_BODY, None, true);
}
#[test]
fn uri_primitive125_public_file_controls() {
    file_case(
        "sentinel",
        &format!("{FILE_BODY}$ERROR('sentinel125');"),
        Some("sentinel125"),
        false,
    );
    let mutant = FILE_BODY.replacen("!=='1'", "!=='2'", 1);
    assert_eq!(mutant.replace("!=='2'", "!=='1'"), FILE_BODY);
    eprintln!("URI125_MUTATION literal_expected_only=true old=1 new=2");
    file_case("mutant", &mutant, Some("numeric"), false);
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
fn let_(index: u16, t: vm::TypeSchema, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index,
        declared_schema: Some(t),
        expr,
        line: 1,
    }
}
fn fresh_slot(next: &mut usize) -> u16 {
    let s = u16::try_from(*next).unwrap();
    *next += 1;
    s
}

struct PrimitiveObserver {
    tag: i64,
    units: Option<usize>,
    abrupt: bool,
    error: bool,
}
impl vm::SourcePlugin for PrimitiveObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("uri125-observer");
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
        let adapter = helper(&ir, "__compat_es5_uri_call");
        let number = helper(&ir, "__compat_es5_number_string_managed");
        let fixed = helper(&ir, "__compat_es5_fixed_scalar_string_managed");
        let kernel = helper(&ir, "__compat_es5_uri_kernel");
        if self.abrupt {
            for id in [adapter, number, fixed, kernel] {
                ir.function_impls
                    .get_mut(&id)
                    .unwrap()
                    .body_stmts
                    .insert(0, check(E::Bool(false)));
            }
            eprintln!("URI125_SKIP adapter_kernel_number_fixed_entry_fatal=true source={source:?}");
            return Ok(ir);
        }
        let read = helper(&ir, "__compat_ref_read");
        let decode = helper(&ir, "__compat_es5_decode_int");
        let checked = helper(&ir, "__compat_es5_checked_base");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let write = helper(&ir, "__compat_es5_root_write");
        let leave = helper(&ir, "__compat_es5_root_leave");
        let to = helper(&ir, "__compat_es5_managed_to_units");
        let from = helper(&ir, "__compat_es5_units_to_managed");
        for (id, allowed) in [(number, self.tag == 3), (fixed, matches!(self.tag, 0..=2))] {
            let p = ir.function_impls[&id].param_slots.clone();
            let checks = if allowed {
                // Publication row is initially empty; a duplicate conversion to
                // the same row would fail this real runtime entry guard.
                let row = add(
                    add(E::Var(p[2]), E::Int(4)),
                    E::Mul(Box::new(E::Var(p[3])), Box::new(E::Int(3))),
                );
                vec![
                    check(eq(E::Var(p[3]), E::Int(0))),
                    check(eq(
                        call(decode, vec![num(call(read, vec![E::Var(p[0]), num(row)]))]),
                        E::Int(0),
                    )),
                ]
            } else {
                vec![check(E::Bool(false))]
            };
            ir.function_impls
                .get_mut(&id)
                .unwrap()
                .body_stmts
                .splice(0..0, checks);
        }
        let mut next = ir.locals;
        let sink = fresh_slot(&mut next);
        let before = fresh_slot(&mut next);
        let pinned = fresh_slot(&mut next);
        let f = ir.function_impls.get_mut(&adapter).unwrap();
        let c = f.param_slots[0];
        let (frame, input) = f
            .body_stmts
            .iter()
            .find_map(|s| match s {
                S::Assign {
                    expr: E::Call(id, _, a, _, _),
                    ..
                } if *id == write => match a.as_slice() {
                    [E::Var(_), E::Var(fr), E::Int(0), E::Var(v)] => Some((*fr, *v)),
                    _ => None,
                },
                _ => None,
            })
            .expect("actual input0 publication");
        let read_cell = |a: E| call(decode, vec![num(call(read, vec![E::Var(c), num(a)]))]);
        let row = |entry: i64, o: i64| add(E::Var(frame), E::Int(4 + entry * 3 + o));
        let base = |v| {
            call(
                checked,
                vec![E::Var(c), get(v, "ref_slot"), get(v, "generation")],
            )
        };
        let rooted = |entry: i64, v: u16, tag: i64| {
            vec![
                check(eq(read_cell(row(entry, 0)), E::Int(tag))),
                check(eq(read_cell(row(entry, 1)), get(v, "ref_slot"))),
                check(eq(read_cell(row(entry, 2)), get(v, "generation"))),
            ]
        };
        let first=f.body_stmts.iter().position(|s|matches!(s,S::Assign{expr:E::Call(id,_,a,_,_),..}if *id==write&&matches!(a.get(2),Some(E::Int(0))))).unwrap();
        let mut initial = vec![
            check(eq(get(input, "tag"), E::Int(self.tag))),
            check(eq(read_cell(add(E::Var(frame), E::Int(3))), E::Int(2))),
            check(eq(read_cell(E::Int(3)), E::Var(frame))),
        ];
        if self.tag == 4 {
            initial.extend(rooted(0, input, 4));
        } else {
            for o in 0..3 {
                initial.push(check(eq(read_cell(row(0, o)), E::Int(0))));
            }
        }
        f.body_stmts.splice(first + 1..first + 1, initial);
        let at = f
            .body_stmts
            .iter()
            .position(|s| matches!(s,S::Let{expr:E::Call(id,_,_,_,_),..}if *id==to))
            .unwrap();
        // The producer handoff and reaffirmation have completed. This is a
        // permitted GC point, before copying input and before output publication.
        let mut converted = rooted(0, input, 4);
        converted.extend([
            let_(pinned, T::Int, base(input)),
            let_(before, T::Int, read_cell(E::Int(11))),
            let_(sink, T::Int, call(collect, vec![E::Var(c)])),
            check(eq(read_cell(E::Int(11)), add(E::Var(before), E::Int(1)))),
            check(eq(base(input), E::Var(pinned))),
        ]);
        converted.extend(rooted(0, input, 4));
        f.body_stmts.splice(at..at, converted);
        let after_kernel = f
            .body_stmts
            .iter()
            .position(|s| matches!(s,S::Let{expr:E::Call(id,_,_,_,_),..}if *id==kernel))
            .unwrap()
            + 1;
        let mut through_kernel = rooted(0, input, 4);
        through_kernel.push(check(eq(base(input), E::Var(pinned))));
        f.body_stmts
            .splice(after_kernel..after_kernel, through_kernel);
        let output = f
            .body_stmts
            .iter()
            .find_map(|s| match s {
                S::IfElse { then_branch, .. } => then_branch.iter().find_map(|s| match s {
                    S::Assign {
                        index,
                        expr: E::Call(id, _, a, _, _),
                        ..
                    } if *id == from && matches!(a.get(3), Some(E::Int(1))) => Some(*index),
                    _ => None,
                }),
                _ => None,
            })
            .expect("actual output1 publication");
        let last = f.body_stmts.len() - 1;
        assert!(matches!(&f.body_stmts[last],S::Assign{expr:E::Call(id,_,_,_,_),..}if *id==leave));
        let mut final_checks = rooted(0, input, 4);
        final_checks.extend(rooted(1, output, if self.error { 5 } else { 4 }));
        final_checks.push(check(eq(base(input), E::Var(pinned))));
        final_checks.push(check(E::Gt(Box::new(base(output)), Box::new(E::Int(0)))));
        // Read-only assertions here: no injected GC or managed publication in
        // the output/leave/dispatch/caller B1 handoff.
        f.body_stmts.splice(last..last, final_checks);
        ir.locals = next;
        eprintln!(
            "URI125_ROOTS {}",
            serde_json::json!({"source":source,"adapter":adapter,"number":number,"fixed":fixed,"kernel":kernel,"to":to,"from":from,"frame":frame,"input":input,"output":output,"initial_tag":self.tag,"private_entries":2,"input_entry":0,"output_entry":1,"matching_producer_empty_entry_guard":true,"other_producer_entry_fatal":true,"String_both_producers_fatal":self.tag==4,"allowed_gc_before_copy":true,"forced_gc_before_allocations":true,"no_postpublication_gc":true})
        );
        Ok(ir)
    }
}
fn observed(source: &str, tag: i64, expected: &str, abrupt: bool, error: bool) {
    let source = source.to_owned();
    let expected: Vec<u16> = expected.encode_utf16().collect();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move||{
        let observer=Box::leak(Box::new(PrimitiveObserver{tag,units:Some(expected.len()),abrupt,error}));
        let mut m=probe_vm(&source,observer,"uri125-observer");let status=m.run();
        eprintln!("URI125_ROOT_VM {}",serde_json::json!({"source":source,"status":format!("{status:?}"),"fuel":m.get_fuel(),"yield_reason":format!("{:?}",m.last_yield_reason()),"native":m.jit_native_exec_count(),"aot":m.aot_exec_count(),"stack":format!("{:?}",m.stack())}));
        assert_eq!(status.unwrap(),vm::VmStatus::Halted);assert_eq!((m.jit_native_exec_count(),m.aot_exec_count()),(0,0));
        let at=m.stack().iter().position(|v|*v==vm::Value::string("P7_OBSERVED")).unwrap();
        let vm::Value::Map(boxed)=&m.stack()[at+1]else{panic!("managed result")};assert_eq!(boxed.len(),5);
        for (k,v) in [("tag",vm::Value::Int(4)),("number",vm::Value::Float(0.0)),("flag",vm::Value::Bool(false))]{assert_eq!(boxed.get(&vm::Value::string(k)),Some(&v));}
        for k in ["ref_slot","generation"]{assert!(matches!(boxed.get(&vm::Value::string(k)),Some(vm::Value::Int(n))if *n>0));}
        assert_eq!(m.stack()[at+2],vm::Value::Float(expected.len() as f64));
        for(i,u)in expected.iter().enumerate(){assert_eq!(m.stack()[at+3+i],vm::Value::Float(f64::from(*u)));}
        reset(&mut m);oracle_units(&source,&expected);
    }).unwrap().join().unwrap();
}
#[test]
fn uri_primitive125_root_number() {
    observed(
        "function f(x){return encodeURIComponent(x);}f(1e21);",
        3,
        "1e%2B21",
        false,
        false,
    );
}
#[test]
fn uri_primitive125_root_boolean() {
    observed(
        "function f(x){return encodeURI(x);}f(false);",
        2,
        "false",
        false,
        false,
    );
}
#[test]
fn uri_primitive125_root_null() {
    observed("decodeURI(null);", 1, "null", false, false);
}
#[test]
fn uri_primitive125_root_missing() {
    observed("decodeURIComponent();", 0, "undefined", false, false);
}
#[test]
fn uri_primitive125_root_string() {
    observed(
        "function f(x){return encodeURI(x);}f('A B');",
        4,
        "A%20B",
        false,
        false,
    );
}
#[test]
fn uri_primitive125_root_error_finally() {
    observed(
        "function f(){try{decodeURI('%');}catch(e){return e.name;}finally{var o={x:'retained'};o.x;}}f();",
        4,
        "URIError",
        false,
        true,
    );
}
#[test]
fn uri_primitive125_throwing_extra_precedes_string_error() {
    observed(
        "var n=0;var o={get x(){'use strict';throw 'extra125';}};var r;try{decodeURI('%',o.x,(n=n+1));}catch(e){r=e;}finally{var retained={x:1};retained.x;}if(n!==0)throw 'order';r;",
        4,
        "extra125",
        true,
        false,
    );
}
#[test]
fn uri_primitive125_throwing_extra_precedes_object_guard() {
    observed(
        "var n=0;var o={get x(){'use strict';throw 'extra125';}};var r;try{encodeURIComponent({},o.x,(n=n+1));}catch(e){r=e;}finally{var retained={x:1};retained.x;}if(n!==0)throw 'order';r;",
        5,
        "extra125",
        true,
        false,
    );
}

#[test]
fn uri_primitive125_indices_and_combined_demand() {
    for source in [
        "1;",
        "encodeURI(1);",
        "'x'+true;decodeURI(null);",
        "var k=1.5;var o={'1.5':7};o[k];encodeURIComponent(false);",
        "function f(x){return decodeURIComponent(x);}f();",
    ] {
        let ir = parse(source).unwrap();
        let demand = source != "1;";
        let names = [
            "__compat_es5_uri_call",
            "__compat_es5_uri_kernel",
            "__compat_es5_number_string_managed",
            "__compat_es5_fixed_scalar_string_managed",
            "__compat_es5_managed_to_units",
            "__compat_es5_units_to_managed",
        ];
        for name in names {
            assert_eq!(
                ir.functions.iter().filter(|f| f.name == name).count(),
                usize::from(demand),
                "{source} {name}"
            );
        }
        let ids: std::collections::HashSet<_> = ir.functions.iter().map(|f| f.index).collect();
        assert_eq!(ids.len(), ir.functions.len());
        for f in ir.function_impls.values() {
            assert!(f.param_slots.iter().all(|s| usize::from(*s) < ir.locals));
        }
        if demand {
            let number = helper(&ir, names[2]);
            let fixed = helper(&ir, names[3]);
            let kernel = helper(&ir, names[1]);
            let adapter = helper(&ir, names[0]);
            let dispatch = helper(&ir, "__compat_es5_dispatch");
            assert!(number < kernel && fixed < kernel && kernel < adapter && adapter < dispatch);
            for (name, arity) in [
                (names[0], 3),
                (names[2], 4),
                (names[3], 4),
                ("__compat_es5_dispatch", 4),
            ] {
                assert_eq!(
                    ir.functions.iter().find(|f| f.name == name).unwrap().arity,
                    arity
                );
            }
            for f in ir
                .functions
                .iter()
                .filter(|f| f.name.starts_with("__compat_es5_fn_"))
            {
                assert_eq!(f.arity, 5);
            }
            eprintln!(
                "URI125_INDICES {}",
                serde_json::json!({"source":source,"number":number,"fixed":fixed,"kernel":kernel,"adapter":adapter,"dispatch":dispatch,"locals":ir.locals,"unique":ids.len()})
            );
        }
    }
}
