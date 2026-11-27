//! Public managed String length reads and ordinary Get fallback.

// The expanded realm and forced-GC probes can use over 20M fuel per run
// (20,970,818 measured). A finite cap at 4x the old 10M budget leaves
// roughly 2x headroom over that sample.
const PROBE_FUEL: u64 = 40_000_000;

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

fn execute(source: &str) -> vm::Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("LENGTH115_SOURCE {}", serde_json::json!({"source": source}));
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!("source={source:?} guest_invocations=0 {error:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(PROBE_FUEL);
            let status = machine.run();
            print_probe_fuel(&machine, PROBE_FUEL);
            eprintln!(
                "LENGTH115_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let result = machine.stack().last().unwrap().clone();
            reset(&mut machine);
            eprintln!(
                "LENGTH115_RESET source={source:?} pending=false resources=0 guest_invocations=1 native=0"
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

/// VM-only variant of `boxed` for expectations that intentionally diverge
/// from Node: no `oracle` call, so the bounded-domain result is authoritative.
fn vm_boxed(source: &str, tag: i64, number: f64, flag: bool) {
    let result = execute(source);
    let vm::Value::Map(map) = result else {
        panic!("full box: {result:?}");
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
}

fn oracle(source: &str, expected: serde_json::Value) {
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    let script = format!(
        "const source={};try{{const v=(0,eval)(source);console.log(JSON.stringify(v===undefined?{{kind:'undefined'}}:{{kind:typeof v,value:v,...(typeof v==='number'?{{negative_zero:Object.is(v,-0)}}:{{}})}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',value:String(e)}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "LENGTH115_NODE {}",
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

#[test]
fn string_length115_empty_public() {
    boxed("\"\".length;", 3, 0.0, false);
}

#[test]
fn string_length115_units() {
    for (source, n) in [
        (r#""ABCDEFGH".length;"#, 8.0),
        (r#""😀".length;"#, 2.0),
        (r#""a\0b"["length"];"#, 3.0),
        (r#""\uD800".length;"#, 1.0),
        (r#""\uDC00".length;"#, 1.0),
        (r#"(("ab"))["\u006cength"] + 2;"#, 4.0),
    ] {
        boxed(source, 3, n, false);
    }
}

#[test]
fn string_length115_binding_functions() {
    for (source, n) in [
        (
            r#"var s="abc";var alias=s;s="x";alias.length+s.length;"#,
            4.0,
        ),
        (
            r#"function f(){'use strict';var s='abcd';return s.length+1;}f();"#,
            5.0,
        ),
        (
            r#"'use strict';function f(){return 'abc'.length;}f();"#,
            3.0,
        ),
        (r#"function f(){return 'abc'.length;}f();"#, 3.0),
        (r#"(function(){return 'abc'.length;})();"#, 3.0),
        (r#"function id(s){return s;}id('abcd').length;"#, 4.0),
        (r#"function len(s){return s.length;}len('abc');"#, 3.0),
    ] {
        boxed(source, 3, n, false);
    }
}

#[test]
fn string_length115_uri_runtime() {
    for (source, n) in [
        (r#"decodeURIComponent('%F0%9F%98%80').length;"#, 2.0),
        (r#"encodeURIComponent('😀').length;"#, 12.0),
        (
            r#"function f(s){return decodeURIComponent(s).length;}f('%00x');"#,
            2.0,
        ),
        (r#"var s=decodeURIComponent('%41%42');s.length;"#, 2.0),
    ] {
        boxed(source, 3, n, false);
    }
}

#[test]
fn string_length115_fallback() {
    for source in [
        r#"var o={length:7};o.length===7;"#,
        r#"function f(a,b){}f.length===2;"#,
        r#"function f(o){return o.length;}f({length:7})===7;"#,
        r#"function f(o){return o.length;}f({length:'abc'})==='abc';"#,
        r#"function f(o){return o.length;}var o={get length(){'use strict';return this;}};f(o)===o;"#,
        r#"function f(o){return o.length;}var p={get length(){'use strict';return this;}};var o=Object.create(p);f(o)===o;"#,
    ] {
        boxed(source, 2, 0.0, true);
    }
}

#[test]
fn string_length115_once_abrupt() {
    for source in [
        r#"function f(o){o.n=o.n===0?1:99;return 'abc';}var o={n:0};var n=f(o).length;n===3&&o.n===1;"#,
        r#"var o={n:0,get s(){'use strict';this.n=this.n===0?1:99;return 'abc';}};var n=o.s.length;n===3&&o.n===1;"#,
        r#"function f(o){o.n=1;throw 'payload';}var o={n:0};var ok=false;try{f(o).length;o.n=99;}catch(e){ok=e==='payload';}finally{o.n=o.n===1?2:99;}ok&&o.n===2;"#,
        r#"function f(o){return o.length;}var n=0;var ok=false;try{f({get length(){'use strict';throw 'managed';}});}catch(e){ok=e==='managed';}finally{n=1;}ok&&n===1;"#,
        r#"function f(o){return o.length;}var ok=false;try{f(null);}catch(e){ok=typeof e==='object';}ok;"#,
        r#"function f(o){return o.length;}var ok=false;try{f();}catch(e){ok=typeof e==='object';}ok;"#,
    ] {
        boxed(source, 2, 0.0, true);
    }
}

fn refuse(source: &str, expected: &str) {
    let err = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source) {
        Ok(_) => panic!("unsupported source compiled: {source}"),
        Err(err) => format!("{err:?}"),
    };
    eprintln!(
        "LENGTH115_REFUSAL {}",
        serde_json::json!({"source":source,"diagnostic":err,"guest_invocations":0})
    );
    assert!(err.contains(expected), "{source}: {err}");
}

fn char_at_units(source: &str, expected: &[u16]) {
    let source = source.to_owned();
    let expected = expected.to_vec();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "LENGTH115_CHAR_AT_SOURCE {}",
                serde_json::json!({"source": source, "expected_units": expected})
            );
            let observer = Box::leak(Box::new(object_observer::Observer {
                units: Some(expected.len()),
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "LENGTH115_CHAR_AT_VM source={source:?} status={status:?} aot={} jit={} stack={:?}",
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
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
                assert!(matches!(
                    boxed.get(&vm::Value::string(key)),
                    Some(vm::Value::Int(n)) if *n > 0
                ));
            }
            assert_eq!(stack[at + 2], vm::Value::Float(expected.len() as f64));
            for (i, unit) in expected.iter().enumerate() {
                assert_eq!(stack[at + 3 + i], vm::Value::Float(f64::from(*unit)));
            }
            reset(&mut machine);
            eprintln!(
                "LENGTH115_CHAR_AT_RESET source={source:?} pending={} resources={} aot={} jit={}",
                machine.scope_reset_pending(),
                machine.host_context().resource_count(),
                machine.aot_exec_count(),
                machine.jit_native_exec_count()
            );
            char_at_units_oracle(&source, &expected);
        })
        .unwrap()
        .join()
        .unwrap();
}

fn char_at_units_oracle(source: &str, expected: &[u16]) {
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    let script = format!(
        "(()=>{{const source={};const result=(0,eval)(source);if(typeof result!=='string'){{throw new Error('typeof '+typeof result);}}const units=[];for(let i=0;i<result.length;i=i+1){{units.push(result.charCodeAt(i));}}console.log(JSON.stringify(units));}})();",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "LENGTH115_CHAR_AT_NODE {}",
        serde_json::json!({
            "source": source,
            "version_argv": ["timeout", "15", "node", "--version"],
            "version_rc": version.status.code(),
            "version": String::from_utf8_lossy(&version.stdout),
            "version_stderr": String::from_utf8_lossy(&version.stderr),
            "argv": ["timeout", "15", "node", "-e", &script],
            "rc": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        })
    );
    assert_eq!(version.status.code(), Some(0), "{source}");
    assert_eq!(output.status.code(), Some(0), "{source}");
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual, serde_json::to_value(expected).unwrap(), "{source}");
}

#[test]
fn string_length115_dynamic_length_key_is_undefined() {
    let prefix = "";
    let source = "function f(o){var k=o.length;var p={};return p[k];}f({length:1});";
    boxed(&format!("{prefix}{source}"), 0, 0.0, false);
}
#[test]
fn string_length115_refuse_capture() {
    let prefix = "";
    let source = "function f(){var s='abc';function g(){return s.length;}return g();}f();";
    refuse(&format!("{prefix}{source}"), "capture");
}
#[test]
fn string_length115_ok_length_dynamic_key_is_undefined() {
    let prefix = "'ok'.length;";
    let source = "function f(o){var k=o.length;var p={};return p[k];}f({length:1});";
    boxed(&format!("{prefix}{source}"), 0, 0.0, false);
}
#[test]
fn string_length115_refuse_ok_length_capture() {
    let prefix = "'ok'.length;";
    let source = "function f(){var s='abc';function g(){return s.length;}return g();}f();";
    refuse(&format!("{prefix}{source}"), "capture");
}
#[test]
fn string_length115_unknown_member_is_undefined() {
    boxed("'abc'.other;", 0, 0.0, false);
}
#[test]
fn string_length115_boundary_index() {
    let source = "'abc'[0];";
    let err = match parse(source) {
        Ok(_) => panic!("out of profile: {source}"),
        Err(e) => e,
    };
    eprintln!("LENGTH115_BOUNDARY source={source:?} error={err:?} guest_invocations=0");
}
#[test]
fn string_length115_char_at_zero_units() {
    char_at_units("'abc'.charAt(0);", &[97u16]);
}
#[test]
fn string_length115_boundary_length_set() {
    let source = "'abc'.length=1;";
    let err = match parse(source) {
        Ok(_) => panic!("out of profile: {source}"),
        Err(e) => e,
    };
    eprintln!("LENGTH115_BOUNDARY source={source:?} error={err:?} guest_invocations=0");
}
#[test]
fn string_length115_boundary_length_delete() {
    let source = "delete 'abc'.length;";
    let err = match parse(source) {
        Ok(_) => panic!("out of profile: {source}"),
        Err(e) => e,
    };
    eprintln!("LENGTH115_BOUNDARY source={source:?} error={err:?} guest_invocations=0");
}
#[test]
fn string_length115_length_call_is_runtime_type_error() {
    let source = "'abc'.length();";
    let admitted = parse(source);
    assert!(admitted.is_ok(), "{source} {admitted:?}");
    let caught =
        format!("var caught=false;try{{{source}}}catch(e){{caught=e.name==='TypeError';}}caught;");
    boxed(&caught, 2, 0.0, true);
}
#[test]
fn string_length115_boundary_string_ctor() {
    let source = "String('abc');";
    let admitted = parse(source);
    assert!(admitted.is_ok(), "{source} {admitted:?}");
    let caught =
        format!("var caught=false;try{{{source}}}catch(e){{caught=e.name==='TypeError';}}caught;");
    guest_flag(&caught);
    eprintln!("LENGTH115_BOUNDARY source={source:?} call=TypeError");
}
#[test]
fn string_length115_boxed_strict_length_octal_property() {
    boxed(
        "function f(){'use strict';return 'abc'.length;}f();var o={'\\141':7};o.a;",
        3,
        7.0,
        false,
    );
}
#[test]
fn string_length115_boxed_length_property_key() {
    boxed(
        "function f(){var k='abc'.length;var p={'3':7};return p[k];}f();",
        3,
        7.0,
        false,
    );
}
#[test]
fn micro_string_char_at_ascii_raw_units() {
    char_at_units(r#""abc".charAt(1);"#, &[98u16]);
}
#[test]
fn micro_string_char_at_positive_oob() {
    char_at_units(r#""abc".charAt(3);"#, &[]);
}
#[test]
fn micro_string_char_at_negative() {
    char_at_units(r#""abc".charAt(-1);"#, &[]);
}
#[test]
fn micro_string_char_at_empty_index_zero() {
    char_at_units(r#""".charAt(0);"#, &[]);
}
#[test]
fn micro_string_char_at_nul_unit() {
    char_at_units(r#""a\0b".charAt(1);"#, &[0u16]);
}
#[test]
fn micro_string_char_at_lone_high() {
    char_at_units(r#""\uD800".charAt(0);"#, &[55296u16]);
}
#[test]
fn micro_string_char_at_lone_low() {
    char_at_units(r#""\uDC00".charAt(0);"#, &[56320u16]);
}
#[test]
fn micro_string_char_at_astral_unit_0() {
    char_at_units(r#""\uD83D\uDE00".charAt(0);"#, &[55357u16]);
}
#[test]
fn micro_string_char_at_astral_unit_1() {
    char_at_units(r#""\uD83D\uDE00".charAt(1);"#, &[56832u16]);
}
#[test]
fn micro_string_char_at_az_omitted() {
    char_at_units(r#""AZ".charAt();"#, &[65u16]);
}
#[test]
fn micro_string_char_at_az_nan() {
    char_at_units(r#""AZ".charAt(0/0);"#, &[65u16]);
}
#[test]
fn micro_string_char_at_az_neg_zero() {
    char_at_units(r#""AZ".charAt(-0);"#, &[65u16]);
}
#[test]
fn micro_string_char_at_az_fraction() {
    char_at_units(r#""AZ".charAt(1.9);"#, &[90u16]);
}
#[test]
fn micro_string_char_at_az_true() {
    char_at_units(r#""AZ".charAt(true);"#, &[90u16]);
}
#[test]
fn micro_string_char_at_az_false() {
    char_at_units(r#""AZ".charAt(false);"#, &[65u16]);
}
#[test]
fn micro_string_char_at_az_null() {
    char_at_units(r#""AZ".charAt(null);"#, &[65u16]);
}
#[test]
fn micro_string_char_at_az_numeric_string() {
    char_at_units(r#""AZ".charAt("1.9");"#, &[90u16]);
}
#[test]
fn micro_string_char_at_nonliteral() {
    char_at_units(r#"var s="abc";s.charAt(1);"#, &[98u16]);
}
#[test]
fn micro_string_char_at_computed() {
    char_at_units(r#"var s="abc";s["charAt"](1);"#, &[98u16]);
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
fn string_length115_file_cookie() {
    const SOURCE: &str = "function $ERROR(m){throw m;}function len(s){return s.length;}if(len(decodeURIComponent('%F0%9F%98%80'))!==2){$ERROR('length');}\n;1;\n";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            let path =
                std::env::temp_dir().join(format!("length115-cookie-{}.js", std::process::id()));
            std::fs::write(&path, SOURCE).unwrap();
            let result =
                pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(
                    &path,
                )
                .unwrap();
            assert_eq!(result.cookie, 1);
            assert_eq!(
                (result.aot_exec_count, result.jit_native_exec_count),
                (0, 0)
            );
            assert!(
                result.certification_valid_before_reset && !result.certification_valid_after_reset
            );
            assert!(result.owned_arrays_retired);
            assert_eq!(result.resources_after_reset, 0);
            eprintln!("LENGTH115_FILE source={SOURCE:?} observation={result:?}");
        })
        .unwrap()
        .join()
        .unwrap();
    oracle(SOURCE, serde_json::json!({"kind":"number","value":1}));
}

#[test]
fn string_length115_staged_original_probes() {
    std::thread::Builder::new().stack_size(33_554_432).spawn(|| {
        let id="S8.4_A3";
            let body=include_str!("fixtures/test262117/S8.4_A3.js");
            let source=format!("function $ERROR(message) {{ 'use strict'; throw message; }}\n{body}\n;1;\n");
            let path=std::env::temp_dir().join(format!("length115-original-{id}.js"));
            std::fs::write(&path,source.as_bytes()).unwrap();
            let observation=pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(&path).unwrap();
            assert_eq!(observation.cookie,1);
            assert_eq!((observation.aot_exec_count,observation.jit_native_exec_count),(0,0));
            assert!(observation.certification_valid_before_reset&&!observation.certification_valid_after_reset);
            assert_eq!(observation.resources_after_reset,0);
            assert!(observation.owned_arrays_retired);
            eprintln!("LENGTH115_UPSTREAM {}",serde_json::json!({"id":id,"original_body":body,"adapted_source":source,"observation":format!("{observation:?}"),"guest_invocations":1,"prospective_upstream_id":"S8.4_A3","adapter_own_strict":true}));
            oracle(&source,serde_json::json!({"kind":"number","value":1}));
    }).unwrap().join().unwrap();
}

#[path = "javascript_es5/object_observer.rs"]
mod object_observer;

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
fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .index
}
fn check(e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Expr {
        expr: call(vm::BuiltinFunction::Assert.call_index(), vec![e]),
        line: 1,
    }
}
fn plus(a: vm::Expr, b: vm::Expr) -> vm::Expr {
    vm::Expr::Add(Box::new(a), Box::new(b))
}
fn eq(a: vm::Expr, b: vm::Expr) -> vm::Expr {
    vm::Expr::Eq(Box::new(num(a)), Box::new(num(b)))
}

struct LengthObserver {
    corruption: u8,
    units: Option<usize>,
    type_error: bool,
    force_gc_after_bootstrap: bool,
}

fn arm_gc_after_bootstrap(ir: &mut vm::FrontendIr, write: u16) {
    use vm::{Expr as E, Stmt as S, TypeSchema as T};
    let bootstrap = helper(ir, "__compat_es5_intrinsic_bootstrap");
    let mut sites = ir.stmts.iter().enumerate().filter_map(|(at, stmt)| {
        if let S::Let {
            declared_schema: Some(T::Null),
            expr: E::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == bootstrap
            && let [E::Var(carrier)] = args.as_slice()
        {
            Some((at, *carrier))
        } else {
            None
        }
    });
    let (at, carrier) = sites.next().expect("typed bootstrap call");
    assert!(sites.next().is_none(), "expected a single bootstrap call");
    let sink = u16::try_from(ir.locals).unwrap();
    ir.locals += 1;
    // Arm the next allocation only after bootstrap returns. The adapter
    // below separately collects while the converted receiver is live.
    ir.stmts.insert(
        at + 1,
        S::Let {
            index: sink,
            declared_schema: Some(T::Number),
            expr: call(write, vec![E::Var(carrier), E::Float(10.0), E::Float(32.0)]),
            line: 1,
        },
    );
}

const OLD_FUNCTION_HEADER: u8 = 8;
const OBJECT_BRAND_ASSERT_LINE: u32 = 2_000_001;

fn function_header_bases(stmts: &[vm::Stmt], begin: u16) -> Vec<u16> {
    use vm::{Expr as E, Stmt as S};
    let get = vm::BuiltinFunction::Get.call_index();
    let allocs: Vec<u16> = stmts
        .iter()
        .filter_map(|stmt| match stmt {
            S::Let {
                index,
                expr: E::Call(id, _, args, _, _),
                ..
            } if *id == begin && matches!(args.as_slice(), [_, _, E::Int(4)]) => Some(*index),
            _ => None,
        })
        .collect();
    stmts
        .iter()
        .filter_map(|stmt| match stmt {
            S::Let {
                index,
                expr: E::Call(id, _, args, _, _),
                ..
            } if *id == get && matches!(args.as_slice(), [E::Var(alloc), E::String(key)] if key == "base" && allocs.contains(alloc)) => {
                Some(*index)
            }
            _ => None,
        })
        .collect()
}

fn corrupt_function_header(expr: &mut vm::Expr, write: u16, base_slot: u16) -> bool {
    use vm::Expr as E;
    let E::Call(id, _, args, _, _) = expr else {
        return false;
    };
    if *id != write {
        return false;
    }
    let [E::Var(_), E::Div(address, _), E::Div(payload, _)] = args.as_mut_slice() else {
        return false;
    };
    let E::Add(base, offset) = address.as_ref() else {
        return false;
    };
    if !matches!((base.as_ref(), offset.as_ref()), (E::Var(slot), E::Int(2)) if *slot == base_slot)
        || !matches!(payload.as_ref(), E::Int(9))
    {
        return false;
    }
    // One own property and extensible under the old 4+1 layout.
    **payload = E::Int(5);
    true
}

/// Corrupt the packed header of a source function. Kind is an argument of
/// `node_begin`, not a cell write, and a bare `Int(9)` at cell 2 is also an
/// environment count or a one-property object header.
fn inject_old_function_header(stmts: &mut [vm::Stmt], begin: u16, write: u16) -> usize {
    use vm::Stmt as S;
    let bases = function_header_bases(stmts, begin);
    let mut changed = 0;
    for stmt in stmts.iter_mut() {
        match stmt {
            S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                for &base in &bases {
                    changed += usize::from(corrupt_function_header(expr, write, base));
                }
                changed += inject_header_expr(expr, begin, write);
            }
            S::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                changed += inject_old_function_header(then_branch, begin, write);
                changed += inject_old_function_header(else_branch, begin, write);
            }
            S::While { body, .. } => changed += inject_old_function_header(body, begin, write),
            _ => {}
        }
    }
    changed
}

fn inject_header_expr(expr: &mut vm::Expr, begin: u16, write: u16) -> usize {
    use vm::Expr as E;
    match expr {
        E::Block { stmts, expr } => {
            inject_old_function_header(stmts, begin, write) + inject_header_expr(expr, begin, write)
        }
        E::Call(_, _, args, _, _) => args
            .iter_mut()
            .map(|arg| inject_header_expr(arg, begin, write))
            .sum(),
        E::ResultConstruct { payload, .. } => inject_header_expr(payload, begin, write),
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            inject_header_expr(condition, begin, write)
                + inject_header_expr(then_expr, begin, write)
                + inject_header_expr(else_expr, begin, write)
        }
        _ => 0,
    }
}

fn label_object_brand_assert(stmts: &mut [vm::Stmt]) -> usize {
    use vm::{Expr as E, Stmt as S};
    stmts.iter_mut().map(|stmt| match stmt {
        S::Expr { expr: E::Call(id, _, args, _, _), line }
            if *id == vm::BuiltinFunction::Assert.call_index() => {
                // Identify `!(kind == FUNCTION && error) && (!instance || error)`.
                if let [E::And(not_function_error, valid_instance)] = args.as_slice()
                    && let E::IfElse { condition, then_expr, else_expr } = not_function_error.as_ref()
                    && matches!((then_expr.as_ref(), else_expr.as_ref()), (E::Bool(false), E::Bool(true)))
                    && let E::And(is_function, error) = condition.as_ref()
                    && let E::Eq(_, kind) = is_function.as_ref()
                    && matches!(kind.as_ref(), E::Int(4))
                    && let E::Or(_, legacy_error) = valid_instance.as_ref()
                    && matches!((error.as_ref(), legacy_error.as_ref()), (E::Var(a), E::Var(b)) if a == b)
                {
                    *line = OBJECT_BRAND_ASSERT_LINE;
                    1
                } else {
                    0
                }
            }
        S::IfElse { then_branch, else_branch, .. } => {
            label_object_brand_assert(then_branch) + label_object_brand_assert(else_branch)
        }
        S::While { body, .. } => label_object_brand_assert(body),
        _ => 0,
    }).sum()
}

impl vm::SourcePlugin for LengthObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("length115-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let observer = object_observer::Observer {
            force_gc: !self.force_gc_after_bootstrap,
            units: self.units,
            type_error: self.type_error,
            ..Default::default()
        };
        let mut ir = vm::SourcePlugin::parse_source(&observer, source, context)?;
        let id = helper(&ir, "__compat_es5_string_length_get");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let read = helper(&ir, "__compat_ref_read");
        let write = helper(&ir, "__compat_ref_write");
        let begin = helper(&ir, "__compat_es5_node_begin");
        if self.force_gc_after_bootstrap {
            arm_gc_after_bootstrap(&mut ir, write);
        }
        if self.corruption == OLD_FUNCTION_HEADER {
            let in_script = inject_old_function_header(&mut ir.stmts, begin, write);
            let in_helpers = ir.function_impls.values_mut().map(|implementation| {
                inject_old_function_header(&mut implementation.body_stmts, begin, write)
            }).sum::<usize>();
            assert_eq!(in_script, 1, "expected the one source function header write");
            assert_eq!(in_helpers, 0, "prelude helpers must not use the constant function header");
            let collector = ir.function_impls.get_mut(&collect).unwrap();
            // The collector inlines the same brand check into more than one scan arm.
            assert!(
                label_object_brand_assert(&mut collector.body_stmts) >= 1,
                "collector is missing the object brand assert"
            );
        }
        let checked = helper(&ir, "__compat_es5_checked_base");
        let decode = helper(&ir, "__compat_es5_decode_int");
        let f = ir.function_impls.get_mut(&id).unwrap();
        let (c, r) = (f.param_slots[0], f.param_slots[1]);
        let first = u16::try_from(ir.locals).unwrap();
        ir.locals += 11;
        let (b, base, key, frame, cap, i, row, base_found, key_found, before, ack) = (
            first,
            first + 1,
            first + 2,
            first + 3,
            first + 4,
            first + 5,
            first + 6,
            first + 7,
            first + 8,
            first + 9,
            first + 10,
        );
        let rd = |address: E| num(call(read, vec![E::Var(c), num(address)]));
        let dec = |address: E| call(decode, vec![rd(address)]);
        let let_ = |index, schema, expr| S::Let {
            index,
            declared_schema: Some(schema),
            expr,
            line: 1,
        };
        let set = |index, expr| S::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line: 1,
        };
        let matches = |slot: E, generation: E| {
            E::And(
                Box::new(eq(rd(plus(E::Var(row), E::Int(1))), slot)),
                Box::new(eq(rd(plus(E::Var(row), E::Int(2))), generation)),
            )
        };
        let mut body = vec![
            let_(
                b,
                T::Named("__CompatEs5Primitive".into(), vec![]),
                get(r, "base"),
            ),
            let_(
                base,
                T::Int,
                call(
                    checked,
                    vec![E::Var(c), get(b, "ref_slot"), get(b, "generation")],
                ),
            ),
            let_(
                key,
                T::Int,
                call(
                    checked,
                    vec![E::Var(c), get(r, "key_slot"), get(r, "key_generation")],
                ),
            ),
            let_(frame, T::Int, dec(E::Int(3))),
            check(E::Gt(Box::new(E::Var(frame)), Box::new(E::Int(0)))),
            let_(cap, T::Int, dec(plus(E::Var(frame), E::Int(3)))),
            let_(i, T::Int, E::Int(0)),
            let_(row, T::Int, E::Int(0)),
            let_(base_found, T::Bool, E::Bool(false)),
            let_(key_found, T::Bool, E::Bool(false)),
            S::While {
                condition: E::Lt(Box::new(E::Var(i)), Box::new(E::Var(cap))),
                body: vec![
                    set(
                        row,
                        plus(
                            plus(E::Var(frame), E::Int(4)),
                            E::Mul(Box::new(E::Var(i)), Box::new(E::Int(3))),
                        ),
                    ),
                    set(
                        base_found,
                        E::Or(
                            Box::new(E::Var(base_found)),
                            Box::new(E::And(
                                Box::new(eq(rd(E::Var(row)), get(b, "tag"))),
                                Box::new(matches(get(b, "ref_slot"), get(b, "generation"))),
                            )),
                        ),
                    ),
                    set(
                        key_found,
                        E::Or(
                            Box::new(E::Var(key_found)),
                            Box::new(E::And(
                                Box::new(eq(rd(E::Var(row)), E::Int(4))),
                                Box::new(matches(get(r, "key_slot"), get(r, "key_generation"))),
                            )),
                        ),
                    ),
                    set(i, plus(E::Var(i), E::Int(1))),
                ],
                line: 1,
            },
            check(E::Var(base_found)),
            check(E::Var(key_found)),
            let_(before, T::Int, dec(E::Int(11))),
            let_(ack, T::Int, call(collect, vec![E::Var(c)])),
            check(eq(dec(E::Int(11)), plus(E::Var(before), E::Int(1)))),
            check(eq(
                call(
                    checked,
                    vec![E::Var(c), get(b, "ref_slot"), get(b, "generation")],
                ),
                E::Var(base),
            )),
            check(eq(
                call(
                    checked,
                    vec![E::Var(c), get(r, "key_slot"), get(r, "key_generation")],
                ),
                E::Var(key),
            )),
            check(eq(dec(plus(E::Var(key), E::Int(2))), E::Int(6))),
        ];
        for (offset, unit) in "length".encode_utf16().enumerate() {
            body.push(check(eq(
                dec(plus(E::Var(key), E::Int(3 + offset as i64))),
                E::Int(i64::from(unit)),
            )));
        }
        body.push(S::IfElse {
            condition: eq(get(b, "tag"), E::Int(4)),
            then_branch: vec![
                check(eq(dec(plus(E::Var(base), E::Int(2))), E::Int(2))),
                check(eq(dec(plus(E::Var(base), E::Int(3))), E::Int(65))),
                check(eq(dec(plus(E::Var(base), E::Int(4))), E::Int(66))),
            ],
            else_branch: vec![],
            line: 1,
        });
        // Metadata controls run after the real roots/generations have been qualified.
        let mutation = match self.corruption {
            1 => Some((plus(E::Var(base), E::Int(0)), E::Float(3.0))),
            2 => Some((
                E::Sub(Box::new(E::Var(base)), Box::new(E::Int(3))),
                E::Float(3.0),
            )),
            3 => Some((plus(E::Var(key), E::Int(3)), E::Float(120.0))),
            4 => Some((plus(E::Var(base), E::Int(2)), E::Float(0.5))),
            5 => Some((plus(E::Var(base), E::Int(2)), E::Float(1.0))),
            _ => None,
        };
        if let Some((address, payload)) = mutation {
            body.push(S::Expr {
                expr: call(write, vec![E::Var(c), num(address), payload]),
                line: 1,
            });
        }
        if self.corruption == 6 {
            let bad = call(
                vm::BuiltinFunction::Set.call_index(),
                vec![
                    E::Var(b),
                    E::String("generation".into()),
                    plus(get(b, "generation"), E::Int(1)),
                ],
            );
            body.push(set(
                r,
                call(
                    vm::BuiltinFunction::Set.call_index(),
                    vec![E::Var(r), E::String("base".into()), bad],
                ),
            ));
        }
        if self.corruption == 7 {
            body.push(set(
                r,
                call(
                    vm::BuiltinFunction::Set.call_index(),
                    vec![
                        E::Var(r),
                        E::String("key_generation".into()),
                        plus(get(r, "key_generation"), E::Int(1)),
                    ],
                ),
            ));
        }
        f.body_stmts.splice(0..0, body);
        eprintln!(
            "LENGTH115_ADAPTER_ROOT source={source:?} adapter={id} carrier={c} reference={r} corruption={} active_frame_base_key_rows=true collection_before_Result=true",
            self.corruption
        );
        eprintln!(
            "LENGTH115_IR_HELPERS {:?}",
            ir.functions
                .iter()
                .map(|decl| (decl.index, decl.name.as_str()))
                .collect::<Vec<_>>()
        );
        Ok(ir)
    }
}
/// Forces a real collection at entry to the UTF-16 bridge while a trim
/// receiver's coerced text is supposed to be rooted at frame entry 2.
struct TrimGcObserver;
impl vm::SourcePlugin for TrimGcObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("trim169-gc-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        // A wide `force_gc` patch writes a Number into the realm bootstrap
        // node header and makes bootstrap's own collection fail before any
        // trim call (the independent non-trim control fails identically).
        // This witness only forces the one collection it measures, at entry
        // to the UTF-16 bridge after the receiver text is rooted.
        let observer = object_observer::Observer {
            force_gc: false,
            units: Some(2),
            type_error: false,
            ..Default::default()
        };
        let mut ir = vm::SourcePlugin::parse_source(&observer, source, context)?;
        let to_units = helper(&ir, "__compat_es5_managed_to_units");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let observe = helper(&ir, "__compat_es5_heap_observe");
        let before = u16::try_from(ir.locals).expect("trim gc collection slot");
        ir.locals += 1;
        let function = ir.function_impls.get_mut(&to_units).unwrap();
        let carrier = function.param_slots[0];
        function.body_stmts.splice(
            0..0,
            [
                S::Let {
                    index: before,
                    declared_schema: Some(T::Int),
                    // Selector 5 is OBSERVE_COLLECTIONS; arg 0 is unused.
                    expr: call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]),
                    line: 1,
                },
                S::Expr {
                    expr: call(collect, vec![E::Var(carrier)]),
                    line: 1,
                },
                check(eq(
                    call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]),
                    plus(E::Var(before), E::Int(1)),
                )),
            ],
        );
        Ok(ir)
    }
}
fn namespaced_capability_catalog() -> std::collections::BTreeMap<u16, String> {
    let mut catalog = std::collections::BTreeMap::new();
    for namespace in vm::builtin_namespace_specs() {
        for member in namespace.members {
            let builtin = vm::resolve_builtin_namespace_call(namespace.namespace, member.name)
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
}

fn program_required_capabilities(
    program: &vm::Program,
    namespaced: &std::collections::BTreeMap<u16, String>,
) -> Vec<u16> {
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
}

fn capability_refs(
    indices: &[u16],
    namespaced: &std::collections::BTreeMap<u16, String>,
) -> Vec<serde_json::Value> {
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
}

fn probe_vm(source: &str, observer: &'static dyn vm::SourcePlugin, id: &'static str) -> vm::Vm {
    probe_vm_with_fuel(source, observer, id, PROBE_FUEL)
}

fn probe_vm_with_fuel(
    source: &str,
    observer: &'static dyn vm::SourcePlugin,
    id: &'static str,
    fuel: u64,
) -> vm::Vm {
    probe_vm_with_array_module(
        source,
        observer,
        id,
        fuel,
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
    )
}

fn probe_vm_with_array_module(
    source: &str,
    observer: &'static dyn vm::SourcePlugin,
    id: &'static str,
    fuel: u64,
    arrays: vm::HostModuleDescriptor,
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
        arrays,
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
    }
    const GRANTS: &[&str] = &[
        "math::floor",
        "math::copysign",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
        "bytes::from_utf8",
    ];
    for &name in GRANTS {
        registry.allow_builtin(name).unwrap();
    }
    let namespaced = namespaced_capability_catalog();
    let required = program_required_capabilities(machine.program(), &namespaced);
    let granted: Vec<u16> = GRANTS
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
    let canonical_trunc = vm::BuiltinFunction::from_namespaced_name("math::trunc")
        .expect("canonical math::trunc must resolve in the builtin catalog");
    assert_eq!(canonical_trunc, vm::BuiltinFunction::MathTrunc);
    eprintln!(
        "LENGTH115_GC_CAPABILITIES {}",
        serde_json::json!({
            "fixture": id,
            "required": capability_refs(&required, &namespaced),
            "granted": capability_refs(&granted, &namespaced),
            "missing": capability_refs(&missing, &namespaced),
            "canonical_trunc": {
                "index": canonical_trunc.call_index(),
                "hex": format!("0x{:04X}", canonical_trunc.call_index()),
                "canonical": "math::trunc",
            },
        })
    );
    if missing.len() == 1 && missing[0] == canonical_trunc.call_index() {
        registry.allow_builtin("math::trunc").unwrap();
    } else {
        assert!(
            missing.is_empty(),
            "unexpected explicit host capability gap: {}",
            serde_json::json!({
                "missing": capability_refs(&missing, &namespaced),
            })
        );
    }
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(fuel);
    machine
}

#[test]
fn array_buffer176_constructor_shape() {
    guest_flag(r#"typeof ArrayBuffer === "function" && ArrayBuffer.length === 1;"#);
}

#[test]
fn array_buffer176_getter_is_inherited() {
    guest_flag(
        r#"var b = new ArrayBuffer(16); b.byteLength === 16 && b["byteLength"] === 16 && !b.hasOwnProperty("byteLength");"#,
    );
}

#[test]
fn array_buffer176_object_keys() {
    guest_flag("var keys = Object.keys(new ArrayBuffer(8)); Array.isArray(keys) && keys.length === 0;");
    guest_flag("var id = function(x) { return x; }; var keys = Object.keys(id(new ArrayBuffer(8))); Array.isArray(keys) && keys.length === 0;");
}

#[test]
fn array_buffer176_identity() {
    guest_flag(
        "var id = function(x) { return x; }; var a = new ArrayBuffer(4); var b = new ArrayBuffer(4); id(a).byteLength === 4 && a !== b;",
    );
}

#[test]
fn array_buffer176_default_length() {
    guest_flag(
        "var a = new ArrayBuffer(); var b = new ArrayBuffer(undefined); a.byteLength === 0 && b.byteLength === 0;",
    );
}

#[test]
fn array_buffer176_length_conversion() {
    guest_flag(
        r#"var a = new ArrayBuffer(3.9); var b = new ArrayBuffer(-0.9); var c = new ArrayBuffer("5"); a.byteLength === 3 && b.byteLength === 0 && c.byteLength === 5;"#,
    );
    guest_flag("new ArrayBuffer(0 / 0).byteLength === 0;");
}

#[test]
fn array_buffer176_quota_error() {
    // A valid Micro length above the local cap aborts before guest catch.
    let over_cap =
        "var caught = false; try { new ArrayBuffer(32769); } catch (e) { caught = true; } caught;";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(over_cap)
                    .unwrap();
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(40_000_000);
            let error = machine.run().expect_err("the byte quota is a host failure");
            assert!(
                matches!(&error, vm::VmError::HostError(message)
                    if message == "array buffer length exceeds host quota"),
                "{error:?}"
            );
            eprintln!("new ArrayBuffer(32769): {error:?}");
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn array_buffer176_quota_error_releases_root_frame() {
    use vm::{Expr as E, Stmt as S, Value};

    const ID: &str = "array-buffer176-quota-root-observer";
    const ENTRY: &str = "ArrayBuffer entry root capacity";
    const ACTIVE_ROOT_CAPACITY: i64 = 4; // __compat_es5_heap_observe selector
    const HDR_ACTIVE_ROOT_CAPACITY: i64 = 9; // raw realm header offset

    struct QuotaRootObserver;
    impl vm::SourcePlugin for QuotaRootObserver {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            let mut descriptor =
                vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
            descriptor.id = vm::FrontendId::new(ID);
            descriptor
        }

        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let mut ir = vm::SourcePlugin::parse_source(
                &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
                source,
                context,
            )?;
            let observe = helper(&ir, "__compat_es5_heap_observe");
            let buffer = helper(&ir, "__compat_es5_array_buffer");
            let implementation = ir.function_impls.get_mut(&buffer).unwrap();
            let carrier = implementation.param_slots[0];
            implementation.body_stmts.splice(
                0..0,
                [
                    S::Expr {
                        expr: E::String(ENTRY.into()),
                        line: 1,
                    },
                    S::Expr {
                        expr: call(
                            observe,
                            vec![E::Var(carrier), E::Int(ACTIVE_ROOT_CAPACITY), E::Int(0)],
                        ),
                        line: 1,
                    },
                ],
            );
            Ok(ir)
        }
    }

    thread_local! {
        static BACKING: std::cell::RefCell<Option<Value>> = const { std::cell::RefCell::new(None) };
    }
    static READ: std::sync::OnceLock<vm::StaticHostFunction> = std::sync::OnceLock::new();
    fn observe_read(machine: &mut vm::Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        BACKING.with(|slot| *slot.borrow_mut() = Some(args[0].clone()));
        READ.get().unwrap()(machine, args)
    }

    fn observed_read_descriptor() -> vm::HostFunctionDescriptor {
        use vm::HostAdapterDescriptor::*;
        let mut read = pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
            .descriptors()
            .into_iter()
            .find(|descriptor| descriptor.schema.name == "arrays::read")
            .unwrap();
        let (original, wrapper) = match read.adapter {
            Static(f) => (f, Static(observe_read)),
            StaticStack(f) => (f, StaticStack(observe_read)),
            StaticStackRuntimeOwned(f) => (f, StaticStackRuntimeOwned(observe_read)),
            _ => panic!("arrays::read must have a VM-aware static adapter"),
        };
        READ.get_or_init(|| original);
        read.adapter = wrapper;
        read
    }

    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            static OBS: QuotaRootObserver = QuotaRootObserver;
            type Descriptor = fn() -> vm::HostFunctionDescriptor;
            static FUNCTIONS: std::sync::OnceLock<Vec<Descriptor>> = std::sync::OnceLock::new();
            let mut arrays = pd_vm_compat_frontends::runtime::array::typescript_array_host_module();
            arrays.functions = FUNCTIONS.get_or_init(|| {
                arrays
                    .functions
                    .iter()
                    .map(|&descriptor| {
                        if descriptor().schema.name == "arrays::read" {
                            observed_read_descriptor as Descriptor
                        } else {
                            descriptor
                        }
                    })
                    .collect()
            });
            let mut machine =
                probe_vm_with_array_module("new ArrayBuffer(32769);", &OBS, ID, PROBE_FUEL, arrays);

            let error = machine.run().expect_err("the byte quota is a host failure");
            assert!(
                matches!(&error, vm::VmError::HostError(message)
                    if message == "array buffer length exceeds host quota"),
                "{error:?}"
            );
            let entry = machine
                .stack()
                .iter()
                .rposition(|value| *value == Value::string(ENTRY))
                .expect("ArrayBuffer entry observation");
            let Value::Int(before) = machine.stack()[entry + 1] else {
                panic!("missing entry root capacity");
            };
            // Read the same live heap after the fatal error, before reset can hide a leak.
            let backing = BACKING.with(|slot| slot.borrow_mut().take()).unwrap();
            let after = match READ.get().unwrap()(
                &mut machine,
                &[backing, Value::Int(HDR_ACTIVE_ROOT_CAPACITY)],
            )
            .expect("read the live root capacity header")
            {
                vm::CallOutcome::Return(vm::CallReturn::One(Value::Int(value))) => value,
                vm::CallOutcome::Return(vm::CallReturn::One(Value::Float(value))) => value as i64,
                _ => panic!("expected a numeric root capacity"),
            };
            eprintln!("ArrayBuffer quota: active root capacity before={before}, after={after}");
            assert_eq!(after, before, "quota failure leaked active root capacity");
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn array_buffer176_range_error() {
    guest_flag("new ArrayBuffer(32768).byteLength === 32768;");
    eprintln!("new ArrayBuffer(32768).byteLength === 32768: true");
    for length in ["1073741824", "-1", "Infinity", "-Infinity"] {
        guest_flag(&format!(
            r#"var id = function(x) {{ return x; }}; var ok = false; try {{ new ArrayBuffer(id({length})); }} catch (e) {{ ok = e.name === "RangeError" && e.message === "invalid array index"; }} ok;"#
        ));
    }
}

#[test]
fn array_buffer176_without_new() {
    guest_flag(
        r#"var n = 0; var ok = false; try { ArrayBuffer(n = 1); } catch (e) { ok = e.name === "TypeError" && e.message === "must be called with new"; } ok && n === 1;"#,
    );
}

#[test]
fn array_buffer176_getter_checks_brand() {
    guest_flag(
        r#"var id = function(x) { return x; }; var ok = false; try { id(ArrayBuffer.prototype).byteLength; } catch (e) { ok = e.name === "TypeError" && e.message === "expected an ArrayBuffer"; } ok;"#,
    );
    guest_flag(
        r#"var ok = false; try { Object.create(ArrayBuffer.prototype).byteLength; } catch (e) { ok = e.name === "TypeError"; } ok;"#,
    );
}

#[test]
fn array_buffer176_abrupt_conversion_order() {
    guest_flag(
        r#"var n = 0; var ok = false; var o = {valueOf: function() { "use strict"; throw "length"; }}; try { new ArrayBuffer(o, (n = 1)); } catch (e) { ok = e === "length"; } ok && n === 1;"#,
    );
}

#[test]
fn array_buffer176_gc() {
    // The observer below also verifies a real collection and physical zeros.
    array_buffer176_gc_probe();
}

#[test]
fn array_buffer176_no_slice() {
    guest_flag(r#"var b = new ArrayBuffer(4); typeof b.slice === "undefined";"#);
    guest_flag(
        r#"var b = new ArrayBuffer(4); var n = 0; try { b["0"]; } catch (e) { if (e.name === "TypeError") n = n + 1; } try { b["0"] = 9; } catch (e) { if (e.name === "TypeError") n = n + 1; } n === 2;"#,
    );
}

#[test]
fn array_buffer176_views_refused() {
    assert!(parse("new ArrayBuffer(4).byteLength;").is_ok());
    // DataView stays outside the slice. Typed-array aliases are plain builtin
    // aliases and now follow the admitted buffer-view construction.
    let Err(error) = parse("var b = new ArrayBuffer(4); new DataView(b);") else {
        panic!("unexpected admission: DataView")
    };
    assert!(format!("{error:?}").contains("unsupported JavaScript ES5"));
    for source in [
        "var b = new ArrayBuffer(4); var C = Float32Array; new C(b);",
        "var b = new ArrayBuffer(8); var C = Float64Array; new C(b);",
        "var b = new ArrayBuffer(4); var C = Uint8Array; new C(b);",
        "var b = new ArrayBuffer(4); var C = Int16Array; new C(b);",
    ] {
        parse(source)
            .unwrap_or_else(|error| panic!("typed-array alias must be admitted: {source}\n{error:?}"));
    }
}

#[test]
fn typed_array_integer_names_admitted() {
    let source = "var b = new ArrayBuffer(8); new Int8Array(b); new Uint8ClampedArray(b); new Int16Array(b); new Uint16Array(b); new Int32Array(b); new Uint32Array(b);";
    parse(source).unwrap_or_else(|error| panic!("integer buffer windows were rejected: {error:?}"));
}

fn integer_view_windows(name: &str, width: u32, conversions: &str) {
    let source = format!(r#"
        var b = new ArrayBuffer(12);
        var all = new {name}(b);
        var tail = new {name}(b, {width}, undefined);
        var v = new {name}(b, {width}, 2);
        var empty = new {name}(b, 12, 0);
        var p = {name}.prototype;
        if ({name}.BYTES_PER_ELEMENT !== {width} || p.BYTES_PER_ELEMENT !== {width}) throw "width";
        if (Object.getPrototypeOf(v) !== p || p.constructor !== {name}) throw "prototype";
        if (all.length !== {total} || all.byteLength !== 12 || all.byteOffset !== 0) throw "whole";
        if (tail.length !== {tail} || tail.byteOffset !== {width} || tail.buffer !== b) throw "tail";
        if (v.length !== 2 || v.byteLength !== {window} || v.byteOffset !== {width}) throw "window";
        if (empty.length !== 0 || empty.byteOffset !== 12 || v[2] !== undefined) throw "bounds";
        {alignment}
        {conversions}
        true;
    "#,
        total = 12 / width,
        tail = 12 / width - 1,
        window = 2 * width,
        alignment = if width == 1 {
            r#"
            var odd = new ArrayBuffer(3);
            if (new VIEW(odd, 1).length !== 2) throw "byte alignment and divisibility";
            "#.replace("VIEW", name)
        } else {
            r#"
            var errors = 0;
            try { new VIEW(b, 1, 0); } catch (e) {
                if (e.name === "RangeError" && e.message === "invalid offset") errors = errors + 1;
            }
            var odd = new ArrayBuffer(9);
            try { new VIEW(odd); } catch (f) {
                if (f.name === "RangeError" && f.message === "invalid length") errors = errors + 1;
            }
            if (errors !== 2 || new VIEW(odd, 0, 1).length !== 1) throw "alignment";
            "#.replace("VIEW", name)
        },
    );
    guest_flag(&source);
}

#[test]
fn typed_array_integer_int8() {
    integer_view_windows("Int8Array", 1, r#"
        v[0] = 256;
        if (v[0] !== 0) throw "256 to zero";
        v[0] = "-1";
        if (v[0] !== -1) throw "negative one";
        v[1] = 128.9;
        if (v[1] !== -128) throw "sign bit";
    "#);
}

#[test]
fn typed_array_integer_clamped() {
    integer_view_windows("Uint8ClampedArray", 1, r#"
        v[0] = 0.5;
        v[1] = 1.5;
        if (v[0] !== 0 || v[1] !== 2) throw "ties to even";
        v[0] = -1;
        v[1] = 300;
        if (v[0] !== 0 || v[1] !== 255) throw "saturate";
        v[0] = 2.5;
        v[1] = 0 / 0;
        if (v[0] !== 2 || v[1] !== 0) throw "tie and NaN";
    "#);
}

#[test]
fn typed_array_integer_int16() {
    integer_view_windows("Int16Array", 2, r#"
        v[0] = 65536.75;
        v[1] = 32768;
        if (v[0] !== 0 || v[1] !== -32768) throw "wrap and sign bit";
        v[0] = -1;
        if (v[0] !== -1) throw "negative one";
    "#);
}

#[test]
fn typed_array_integer_uint16() {
    integer_view_windows("Uint16Array", 2, r#"
        v[0] = -1;
        v[1] = 65537;
        if (v[0] !== 65535 || v[1] !== 1) throw "unsigned wrap";
        v[0] = 1 / 0;
        if (v[0] !== 0) throw "infinity";
    "#);
}

#[test]
fn typed_array_integer_int32() {
    integer_view_windows("Int32Array", 4, r#"
        v[0] = 4294967296;
        v[1] = 2147483648;
        if (v[0] !== 0 || v[1] !== -2147483648) throw "wrap and sign bit";
        v[0] = -1;
        if (v[0] !== -1) throw "negative one";
    "#);
}

#[test]
fn typed_array_integer_uint32() {
    integer_view_windows("Uint32Array", 4, r#"
        v[0] = -1.9;
        v[1] = 4294967297;
        if (v[0] !== 4294967295 || v[1] !== 1) throw "unsigned wrap";
        v[0] = 4294967296;
        if (v[0] !== 0) throw "modulus";
    "#);
}

#[test]
fn typed_array_integer_shared_backing() {
    guest_flag(r#"
        var b = new ArrayBuffer(4);
        var bytes = new Uint8Array(b, 0, 4);
        var signed = new Int32Array(b, 0, 1);
        var unsigned = new Uint32Array(b);
        if (Object.getPrototypeOf(Int32Array) !== Object.getPrototypeOf(Uint8Array)) throw "abstract constructor";
        if (Object.getPrototypeOf(Int32Array.prototype) !== Object.getPrototypeOf(Uint32Array.prototype)) throw "shared getters";
        if (signed.buffer !== bytes.buffer || signed.buffer !== unsigned.buffer) throw "different backings";
        signed[0] = 16909060;
        if (bytes[0] !== 4 || bytes[1] !== 3 || bytes[2] !== 2 || bytes[3] !== 1) throw "byte order";
        bytes[3] = -1;
        if (bytes[3] !== 255 || signed[0] !== -16645372 || unsigned[0] !== 4278321924) throw "byte alias";
        unsigned[0] = 4294967295;
        if (signed[0] !== -1 || bytes[1] !== 255) throw "word alias";
        true;
    "#);
}

#[test]
fn typed_array_integer_conversion_order() {
    guest_flag(r#"
        var b = new ArrayBuffer(8);
        var v = new Int16Array(b);
        var offset = {n: 0, valueOf: function(){"use strict"; this.n = this.n + 1; return 1;}};
        var count = {n: 0, valueOf: function(){"use strict"; this.n = this.n + 1; return 5;}};
        var errors = 0;
        try { new Int16Array(b, offset, count); } catch (a) {
            if (a.name === "RangeError" && a.message === "invalid offset") errors = errors + 1;
        }
        if (errors !== 1 || offset.n !== 1 || count.n !== 0) throw "offset first";
        try { new Int16Array(b, 2, count); } catch (c) {
            if (c.name === "RangeError" && c.message === "invalid length") errors = errors + 1;
        }
        if (errors !== 2 || count.n !== 1) throw "length second";
        try { new Int16Array(b, 1073741824); } catch (d) {
            if (d.name === "RangeError" && d.message === "invalid array index") errors = errors + 1;
        }
        try { new Int16Array(b, 0, -1); } catch (e) {
            if (e.name === "RangeError" && e.message === "invalid array index") errors = errors + 1;
        }
        var rhs = {n: 0, valueOf: function(){"use strict"; this.n = this.n + 1; return -1;}};
        try { v[4] = rhs; } catch (f) {
            if (f.name === "TypeError" && f.message === "invalid array subscript") errors = errors + 1;
        }
        if (v[4] !== undefined || errors !== 5 || rhs.n !== 1) throw "out of bounds conversion";
        v[0] = rhs;
        if (v[0] !== -1 || rhs.n !== 2) throw "in bounds conversion";
        var abrupt = {valueOf: function(){"use strict"; throw "RHS";}};
        try { v[0] = abrupt; } catch (g) { if (g === "RHS") errors = errors + 1; }
        if (v[0] !== -1 || errors !== 6) throw "abrupt conversion";
        true;
    "#);
}

#[test]
fn typed_array_integer_refusals() {
    guest_flag(r#"
        var v = new Int16Array(new ArrayBuffer(4));
        var C = Int16Array;
        var errors = 0;
        try { C(v.buffer); } catch (a) {
            if (a.name === "TypeError" && a.message === "must be called with new") errors = errors + 1;
        }
        try { new Int16Array(2); } catch (b) {
            if (b.name === "TypeError" && b.message === "unsupported object class") errors = errors + 1;
        }
        try { new Int16Array([1]); } catch (c) {
            if (c.name === "TypeError" && c.message === "unsupported object class") errors = errors + 1;
        }
        try { new Int16Array(v); } catch (d) {
            if (d.name === "TypeError" && d.message === "unsupported object class") errors = errors + 1;
        }
        try { v.extra = 1; } catch (e) {
            if (e.name === "TypeError" && e.message === "unsupported object class") errors = errors + 1;
        }
        errors === 5;
    "#);
}

#[test]
fn typed_array_float32_values() {
    guest_flag(r#"
        var b = new ArrayBuffer(16);
        var all = new Float32Array(b);
        var v = new Float32Array(b, 4, 2);
        var tail = new Float32Array(b, 4, undefined);
        if (Float32Array.BYTES_PER_ELEMENT !== 4 || Float32Array.prototype.BYTES_PER_ELEMENT !== 4) throw "width";
        if (all.length !== 4 || all[0] !== 0 || 1 / all[0] !== Infinity) throw "whole window";
        if (v.length !== 2 || v.byteOffset !== 4 || v.byteLength !== 8 || tail.length !== 3 || v.buffer !== b) throw "offset window";
        v[0] = 0.1;
        if (v[0] !== 0.10000000149011612 || all[1] !== v[0]) throw "rounded 0.1";
        v[0] = 16777217;
        v[1] = 16777219;
        if (v[0] !== 16777216 || v[1] !== 16777220) throw "ties to even";
        v[0] = 1e40;
        v[1] = NaN;
        if (v[0] !== Infinity || v[1] === v[1]) throw "overflow and NaN";
        v[0] = -0;
        v[1] = undefined;
        if (1 / v[0] !== -Infinity || v[1] === v[1]) throw "negative zero and undefined";
        v[0] = -7.006492321624085e-46;
        v[1] = 1.401298464324817e-45;
        if (1 / v[0] !== -Infinity || v[1] !== 1.401298464324817e-45) throw "subnormal";
        true;
    "#);
}

#[test]
fn typed_array_float64_values() {
    guest_flag(r#"
        var b = new ArrayBuffer(24);
        var all = new Float64Array(b);
        var v = new Float64Array(b, 8);
        if (Float64Array.BYTES_PER_ELEMENT !== 8 || Float64Array.prototype.BYTES_PER_ELEMENT !== 8) throw "width";
        if (all.length !== 3 || 1 / all[0] !== Infinity) throw "whole window";
        if (v.length !== 2 || v.byteLength !== 16 || v.byteOffset !== 8 || v.buffer !== b) throw "offset window";
        v[0] = 0.1;
        v[1] = 1e308;
        if (v[0] !== 0.1 || all[1] !== 0.1 || v[1] !== 1e308) throw "binary64 precision";
        v[0] = NaN;
        v[1] = -0;
        if (v[0] === v[0] || 1 / v[1] !== -Infinity) throw "NaN and negative zero";
        v[0] = 5e-324;
        v[1] = 1.7976931348623157e308;
        if (v[0] !== 5e-324 || v[1] !== 1.7976931348623157e308) throw "binary64 exponent limits";
        true;
    "#);
}

#[test]
fn typed_array_float_shared_backing() {
    guest_flag(r#"
        var b = new ArrayBuffer(16);
        var bytes = new Uint8Array(b);
        var words = new Uint32Array(b);
        var f32 = new Float32Array(b, 4, 1);
        var f64 = new Float64Array(b, 8, 1);
        if (Object.getPrototypeOf(f32) !== Float32Array.prototype || Object.getPrototypeOf(f64) !== Float64Array.prototype) throw "prototypes";
        if (Object.getPrototypeOf(Float32Array.prototype) !== Object.getPrototypeOf(Float64Array.prototype)) throw "shared getters";
        f32[0] = 0.1;
        if (bytes[3] !== 0 || bytes[4] !== 205 || bytes[5] !== 204 || bytes[6] !== 204 || bytes[7] !== 61) throw "float32 little endian";
        f64[0] = 0.1;
        if (words[2] !== 2576980378 || words[3] !== 1069128089) throw "float64 little endian";
        words[1] = 2147483648;
        words[2] = 1;
        words[3] = 0;
        if (1 / f32[0] !== -Infinity || f64[0] !== 5e-324) throw "integer-to-float alias";
        words[3] = 2146435072;
        if (f64[0] === f64[0]) throw "NaN payload";
        true;
    "#);
}

#[test]
fn typed_array_float_errors() {
    guest_flag(r#"
        var b = new ArrayBuffer(16);
        var odd = new ArrayBuffer(9);
        var count = {n: 0, valueOf: function(){"use strict"; this.n = this.n + 1; return 1;}};
        var errors = 0;
        try { new Float32Array(b, 2, count); } catch (a) { if (a.name === "RangeError" && a.message === "invalid offset") errors++; }
        try { new Float64Array(b, 4, count); } catch (c) { if (c.name === "RangeError" && c.message === "invalid offset") errors++; }
        try { new Float64Array(b, 24, count); } catch (d) { if (d.name === "RangeError" && d.message === "invalid offset") errors++; }
        if (errors !== 3 || count.n !== 0) throw "offset before length";
        try { new Float32Array(odd); } catch (e) { if (e.name === "RangeError" && e.message === "invalid length") errors++; }
        try { new Float64Array(odd, 8); } catch (f) { if (f.name === "RangeError" && f.message === "invalid length") errors++; }
        try { new Float64Array(b, 8, 2); } catch (g) { if (g.name === "RangeError" && g.message === "invalid length") errors++; }
        try { new Float32Array(b, -1); } catch (h) { if (h.name === "RangeError" && h.message === "invalid array index") errors++; }
        try { new Float64Array(b, 0, 1073741824); } catch (i) { if (i.name === "RangeError" && i.message === "invalid array index") errors++; }
        if (errors !== 8 || new Float64Array(odd, 0, 1).length !== 1) throw "length and index";
        try { Float32Array(b); } catch (j) { if (j.name === "TypeError" && j.message === "must be called with new") errors++; }
        try { Float64Array(b); } catch (k) { if (k.name === "TypeError" && k.message === "must be called with new") errors++; }
        var fake = {byteLength: 16};
        try { new Float64Array(fake); } catch (m) { if (m.name === "TypeError" && m.message === "unsupported object class") errors++; }
        var v = new Float32Array(b, 0, 1);
        try { v[1] = count; } catch (n) { if (n.name === "TypeError" && n.message === "invalid array subscript") errors++; }
        if (errors !== 12 || count.n !== 1 || v[1] !== undefined) throw "brand and bounds";
        v[0] = count;
        if (count.n !== 2 || v[0] !== 1) throw "ToNumber before store";
        var abrupt = {valueOf: function(){"use strict"; throw "rhs";}};
        try { v[1] = abrupt; } catch (p) { if (p === "rhs") errors++; }
        errors === 13;
    "#);
}

#[test]
fn typed_array_float_scope_refusals() {
    guest_flag(r#"
        var v = new Float64Array(new ArrayBuffer(8));
        var errors = 0;
        try { new Float32Array(2); } catch (a) { if (a.message === "unsupported object class") errors++; }
        try { new Float64Array([1]); } catch (b) { if (b.message === "unsupported object class") errors++; }
        try { new Float32Array(v); } catch (c) { if (c.message === "unsupported object class") errors++; }
        try { v.fill; } catch (h) { if (h.message === "unsupported object class") errors++; }
        try { v.extra = 7; } catch (i) { if (i.message === "unsupported object class") errors++; }
        errors === 5;
    "#);
}

#[test]
fn typed_array_methods_subarray_windows() {
    guest_flag(r#"
        var b = new ArrayBuffer(16);
        var v = new Int16Array(b, 2, 5);
        v[0] = 10; v[1] = 20; v[2] = 30; v[3] = 40; v[4] = 50;
        var whole = v.subarray();
        var tail = v['subarray'](-3.9, -1);
        var nested = tail.subarray(1);
        nested[0] = 42;
        if (!(whole !== v && whole.buffer === b && whole.length === 5 && whole.byteOffset === 2)) throw 'whole';
        if (!(tail.constructor === Int16Array && tail.buffer === b && tail.byteOffset === 6)) throw 'tail identity';
        if (!(tail.length === 2 && tail[0] === 30 && nested.byteOffset === 8 && v[3] === 42)) throw 'nested alias';
        if (!(v.subarray(99).byteOffset === 12 && v.subarray(99).length === 0)) throw 'positive clamp';
        if (!(v.subarray(3, 1).byteOffset === 8 && v.subarray(3, 1).length === 0)) throw 'reversed';
        if (!(v.subarray(NaN, undefined).length === 5 && v.subarray(-Infinity, NaN).length === 0)) throw 'NaN';
        v.subarray(0, Infinity).length === 5;
    "#);
}

#[test]
fn typed_array_methods_subarray_conversion() {
    guest_flag(r#"
        var v = new Uint8Array(new ArrayBuffer(4));
        var state = { n: 0 };
        var begin = { state: state, valueOf: function(){ "use strict"; this.state.n = 1; return -2; } };
        var end = { state: state, valueOf: function(){ "use strict"; this.state.n = this.state.n * 10 + 2; throw 'end'; } };
        var hit = false; var brand = false;
        try { v.subarray(begin, end); } catch (x) { hit = x === 'end'; }
        try { v.subarray.call({}, begin, end); } catch (y) { brand = y.name === 'TypeError' && y.message === 'not a TypedArray'; }
        hit && brand && state.n === 12;
    "#);
}

#[test]
fn typed_array_methods_set_float_bits_and_overlap() {
    guest_flag(r#"
        var b = new ArrayBuffer(16);
        var f = new Float32Array(b);
        var u = new Uint8Array(b);
        f[0] = 0.1; f[1] = 2;
        var first = f.set(f.subarray(0, 2), 1);
        var bytes = u[4] === 205 && u[5] === 204 && u[6] === 204 && u[7] === 61;
        u[8] = 1; u[9] = 0; u[10] = 192; u[11] = 127;
        var second = f.set(f.subarray(1, 3));
        if (!(first === undefined && second === undefined && bytes)) throw 'rightward copy';
        if (!(u[0] === 205 && u[1] === 204 && u[2] === 204 && u[3] === 61)) throw 'float bits';
        u[4] === 1 && u[5] === 0 && u[6] === 192 && u[7] === 127;
    "#);
}

#[test]
fn typed_array_methods_set_cross_type_live_overlap() {
    guest_flag(r#"
        var b = new ArrayBuffer(8);
        var src = new Uint8Array(b);
        var dst = new Uint16Array(b);
        src[0] = 7; src[1] = 9;
        dst.set(src.subarray(0, 2));
        dst[0] === 7 && dst[1] === 0 && src[1] === 0;
    "#);
}

#[test]
fn typed_array_methods_set_array_like() {
    guest_flag(r#"
        var v = new Uint8Array(new ArrayBuffer(5));
        var plain = v.set({length:2, 0:7, 1:8}, '1.9');
        var array = v.set([260, -1], 3);
        var n = 0;
        var source = {get length(){ "use strict"; return 4294967298; },
            get 0(){ "use strict"; return {valueOf: function(){ "use strict"; return 3; }}; },
            get 1(){ "use strict"; throw 'stop'; }};
        try { v.set(source, NaN); } catch (x) { if (x === 'stop') n++; }
        plain === undefined && array === undefined && n === 1 &&
        v[0] === 3 && v[1] === 7 && v[2] === 8 && v[3] === 4 && v[4] === 255;
    "#);
}

#[test]
fn typed_array_methods_set_errors() {
    guest_flag(r#"
        var v = new Uint8Array(new ArrayBuffer(2));
        var hit = 0;
        try { v.set({length:3, get 0(){ throw 'read'; }}); } catch (a) { if (a.name === 'RangeError' && a.message === 'invalid array length') hit++; }
        try { v.set({length:1}, 2); } catch (b) { if (b.name === 'RangeError' && b.message === 'invalid array length') hit++; }
        try { v.set(null, -1); } catch (c) { if (c.name === 'RangeError' && c.message === 'invalid array length') hit++; }
        try { v.set(null, Infinity); } catch (d) { if (d.name === 'TypeError' && d.message === 'not an object') hit++; }
        try { v.set('12'); } catch (e) { if (e.name === 'TypeError' && e.message === 'not an object') hit++; }
        try { v.set.call({}, []); } catch (f) { if (f.name === 'TypeError' && f.message === 'not a TypedArray') hit++; }
        try { new Uint8Array(v.buffer, 0, 3); } catch (g) { if (g.name === 'RangeError' && g.message === 'invalid length') hit++; }
        try { v.set([], Infinity); } catch (h) { if (h.name === 'RangeError' && h.message === 'invalid array length') hit++; }
        hit === 8 && v.set([], 2) === undefined;
    "#);
}

#[test]
fn typed_array_methods_join() {
    guest_flag(r#"
        var v = new Float64Array(new ArrayBuffer(40));
        v[0] = -0; v[1] = -1.25; v[2] = Infinity; v[3] = -Infinity; v[4] = NaN;
        var sep = {v: v, toString: function(){ "use strict"; this.v[0] = 7; return '🐱'; }};
        var changed = v.join(sep);
        changed === '7🐱-1.25🐱Infinity🐱-Infinity🐱NaN' &&
        v.join() === '7,-1.25,Infinity,-Infinity,NaN' && v.join(undefined) === v.join() &&
        v.subarray(4, 4).join('-') === '' && v.subarray(4).join('ignored') === 'NaN';
    "#);
}

#[test]
fn typed_array_methods_to_string() {
    guest_flag(r#"
        var v = new Uint8Array(new ArrayBuffer(2));
        v[0] = 1; v[1] = 2;
        var poison = { toString: function(){ "use strict"; throw 'unexpected'; } };
        var p = Object.getPrototypeOf(Uint8Array.prototype);
        if (!(p.hasOwnProperty('subarray') && p.hasOwnProperty('set') && p.hasOwnProperty('join') && p.hasOwnProperty('toString'))) throw 'method ownership';
        if (!(v.subarray === p.subarray && v.set === p.set && v.join === p.join && v.toString === p.toString)) throw 'method identity';
        if (!(p.subarray.length === 2 && p.set.length === 1 && p.join.length === 1 && p.toString.length === 0)) throw 'method arity';
        v.toString(poison) === '1,2' && v.toString() === v.join();
    "#);
}

#[test]
fn typed_array_prototypes_are_plain_objects() {
    // Upstream MicroQuickJS presents both the shared and concrete TypedArray
    // prototype shells, and ArrayBuffer.prototype, as ordinary objects.
    guest_flag(r#"
        var t = Object.prototype.toString;
        var a = t.call(Uint8Array.prototype) === '[object Object]';
        var b = t.call(Int8Array.prototype) === '[object Object]';
        var c = t.call(ArrayBuffer.prototype) === '[object Object]';
        a && b && c;
    "#);
}

#[test]
fn typed_array_methods_join_borrowed() {
    guest_flag(r#"
        var v = new Uint8Array(new ArrayBuffer(0));
        var o = {length:3, 0:7, 2:null};
        var hit = false;
        try { v.join.call(1); } catch (x) { hit = x.name === 'TypeError' && x.message === 'not an object'; }
        hit && v.join.call(o, '-') === '7--' && v.toString.call(o, '-') === '7,,' &&
        v.join.call([4, 5]) === '4,5';
    "#);
}

#[test]
fn typed_array176_windows() {
    guest_flag(r#"
        var b = new ArrayBuffer(6);
        var all = new Uint8Array(b);
        var tail = new Uint8Array(b, "2");
        var window = new Uint8Array(b, 1.9, 3);
        var empty = new Uint8Array(b, 6, undefined);
        var p = Uint8Array.prototype;
        var parent = Object.getPrototypeOf(p);
        if (Uint8Array.length !== 3 || Uint8Array.BYTES_PER_ELEMENT !== 1) throw "constructor";
        if (p.BYTES_PER_ELEMENT !== 1 || p.constructor !== Uint8Array) throw "prototype";
        if (Object.getPrototypeOf(Uint8Array) !== parent.constructor) throw "constructor parent";
        if (Object.getPrototypeOf(all) !== p || p.hasOwnProperty("buffer")) throw "instance parent";
        if (!parent.hasOwnProperty("buffer")) throw "shared getter";
        if (all.length !== 6 || all.byteOffset !== 0) throw "whole buffer";
        if (tail.length !== 4 || tail.byteLength !== 4 || tail.byteOffset !== 2) throw "tail";
        if (window.length !== 3 || window.byteLength !== 3 || window.byteOffset !== 1) throw "window";
        if (window.buffer !== b || empty.length !== 0 || empty.byteOffset !== 6) throw "empty and identity";
        true;
    "#);
}

#[test]
fn typed_array176_refusals_and_brand() {
    guest_flag(r#"
        var b = new ArrayBuffer(4);
        var v = new Uint8Array(b);
        var count = 0;
        var n = 0;
        try { Uint8Array((n = 1)); } catch (e) { if (e.name === "TypeError" && e.message === "must be called with new") count = count + 1; }
        var bad = [function(){new Uint8Array();}, function(){new Uint8Array(4);},
            function(){new Uint8Array([1]);}, function(x){new Uint8Array(x);},
            function(){new Uint8Array({});}, function(){Uint8Array.prototype.length;},
            function(x){x.extra = 3;}, function(x){x.fill(0);},
            function(){Object.getPrototypeOf(Uint8Array)();},
            function(){new Uint8Array(new ArrayBuffer(1), 0, 1, 0);}];
        var i = 0;
        while (i < bad.length) {
            var attempt = bad[i];
            try { attempt(v); } catch (e) { if (e.name === "TypeError" && e.message === "unsupported object class") count = count + 1; }
            i = i + 1;
        }
        n === 1 && count === 11;
    "#);
}

#[test]
fn typed_array176_conversion_order() {
    guest_flag(r#"
        var b = new ArrayBuffer(4);
        var bad = {valueOf: function(){ "use strict"; throw "length"; }};
        var count = 0;
        try { new Uint8Array(b, -1, bad); } catch (e) { if (e.name === "RangeError" && e.message === "invalid array index") count = count + 1; }
        try { new Uint8Array(b, 1073741824, bad); } catch (tooBig) { if (tooBig.name === "RangeError" && tooBig.message === "invalid array index") count = count + 1; }
        try { new Uint8Array(b, 5, bad); } catch (badOffset) { if (badOffset.name === "RangeError" && badOffset.message === "invalid offset") count = count + 1; }
        try { new Uint8Array(b, 4, bad); } catch (badLength) { if (badLength === "length") count = count + 1; }
        var arg = {n: 0, valueOf: function(){ "use strict"; this.n = this.n + 1; return this.n; }};
        var view = new Uint8Array(b, arg, arg);
        if (count !== 4) throw "conversion errors";
        if (arg.n !== 2 || view.byteOffset !== 1 || view.length !== 2) throw "offset before length";
        true;
    "#);
}

#[test]
fn typed_array176_invalid_offset_skips_length() {
    guest_flag(r#"
        var b = new ArrayBuffer(4);
        var offset = {n: 0, valueOf: function(){ "use strict"; this.n = this.n + 1; return 5; }};
        var length = {n: 0, valueOf: function(){ "use strict"; this.n = this.n + 1; throw "length converted"; }};
        var ok = false;
        try { new Uint8Array(b, offset, length); } catch (e) {
            ok = e.name === "RangeError" && e.message === "invalid offset";
        }
        if (offset.n !== 1 || length.n !== 0) throw "unexpected conversion counts";
        ok;
    "#);
}

#[test]
fn typed_array176_invalid_length_message() {
    guest_flag(r#"
        var b = new ArrayBuffer(4);
        var count = 0;
        try { new Uint8Array(b, 3, 2); } catch (e) { if (e.name === "RangeError" && e.message === "invalid length") count = count + 1; }
        try { new Uint8Array(b, 0, -1); } catch (negative) { if (negative.name === "RangeError" && negative.message === "invalid array index") count = count + 1; }
        try { new Uint8Array(b, 0, Infinity); } catch (infinite) { if (infinite.name === "RangeError" && infinite.message === "invalid array index") count = count + 1; }
        count === 3;
    "#);
}

#[test]
fn typed_array176_alias_bytes() {
    guest_flag(r#"
        var b = new ArrayBuffer(5);
        var a = new Uint8Array(b, 1, 3);
        var c = new Uint8Array(b, 2, 2);
        var id = function(x){ return x; };
        var key = "1";
        var zero = c[0];
        id(a)[key] = 46;
        var forward = c["0"] === 46;
        c[1] = 97;
        zero === 0 && forward && a[2] === 97 && a.buffer === b && c.buffer === b;
    "#);
}

#[test]
fn typed_array176_byte_conversion_and_bounds() {
    guest_flag(r#"
        var a = new Uint8Array(new ArrayBuffer(3), 1, 2);
        var value = {valueOf: function(){ "use strict"; return "-1.75"; }};
        var abrupt = {valueOf: function(){ "use strict"; throw "rhs"; }};
        var result = (a[0] = -1);
        a[1] = value;
        var count = 0;
        try { a[2] = 8; } catch (e) { if (e.name === "TypeError" && e.message === "invalid array subscript") count = count + 1; }
        try { a[2] = abrupt; } catch (e) { if (e === "rhs") count = count + 1; }
        try { a[-1] = 8; } catch (e) { if (e.name === "TypeError" && e.message === "invalid array subscript") count = count + 1; }
        var bytes = a[0] === 255 && a[1] === 255;
        a[1] = 259;
        bytes && a[1] === 3 && a[2] === undefined && a[-1] === undefined && result === -1 && count === 3;
    "#);
}

#[test]
fn typed_array176_index_presence() {
    guest_flag(r#"
        var b = new ArrayBuffer(6);
        var a = new Uint8Array(b, 3, 2);
        var k = Object.keys(a);
        var p = [1, 2, 3];
        var t = ["x", false];
        var empty = new Uint8Array(b, 6);
        if (Array.isArray(a) || 0 in a || "1" in a) throw "virtual indices are not ordinary properties";
        if (2 in a || "01" in a || -1 in a) throw "out of range";
        if (!a.hasOwnProperty(0) || !a.hasOwnProperty("1") || a.hasOwnProperty(2)) throw "own index";
        if (!("length" in a) || a.hasOwnProperty("length")) throw "inherited getter";
        if (k.length !== 2 || k[0] !== "0" || k[1] !== "1") throw "keys";
        if (Object.keys(empty).length !== 0 || 0 in empty) throw "empty view";
        if (0 in b || Object.keys(b).length !== 0) throw "buffer";
        if (Object.keys(p).length !== 3 || !p.hasOwnProperty(2)) throw "packed array";
        if (Object.keys(t).length !== 2 || !t.hasOwnProperty(1)) throw "tagged array";
        true;
    "#);
}

#[test]
fn typed_array176_in_vs_hasown() {
    guest_flag(r#"
        var view = new Uint8Array(new ArrayBuffer(2));
        view[0] = 7;
        if (0 in view || "1" in view) throw "in saw a virtual index";
        if (!view.hasOwnProperty(0) || !view.hasOwnProperty("1")) throw "missing own index";
        if (!("length" in view)) throw "missing inherited getter";
        view[0] === 7;
    "#);
}

fn array_buffer176_gc_probe() {
    use vm::{Expr as E, Stmt as S, TypeSchema as T};

    struct BufferObserver;
    impl vm::SourcePlugin for BufferObserver {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            let mut descriptor =
                vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
            descriptor.id = vm::FrontendId::new("array-buffer176-observer");
            descriptor
        }

        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let mut ir = vm::SourcePlugin::parse_source(
                &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
                source,
                context,
            )?;
            let helper_id = helper(&ir, "__compat_es5_array_buffer");
            let observe = helper(&ir, "__compat_es5_heap_observe");
            let collect = helper(&ir, "__compat_es5_heap_collect");
            let checked = helper(&ir, "__compat_es5_checked_base");
            let read = helper(&ir, "__compat_ref_read");
            let next = u16::try_from(ir.locals).unwrap();
            ir.locals += 2;
            let [before, base] = [next, next + 1];
            let implementation = ir.function_impls.get_mut(&helper_id).unwrap();
            let (carrier, mode, receiver) = (
                implementation.param_slots[0],
                implementation.param_slots[1],
                implementation.param_slots[2],
            );
            let count = || call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]);
            let checked = || {
                call(
                    checked,
                    vec![
                        E::Var(carrier),
                        get(receiver, "ref_slot"),
                        get(receiver, "generation"),
                    ],
                )
            };
            let rd = |offset| {
                call(
                    read,
                    vec![E::Var(carrier), num(plus(E::Var(base), E::Int(offset)))],
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
                let_int(base, checked()),
                S::Expr {
                    expr: call(collect, vec![E::Var(carrier)]),
                    line: 1,
                },
                check(eq(count(), plus(E::Var(before), E::Int(1)))),
                check(eq(checked(), E::Var(base))),
                check(eq(rd(0), E::Int(7))),
                check(eq(rd(5), E::Int(8))),
            ];
            for offset in 6..14 {
                assertions.push(check(eq(rd(offset), E::Float(0.0))));
            }
            const GET_LENGTH_MODE: i64 = 301;
            implementation.body_stmts.insert(
                0,
                S::IfElse {
                    condition: eq(E::Var(mode), E::Int(GET_LENGTH_MODE)),
                    then_branch: assertions,
                    else_branch: Vec::new(),
                    line: 1,
                },
            );
            Ok(ir)
        }
    }

    static OBS: BufferObserver = BufferObserver;
    const SOURCE: &str = "var id = function(x) { return x; }; var b = new ArrayBuffer(8); var keep = id(b); b = null; var i = 0; while (i < 40) { var p = {x:i}; i = i + 1; } id(keep).byteLength;";
    let stack = observed_for(
        SOURCE,
        &OBS,
        "array-buffer176-observer",
        0,
        true,
        80_000_000,
    );
    let Some(vm::Value::Map(result)) = stack.last() else {
        panic!("missing Number box")
    };
    assert_eq!(
        result.get(&vm::Value::string("tag")),
        Some(&vm::Value::Int(3))
    );
    assert_eq!(
        result.get(&vm::Value::string("number")),
        Some(&vm::Value::Float(8.0))
    );
}

fn view_gc_retains_backing<const BYTE: i64>(source: &str) {
    use vm::{Expr as E, Stmt as S, TypeSchema as T};

    struct ViewObserver<const BYTE: i64>;
    impl<const BYTE: i64> vm::SourcePlugin for ViewObserver<BYTE> {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            let mut descriptor = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
            descriptor.id = vm::FrontendId::new("typed-array176-observer");
            descriptor
        }

        fn parse_source(&self, source: &str, context: &vm::SourceFrontendContext<'_>) -> Result<vm::FrontendIr, vm::ParseError> {
            let mut ir = vm::SourcePlugin::parse_source(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let view_helper = helper(&ir, "__compat_es5_typed_array");
            let observe = helper(&ir, "__compat_es5_heap_observe");
            let collect = helper(&ir, "__compat_es5_heap_collect");
            let checked = helper(&ir, "__compat_es5_checked_base");
            let read = helper(&ir, "__compat_ref_read");
            let decode = helper(&ir, "__compat_es5_decode_int");
            let decode_generation = helper(&ir, "__compat_es5_decode_u32");
            let first = u16::try_from(ir.locals).unwrap();
            ir.locals += 3;
            let [before, view, backing] = [first, first + 1, first + 2];
            let implementation = ir.function_impls.get_mut(&view_helper).unwrap();
            let [carrier, mode, receiver] = implementation.param_slots[..3] else { unreachable!() };
            let count = || call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]);
            let handle = |slot, generation| call(checked, vec![E::Var(carrier), slot, generation]);
            let view_base = || handle(get(receiver, "ref_slot"), get(receiver, "generation"));
            let read_cell = |base, offset| call(read, vec![E::Var(carrier), num(plus(E::Var(base), E::Int(offset)))]);
            let backing_base = || handle(call(decode, vec![read_cell(view, 5)]), call(decode_generation, vec![read_cell(view, 6)]));
            let let_int = |index, expr| S::Let { index, declared_schema: Some(T::Int), expr, line: 1 };
            implementation.body_stmts.insert(0, S::IfElse {
                condition: eq(E::Var(mode), E::Int(330)),
                then_branch: vec![
                    let_int(before, count()), let_int(view, view_base()), let_int(backing, backing_base()),
                    S::Expr { expr: call(collect, vec![E::Var(carrier)]), line: 1 },
                    check(eq(count(), plus(E::Var(before), E::Int(1)))),
                    check(eq(view_base(), E::Var(view))), check(eq(backing_base(), E::Var(backing))),
                    check(eq(read_cell(view, 0), E::Int(8))), check(eq(read_cell(backing, 0), E::Int(7))),
                    // ArrayBuffer's six header cells precede byte zero.
                    check(eq(read_cell(backing, 6 + BYTE), E::Float(169.0))),
                ],
                else_branch: Vec::new(), line: 1,
            });
            Ok(ir)
        }
    }
    let stack = observed_for(source, &ViewObserver::<BYTE>, "typed-array176-observer", 0, true, 80_000_000);
    let Some(vm::Value::Map(result)) = stack.last() else { panic!("missing Boolean box") };
    assert_eq!(result.get(&vm::Value::string("flag")), Some(&vm::Value::Bool(true)));
}

#[test]
fn typed_array176_gc_retains_backing() {
    view_gc_retains_backing::<1>(r#"
        var b = new ArrayBuffer(3);
        var v = new Uint8Array(b, 1);
        b = null;
        v[0] = 169;
        var n = v.length;
        var read = v[0];
        v[1] = 84;
        n === 2 && read === 169 && v[1] === 84;
    "#);
}

#[test]
fn typed_array_float_gc_retains_backing() {
    view_gc_retains_backing::<0>(r#"
        var b = new ArrayBuffer(16);
        var v = new Float64Array(b);
        b = null;
        v[0] = 1.0000000000000375;
        var n = v.length;
        var read = v[0];
        v[1] = -0.1;
        n === 2 && read === 1.0000000000000375 && v[1] === -0.1;
    "#);
}

#[test]
fn typed_array_integer_gc_retains_backing() {
    view_gc_retains_backing::<1>(r#"
        var b = new ArrayBuffer(8);
        var v = new Int32Array(b);
        b = null;
        v[0] = 43264;
        var n = v.length;
        var read = v[0];
        v[1] = -256;
        n === 2 && read === 43264 && v[1] === -256;
    "#);
}

fn print_probe_fuel(machine: &vm::Vm, cap: u64) {
    let remaining = machine.get_fuel().expect("probe fuel was configured");
    eprintln!("LENGTH115_FUEL remaining={remaining} of {cap}");
}

fn observed(source: &str, observer: &'static LengthObserver, success: bool) -> Vec<vm::Value> {
    observed_for(source, observer, "length115-observer", observer.corruption, success, PROBE_FUEL)
}

fn observed_for<T>(
    source: &str,
    observer: &'static T,
    id: &'static str,
    corruption: u8,
    success: bool,
    fuel: u64,
) -> Vec<vm::Value>
where
    T: vm::SourcePlugin + Sync + 'static,
{
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = probe_vm_with_fuel(&source, observer, id, fuel);
            let status = machine.run();
            print_probe_fuel(&machine, fuel);
            eprintln!("LENGTH115_PROGRAM_HELPER_METADATA debug_names={:?} script_functions={:?} prototype_targets={:?}", machine.program().debug.as_ref().map(|debug| debug.functions.iter().enumerate().map(|(i, f)| (i, f.name.as_str())).collect::<Vec<_>>()), machine.program().script_functions, machine.program().callable_prototypes.iter().enumerate().map(|(i, p)| (i, p.target)).collect::<Vec<_>>());
            eprintln!(
                "LENGTH115_OBSERVED_STATE source={source:?} ip={} fuel={:?} frames={:?}",
                machine.ip(),
                machine.get_fuel(),
                machine.execution_frames()
            );
            eprintln!(
                "LENGTH115_OBSERVED source={source:?} corruption={} status={status:?} stack={:?}",
                corruption,
                machine.stack()
            );
            if success {
                assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            } else {
                let error = status.expect_err("malformed managed metadata must refuse");
                let diagnostic = format!("{error:?}");
                assert!(
                    diagnostic.contains("Assertion") || diagnostic.contains("assert"),
                    "{diagnostic}"
                );
                assert!(!diagnostic.contains("Fuel"), "{diagnostic}");
                if corruption == OLD_FUNCTION_HEADER {
                    assert!(matches!(error, vm::VmError::HostError(_)), "{diagnostic}");
                    assert_eq!(
                        machine.program().debug.as_ref().and_then(|debug| {
                            // A failed guest builtin call leaves the VM at the next instruction.
                            debug.line_for_offset(machine.ip().saturating_sub(4))
                        }),
                        Some(OBJECT_BRAND_ASSERT_LINE),
                        "old function header must fail the collector's object brand check",
                    );
                }
            }
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let stack = machine.stack().to_vec();
            reset(&mut machine);
            stack
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn string_length115_gc_before_read() {
    static OBS: LengthObserver = LengthObserver {
        corruption: 0,
        units: None,
        type_error: false,
        force_gc_after_bootstrap: false,
    };
    for source in [
        "function len(s){var sibling={a:'allocate'};return s.length;}len('AB');",
        "function len(s,t){return s.length;}len('AB',{x:'sibling'});",
    ] {
        let stack = observed(source, &OBS, true);
        let at = stack
            .iter()
            .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
            .unwrap()
            + 1;
        let vm::Value::Map(map) = &stack[at] else {
            panic!("box")
        };
        assert_eq!(
            map.get(&vm::Value::string("number")),
            Some(&vm::Value::Float(2.0))
        );
        assert_eq!(map.get(&vm::Value::string("tag")), Some(&vm::Value::Int(3)));
        oracle(source, serde_json::json!({"kind":"number","value":2}));
    }
}

#[test]
fn string_length115_fallback_managed_gc() {
    static STRING: LengthObserver = LengthObserver {
        corruption: 0,
        units: Some(2),
        type_error: false,
        force_gc_after_bootstrap: false,
    };
    for source in [
        "function f(o){return o.length;}var r=f({get length(){'use strict';var p={x:'alloc'};return 'AB';}});r;",
        "function f(o){return o.length;}var r;try{f({get length(){'use strict';throw 'AB';}});}catch(e){r=e;}finally{var p={x:'alloc'};}r;",
    ] {
        let stack = observed(source, &STRING, true);
        let at = stack
            .iter()
            .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
            .unwrap()
            + 1;
        assert_eq!(
            &stack[at + 1..at + 4],
            &[
                vm::Value::Float(2.0),
                vm::Value::Float(65.0),
                vm::Value::Float(66.0)
            ]
        );
        oracle(source, serde_json::json!({"kind":"string","value":"AB"}));
    }
}

#[test]
fn string_length115_typeerror_identity() {
    // Null and Undefined keep ordinary Get's actual TypeError prototype/category.
    // No adapter-entry checked_base instrumentation for non-managed primitive bases.
    static OBS: std::sync::LazyLock<object_observer::Observer> =
        std::sync::LazyLock::new(|| object_observer::Observer {
            type_error: true,
            force_gc: true,
            ..Default::default()
        });
    for source in [
        "function f(o){return o.length;}var r;try{f(null);}catch(e){r=e;}r;",
        "function f(o){return o.length;}var r;try{f();}catch(e){r=e;}r;",
    ] {
        let source = source.to_owned();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut machine = probe_vm(&source, &*OBS, "javascript-es5-object-observer");
                let status = machine.run();
                print_probe_fuel(&machine, PROBE_FUEL);
                assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                let stack = machine.stack();
                let at = stack
                    .iter()
                    .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
                    .unwrap()
                    + 1;
                assert_eq!(
                    &stack[at + 1..at + 5],
                    &[
                        vm::Value::Float(3.0),
                        vm::Value::Float(3.0),
                        vm::Value::Bool(true),
                        vm::Value::Bool(true)
                    ]
                );
                eprintln!("LENGTH115_TYPEERROR source={source:?} stack={stack:?}");
                reset(&mut machine);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

fn string_length115_malformed_case(index: usize) {
    static BAD: [LengthObserver; 6] = [
        LengthObserver {
            corruption: 1,
            units: None,
            type_error: false,
            force_gc_after_bootstrap: false,
        },
        LengthObserver {
            corruption: 2,
            units: None,
            type_error: false,
            force_gc_after_bootstrap: false,
        },
        LengthObserver {
            corruption: 3,
            units: None,
            type_error: false,
            force_gc_after_bootstrap: false,
        },
        LengthObserver {
            corruption: 4,
            units: None,
            type_error: false,
            force_gc_after_bootstrap: false,
        },
        LengthObserver {
            corruption: 6,
            units: None,
            type_error: false,
            force_gc_after_bootstrap: false,
        },
        LengthObserver {
            corruption: 7,
            units: None,
            type_error: false,
            force_gc_after_bootstrap: false,
        },
    ];
    observed("'AB'.length;", &BAD[index], false);
}

#[test]
fn string_length115_malformed_corruption_1() {
    string_length115_malformed_case(0);
}

#[test]
fn string_length115_malformed_corruption_2() {
    string_length115_malformed_case(1);
}

#[test]
fn string_length115_malformed_corruption_3() {
    string_length115_malformed_case(2);
}

#[test]
fn string_length115_malformed_corruption_4() {
    string_length115_malformed_case(3);
}

#[test]
fn string_length115_malformed_corruption_6() {
    string_length115_malformed_case(4);
}

#[test]
fn string_length115_malformed_corruption_7() {
    string_length115_malformed_case(5);
}

#[test]
fn string_length115_valid_extent_tail() {
    static TAIL: LengthObserver = LengthObserver {
        corruption: 5,
        units: None,
        type_error: false,
        force_gc_after_bootstrap: false,
    };
    let stack = observed("'AB'.length;", &TAIL, true);
    let at = stack
        .iter()
        .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
        .unwrap()
        + 1;
    let vm::Value::Map(map) = &stack[at] else {
        panic!("box")
    };
    assert_eq!(
        map.get(&vm::Value::string("number")),
        Some(&vm::Value::Float(1.0))
    );
}

#[test]
fn string_length115_member_call_is_not_a_read_site() {
    let source = "function f(o){return o.length();}f({length:function(){'use strict';return 7;}});";
    let ir = parse(source).unwrap();
    assert!(
        !ir.functions
            .iter()
            .any(|f| f.name == "__compat_es5_string_length_get"),
        "member-call-only source must retain original Get without adapter demand"
    );
    boxed(source, 3, 7.0, false);
}

#[test]
fn string_length115_staged_admission_current() {
    let id = "S8.4_A3";
    let body = include_str!("fixtures/test262117/S8.4_A3.js");
    let source =
        format!("function $ERROR(message) {{ 'use strict'; throw message; }}\n{body}\n;1;\n");
    let ir = parse(&source).unwrap();
    assert!(
        ir.functions
            .iter()
            .any(|f| f.name == "__compat_es5_string_concat")
    );
    eprintln!(
        "LENGTH115_CURRENT_UPSTREAM id={id} source={source:?} parse_success=true VM_invocations=0 original_body_unchanged=true adapter_own_strict=true"
    );
}

fn call_indices(debug: &str) -> Vec<u16> {
    debug
        .match_indices("Call(")
        .filter_map(|(at, _)| debug[at + 5..].split(',').next()?.trim().parse().ok())
        .collect()
}

#[test]
fn string_length115_dispatch_indices() {
    let source = "function len(s){return s.length;}function key(){var k=1.5;var o={'1.5':7};return o[k];}var o={get length(){'use strict';return decodeURIComponent('%41%42').length;}};var v=(function(x){return x.length;})(o);v===2&&len('AB')===2&&key()===7&&Math.abs(-3)===3&&'xy'.length===2;";
    let ir = parse(source).unwrap();
    let adapter = helper(&ir, "__compat_es5_string_length_get");
    let get = helper(&ir, "__compat_es5_ref_get");
    let dispatch = helper(&ir, "__compat_es5_dispatch");
    let mut source_indices: Vec<_> = ir
        .functions
        .iter()
        .filter(|f| f.name.starts_with("__compat_es5_fn_"))
        .map(|f| f.index)
        .collect();
    source_indices.sort();
    let mut inits: Vec<_> = ir
        .functions
        .iter()
        .filter(|f| f.name.starts_with("__compat_es5_utf16_init_"))
        .map(|f| f.index)
        .collect();
    inits.sort();
    let indices: std::collections::HashSet<_> = ir.functions.iter().map(|f| f.index).collect();
    assert_eq!(indices.len(), ir.functions.len());
    // Ordinary Get is inside the object-helper block (get=63, adapter=93
    // in this fixture). Anchor the adapter to the end of that block.
    let last_ordinary = helper(&ir, "__compat_es5_array_method");
    assert!(get < last_ordinary);
    assert_eq!(adapter, last_ordinary + 1);
    let math = [
        "__compat_es5_numeric_scan",
        "__compat_es5_numeric_primitive",
        "__compat_es5_numeric_call",
        "__compat_es5_round_decimal",
    ];
    for (index, name) in (adapter + 1..).zip(math) {
        assert_eq!(helper(&ir, name), index, "Math helper {name}");
    }
    assert_eq!(source_indices.len(), 4);
    assert!(source_indices.iter().copied().eq(dispatch - 4..dispatch));
    assert_eq!(source_indices[0], adapter + 1 + math.len() as u16);
    assert!(!inits.is_empty());
    assert!(
        inits
            .iter()
            .copied()
            .zip(dispatch + 1..)
            .all(|(actual, expected)| actual == expected)
    );
    let get_calls = call_indices(&format!("{:?}", ir.function_impls[&get]));
    assert!(
        get_calls.contains(&dispatch),
        "reserved getter dispatch must equal actual dispatch"
    );
    let adapter_calls = call_indices(&format!("{:?}", ir.function_impls[&adapter]));
    assert!(adapter_calls.contains(&get));
    let declaration = ir.functions.iter().find(|f| f.index == adapter).unwrap();
    assert_eq!(
        declaration.return_schema,
        Some(vm::TypeSchema::Result {
            ok: Box::new(vm::TypeSchema::Named("__CompatEs5Primitive".into(), vec![])),
            err: Box::new(vm::TypeSchema::Named("__CompatEs5Primitive".into(), vec![])),
        })
    );
    let proof = serde_json::json!({"source":source,"adapter":adapter,"ordinary_get":get,"reserved_getter_dispatch":dispatch,"actual_dispatch":dispatch,"source_function_indices":source_indices,"utf16_initializer_indices":inits,"unique_helper_indices":indices.len(),"locals":ir.locals,"fallback_actual_get_call":true});
    eprintln!("LENGTH115_INDICES {proof}");
    let dir = std::env::temp_dir().join("length115-dispatch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("proof.json"),
        serde_json::to_vec_pretty(&proof).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("functions.debug"), format!("{:#?}", ir.functions)).unwrap();
    std::fs::write(
        dir.join("get.debug"),
        format!("{:#?}", ir.function_impls[&get]),
    )
    .unwrap();
    std::fs::write(
        dir.join("adapter.debug"),
        format!("{:#?}", ir.function_impls[&adapter]),
    )
    .unwrap();
    boxed(source, 2, 0.0, true);
    for source in [
        "var s='AB';s;",
        "var o={length:7};o.length;",
        "function f(a,b){}f.length;",
    ] {
        let ir = parse(source).unwrap();
        assert!(
            !ir.functions
                .iter()
                .any(|f| f.name == "__compat_es5_string_length_get")
        );
        eprintln!(
            "LENGTH115_DORMANT source={source:?} adapter_absent=true locals={} functions={}",
            ir.locals,
            ir.functions.len()
        );
    }
}

#[test]
fn micro_braced_unicode_literals_public_sloppy() {
    // Every braced-literal value is bound before evaluation; the single trailing
    // boolean is the program completion that the boxed map reports. The flat
    // conjunction below keeps one `&&` per statement: same terms, same order,
    // same short-circuit truth value, with a shallow harness AST shape.
    let source = r#"var nul="\u{0}";var a="\u{61}";var bmp="\u{20ac}";var astral="\u{10000}";var maxcp="\u{10ffff}";var hs="\u{d800}";var ls="\u{dfff}";var overflow="\u{101234}";var escaped="\\u{61}";var upper="\u{20AC}";var zeros="\u{000061}";var braced_ok=true;braced_ok=braced_ok&&nul==="\0";braced_ok=braced_ok&&nul.length===1;braced_ok=braced_ok&&a==="a";braced_ok=braced_ok&&a.length===1;braced_ok=braced_ok&&bmp==="\u20ac";braced_ok=braced_ok&&bmp.length===1;braced_ok=braced_ok&&astral==="\uD800\uDC00";braced_ok=braced_ok&&astral.length===2;braced_ok=braced_ok&&maxcp==="\uDBFF\uDFFF";braced_ok=braced_ok&&maxcp.length===2;braced_ok=braced_ok&&hs==="\uD800";braced_ok=braced_ok&&hs.length===1;braced_ok=braced_ok&&((hs==="\uD801")?false:true);braced_ok=braced_ok&&((hs==="\uFFFD")?false:true);braced_ok=braced_ok&&ls==="\uDFFF";braced_ok=braced_ok&&ls.length===1;braced_ok=braced_ok&&((ls===hs)?false:true);braced_ok=braced_ok&&((ls==="\uFFFD")?false:true);braced_ok=braced_ok&&overflow==="\uDBC4\uDE34";braced_ok=braced_ok&&overflow.length===2;braced_ok=braced_ok&&escaped.length===6;braced_ok=braced_ok&&((escaped===a)?false:true);braced_ok=braced_ok&&upper==="\u20ac";braced_ok=braced_ok&&zeros==="a";braced_ok;"#;
    boxed(source, 2, 0.0, true);
}

#[test]
fn micro_braced_unicode_literals_public_strict() {
    // Strict prologue and the strict input subset are preserved verbatim; the
    // trailing predicate list is flattened the same way as the sloppy variant.
    let source = r#"'use strict';var nul="\u{0}";var a="\u{61}";var bmp="\u{20ac}";var astral="\u{10000}";var maxcp="\u{10ffff}";var hs="\u{d800}";var ls="\u{dfff}";var overflow="\u{101234}";var escaped="\\u{61}";var braced_ok=true;braced_ok=braced_ok&&nul==="\0";braced_ok=braced_ok&&nul.length===1;braced_ok=braced_ok&&a==="a";braced_ok=braced_ok&&bmp==="\u20ac";braced_ok=braced_ok&&bmp.length===1;braced_ok=braced_ok&&astral==="\uD800\uDC00";braced_ok=braced_ok&&astral.length===2;braced_ok=braced_ok&&maxcp==="\uDBFF\uDFFF";braced_ok=braced_ok&&maxcp.length===2;braced_ok=braced_ok&&hs==="\uD800";braced_ok=braced_ok&&hs.length===1;braced_ok=braced_ok&&ls==="\uDFFF";braced_ok=braced_ok&&ls.length===1;braced_ok=braced_ok&&((ls===hs)?false:true);braced_ok=braced_ok&&((hs==="\uFFFD")?false:true);braced_ok=braced_ok&&overflow==="\uDBC4\uDE34";braced_ok=braced_ok&&overflow.length===2;braced_ok=braced_ok&&escaped.length===6;braced_ok=braced_ok&&((escaped===a)?false:true);braced_ok;"#;
    boxed(source, 2, 0.0, true);
}

#[test]
fn micro_braced_unicode_literal_public_smoke() {
    // Smallest end-to-end ASCII witness for the public braced-literal path.
    boxed(r#""\u{61}" === "a";"#, 2, 0.0, true);
}

#[test]
fn micro_braced_unicode_literal_escaped_directive_remains_sloppy() {
    // The escaped prologue is not a Use Strict Directive by source spelling, so
    // the legacy sloppy octal escape "\077" stays an Annex-B '?' here.
    let source = r#""use\u{20}strict"; "\077" === "?";"#;
    boxed(source, 2, 0.0, true);
}

#[test]
fn micro_braced_unicode_literal_malformed_rejected() {
    for source in [
        r#"var s="\u{}";"#,
        r#"var s="\u{gg}";"#,
        r#"var s="\u{110000}";"#,
        r#"var s="\u{61";"#,
        r#"var s="\u{00_61}";"#,
        r#"var s="\u{-1}";"#,
    ] {
        let err = match parse(source) {
            Ok(_) => panic!("malformed braced Unicode literal parsed: {source}"),
            Err(err) => err,
        };
        eprintln!("LENGTH115_BRACED_MALFORMED source={source:?} error={err:?} guest_invocations=0");
    }
}

#[test]
fn micro_string_char_code_at_public_value() {
    boxed(r#""A".charCodeAt(0);"#, 3, 65.0, false);
}

#[test]
fn micro_char_code_at_utf16_units() {
    boxed(
        "var s=\"\\u{1F600}\\uD800x\"; s.charCodeAt(0)===55357 && s.charCodeAt(1)===56832 && s.charCodeAt(2)===55296 && s.charCodeAt(3)===120;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_char_code_at_position_conversions() {
    boxed(
        "var s=\"AZ\";var ok=true;ok=ok&&s.charCodeAt()===65;ok=ok&&s.charCodeAt(0/0)===65;ok=ok&&s.charCodeAt(-0)===65;ok=ok&&s.charCodeAt(-0.9)===65;ok=ok&&s.charCodeAt(1.9)===90;ok=ok&&s.charCodeAt(true)===90;ok=ok&&s.charCodeAt(false)===65;ok=ok&&s.charCodeAt(null)===65;ok=ok&&s.charCodeAt(undefined)===65;ok=ok&&s.charCodeAt(\"1.9\")===90;ok=ok&&s.charCodeAt(\" 1 \")===90;ok=ok&&s.charCodeAt(\"garbage\")===65;ok;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_char_code_at_nonfinite_bounds() {
    boxed(
        "var s=\"AZ\";var a=s.charCodeAt(-1);var b=s.charCodeAt(2);var c=s.charCodeAt(1/0);var d=s.charCodeAt(-1/0);typeof a===\"number\"&&a!==a&&b!==b&&c!==c&&d!==d;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_char_code_at_math_parser_coexistence() {
    boxed("\"A\".charCodeAt(Math.trunc(0.9));", 3, 65.0, false);
}

#[test]
fn micro_char_code_at_receiver_index_order() {
    boxed(
        "var o={n:0,m:\"\".charCodeAt,toString:function(){\"use strict\";this.n=this.n*10+1;return \"AZ\";}};var p={owner:o,valueOf:function(){\"use strict\";var owner=this.owner;owner.n=owner.n*10+2;return 1;}};var r=o.m(p);r===90&&o.n===12;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_char_code_at_index_throw_payload_finally() {
    boxed(
        "var o={n:0,m:\"\".charCodeAt,toString:function(){\"use strict\";this.n=this.n*10+1;return \"AZ\";}};var p={owner:o,payload:\"\\u{1F600}\",valueOf:function(){\"use strict\";var owner=this.owner;owner.n=owner.n*10+2;throw this.payload;}};var got=\"\";var f=0;try{o.m(p);}catch(e){got=e;}finally{f=f+1;}got===\"\\uD83D\\uDE00\"&&o.n===12&&f===1;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_char_code_at_receiver_throw_skips_index() {
    boxed(
        "var o={n:0,m:\"\".charCodeAt,toString:function(){\"use strict\";this.n=this.n*10+1;throw 7;}};var p={owner:o,valueOf:function(){\"use strict\";var owner=this.owner;owner.n=owner.n*10+2;return 1;}};var got=0;var f=0;try{o.m(p);}catch(e){got=e;}finally{f=f+1;}got===7&&o.n===1&&f===1;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_char_code_at_callee_extra_args() {
    boxed(
        "var o={n:0,m:\"\".charCodeAt,toString:function(){\"use strict\";this.n=this.n+1;return \"AZ\";}};var arity=o.m.length;var r=o.m((o.m=7,1),(o.n=o.n+10));arity===1&&r===90&&o.n===11&&o.m===7;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_char_code_at_nan_float_box() {
    let value = execute("\"A\".charCodeAt(1);");
    let vm::Value::Map(map) = value else {
        panic!("expected Number box");
    };
    assert_eq!(map.len(), 5);
    for (key, expected) in [
        ("tag", vm::Value::Int(3)),
        ("flag", vm::Value::Bool(false)),
        ("ref_slot", vm::Value::Int(0)),
        ("generation", vm::Value::Int(0)),
    ] {
        assert_eq!(map.get(&vm::Value::string(key)), Some(&expected));
    }
    let Some(vm::Value::Float(number)) = map.get(&vm::Value::string("number")) else {
        panic!("expected Float NaN payload");
    };
    assert!(number.is_nan());
}

// The managed String length read forces and counts a real collection while
// the source function is live, before reading its own length.
const SOURCE_FUNCTION_GC: &str = r#"var f = function(a, b) { return a; };
    var text = "A" + "B"; text.length;
    f.length === 2;"#;
const SOURCE_FUNCTION_GC_FUEL: u64 = 44_000_000;

#[test]
fn source_function_old_header_is_rejected_by_real_gc() {
    static OBS: LengthObserver = LengthObserver {
        corruption: OLD_FUNCTION_HEADER,
        units: None,
        type_error: false,
        force_gc_after_bootstrap: true,
    };
    observed_for(
        SOURCE_FUNCTION_GC,
        &OBS,
        "length115-observer",
        OBS.corruption,
        false,
        SOURCE_FUNCTION_GC_FUEL,
    );
}

#[test]
fn source_function_length_survives_real_gc() {
    static OBS: LengthObserver = LengthObserver {
        corruption: 0,
        units: None,
        type_error: false,
        force_gc_after_bootstrap: true,
    };
    let stack = observed_for(
        SOURCE_FUNCTION_GC,
        &OBS,
        "length115-observer",
        OBS.corruption,
        true,
        SOURCE_FUNCTION_GC_FUEL,
    );
    let marker = stack
        .iter()
        .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
        .expect("observed result marker");
    let vm::Value::Map(result) = &stack[marker + 1] else {
        panic!("expected Boolean box");
    };
    assert_eq!(result.get(&vm::Value::string("tag")), Some(&vm::Value::Int(2)));
    assert_eq!(result.get(&vm::Value::string("flag")), Some(&vm::Value::Bool(true)));
}

#[test]
fn micro_char_code_at_converted_receiver_real_gc() {
    // ToString completes with a fresh "AZ"; valueOf's real "AB".length read
    // forces the collection before the final index read of the converted
    // receiver.
    const GC_FIXTURE_FUEL: u64 = 44_000_000;
    static OBS: LengthObserver = LengthObserver {
        corruption: 0,
        units: None,
        type_error: false,
        force_gc_after_bootstrap: true,
    };
    let source = r#"var o={m:"".charCodeAt,toString:function(){"use strict";return "A"+"Z";}};var p={valueOf:function(){"use strict";var text="A"+"B";return text.length-1;}};o.m(p);"#;
    let stack = observed_for(source, &OBS, "length115-observer", OBS.corruption, true, GC_FIXTURE_FUEL);
    let at = stack
        .iter()
        .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
        .unwrap()
        + 1;
    let vm::Value::Map(map) = &stack[at] else {
        panic!("Number box");
    };
    assert_eq!(map.len(), 5);
    assert_eq!(map.get(&vm::Value::string("tag")), Some(&vm::Value::Int(3)));
    assert_eq!(
        map.get(&vm::Value::string("flag")),
        Some(&vm::Value::Bool(false))
    );
    assert_eq!(
        map.get(&vm::Value::string("ref_slot")),
        Some(&vm::Value::Int(0))
    );
    assert_eq!(
        map.get(&vm::Value::string("generation")),
        Some(&vm::Value::Int(0))
    );
    let Some(vm::Value::Float(actual)) = map.get(&vm::Value::string("number")) else {
        panic!("Number payload");
    };
    assert_eq!(actual.to_bits(), 90.0f64.to_bits());
}

#[test]
fn micro_char_code_at_throw_payload_real_gc() {
    // The first collection runs inside ToNumber; the fresh concat "CD" is then
    // thrown and only the pending exception keeps it alive across the nested
    // finally, whose "AB".length read forces the second real collection.
    // The expanded Array realm used 48,791,013 fuel in the 64M diagnostic.
    const GC_FIXTURE_FUEL: u64 = 52_000_000;
    static OBS: LengthObserver = LengthObserver {
        corruption: 0,
        units: Some(2),
        type_error: false,
        force_gc_after_bootstrap: false,
    };
    let source = r#"var o={m:"".charCodeAt,toString:function(){"use strict";return "A"+"Z";}};var p={valueOf:function(){"use strict";var text="A"+"B";if(text.length!==2){throw 99;}throw "C"+"D";}};var got="";var finalLength=0;try{try{o.m(p);}finally{var text="A"+"B";finalLength=text.length;}}catch(e){got=e;}if(finalLength!==2){throw 98;}got;"#;
    let stack = observed_for(source, &OBS, "length115-observer", OBS.corruption, true, GC_FIXTURE_FUEL);
    let at = stack
        .iter()
        .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
        .unwrap()
        + 1;
    let vm::Value::Map(map) = &stack[at] else {
        panic!("String box");
    };
    assert_eq!(map.len(), 5);
    assert_eq!(map.get(&vm::Value::string("tag")), Some(&vm::Value::Int(4)));
    assert_eq!(
        map.get(&vm::Value::string("number")),
        Some(&vm::Value::Float(0.0))
    );
    assert_eq!(
        map.get(&vm::Value::string("flag")),
        Some(&vm::Value::Bool(false))
    );
    let Some(vm::Value::Int(ref_slot)) = map.get(&vm::Value::string("ref_slot")) else {
        panic!("ref_slot");
    };
    assert!(*ref_slot > 0);
    let Some(vm::Value::Int(generation)) = map.get(&vm::Value::string("generation")) else {
        panic!("generation");
    };
    assert!(*generation >= 0);
    assert_eq!(
        &stack[at + 1..at + 4],
        &[
            vm::Value::Float(2.0),
            vm::Value::Float(67.0),
            vm::Value::Float(68.0)
        ]
    );
}

#[test]
fn micro_string_char_at_ascii_public() {
    boxed("\"abc\".charAt(1)===\"b\";", 2, 0.0, true);
}

#[test]
fn primitive_static_name155_char_code_at() {
    boxed(r#"var s="abc";s["charCodeAt"](1);"#, 3, 98.0, false);
}

#[test]
fn primitive_static_name155_string_has_own_property() {
    boxed(r#"var s="abc";s["hasOwnProperty"]("x");"#, 2, 0.0, false);
}

#[test]
fn primitive_static_name155_absent_name() {
    boxed(r#"var s="abc";s["__absent_name155__"];"#, 0, 0.0, false);
}

#[test]
fn primitive_static_name155_length() {
    boxed(r#"var s="abc";s["length"];"#, 3, 3.0, false);
}

#[test]
fn primitive_static_name155_noncanonical_zero_zero() {
    boxed(r#"var s="abc";s["00"];"#, 0, 0.0, false);
}

#[test]
fn primitive_static_name155_number_has_own_property() {
    boxed(r#"(7)["hasOwnProperty"]("x");"#, 2, 0.0, false);
}

#[test]
fn primitive_static_name155_boolean_has_own_property() {
    boxed(r#"true["hasOwnProperty"]("x");"#, 2, 0.0, false);
}

#[test]
fn primitive_static_name155_number_decimal_name() {
    boxed(r#"(7)["0"];"#, 0, 0.0, false);
}

#[test]
fn primitive_static_name155_boolean_decimal_name() {
    boxed(r#"true["0"];"#, 0, 0.0, false);
}

#[test]
fn primitive_static_name155_refuse_canonical_index() {
    let source = r#"var s="abc";s["0"];"#;
    let err = match parse(source) {
        Ok(_) => panic!("out of profile: {source}"),
        Err(e) => e,
    };
    eprintln!("LENGTH115_BOUNDARY source={source:?} error={err:?} guest_invocations=0");
}

#[test]
fn primitive_static_name155_refuse_escaped_zero() {
    let source = r#"var s="abc";s["\u0030"];"#;
    let err = match parse(source) {
        Ok(_) => panic!("out of profile: {source}"),
        Err(e) => e,
    };
    eprintln!("LENGTH115_BOUNDARY source={source:?} error={err:?} guest_invocations=0");
}

#[test]
fn primitive_static_name155_refuse_nonliteral_key() {
    let source = r#"var s="abc";var k="charAt";s[k](1);"#;
    let err = match parse(source) {
        Ok(_) => panic!("out of profile: {source}"),
        Err(e) => e,
    };
    eprintln!("LENGTH115_BOUNDARY source={source:?} error={err:?} guest_invocations=0");
}

#[test]
fn primitive_static_name155_refuse_paren_key() {
    let source = r#"var s="abc";s[("charAt")](1);"#;
    let err = match parse(source) {
        Ok(_) => panic!("out of profile: {source}"),
        Err(e) => e,
    };
    eprintln!("LENGTH115_BOUNDARY source={source:?} error={err:?} guest_invocations=0");
}

#[test]
fn micro_string_char_at_borrowed_order() {
    boxed(
        "var o={n:0,m:\"\".charAt,toString:function(){\"use strict\";this.n=this.n*10+1;return \"AZ\";}};var p={owner:o,valueOf:function(){\"use strict\";var owner=this.owner;owner.n=owner.n*10+2;return 1;}};var r=o.m(p);r===\"Z\"&&o.n===12;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_char_at_receiver_throw_skips_index() {
    boxed(
        "var o={n:0,m:\"\".charAt,toString:function(){\"use strict\";this.n=this.n*10+1;throw 7;}};var p={owner:o,valueOf:function(){\"use strict\";var owner=this.owner;owner.n=owner.n*10+2;return 1;}};var got=0;var f=0;try{o.m(p);}catch(e){got=e;}finally{f=f+1;}got===7&&o.n===1&&f===1;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_char_at_index_throw_payload_finally() {
    boxed(
        "var o={n:0,m:\"\".charAt,toString:function(){\"use strict\";this.n=this.n*10+1;return \"AZ\";}};var p={owner:o,payload:\"\\u{1F600}\",valueOf:function(){\"use strict\";var owner=this.owner;owner.n=owner.n*10+2;throw this.payload;}};var got=\"\";var f=0;try{o.m(p);}catch(e){got=e;}finally{f=f+1;}got===\"\\uD83D\\uDE00\"&&o.n===12&&f===1;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_char_at_callee_extra_args() {
    boxed(
        "var o={n:0,m:\"\".charAt,toString:function(){\"use strict\";this.n=this.n+1;return \"AZ\";}};var arity=o.m.length;var r=o.m((o.m=7,1),(o.n=o.n+10));arity===1&&r===\"Z\"&&o.n===11&&o.m===7;",
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_code_point_at_astral_not_leading_unit() {
    let source = r#""\u{10ffff}".codePointAt(0)===1114111&&"\u{10ffff}".charCodeAt(0)===56319"#;
    boxed(source, 2, 0.0, true);
}

#[test]
fn micro_code_point_at_trailing_unit() {
    let source = r#""\u{10ffff}".codePointAt(1)"#;
    boxed(source, 3, 57343.0, false);
}

#[test]
fn micro_code_point_at_unpaired_leading() {
    let source = r#""\uD800".codePointAt(0)"#;
    boxed(source, 3, 55296.0, false);
}

#[test]
fn micro_code_point_at_finite_oob_undefined() {
    let source = r#""\u{10ffff}".codePointAt(2)"#;
    boxed(source, 0, 0.0, false);
}

#[test]
fn micro_string_concat_two_units_public() {
    char_at_units(r#""a".concat("b");"#, &[97u16, 98u16]);
}

#[test]
fn micro_string_concat_zero_args_public() {
    char_at_units(r#""a".concat();"#, &[97u16]);
}

#[test]
fn micro_string_concat_empty_result_public() {
    char_at_units(r#""".concat("");"#, &[]);
}

#[test]
fn micro_string_concat_multiple_args_utf16_public() {
    char_at_units(
        r#""\uD800".concat("\uDC00", "x");"#,
        &[55296u16, 56320u16, 120u16],
    );
}

#[test]
fn micro_string_substring_middle_public() {
    char_at_units(r#""abcd".substring(1, 3);"#, &[98u16, 99u16]);
}

#[test]
fn micro_string_substring_zero_args_public() {
    char_at_units(r#""abcd".substring();"#, &[97u16, 98u16, 99u16, 100u16]);
}

#[test]
fn micro_string_substring_missing_end_public() {
    char_at_units(r#""abcd".substring(1);"#, &[98u16, 99u16, 100u16]);
}

#[test]
fn micro_string_substring_undefined_end_public() {
    char_at_units(
        r#""abcd".substring(1, undefined);"#,
        &[98u16, 99u16, 100u16],
    );
}

#[test]
fn micro_string_substring_reversed_public() {
    char_at_units(r#""abcd".substring(3, 1);"#, &[98u16, 99u16]);
}

#[test]
fn micro_string_substring_empty_range_public() {
    char_at_units(r#""abcd".substring(2, 2);"#, &[]);
}

#[test]
fn micro_string_substring_finite_clamps_public() {
    char_at_units(
        r#""abcd".substring(-2, 20);"#,
        &[97u16, 98u16, 99u16, 100u16],
    );
}

#[test]
fn micro_string_slice_relative_negative_public() {
    char_at_units(r#""abcd".slice(-3, -1);"#, &[98u16, 99u16]);
}

#[test]
fn micro_string_slice_zero_args_public() {
    char_at_units(r#""abcd".slice();"#, &[97u16, 98u16, 99u16, 100u16]);
}

#[test]
fn micro_string_slice_missing_end_public() {
    char_at_units(r#""abcd".slice(-2);"#, &[99u16, 100u16]);
}

#[test]
fn micro_string_slice_undefined_end_public() {
    char_at_units(r#""abcd".slice(1, undefined);"#, &[98u16, 99u16, 100u16]);
}

#[test]
fn micro_string_slice_reversed_empty_public() {
    char_at_units(r#""abcd".slice(3, 1);"#, &[]);
}

#[test]
fn micro_string_slice_empty_range_public() {
    char_at_units(r#""abcd".slice(2, 2);"#, &[]);
}

#[test]
fn micro_string_slice_finite_clamps_public() {
    char_at_units(r#""abcd".slice(-20, 20);"#, &[97u16, 98u16, 99u16, 100u16]);
}

#[test]
fn micro_string_slice_utf16_trailing_unit_public() {
    char_at_units(r#""\uD800\uDC00".slice(1, 2);"#, &[56320u16]);
}

#[test]
fn micro_string_index_of_middle_public() {
    boxed(r#""abc".indexOf("b");"#, 3, 1.0, false);
}

#[test]
fn micro_string_index_of_miss_public() {
    boxed(r#""abc".indexOf("z");"#, 3, -1.0, false);
}

#[test]
fn micro_string_index_of_empty_needle_clamped_end_public() {
    boxed(r#""abc".indexOf("", 20);"#, 3, 3.0, false);
}

#[test]
fn micro_string_index_of_negative_position_public() {
    boxed(r#""abc".indexOf("a", -2);"#, 3, 0.0, false);
}

#[test]
fn micro_string_index_of_start_position_first_eligible_public() {
    boxed(r#""ababa".indexOf("ba", 2);"#, 3, 3.0, false);
}

#[test]
fn micro_string_index_of_fractional_to_integer_public() {
    boxed(r#""abc".indexOf("b", 1.9);"#, 3, 1.0, false);
}

#[test]
fn micro_string_index_of_utf16_low_surrogate_inside_pair_public() {
    boxed(r#""\uD800\uDC00".indexOf("\uDC00");"#, 3, 1.0, false);
}

#[test]
fn micro_string_index_of_omitted_search_public() {
    boxed(r#""undefined!".indexOf();"#, 3, 0.0, false);
}

#[test]
fn micro_string_index_of_visible_length_public() {
    boxed(r#""abc".indexOf.length;"#, 3, 1.0, false);
}

#[test]
fn micro_string_index_of_needle_longer_than_receiver_public() {
    boxed(r#""ab".indexOf("abc");"#, 3, -1.0, false);
}

#[test]
fn micro_string_index_of_nonempty_past_end_public() {
    boxed(r#""abc".indexOf("b", 20);"#, 3, -1.0, false);
}

#[test]
fn micro_string_index_of_partial_match_advance_public() {
    boxed(r#""aab".indexOf("ab");"#, 3, 1.0, false);
}

#[test]
fn micro_string_index_of_borrowed_conversion_order_public() {
    boxed(
        r#"var o={n:0,m:"".indexOf,toString:function(){"use strict";this.n=this.n*10+1;return "ababa";}};var s={owner:o,toString:function(){"use strict";var owner=this.owner;owner.n=owner.n*10+2;return "ba";}};var p={owner:o,valueOf:function(){"use strict";var owner=this.owner;owner.n=owner.n*10+3;return 2;}};var r=o.m(s,p);r===3&&o.n===123;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_index_of_position_valueof_undefined_public() {
    boxed(
        r#"var o={n:0,m:"".indexOf,toString:function(){"use strict";this.n=this.n*10+1;return "ababa";}};var s={owner:o,toString:function(){"use strict";var owner=this.owner;owner.n=owner.n*10+2;return "ba";}};var p={owner:o,valueOf:function(){"use strict";var owner=this.owner;owner.n=owner.n*10+3;return undefined;},toString:function(){"use strict";var owner=this.owner;owner.n=owner.n*10+4;throw 9;}};var r=o.m(s,p);r===1&&o.n===123;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_index_of_search_getter_skips_position_public() {
    boxed(
        r#"var o={n:0,m:"".indexOf,toString:function(){"use strict";this.n=this.n*10+1;return "ababa";}};var s={owner:o,get toString(){"use strict";var owner=this.owner;owner.n=owner.n*10+2;throw 7;}};var p={owner:o,valueOf:function(){"use strict";var owner=this.owner;owner.n=owner.n*10+3;return 2;}};var got=0;var f=0;try{o.m(s,p);}catch(e){got=e;}finally{f=f+1;}got===7&&o.n===12&&f===1;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_index_of_position_throw_utf16_payload_public() {
    boxed(
        r#"var o={n:0,m:"".indexOf,toString:function(){"use strict";this.n=this.n*10+1;return "ababa";}};var s={owner:o,toString:function(){"use strict";var owner=this.owner;owner.n=owner.n*10+2;return "ba";}};var p={owner:o,payload:"\uD800\uDC00",valueOf:function(){"use strict";var owner=this.owner;owner.n=owner.n*10+3;throw this.payload;}};var got="";var f=0;try{o.m(s,p);}catch(e){got=e;}finally{f=f+1;}got==="\uD800\uDC00"&&o.n===123&&f===1;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_index_of_extracted_undefined_receiver_public() {
    boxed(
        r#"function g(){"use strict";var f="".indexOf;f();}var ok=false;try{g();}catch(e){ok=e.name==='TypeError';}ok;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_last_index_of_last_match_public() {
    boxed(r#""ababa".lastIndexOf("ba");"#, 3, 3.0, false);
}

#[test]
fn micro_string_last_index_of_miss_public() {
    boxed(r#""ababa".lastIndexOf("z");"#, 3, -1.0, false);
}

#[test]
fn micro_string_last_index_of_empty_default_end_public() {
    boxed(r#""abc".lastIndexOf("");"#, 3, 3.0, false);
}

#[test]
fn micro_string_last_index_of_negative_clamp_zero_public() {
    boxed(r#""ababa".lastIndexOf("a",-2);"#, 3, 0.0, false);
}

#[test]
fn micro_string_last_index_of_fractional_start_public() {
    boxed(r#""ababa".lastIndexOf("ba",2.9);"#, 3, 1.0, false);
}

#[test]
fn micro_string_last_index_of_utf16_last_low_public() {
    boxed(
        r#""\uD800\uDC00\uD800\uDC00".lastIndexOf("\uDC00");"#,
        3,
        3.0,
        false,
    );
}

#[test]
fn micro_string_last_index_of_partial_reverse_advance_public() {
    boxed(r#""aaba".lastIndexOf("aab");"#, 3, 0.0, false);
}

#[test]
fn micro_string_last_index_of_visible_length_public() {
    boxed(r#""abc".lastIndexOf.length;"#, 3, 1.0, false);
}

#[test]
fn string_prototype_to_string_ascii_primitive_returns_same_units() {
    char_at_units("\"abc\".toString();", &[0x0061, 0x0062, 0x0063]);
}

#[test]
fn string_prototype_to_string_borrowed_ordinary_nohook_type_error() {
    boxed(
        r#"var n=0;var f="".toString;if(typeof f!=="function")n=1;var o={n:0,m:f,toString:function(){"use strict";this.n=this.n+1;return "hook";},valueOf:function(){"use strict";this.n=this.n+1;return "hook";}};var got=0;try{o.m();}catch(e){if(e.name==="TypeError")got=1;}if(n===0&&got!==1)n=2;if(n===0&&o.n!==0)n=3;n;"#,
        3,
        0.0,
        false,
    );
}

#[test]
fn string_prototype_to_string_extracted_undefined_type_error() {
    boxed(
        r#"function g(){"use strict";var f="".toString;if(typeof f!=="function")return 1;try{f();return 2;}catch(e){if(e.name==="TypeError")return 0;return 3;}}g();"#,
        3,
        0.0,
        false,
    );
}

#[test]
fn string_prototype_to_string_extra_arg_once_then_throw_before_dispatch() {
    boxed(
        r#"var n=0;var f="".toString;if(typeof f!=="function")n=1;var box={n:0};function side(b){b.n=b.n+1;return 9;}var got="AZ".toString(side(box));if(n===0&&box.n!==1)n=2;if(n===0&&got!=="AZ")n=3;function boom(b){b.n=b.n+1;throw 7;}var payload=0;try{"AZ".toString(boom(box));}catch(e){payload=e;}if(n===0&&box.n!==2)n=4;if(n===0&&payload!==7)n=5;n;"#,
        3,
        0.0,
        false,
    );
}

#[test]
fn string_prototype_to_string_public_length_zero() {
    boxed(
        r#"var f="abc".toString;typeof f==="function"&&f.length===0;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn object_prototype_to_string_class_label_unchanged() {
    boxed(
        r#"var o={};var t=Object.prototype.toString;o.toString()==="[object Object]"&&t()==="[object Undefined]";"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_to_lower_case_ascii_public_value() {
    char_at_units(
        "\"Az09!z\".toLowerCase();",
        &[0x0061, 0x007a, 0x0030, 0x0039, 0x0021, 0x007a],
    );
}

#[test]
fn micro_string_to_upper_case_ascii_public_value() {
    char_at_units(
        "\"aZ09!z\".toUpperCase();",
        &[0x0041, 0x005a, 0x0030, 0x0039, 0x0021, 0x005a],
    );
}

#[test]
fn micro_string_case_ascii_endpoints() {
    char_at_units("\"AZaz\".toLowerCase();", &[0x0061, 0x007a, 0x0061, 0x007a]);
    char_at_units("\"AZaz\".toUpperCase();", &[0x0041, 0x005a, 0x0041, 0x005a]);
}

#[test]
fn micro_string_case_empty_and_unchanged_units() {
    char_at_units("\"\".toLowerCase();", &[]);
    char_at_units("\"\".toUpperCase();", &[]);
    char_at_units("\"09!\".toLowerCase();", &[0x0030, 0x0039, 0x0021]);
    char_at_units("\"09!\".toUpperCase();", &[0x0030, 0x0039, 0x0021]);
}

#[test]
fn micro_string_case_public_length_zero() {
    boxed(
        r#"var a="A".toLowerCase;var b="A".toUpperCase;typeof a==="function"&&a.length===0&&typeof b==="function"&&b.length===0;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_case_extra_arg_once_then_throw_before_dispatch() {
    boxed(
        r#"var n=0;var f="".toLowerCase;var h="".toUpperCase;if(typeof f!=="function"||typeof h!=="function")n=1;var box={n:0};function side(b){b.n=b.n+1;return 9;}var low="Az".toLowerCase(side(box));var up="aZ".toUpperCase(side(box));if(n===0&&box.n!==2)n=2;if(n===0&&low!=="az")n=3;if(n===0&&up!=="AZ")n=4;function boom(b){b.n=b.n+1;throw 7;}var payload=0;try{"Az".toLowerCase(boom(box));}catch(e){payload=e;}if(n===0&&box.n!==3)n=5;if(n===0&&payload!==7)n=6;payload=0;try{"aZ".toUpperCase(boom(box));}catch(e){payload=e;}if(n===0&&box.n!==4)n=7;if(n===0&&payload!==7)n=8;n;"#,
        3,
        0.0,
        false,
    );
}

#[test]
fn micro_string_case_object_tostring_stops_before_valueof() {
    boxed(
        r#"var n=0;var f="".toLowerCase;var h="".toUpperCase;if(typeof f!=="function"||typeof h!=="function")n=1;var box={n:0,low:f,up:h,toString:function(){"use strict";this.n=this.n+1;return "Ab";},valueOf:function(){"use strict";this.n=this.n+1;return "NO";}};var low=box.low();var up=box.up();if(n===0&&box.n!==2)n=2;if(n===0&&low!=="ab")n=3;if(n===0&&up!=="AB")n=4;n;"#,
        3,
        0.0,
        false,
    );
}

#[test]
fn micro_string_case_nullish_type_error_has_method() {
    boxed(
        r#"var n=0;var f="".toLowerCase;var h="".toUpperCase;if(typeof f!=="function"||typeof h!=="function")n=1;var hits=0;try{f.call(null);}catch(e){if(e.name==="TypeError")hits=hits+1;else if(n===0)n=3;}try{f.call(undefined);}catch(e){if(e.name==="TypeError")hits=hits+1;else if(n===0)n=4;}try{h.call(null);}catch(e){if(e.name==="TypeError")hits=hits+1;else if(n===0)n=5;}try{h.call(undefined);}catch(e){if(e.name==="TypeError")hits=hits+1;else if(n===0)n=6;}n===0&&hits===4;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_case_non_ascii_letter_stays_unmapped() {
    // MicroQuickJS ASCII mapping leaves U+00C0 / U+00E0 unchanged.
    // Node maps them. This witness does not call the Node oracle.
    for (source, unit) in [
        (r#""\u00C0".toLowerCase().charCodeAt(0);"#, 192.0),
        (r#""\u00E0".toUpperCase().charCodeAt(0);"#, 224.0),
    ] {
        let result = execute(source);
        let vm::Value::Map(map) = result else {
            panic!("boxed number {source}")
        };
        let Some(vm::Value::Float(actual)) = map.get(&vm::Value::string("number")) else {
            panic!("number payload {source}")
        };
        assert_eq!(*actual, unit, "{source}");
    }
}

#[test]
fn micro_string_case_surrogate_units_kept() {
    char_at_units(
        "\"A\\uD800\\uDC00Z\".toLowerCase();",
        &[0x0061, 0xD800, 0xDC00, 0x007a],
    );
    char_at_units(
        "\"A\\uD800\\uDC00Z\".toUpperCase();",
        &[0x0041, 0xD800, 0xDC00, 0x005a],
    );
}


#[test]
fn array_length165_literal_public() {
    boxed(r#"var a=[1,2];a.length;"#, 3, 2.0, false);
}

#[test]
fn array_length165_empty_public() {
    boxed(r#"var e=[];e.length;"#, 3, 0.0, false);
}

#[test]
fn array_length165_returned_public() {
    boxed(r#"function f(){return ["x",1];}f().length;"#, 3, 2.0, false);
}

#[test]
fn array_length165_prototype_public() {
    boxed(r#"var p=Object.getPrototypeOf([1,2]);p.length;"#, 3, 0.0, false);
}

#[test]
fn array_length165_prototype_chain_public() {
    boxed(r#"var a=[1,2];var p=Object.getPrototypeOf(a);a.length===2&&p.length===0&&p.hasOwnProperty("length")&&Object.getPrototypeOf(p)===Object.prototype;"#, 2, 0.0, true);
}

#[test]
fn array_length165_shrink_public() {
    boxed(
        "var a=[1,2,3];a.length=1;a.length===1&&a[0]===1&&a[1]===undefined&&!a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_grow_public() {
    // Micro materializes a grown index as own undefined. Node leaves a hole.
    micro_box(
        "var a=[1];a.length=3;a.length===3&&a[0]===1&&a[1]===undefined&&a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

fn micro_box(source: &str, tag: i64, number: f64, flag: bool) {
    let result = execute(source);
    let vm::Value::Map(map) = result else {
        panic!("full box: {result:?}");
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
    let script = format!(
        "const source={};try{{const v=(0,eval)(source);console.log(JSON.stringify({{kind:typeof v,value:v}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',name:e&&e.name,value:String(e)}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output();
    eprintln!("LENGTH165_MICRO_ONLY source={source:?} node={output:?}");
}

#[test]
fn array_length165_alias_public() {
    boxed(
        "var a=[1,2,3];var b=a;a.length=1;b===a&&b.length===1&&b[0]===1&&b[1]===undefined&&!b.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_dynamic_public() {
    boxed(
        "var a=[1,2,3];function f(x){return x;}f(a).length=1;a.length===1&&a[0]===1&&a[1]===undefined&&!a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_no_resurrect_public() {
    // Shrink then grow must not revive the old number. Micro materialises an
    // own undefined at the new index; Node treats that index as a hole.
    micro_box(
        "var a=[1,2,3];a.length=1;a.length=3;a.length===3&&a[0]===1&&a[1]===undefined&&a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_zero_public() {
    boxed(
        "var a=[1];a.length=0;a.length===0&&a[0]===undefined&&!a.hasOwnProperty(\"0\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_whole_public() {
    boxed(
        "var a=[1,2,3];a.length=1.0;a.length===1&&a[0]===1&&a[1]===undefined&&!a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_negative_micro() {
    micro_box(
        "var a=[1,2];var ok=false;try{a.length=-1;}catch(e){ok=e.name==='TypeError';}ok&&a.length===2;",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_fraction_micro() {
    micro_box(
        "var a=[1,2,3];a.length=1.9;a.length===1&&a[0]===1&&a[1]===undefined&&!a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_next_write_public() {
    boxed(
        "var a=[1,2,3];a.length=1;a[0]=9;a.length===1&&a[0]===9&&a[1]===undefined;",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_expr_public() {
    boxed(
        "var a=[1,2];var r=(a.length=1);r===1&&a.length===1&&a[0]===1&&a[1]===undefined;",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_bound_public() {
    micro_box(
        "var a=[1];a.length=65535;a.length===65535&&a[0]===1&&a[1]===undefined&&a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_bound65536_public() {
    micro_box(
        "var a=[1];a.length=65536;a.length===65536&&a[0]===1&&a[1]===undefined&&a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_range_micro() {
    micro_box(
        "var a=[1];var ok=false;try{a.length=1073741824;}catch(e){ok=e.name==='TypeError';}ok&&a.length===1;",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_for_of_public() {
    micro_box(
        "var a=[1,2,3];a.length=1;a.length=3;var n=0,u=0;for(var x of a){n=n+1;if(x===undefined){u=u+1;}}n===3&&u===2&&a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_index_var_public() {
    micro_box(
        "var a=[1];a.length=3;var i=1;a[i]===undefined&&a.hasOwnProperty(\"1\");",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_plain70000_public() {
    micro_box(
        "var a=[1];a.length=70000;a.length===70000&&a[0]===1&&a[1]===undefined;",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_call_object_micro() {
    micro_box(
        "function f(x){return x;}var box={x:1};f(box).length=1;box.length===1;",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_call_type_micro() {
    micro_box(
        "function f(){return \"x\";}var ok=false;try{f().length=1;}catch(e){ok=e.name==='TypeError';}ok;",
        2,
        0.0,
        true,
    );
}

/// Array-only roots must survive a real managed collection. The fresh string
/// and object are not kept by any other binding; the loop crosses the
/// allocation interval after publication.
#[test]
fn array_length165_gc_array_only_roots() {
    boxed(
        "var a=[\"\"+\"kept\",{v:7}];var t={};var i=0;while(i<40){t={k:i};i=i+1;}a[0]===\"kept\"&&a[1].v===7;",
        2,
        0.0,
        true,
    );
}

/// Object.keys must return a real array: public length and index 0 both hold.
#[test]
fn array_length165_object_keys_length_and_index() {
    boxed(
        "var k=Object.keys({a:1});k.length===1&&k[0]===\"a\";",
        2,
        0.0,
        true,
    );
}

/// Length write inside the loop must not keep the entry constant.
#[test]
fn array_length165_loop_length_write() {
    boxed(
        "var a=[1,2,3];var n=0;var i=0;while(i<a.length){if(i===1){a.length=1;}i=i+1;n=n+1;}n===2;",
        2,
        0.0,
        true,
    );
}

/// Null length is ToInt32 0. One source.
#[test]
fn array_length165_toint32_null() {
    boxed("var a=[1,2];a.length=null;a.length===0;", 2, 0.0, true);
}

/// Numeric string length truncates through ToInt32. One source.
#[test]
fn array_length165_toint32_string() {
    boxed("var a=[1,2,3];a.length=\"1\";a.length===1&&a[0]===1;", 2, 0.0, true);
}

/// Micro ToInt32(2^32) is 0 and in range. Node ToUint32 differs from
/// ToNumber, so it throws RangeError. Guest only. One source.
#[test]
fn array_length165_toint32_wrap() {
    micro_box(
        "var a=[1,2];a.length=4294967296;a.length===0;",
        2,
        0.0,
        true,
    );
}

/// 2^30 is an int32 but above Micro's 2^30-1 length bound. Node accepts it.
/// Guest only. One source.
#[test]
fn array_length165_toint32_bound() {
    micro_box(
        "var a=[1];var ok=false;try{a.length=1073741824;}catch(e){ok=e.name==='TypeError';}ok&&a.length===1;",
        2,
        0.0,
        true,
    );
}

/// After grow, a literal index in [present, length) must store and read back.
/// Common with Node. The unwritten gap stays undefined; own-ness is separate.
#[test]
fn array_length165_grow_write_literal() {
    boxed(
        "var a=[1,2,3];a.length=1;a.length=3;a[2]=9;a[2]===9&&a.length===3&&a[0]===1&&a[1]===undefined;",
        2,
        0.0,
        true,
    );
}

/// Same store as the first grow literal, kept in its own selector.
#[test]
fn array_length165_grow_write_literal_fresh() {
    boxed(
        "var a=[1];a.length=3;a[2]=9;a[2]===9&&a.length===3&&a[0]===1&&a[1]===undefined;",
        2,
        0.0,
        true,
    );
}

/// A hole inside the new length accepts a literal store.
#[test]
fn array_length165_grow_write_literal_gap() {
    boxed(
        "var a=[1];a.length=3;a[1]=9;a[1]===9&&a.length===3&&a[0]===1&&a[2]===undefined;",
        2,
        0.0,
        true,
    );
}

/// Length 13105 does not allocate cells. A hole read must not expand.
/// A store at 13104 or 13105 cannot fit in the 65536-cell extent and host-halts.
#[test]
fn array_length165_length_extent_bound() {
    guest_flag("var a=[1];a.length=13105;a.length===13105&&a[0]===1&&a[13104]===undefined;");
}

/// Dynamic index uses the same store as the literal form.
#[test]
fn array_length165_grow_write_dynamic() {
    boxed(
        "var a=[1];a.length=3;var k=2;a[k]=9;a[2]===9&&a.length===3&&a[1]===undefined;",
        2,
        0.0,
        true,
    );
}

/// Alias keeps the same object identity and sees the stored element.
#[test]
fn array_length165_grow_write_alias() {
    boxed(
        "var a=[1];var b=a;a.length=3;a[2]=9;b===a&&b[2]===9&&b.length===3&&b[0]===1&&b[1]===undefined;",
        2,
        0.0,
        true,
    );
}

/// Micro materializes the unwritten gap as own undefined. Node leaves a hole.
#[test]
fn array_length165_grow_write_hole_micro() {
    micro_box(
        "var a=[1];a.length=3;a[2]=9;a[1]===undefined&&a.hasOwnProperty(\"1\")&&a[2]===9;",
        2,
        0.0,
        true,
    );
}

/// A managed string written into a grown index must round-trip.
#[test]
fn array_length165_grow_write_string() {
    boxed(
        "var a=[1];a.length=2;a[1]=\"hi\";a[1]===\"hi\"&&a.length===2&&a[0]===1;",
        2,
        0.0,
        true,
    );
}

/// A managed object written into a grown index keeps identity.
#[test]
fn array_length165_grow_write_object() {
    boxed(
        "var a=[1];a.length=2;var o={v:7};a[1]=o;a[1]===o&&a[1].v===7&&a.length===2;",
        2,
        0.0,
        true,
    );
}

/// Length read in a while body must see the write from the previous iteration.
#[test]
fn array_length165_loop_while_body() {
    boxed(
        "var a=[1,2,3];var s=0;var i=0;while(i<2){s=s+a.length;a.length=1;i=i+1;}s===4;",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_loop_do_body() {
    boxed(
        "var a=[1,2,3];var s=0;var i=0;do{s=s+a.length;a.length=1;i=i+1;}while(i<2);s===4;",
        2,
        0.0,
        true,
    );
}

#[test]
fn array_length165_loop_for_body() {
    boxed(
        "var a=[1,2,3];var s=0;for(var i=0;i<2;i=i+1){s=s+a.length;a.length=1;}s===4;",
        2,
        0.0,
        true,
    );
}

fn guest_flag(source: &str) {
    let result = execute(source);
    let vm::Value::Map(map) = result else {
        panic!("full box: {result:?}");
    };
    assert_eq!(
        map.get(&vm::Value::string("tag")),
        Some(&vm::Value::Int(2)),
        "{source}"
    );
    assert_eq!(
        map.get(&vm::Value::string("flag")),
        Some(&vm::Value::Bool(true)),
        "{source}"
    );
}

#[test]
fn array_sort_index_read_after_sort() {
    guest_flag("var a=[2,1,3];a.sort();a[0]===1&&a[1]===2&&a[2]===3;");
}

#[test]
fn array_sort_packed_default_alias() {
    guest_flag("var a=[2,10,1];var b=a;var r=a.sort();r===a&&b===a&&b.length===3&&a[0]===1&&a[1]===10&&a[2]===2&&b[0]===1;");
}

#[test]
fn array_sort_prototype_method() {
    guest_flag(r#"var a=[1];var p=Object.getPrototypeOf(a);typeof a.sort==="function"&&a.sort.length===1&&p.hasOwnProperty("sort")&&p.sort===a.sort&&!a.hasOwnProperty("sort");"#);
}

#[test]
fn array_sort_computed_explicit_undefined() {
    guest_flag(r#"var a=[2,10];var n=0;var r=a["sort"](undefined,n=n+1);r===a&&n===1&&a[0]===10&&a[1]===2;"#);
}

#[test]
fn array_sort_no_comparable_pairs() {
    guest_flag("var f=function(x,y){throw 7;};var a=[];var b=[7];var c=[undefined,undefined];a.sort(f)===a&&b.sort(f)===b&&b[0]===7&&c.sort(f)===c&&c[0]===undefined&&c[1]===undefined;");
}

#[test]
fn array_sort_comparator_receiver_and_order() {
    guest_flag(r#"var a=[2,10,1];var r=a.sort(function(x,y,z){"use strict";if(this!==undefined){throw 7;}if(z!==undefined){throw 8;}return y-x;});r===a&&a[0]===10&&a[1]===2&&a[2]===1;"#);
}

#[test]
fn array_sort_comparator_alias_shrink_length_read() {
    guest_flag(
        r#"var id = function(x) { return x; };
        var a = [null, 1, 2];
        var b = id(a);
        a[0] = b;
        a.sort(function(x, y) {
            "use strict";
            if (typeof x === "object" && x.length === 3) { x.pop(); }
            return 0;
        });
        b.length === 2 && a.length === 2 && a[1] === 1;"#,
    );
}

#[test]
fn array_sort_comparator_alias_grow_length_read() {
    guest_flag(
        r#"var id = function(x) { return x; };
        var a = [null, 1];
        var b = id(a);
        a[0] = b;
        a["sort"](function(x, y) {
            "use strict";
            if (typeof x === "object" && x.length === 2) { x.push(9); }
            return 0;
        });
        b.length === 3 && a.length === 3 && a[2] === 9;"#,
    );
}

#[test]
fn array_sort_equal_keys_keep_input_order() {
    guest_flag("var x={k:2};var y={k:1};var z={k:2};var w={k:1};var a=[x,y,z,w];a.sort(function(l,r){if(l.k===r.k){return l.k===1?NaN:-0;}return l.k-r.k;});a[0]===y&&a[1]===w&&a[2]===x&&a[3]===z;");
}

#[test]
fn array_sort_default_utf16_and_object() {
    guest_flag(r#"var x={toString:function(){return "b";},valueOf:function(){throw 7;}};var a=["\uE000","\uD83D\uDE00","ba",x];a.sort();a[0]===x&&a[1]==="ba"&&a[2]==="\uD83D\uDE00"&&a[3]==="\uE000";"#);
}

#[test]
fn array_sort_undefined_before_unmaterialized_tail() {
    guest_flag(r#"var a=[undefined,2,undefined,1];a.length=7;a.sort(function(x,y){if(x===undefined){throw 7;}if(y===undefined){throw 8;}return x-y;});a.length===7&&a[0]===1&&a[1]===2&&a[2]===undefined&&a[3]===undefined&&a[6]===undefined&&a.hasOwnProperty("3")&&a.hasOwnProperty("6")&&!a.hasOwnProperty("7");"#);
}

#[test]
fn array_sort_compare_result_to_number() {
    guest_flag(r#"var a=[1,3,2];a.sort(function(x,y){return {n:y-x,valueOf:function(){"use strict";return this.n;}};});var b=[2,1];var got=0;try{b.sort(function(x,y){return {valueOf:function(){throw 9;}};});}catch(e){got=e;}a[0]===3&&a[1]===2&&a[2]===1&&got===9&&b[0]===2&&b[1]===1;"#);
}

#[test]
fn array_sort_comparator_throw_stops_before_writeback() {
    guest_flag("var s={n:0,payload:{id:7}};var x={s:s};var y={s:s};var z={s:s};var a=[x,y,z];var got=null;var ended=0;try{a.sort(function(l,r){l.s.n=l.s.n+1;throw l.s.payload;});}catch(e){got=e;}finally{ended=ended+1;}got===s.payload&&s.n===1&&ended===1&&a[0]===x&&a[1]===y&&a[2]===z;");
}

#[test]
fn array_sort_default_conversion_getter_throw() {
    guest_flag(r#"var x={get toString(){"use strict";throw 11;}};var a=["z",x];var got=0;try{a.sort();}catch(e){got=e;}got===11&&a[0]==="z"&&a[1]===x;"#);
}

#[test]
fn array_sort_invalid_comparator_and_borrowed_brand() {
    guest_flag(r#"var a=[];var o={0:1,length:1,f:a.sort};var bad=false;var brand=false;try{a.sort(null);}catch(e){bad=e.name==="TypeError"&&e.message==="not a function";}try{o.f(function(){throw 42;});}catch(e){brand=e.name==="TypeError";}bad&&brand&&o[0]===1&&o.length===1;"#);
}

#[test]
fn array_sort_large_logical_length_small_prefix() {
    // A direct Array binding exercises wide-length sort without the dynamic receiver path.
    guest_flag("var a = [2, 10]; a.length = 70000; var r = a.sort(); r === a && a.length === 70000 && a[0] === 10 && a[1] === 2 && a[69999] === undefined;");
}

#[test]
fn array_sort_conversion_strings_survive_gc() {
    guest_flag(r#"var a=[{id:2,toString:function(){var s="Z"+"X";return s.length===2?s:"bad";}},{id:1,toString:function(){var s="A"+"B";return s.length===2?s:"bad";}}];a.sort();a[0].id===1&&a[1].id===2;"#);
}

#[test]
fn array_sort_default_vectors() {
    guest_flag("var a=[3,1,2].sort();var b=[10,2].sort();a[0]===1&&a[1]===2&&a[2]===3&&b[0]===10&&b[1]===2;");
}

/// A grown hole is undefined. `undefined + 9` is NaN, not a cell past present.
#[test]
fn array_length165_literal_hole_add() {
    boxed(
        "var a=[1];a.length=2;a[1]+=9;a[1]!==a[1]&&a.length===2&&a[0]===1;",
        2,
        0.0,
        true,
    );
}

/// After grow, a tagged slot holding a number must increment, not halt.
#[test]
fn array_length165_literal_tagged_inc() {
    boxed(
        "var a=[1];a.length=3;a[2]=9;a[2]++;a[2]===10&&a.length===3&&a[1]===undefined;",
        2,
        0.0,
        true,
    );
}

/// A present packed cell still reads the stored number.
#[test]
fn array_length165_literal_packed_inc() {
    boxed(
        "var a=[1,2];a[0]++;a[0]===2&&a[1]===2&&a.length===2;",
        2,
        0.0,
        true,
    );
}

/// The right-hand call runs once. The hole read does not re-enter it.
#[test]
fn array_length165_literal_hole_side_once() {
    boxed(
        "var box={n:0};function side(b){b.n=b.n+1;return 9;}var a=[1];a.length=2;a[1]+=+side(box);box.n===1&&a[1]!==a[1];",
        2,
        0.0,
        true,
    );
}

/// An alias sees the same hole update.
#[test]
fn array_length165_literal_hole_alias() {
    boxed(
        "var a=[1];var b=a;a.length=2;a[1]+=9;a===b&&b[1]!==b[1]&&b.length===2;",
        2,
        0.0,
        true,
    );
}

/// Dynamic index already uses the checked reader. Same hole result.
#[test]
fn array_length165_dynamic_hole_add() {
    boxed(
        "var a=[1];a.length=2;var k=1;a[k]++;a[1]!==a[1]&&a.length===2;",
        2,
        0.0,
        true,
    );
}

/// Micro ToInt32(undefined) is 0. Node throws, so this is guest-only.
#[test]
fn array_length165_length_undefined() {
    guest_flag("var a=[1,2];a.length=undefined;a.length===0;");
}

/// Plain object ToPrimitive is non-numeric, so Micro ToInt32 is 0.
#[test]
fn array_length165_length_plain_object() {
    guest_flag("var a=[1,2];a.length={};a.length===0;");
}

/// valueOf runs once and wins. toString must not run.
/// Node's length setter calls ToNumber twice, so the counter is guest-only.
#[test]
fn array_length165_length_valueof_number() {
    guest_flag(
        "var a=[1,2,3];var o={n:0,valueOf:function(){'use strict';this.n=this.n+1;return 1;},toString:function(){'use strict';this.n=99;return '2';}};a.length=o;a.length===1&&a[0]===1&&o.n===1;",
    );
}

/// valueOf returning an object falls through to toString once.
/// Node repeats ToNumber, so the counter is guest-only.
#[test]
fn array_length165_length_tostring_after_object() {
    guest_flag(
        "var a=[1,2,3];var o={n:0,valueOf:function(){'use strict';this.n=1;return {};},toString:function(){'use strict';this.n=this.n+1;return '1';}};a.length=o;a.length===1&&a[0]===1&&o.n===2;",
    );
}

/// A throwing valueOf is the catch value. toString does not run.
#[test]
fn array_length165_length_valueof_throw() {
    boxed(
        "var a=[1,2];var o={n:0,get valueOf(){'use strict';this.n=1;throw 7;},toString:function(){'use strict';this.n=99;return '2';}};var ok=false;try{a.length=o;}catch(e){ok=e===7&&o.n===1;}ok&&a.length===2;",
        2,
        0.0,
        true,
    );
}

/// A non-callable valueOf is skipped. toString supplies the number.
/// Node repeats ToNumber, so the counter is guest-only.
#[test]
fn array_length165_length_valueof_skip() {
    guest_flag(
        "var a=[1,2,3];var o={n:0,valueOf:0,toString:function(){'use strict';this.n=1;return '1';}};a.length=o;a.length===1&&a[0]===1&&o.n===1;",
    );
}

/// An alias observes the converted length and the surviving element.
#[test]
fn array_length165_length_object_alias() {
    boxed(
        "var a=[1,2,3];var b=a;var o={valueOf:function(){'use strict';return 1;}};b.length=o;a===b&&a.length===1&&a[0]===1;",
        2,
        0.0,
        true,
    );
}

/// Both methods return objects: TypeError, length unchanged. Guest only.
#[test]
fn array_length165_length_primitive_fail() {
    guest_flag(
        "var a=[1,2];var o={n:0,valueOf:function(){'use strict';this.n=1;return {};},toString:function(){'use strict';this.n=this.n+1;return {};}};var ok=false;try{a.length=o;}catch(e){ok=e.name==='TypeError'&&o.n===2;}ok&&a.length===2;",
    );
}

/// A function uses the same converter. Micro ToInt32 is 0. Node differs.
#[test]
fn array_length165_length_function() {
    guest_flag("var a=[1,2];a.length=function(){};a.length===0;");
}

/// An array uses the same converter. Micro ToInt32 is 0. Node differs.
#[test]
fn array_length165_length_array_rhs() {
    guest_flag("var a=[1,2];a.length=[];a.length===0;");
}

/// Decimal string length does not require a Math reference.
#[test]
fn array_length165_length_string_one() {
    boxed(
        "var a=[1,2,3];a.length='1';a.length===1&&a[0]===1;",
        2,
        0.0,
        true,
    );
}

/// Alias and a string binding use the same conversion.
#[test]
fn array_length165_length_string_alias() {
    boxed(
        "var a=[1,2,3];var b=a;var s='1';b.length=s;a===b&&a.length===1&&a[0]===1;",
        2,
        0.0,
        true,
    );
}

/// Empty numeric string is 0. No Math reference.
#[test]
fn array_length165_length_string_empty() {
    boxed(
        "var a=[1,2];a.length='';a.length===0;",
        2,
        0.0,
        true,
    );
}

/// A non-numeric string is NaN, and Micro ToInt32(NaN) is 0.
#[test]
fn array_length165_length_string_bad() {
    guest_flag("var a=[1,2];a.length='x';a.length===0;");
}

/// N01: a call that returns a plain object creates an own `length` data
/// property. The original RHS reference is stored; array ToNumber/valueOf
/// does not run. The receiver is retained so the call-form write is visible.
#[test]
fn array_length165_n01_call_object_valueof() {
    guest_flag(
        "var box={p:1};function f(x){return x;}var o={n:0,valueOf:function(){'use strict';this.n=1;return 1;}};f(box).length=o;box.length===o&&o.n===0;",
    );
}

/// Persistent alias stores the original RHS and does not call valueOf.
#[test]
fn array_length165_n01_alias_identity() {
    guest_flag(
        "var o={n:0,valueOf:function(){'use strict';this.n=1;return 1;}};var box={p:1};var alias=box;alias.length=o;box.length===o&&alias.length===o&&o.n===0;",
    );
}

/// Non-strict nonextensible call receiver does not create length.
#[test]
fn array_length165_n01_nonextensible_silent() {
    guest_flag(
        "var box={p:1};Object.preventExtensions(box);function f(x){return x;}f(box).length=1;box.length===undefined;",
    );
}

/// RHS throw on a call receiver does not create length.
#[test]
fn array_length165_n01_rhs_throw_no_store() {
    guest_flag(
        "var box={p:1};function f(x){return x;}var got=0;try{f(box).length=(function(){'use strict';throw 3;})();}catch(e){got=e;}got===3&&box.length===undefined;",
    );
}

/// String and boolean call receivers stay TypeError; valueOf does not run.
#[test]
fn array_length165_n01_primitive_receivers() {
    guest_flag(
        "function fs(){return 's';}function fb(){return false;}var os={n:0,valueOf:function(){'use strict';this.n=1;return 1;}};var ob={n:0,valueOf:function(){'use strict';this.n=1;return 1;}};var name='none';try{fs().length=os;}catch(e){name=e.name;}var nameb='none';try{fb().length=ob;}catch(e){nameb=e.name;}name==='TypeError'&&os.n===0&&nameb==='TypeError'&&ob.n===0;",
    );
}

/// Strict nonextensible call receiver: TypeError, and length is not created.
#[test]
fn array_length165_n01_nonextensible_strict() {
    guest_flag(
        "'use strict';var box={p:1};Object.preventExtensions(box);function f(x){return x;}var name='none';try{f(box).length=1;}catch(e){name=e.name;}name==='TypeError'&&box.length===undefined;",
    );
}

/// N01: a call that returns a number is not an array. valueOf must not run.
#[test]
fn array_length165_n01_call_number_valueof() {
    guest_flag(
        "function f(){return 1;}var o={n:0,valueOf:function(){'use strict';this.n=1;return 1;}};var name='none';try{f().length=o;}catch(e){name=e.name;}name==='TypeError'&&o.n===0;",
    );
}

/// N01: null call receiver. valueOf must not run.
#[test]
fn array_length165_n01_call_null_valueof() {
    guest_flag(
        "function f(){return null;}var o={n:0,valueOf:function(){'use strict';this.n=1;return 1;}};var name='none';try{f().length=o;}catch(e){name=e.name;}name==='TypeError'&&o.n===0;",
    );
}

/// N01: undefined call receiver. valueOf must not run.
#[test]
fn array_length165_n01_call_undefined_valueof() {
    guest_flag(
        "function f(){return undefined;}var o={n:0,valueOf:function(){'use strict';this.n=1;return 1;}};var name='none';try{f().length=o;}catch(e){name=e.name;}name==='TypeError'&&o.n===0;",
    );
}

/// N01: an array alias still converts once, after the receiver check.
#[test]
fn array_length165_n01_alias_valueof_once() {
    guest_flag(
        "var a=[1,2];var b=a;var o={n:0,valueOf:function(){'use strict';this.n=this.n+1;return 1;}};b.length=o;a===b&&a.length===1&&a[0]===1&&o.n===1;",
    );
}

/// N01: valueOf getter throw on a real array keeps length and wins over store.
#[test]
fn array_length165_n01_getter_throw() {
    guest_flag(
        "var a=[1,2];var o={n:0,get valueOf(){'use strict';this.n=1;throw 7;}};var got=0;var fin=0;try{a.length=o;}catch(e){got=e;}finally{fin=1;}got===7&&fin===1&&o.n===1&&a.length===2&&a[0]===1&&a[1]===2;",
    );
}

/// Ordinary object own `length` is a data property. A non-writable define
/// stores 1; the read is 1. This is not an array-header decode.
#[test]
fn array_length165_n01_readonly_length() {
    guest_flag(
        "var box={};Object.defineProperty(box,'length',{value:1,writable:false});box.length===1;",
    );
}

/// Sloppy assignment to that non-writable own `length` is silent. The value stays 1.
#[test]
fn array_length165_n01_readonly_sloppy() {
    guest_flag(
        "var box={};Object.defineProperty(box,'length',{value:1,writable:false});box.length=9;box.length===1;",
    );
}

/// Original deleted witness: a call that returns the ordinary object, then a
/// sloppy write, still leaves the non-writable own `length` at 1.
#[test]
fn array_length165_n01_readonly_call_sloppy() {
    guest_flag(
        "var box={};Object.defineProperty(box,'length',{value:1,writable:false});function f(x){return x;}f(box).length=9;box.length===1;",
    );
}

/// Strict assignment to that non-writable own `length` is a TypeError and does not store.
#[test]
fn array_length165_n01_readonly_strict() {
    guest_flag(
        "function f(){'use strict';var box={};Object.defineProperty(box,'length',{value:1,writable:false});var ok=false;try{box.length=9;}catch(e){ok=e.name==='TypeError';}return ok&&box.length===1;}f();",
    );
}

/// N02: a throwing RHS expression must not store. undefined must not become 0.
#[test]
fn array_length165_n02_rhs_throw() {
    guest_flag(
        "var a=[1,2];var box={n:0,boom:function(){'use strict';this.n=this.n+1;throw 1;}};var got=0;var fin=0;try{a.length=box.boom();}catch(e){got=e;}finally{fin=1;}got===1&&fin===1&&box.n===1&&a.length===2&&a[0]===1&&a[1]===2;",
    );
}

/// N02: the RHS throw wins over a non-array TypeError. valueOf does not run.
#[test]
fn array_length165_n02_throw_beats_type() {
    guest_flag(
        "function f(){return {x:1};}var box={n:0,valueOf:function(){'use strict';this.n=9;return 1;},boom:function(){'use strict';this.n=this.n+1;throw 3;}};var got=0;try{f().length=box.boom();}catch(e){got=e;}got===3&&box.n===1;",
    );
}

/// N07: a string element increment must use the published number parser.
#[test]
fn array_length165_n07_string_element_inc() {
    guest_flag("var a=['2'];a[0]++;a[0]===3&&a.length===1;");
}

/// Tagged/packed, literal/dynamic, empty, hole, prefix/postfix, one key effect.
#[test]
fn array_length165_n07_tonumber_matrix() {
    guest_flag(
        "var n=0;var box={side:0,k:function(){'use strict';this.side=this.side+1;return 0;}};var a=['2'];var post=a[0]++;if(!(post===2&&a[0]===3))n=1;var pre=++a[0];if(n===0&&!(pre===4&&a[0]===4))n=2;var e=[''];var ep=e[0]++;if(n===0&&!(ep===0&&e[0]===1))n=3;var h=[1];h.length=2;var hole=h[1]++;if(n===0&&hole===hole)n=4;var d=['2'];var dp=d[box.k()]++;if(n===0&&!(box.side===1&&dp===2&&d[0]===3))n=5;var p=[1,2];var pp=p[0]++;if(n===0&&!(pp===1&&p[0]===2))n=6;var t=[true];var tp=t[0]++;if(n===0&&!(tp===1&&t[0]===2))n=7;n===0;",
    );
}

#[test]
fn micro_string_trim_ascii_public_value() {
    char_at_units("\" a \".trim();", &[0x0061]);
}

#[test]
fn micro_string_trim_end_ascii_public_value() {
    char_at_units("\" a \".trimEnd();", &[0x0020, 0x0061]);
}

#[test]
fn micro_string_trim_start_ascii_public_value() {
    char_at_units("\" a \".trimStart();", &[0x0061, 0x0020]);
}

#[test]
fn micro_string_trim_space_table_and_bounds() {
    // One call over every MicroQuickJS space unit; a non-empty result means a
    // listed unit was not treated as a space. Bounded Micro set, not complete
    // ES5.1 WhiteSpace. Neighbor and U+0085 witnesses are separate one-call
    // tests, each reading the trim method once.
    boxed(
        r#"var s="\u0009\u000A\u000B\u000C\u000D\u0020\u00A0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200A\u2028\u2029\u202F\u205F\u3000\uFEFF";s.trim()==="";"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_keeps_u0085_guest_only() {
    // U+0085 is not a MicroQuickJS space. Current Node trims it, so this
    // witness is guest Halted only. Bounded Micro P, not an ES5.1 claim.
    let result = execute(r#""\u0085a\u0085".trim()==="\u0085a\u0085";"#);
    let vm::Value::Map(map) = result else {
        panic!("full box: {result:?}");
    };
    assert_eq!(map.get(&vm::Value::string("tag")), Some(&vm::Value::Int(2)));
    assert_eq!(
        map.get(&vm::Value::string("flag")),
        Some(&vm::Value::Bool(true))
    );
}

#[test]
fn micro_string_trim_ascii_neighbors_kept() {
    // U+0008 and U+000E are the ASCII neighbors just outside 0x09..0x0D.
    // One trim property read and one call.
    boxed(
        r#"" \u0008a\u000E ".trim()==="\u0008a\u000E";"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_unicode_neighbors_kept() {
    // U+200B and U+2027 sit just outside 0x2000..0x200A and 0x2028..0x2029.
    boxed(
        r#"" \u200Ba\u2027 ".trim()==="\u200Ba\u2027";"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_upper_unicode_neighbors_kept() {
    // U+202A is just above 0x2028..0x2029 and U+2030 just above U+202F; the
    // middle fillers cover the other non-space neighbors in one call.
    boxed(
        r#"" \u202A\u007F\u0086\u00A1\u167F\u1681\u1FFF\u205E\u2060\u2FFF\u3001\uFEFEa\u2030 ".trim()==="\u202A\u007F\u0086\u00A1\u167F\u1681\u1FFF\u205E\u2060\u2FFF\u3001\uFEFEa\u2030";"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_method_value_and_length_zero() {
    // One trim property read per guest keeps the bounded frontend's body
    // small. The detached function is the installed prototype method; it is
    // called through the admitted Function.prototype.call with one thisArg.
    // Cross-receiver identity needs a second property read and is recorded as
    // an evidence gap, not claimed here.
    boxed(
        r#"var f="".trim;typeof f==="function"&&f.length===0&&f.call(" a ")==="a";"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_end_method_value_and_length_zero() {
    boxed(
        r#"var f="".trimEnd;typeof f==="function"&&f.length===0&&f.call(" a ")===" a";"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_start_method_value_and_length_zero() {
    boxed(
        r#"var f="".trimStart;typeof f==="function"&&f.length===0&&f.call(" a ")==="a ";"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_null_receiver_type_error() {
    boxed(
        r#"var f="".trim;var hit=0;try{f.call(null);}catch(e){if(e.name==="TypeError")hit=1;}hit===1;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_undefined_receiver_type_error() {
    boxed(
        r#"var f="".trim;var hit=0;try{f.call(undefined);}catch(e){if(e.name==="TypeError")hit=1;}hit===1;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_tostring_before_valueof() {
    boxed(
        r#"var f="".trim;var o={n:0,trim:f,toString:function(){"use strict";this.n=this.n+1;return " a ";},valueOf:function(){"use strict";this.n=this.n+10;return "NO";}};o.trim()==="a"&&o.n===1;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_extra_arg_before_dispatch() {
    boxed(
        r#"var f="".trimEnd;var o={n:0,trimEnd:f,toString:function(){"use strict";this.n=this.n+1;return " a ";}};function boom(b){b.n=b.n+10;throw 7;}var payload=0;try{o.trimEnd(boom(o));}catch(e){payload=e;}payload===7&&o.n===10;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_receiver_text_survives_forced_gc() {
    // The observer's UTF-16 bridge patch collects at entry after ToString
    // produced " AZ " and after trim rooted it at frame entry 2 but before
    // the units are read. The program returns the trimmed String itself, so
    // the observed completion value is a tag-4 String box; `.length` would
    // be a Number and could not witness the copied units.
    static OBS: TrimGcObserver = TrimGcObserver;
    let source =
        r#"var o={m:"".trim,toString:function(){"use strict";return " "+("A"+"Z")+" ";}};o.m();"#;
    let stack = observed_for(source, &OBS, "trim169-gc-observer", 0, true, PROBE_FUEL);
    let at = stack
        .iter()
        .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
        .unwrap()
        + 1;
    let vm::Value::Map(map) = &stack[at] else {
        panic!("String box");
    };
    assert_eq!(map.len(), 5);
    assert_eq!(map.get(&vm::Value::string("tag")), Some(&vm::Value::Int(4)));
    assert_eq!(
        &stack[at + 1..at + 4],
        &[
            vm::Value::Float(2.0),
            vm::Value::Float(65.0),
            vm::Value::Float(90.0)
        ]
    );
}

/// The bounded MicroQuickJS space table as JS source escapes. This is the
/// same 25-code-unit set the `space_table_and_bounds` witness uses; it is not
/// the complete ES5.1 WhiteSpace set.
const TRIM_SPACE_TABLE_ESCAPED: &str = r"\u0009\u000A\u000B\u000C\u000D\u0020\u00A0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200A\u2028\u2029\u202F\u205F\u3000\uFEFF";
const TRIM_SPACE_TABLE_UNITS: [u16; 25] = [
    0x0009, 0x000A, 0x000B, 0x000C, 0x000D, 0x0020, 0x00A0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003,
    0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000,
    0xFEFF,
];

#[test]
fn micro_string_trim_end_space_table_and_bounds() {
    // trimEnd strips every bounded Micro space unit from the right and keeps
    // the left table and `a` byte-for-byte. Bounded Micro P, not complete
    // ES5.1 WhiteSpace.
    let mut expected = TRIM_SPACE_TABLE_UNITS.to_vec();
    expected.push(0x0061);
    char_at_units(
        &format!(
            "\"{table}a{table}\".trimEnd();",
            table = TRIM_SPACE_TABLE_ESCAPED
        ),
        &expected,
    );
}

#[test]
fn micro_string_trim_start_space_table_and_bounds() {
    // trimStart strips every bounded Micro space unit from the left and
    // keeps `a` and the trailing table byte-for-byte.
    let expected: Vec<u16> = std::iter::once(0x0061)
        .chain(TRIM_SPACE_TABLE_UNITS.iter().copied())
        .collect();
    char_at_units(
        &format!(
            "\"{table}a{table}\".trimStart();",
            table = TRIM_SPACE_TABLE_ESCAPED
        ),
        &expected,
    );
}

#[test]
fn micro_string_trim_end_empty_and_all_space() {
    // A detached trimEnd over the exact empty and all-table receivers must
    // return the primitive String "". No `String` global is used and the
    // table is the bounded Micro set.
    boxed(
        &format!(
            r#"var f="".trimEnd;f.call("")===""&&f.call("{table}")==="";"#,
            table = TRIM_SPACE_TABLE_ESCAPED
        ),
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_start_empty_and_all_space() {
    // The trimStart counterpart of the empty/all-table witness.
    boxed(
        &format!(
            r#"var f="".trimStart;f.call("")===""&&f.call("{table}")==="";"#,
            table = TRIM_SPACE_TABLE_ESCAPED
        ),
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_end_nullish_receiver_type_error() {
    // Both null and undefined thisArg must throw TypeError for trimEnd,
    // counted in distinct try/catch blocks.
    boxed(
        r#"var f="".trimEnd;var hit=0;try{f.call(null);}catch(e){if(e.name==="TypeError")hit=hit+1;}try{f.call(undefined);}catch(e){if(e.name==="TypeError")hit=hit+1;}hit===2;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_start_nullish_receiver_type_error() {
    // Both null and undefined thisArg must throw TypeError for trimStart.
    boxed(
        r#"var f="".trimStart;var hit=0;try{f.call(null);}catch(e){if(e.name==="TypeError")hit=hit+1;}try{f.call(undefined);}catch(e){if(e.name==="TypeError")hit=hit+1;}hit===2;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_start_extra_arg_before_dispatch() {
    // The trimStart extra argument is evaluated before dispatch: boom
    // mutates the receiver and throws 7 before toString can run. boom takes
    // its receiver as a parameter, so no outer local is captured.
    boxed(
        r#"var f="".trimStart;var o={n:0,trimStart:f,toString:function(){"use strict";this.n=this.n+1;return " a ";}};function boom(b){b.n=b.n+10;throw 7;}var payload=0;try{o.trimStart(boom(o));}catch(e){payload=e;}payload===7&&o.n===10;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_trim_end_surrogate_units_kept() {
    // The trailing lone high surrogate is not a space: trimEnd keeps D800
    // and the preceding astral pair as exact UTF-16 units.
    char_at_units(
        "\" \\uD83D\\uDE00\\uD800 \".trimEnd();",
        &[0x0020, 0xD83D, 0xDE00, 0xD800],
    );
}

#[test]
fn micro_string_trim_start_surrogate_units_kept() {
    // The leading lone low surrogate is not a space: trimStart keeps DC00
    // and the following astral pair as exact UTF-16 units.
    char_at_units(
        "\" \\uDC00\\uD83D\\uDE00 \".trimStart();",
        &[0xDC00, 0xD83D, 0xDE00, 0x0020],
    );
}

#[test]
fn micro_string_replace_first_match_units() {
    char_at_units(r#""ababa".replace("ba", "X");"#, &[97, 88, 98, 97]);
}

#[test]
fn micro_string_replace_empty_search_inserts_at_zero() {
    char_at_units(r#""abc".replace("", "X");"#, &[88, 97, 98, 99]);
}

#[test]
fn micro_string_replace_not_found_keeps_text() {
    char_at_units(r#""abc".replace("z", "X");"#, &[97, 98, 99]);
}

#[test]
fn micro_string_replace_dollar_escape() {
    char_at_units(r#""aba".replace("b", "$$");"#, &[97, 36, 97]);
}

#[test]
fn micro_string_replace_match_token() {
    char_at_units(r#""abc".replace("b", "<$&>");"#, &[97, 60, 98, 62, 99]);
}

#[test]
fn micro_string_replace_prefix_token() {
    char_at_units(r#""abc".replace("b", "<$`>");"#, &[97, 60, 97, 62, 99]);
}

#[test]
fn micro_string_replace_suffix_token() {
    char_at_units(r#""abc".replace("b", "<$'>");"#, &[97, 60, 99, 62, 99]);
}

#[test]
fn micro_string_replace_numeric_tokens_literal() {
    char_at_units(r#""ab".replace("a", "$1$9$x$");"#, &[36, 49, 36, 57, 36, 120, 36, 98]);
}

#[test]
fn micro_string_replace_surrogates_preserved() {
    char_at_units(r#""\uD800\uDC00\uD800".replace("\uDC00", "<$&>");"#, &[0xd800, 60, 0xdc00, 62, 0xd800]);
}

#[test]
fn micro_string_replace_all_units() {
    char_at_units(r#""aaa".replaceAll("a", "b");"#, &[98, 98, 98]);
}

#[test]
fn micro_string_replace_all_not_found_keeps_text() {
    char_at_units(r#""abc".replaceAll("x", "y");"#, &[97, 98, 99]);
}

#[test]
fn micro_string_replace_all_empty_search_aaa() {
    char_at_units(r#""aaa".replaceAll("", "-");"#, &[45, 97, 45, 97, 45, 97, 45]);
}

#[test]
fn micro_string_replace_all_empty_search_ab() {
    char_at_units(r#""ab".replaceAll("", "-");"#, &[45, 97, 45, 98, 45]);
}

#[test]
fn micro_string_replace_all_empty_search_empty_receiver() {
    char_at_units(r#""".replaceAll("", "-");"#, &[45]);
}

#[test]
fn micro_string_replace_all_empty_search_empty_replacement() {
    char_at_units(r#""ab".replaceAll("", "");"#, &[97, 98]);
}

#[test]
fn micro_string_replace_all_non_overlapping() {
    char_at_units(r#""aaaa".replaceAll("aa", "b");"#, &[98, 98]);
}

#[test]
fn micro_string_replace_all_match_token() {
    char_at_units(r#""ab".replaceAll("a", "$&");"#, &[97, 98]);
}

#[test]
fn micro_string_replace_all_dollar_escape() {
    char_at_units(r#""ab".replaceAll("a", "$$");"#, &[36, 98]);
}

#[test]
fn micro_string_replace_all_dollar_escape_adjacent() {
    char_at_units(r#""ab".replaceAll("a", "$$$$");"#, &[36, 36, 98]);
}

#[test]
fn micro_string_replace_all_numeric_token_literal() {
    char_at_units(r#""ab".replaceAll("a", "$1");"#, &[36, 49, 98]);
}

#[test]
fn micro_string_replace_all_numeric_token_upto_two_digits() {
    char_at_units(r#""ab".replaceAll("a", "$12x");"#, &[36, 49, 50, 120, 98]);
}

#[test]
fn micro_string_replace_all_dollar_tail_and_unknown_are_literal() {
    char_at_units(r#""ab".replaceAll("a", "$");"#, &[36, 98]);
    // "$x" is not a recognized token: the "$" stays literal, so "a" -> "$x".
    char_at_units(r#""ab".replaceAll("a", "$x");"#, &[36, 120, 98]);
}

#[test]
fn micro_string_replace_all_prefix_token_per_match() {
    // Every empty-pattern hit expands $` against its own position.
    char_at_units(r#""ab".replaceAll("", "$`");"#, &[97, 97, 98, 97, 98]);
}

#[test]
fn micro_string_replace_all_suffix_token_per_match() {
    // Every empty-pattern hit expands $' against its own position.
    char_at_units(r#""ab".replaceAll("", "$'");"#, &[97, 98, 97, 98, 98]);
}

#[test]
fn micro_string_replace_all_number_pattern_to_string() {
    char_at_units(r#""a1b1".replaceAll(1, "X");"#, &[97, 88, 98, 88]);
}

#[test]
fn micro_string_replace_all_undefined_pattern_to_string() {
    char_at_units(r#""aub".replaceAll(undefined, "X");"#, &[97, 117, 98]);
}

#[test]
fn micro_string_replace_all_number_replacement_to_string() {
    char_at_units(r#""a1b".replaceAll("1", 7);"#, &[97, 55, 98]);
}

#[test]
fn micro_string_replace_all_missing_replacement_to_string() {
    char_at_units(r#""aub".replaceAll("u");"#, &[97, 117, 110, 100, 101, 102, 105, 110, 101, 100, 98]);
}

#[test]
fn micro_string_replace_all_no_arguments() {
    char_at_units(r#""undefined!".replaceAll();"#, &[117, 110, 100, 101, 102, 105, 110, 101, 100, 33]);
}

#[test]
fn micro_string_replace_all_surrogates_preserved() {
    char_at_units(r#""\uD800\uDC00\uD800".replaceAll("\uDC00", "<$&>");"#, &[0xd800, 60, 0xdc00, 62, 0xd800]);
}

#[test]
fn micro_string_replace_all_rejects_object_pattern() {
    // Node ToStrings an object pattern; this bounded adapter throws the
    // managed TypeError, so assert the exact name without the Node oracle.
    vm_boxed(
        r#"var ok=false;try{"ab".replaceAll({}, "x");}catch(e){if(e.name==="TypeError"){ok=true;}}ok;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_replace_all_rejects_function_replacement() {
    // Node calls a function replacement; this bounded adapter throws the
    // managed TypeError, so assert the exact name without the Node oracle.
    vm_boxed(
        r#"var ok=false;try{"ab".replaceAll("a", function(){return "X";});}catch(e){if(e.name==="TypeError"){ok=true;}}ok;"#,
        2,
        0.0,
        true,
    );
}

#[test]
fn micro_string_replace_all_public_length() {
    char_at_units(r#"(typeof "".replaceAll==="function"&&"".replaceAll.length===2)?"Y":"N";"#, &[89]);
}

#[test]
fn micro_string_replace_all_nullish_receiver_type_error() {
    char_at_units(r#"var f="".replaceAll;var n=0;try{f.call(null,"a","b");}catch(e){if(e.name==="TypeError")n=n+1;}try{f.call(undefined,"a","b");}catch(e){if(e.name==="TypeError")n=n+1;}(n===2)?"Y":"N";"#, &[89]);
}

#[test]
fn micro_string_replace_tostring_order_once() {
    char_at_units(r#"var b={n:0};var s={b:b,toString:function(){"use strict";this.b.n=this.b.n*10+1;return "b";}};var r={b:b,toString:function(){"use strict";this.b.n=this.b.n*10+2;return "$&";}};("abc".replace(s,r)==="abc"&&b.n===12)?"Y":"N";"#, &[89]);
}

#[test]
fn micro_string_replace_nullish_receiver_type_error() {
    char_at_units(r#"var f="".replace;var n=0;try{f.call(null,"a","b");}catch(e){if(e.name==="TypeError")n=n+1;}try{f.call(undefined,"a","b");}catch(e){if(e.name==="TypeError")n=n+1;}(n===2)?"Y":"N";"#, &[89]);
}

#[test]
fn micro_string_replace_public_method_length() {
    char_at_units(r#"(typeof "".replace==="function"&&"".replace.length===2)?"Y":"N";"#, &[89]);
}

#[test]
fn micro_string_replace_default_arguments_to_string() {
    char_at_units(r#""undefined!".replace();"#, &[117, 110, 100, 101, 102, 105, 110, 101, 100, 33]);
    char_at_units(r#""abc".replace("b");"#, &[97, 117, 110, 100, 101, 102, 105, 110, 101, 100, 99]);
}

#[test]
fn micro_string_replace_empty_receiver_empty_search() {
    char_at_units(r#""".replace("", "!");"#, &[33]);
}

#[test]
fn micro_string_replace_search_tostring_throw_skips_value() {
    char_at_units(
        r#"var b={n:0};var s={toString:function(){throw new Error("boom");}};var r={b:b,toString:function(){"use strict";this.b.n=this.b.n+1;return "X";}};var caught=false;try{"abc".replace(s,r);}catch(e){caught=e.name==="TypeError"||e.name==="Error";}(caught&&b.n===0)?"Y":"N";"#,
        &[89],
    );
}

#[test]
fn micro_string_replace_argument_expression_order() {
    char_at_units(
        r#"var n=0;var result="abc".replace((n=n*10+1,"b"),(n=n*10+2,"X"));(n===12&&result==="aXc")?"Y":"N";"#,
        &[89],
    );
}

#[test]
fn micro_string_replace_not_found_value_still_converts() {
    char_at_units(
        r#"var b={n:0};var r={b:b,toString:function(){"use strict";this.b.n=this.b.n+1;return "X";}};var result="abc".replace("z",r);(b.n===1&&result==="abc")?"Y":"N";"#,
        &[89],
    );
}

#[test]
fn micro_string_split_nullish_receiver_type_error() {
    char_at_units(r#"var f="".split;var n=0;try{f.call(null,",");}catch(e){if(e.name==="TypeError")n=n+1;}try{f.call(undefined,",");}catch(e){if(e.name==="TypeError")n=n+1;}n===2?"Y":"N";"#, &[89]);
}
#[test]
fn micro_string_split_method_length_and_array_proto() {
    guest_flag(r#"var a="a,b".split(",");typeof "".split==="function"&&"".split.length===2&&a.length===2&&a[0]==="a"&&a[1]==="b"&&Object.getPrototypeOf(a)===Object.getPrototypeOf([]);"#);
}
#[test]
fn micro_string_split_undefined_separator() {
    char_at_units(r#"var a="a,b".split(undefined);a.length===1?a[0]:"N";"#, &[97,44,98]);
}
#[test]
fn micro_string_split_empty_input_undefined_separator() {
    guest_flag(r#"var a="".split();a.length===1&&a[0]==="";"#);
}
#[test]
fn micro_string_split_literal_metacharacter() {
    char_at_units(r#"var a="a.b.c".split(".");a.length===3?a[0]+a[1]+a[2]:"N";"#, &[97,98,99]);
}
#[test]
fn micro_string_split_adjacent_and_trailing_separator() {
    guest_flag(r#"var a="a,,".split(",");a.length===3&&a[0]==="a"&&a[1]===""&&a[2]==="";"#);
}
#[test]
fn micro_string_split_missing_separator() {
    guest_flag(r#"var a="abc".split("z");a.length===1&&a[0]==="abc";"#);
}
#[test]
fn micro_string_split_empty_separator_units() {
    guest_flag(r#"var a="ab".split("");a.length===2&&a[0]==="a"&&a[1]==="b";"#);
}
#[test]
fn micro_string_split_empty_input_empty_separator() {
    guest_flag(r#""".split("").length===0;"#);
}
#[test]
fn micro_string_split_surrogate_pair_units() {
    char_at_units(r#"var a="\uD83D\uDE00".split("");a.length===2?a[0]+a[1]:"N";"#, &[0xD83D,0xDE00]);
}
#[test]
fn micro_string_split_lone_surrogate_units() {
    char_at_units(r#"var a="\uD800-\uDC00".split("-");a.length===2?a[0]+a[1]:"N";"#, &[0xD800,0xDC00]);
}
#[test]
fn micro_string_split_limit_zero_nan() {
    guest_flag(r#"var a="a,b".split(",",0);var b="a,b".split(",",NaN);a.length===0&&b.length===0;"#);
}
#[test]
fn micro_string_split_limit_fraction() {
    guest_flag(r#"var a="a,b,c".split(",",1.9);a.length===1&&a[0]==="a";"#);
}
#[test]
fn micro_string_split_limit_unsigned_wrap() {
    guest_flag(r#"var a="a,b,c".split(",",-1);var b="a,b".split(",",4294967297);a.length===3&&b.length===1&&b[0]==="a";"#);
}
#[test]
fn micro_string_split_limit_conversion_order() {
    char_at_units(r#"var b={n:0};var s={b:b,toString:function(){"use strict";this.b.n=this.b.n*10+2;return ",";}};var lim={b:b,valueOf:function(){"use strict";this.b.n=this.b.n*10+1;return 0;}};var a="a,b".split(s,lim);(b.n===12&&a.length===0)?"Y":"N";"#, &[89]);
}
#[test]
fn micro_string_split_replace_trim_coexist_gc() {
    // The bridge observer collects after the split receiver and separator
    // have been rooted, including while trim and replace share the adapter.
    static OBS: TrimGcObserver = TrimGcObserver;
    let source = r#"var a=" a,b, ".trim().split(",");(a.length===3&&a[0].replace("a","x")==="x"&&a[1]==="b"&&a[2]==="")?"YZ":"NO";"#;
    let stack = observed_for(source, &OBS, "trim169-gc-observer", 0, true, PROBE_FUEL);
    let at = stack.iter().rposition(|v| *v == vm::Value::string("P7_OBSERVED")).unwrap() + 1;
    let vm::Value::Map(value) = &stack[at] else { panic!("String box") };
    assert_eq!(value.get(&vm::Value::string("tag")), Some(&vm::Value::Int(4)));
    assert_eq!(&stack[at + 1..at + 4], &[vm::Value::Float(2.0), vm::Value::Float(89.0), vm::Value::Float(90.0)]);
    char_at_units_oracle(source, &[89, 90]);
}

#[test]
fn micro_type_error_typeof_function() {
    guest_flag(r#"typeof TypeError==="function";"#);
}
#[test]
fn micro_type_error_length_one() {
    guest_flag("TypeError.length===1;");
}
#[test]
fn micro_type_error_prototype_name() {
    guest_flag(r#"TypeError.prototype.name==="TypeError";"#);
}
#[test]
fn micro_error_prototype_name() {
    guest_flag(r#"var p = Object.getPrototypeOf(new Error("boom")); p.name === "Error" && p.hasOwnProperty("name");"#);
}
#[test]
fn micro_error_prototype_to_string() {
    // `String(ErrorInstance)` and `"x" + ErrorInstance` are outside this
    // frontend's admitted domain (the String call helper refuses Object
    // arguments, and binary Add requires primitive operands), so the
    // observable route is the direct prototype method call.
    guest_flag(r#"var p = Object.getPrototypeOf(new Error("boom"));
var ok = new Error("boom").toString() === "Error: boom";
ok = ok && new Error("boom").toString(1, 2) === "Error: boom";
ok = ok && typeof p.toString === "function" && p.toString.length === 0;
ok = ok && p.hasOwnProperty("toString");
ok;"#);
}
#[test]
fn micro_error_prototype_to_string_name() {
    guest_flag(r#"var e = new Error("boom");
e.name = "N";
var a = e.toString() === "N: boom";
e.name = "";
var b = e.toString() === ": boom";
e.name = undefined;
var c = e.toString() === "Error: boom";
a && b && c;"#);
}
#[test]
fn micro_error_prototype_to_string_message_own_row() {
    // Documented domain divergence: this realm stores the message in the
    // instance's own data row, so assignment succeeds and the result follows.
    // MicroQuickJS keeps it in an internal slot behind a setter-less accessor
    // and the assignment itself throws TypeError.
    guest_flag(r#"var e = new Error("boom");
e.message = "z";
e.toString() === "Error: z" && e.message === "z";"#);
}
#[test]
fn micro_error_prototype_to_string_brand() {
    guest_flag(r#"var t = Object.getPrototypeOf(new Error("boom")).toString;
var a = false; try { t.call({}); } catch (err) { a = err.name === "TypeError"; }
var b = false; try { t.call(null); } catch (err) { b = err.name === "TypeError"; }
var c = false; try { t.call(5); } catch (err) { c = err.name === "TypeError"; }
var d = false; try { t.call(new Uint8Array(1)); } catch (err) { d = err.name === "TypeError"; }
var e = false; try { TypeError.prototype.toString(); } catch (err) { e = err.name === "TypeError"; }
a && b && c && d && e;"#);
}
#[test]
fn micro_error_prototype_to_string_type_error_empty_message() {
    guest_flag(r#"new TypeError().toString() === "TypeError: ";"#);
}
#[test]
fn micro_error_prototype_to_string_name_to_string() {
    guest_flag(r#"var e = new Error("boom");
e.name = { toString: function(){ return "Q"; } };
e.toString() === "Q: boom";"#);
}
#[test]
fn micro_error_instance_to_string_optional_errors() {
    // The optional error allocators publish instances, never prototypes: each
    // one must satisfy the Error brand gate and render its realm name with the
    // unconditional ": " separator even though no own message row exists.
    guest_flag(r#"var a = new SyntaxError().toString() === "SyntaxError: ";
var b = new EvalError().toString() === "EvalError: ";
var c = new ReferenceError().toString() === "ReferenceError: ";
var d = new InternalError().toString() === "InternalError: ";
a && b && c && d;"#);
}
#[test]
fn micro_error_instance_to_string_radix_range_error() {
    // `Number.prototype.toString` builds its own RangeError with one message
    // row through the private range-error allocator.
    guest_flag(r#"var text = "";
try { (5).toString(1); } catch (e) { text = e.toString(); }
text === "RangeError: radix must be between 2 and 36";"#);
}
#[test]
fn micro_error_instance_to_string_internal_uri_error() {
    // The URI adapter's malformed-input Result carries the private URIError
    // instance; it has no own message row, so only the name renders.
    guest_flag(r#"var text = "";
try { decodeURIComponent("%"); } catch (e) { text = e.toString(); }
text === "URIError: ";"#);
}
#[test]
fn micro_error_instance_to_string_typed_array_subscript() {
    // A write one past the view's length is the realm's "invalid array
    // subscript" TypeError, allocated with its own `message` row.
    guest_flag(r#"var a = new Uint8Array(new ArrayBuffer(3), 1, 2); var text = "";
try { a[2] = 1; } catch (e) { text = e.toString(); }
text === "TypeError: invalid array subscript";"#);
}
#[test]
fn micro_error_instance_to_string_callback_type_errors() {
    // The array sort/reduce adapters allocate their own TypeError instances
    // with a `message` row; the brand gate must accept them as instances.
    guest_flag(r#"var ok = false;
try { var a = [2, 1]; a.sort(5); } catch (e) { ok = e.toString() === "TypeError: not a function"; }
var empty = false; try { [].reduce(function(x, y){ return x + y; }); } catch (e) { empty = e.toString() === "TypeError: empty array"; }
ok && empty;"#);
}
#[test]
fn micro_error_instance_brand_prototypes_still_reject() {
    // Prototypes are plain object shells (brand 0, upstream `[object Object]`);
    // only the instance brand admits a receiver, so both must keep throwing
    // TypeError. `Error.prototype` and `TypeError.prototype` are reached
    // through the realm constructor's prototype row because the bare
    // `Error`/`TypeError` globals are not bindings on this slice.
    guest_flag(r#"var p = Object.getPrototypeOf(new Error("boom"));
var a = false; try { p.toString(); } catch (e) { a = e.name === "TypeError"; }
var t = Object.getPrototypeOf(new TypeError());
var b = false; try { t.toString(); } catch (e) { b = e.name === "TypeError"; }
var s = Object.getPrototypeOf(new SyntaxError());
var c = false; try { s.toString(); } catch (e) { c = e.name === "TypeError"; }
a && b && c;"#);
}
#[test]
fn micro_error_prototype_object_class_string() {
    // Upstream MicroQuickJS reports every Error-family prototype as an
    // ordinary object; only instances answer `[object Error]`.
    guest_flag(r#"var t = Object.prototype.toString;
var ep = Object.getPrototypeOf(new Error("boom"));
var tp = Object.getPrototypeOf(new TypeError());
var a = t.call(ep) === "[object Object]";
var b = t.call(tp) === "[object Object]";
var c = t.call(new Error("boom")) === "[object Error]";
a && b && c;"#);
}
#[test]
fn micro_error_prototype_to_string_call_extra_args() {
    // Arity 0: `.call` must accept and ignore arguments after thisArg, while
    // null/ordinary-object receivers still fail the brand gate.
    guest_flag(r#"var e = new Error("boom");
var ts = Object.getPrototypeOf(e).toString;
var a = ts.call(e) === "Error: boom";
var b = ts.call(e, 1, 2) === "Error: boom";
var c = false; try { ts.call(null); } catch (err) { c = err.name === "TypeError"; }
var d = false; try { ts.call({}); } catch (other) { d = other.name === "TypeError"; }
a && b && c && d;"#);
}
#[test]
fn micro_error_prototype_to_string_call_extra_args_typed_array() {
    // Same admission through the TypedArray-enabled branch of the `.call`
    // adapter: code 286 still ignores arguments after thisArg.
    guest_flag(r#"var b = new ArrayBuffer(1);
var v = new Uint8Array(b);
var e = new Error("boom");
var ts = Object.getPrototypeOf(e).toString;
ts.call(e, 1, 2) === "Error: boom" && ts.call(e) === "Error: boom" && v.length === 1;"#);
}
#[test]
fn micro_uri_error_prototype_name() {
    guest_flag(r#"var ok = false; try { decodeURIComponent("%"); } catch (e) { ok = Object.getPrototypeOf(e).name === "URIError"; } ok;"#);
}
#[test]
fn micro_public_syntax_error_length_one() {
    guest_flag(r#"SyntaxError.length === 1;"#);
}

#[test]
fn micro_public_syntax_error_prototype_name() {
    guest_flag(r#"SyntaxError.prototype.name === "SyntaxError" && SyntaxError.prototype.hasOwnProperty("name");"#);
}

#[test]
fn micro_public_syntax_error_construction() {
    guest_flag(r#"var p = SyntaxError.prototype;
var a = SyntaxError(); var b = new SyntaxError(); var c = SyntaxError("x"); var d = new SyntaxError("x");
var ok = Object.getPrototypeOf(a) === p && Object.getPrototypeOf(b) === p;
ok = ok && Object.getPrototypeOf(c) === p && Object.getPrototypeOf(d) === p;
ok = ok && !a.hasOwnProperty("message") && !b.hasOwnProperty("message");
ok = ok && !c.hasOwnProperty("message") && !d.hasOwnProperty("message");
ok = ok && Object.getPrototypeOf(p) === Object.getPrototypeOf(new Error("boom"));
ok && a !== b && c !== d;"#);
}

#[test]
fn micro_public_syntax_error_shadowed() {
    guest_flag(r#"var SyntaxError = 1; SyntaxError === 1;"#);
}

#[test]
fn micro_public_internal_error_length_one() {
    guest_flag(r#"InternalError.length === 1;"#);
}

#[test]
fn micro_public_internal_error_prototype_name() {
    guest_flag(r#"InternalError.prototype.name === "InternalError" && InternalError.prototype.hasOwnProperty("name");"#);
}

#[test]
fn micro_public_internal_error_construction() {
    guest_flag(r#"var p = InternalError.prototype;
var a = InternalError(); var b = new InternalError(); var c = InternalError("x"); var d = new InternalError("x");
var ok = Object.getPrototypeOf(a) === p && Object.getPrototypeOf(b) === p;
ok = ok && Object.getPrototypeOf(c) === p && Object.getPrototypeOf(d) === p;
ok = ok && !a.hasOwnProperty("message") && !b.hasOwnProperty("message");
ok = ok && !c.hasOwnProperty("message") && !d.hasOwnProperty("message");
ok = ok && Object.getPrototypeOf(p) === Object.getPrototypeOf(new Error("boom"));
ok && a !== b && c !== d;"#);
}

#[test]
fn micro_public_internal_error_shadowed() {
    guest_flag(r#"var InternalError = 1; InternalError === 1;"#);
}

#[test]
fn micro_public_uri_error_length_one() {
    guest_flag(r#"URIError.length === 1;"#);
}

#[test]
fn micro_public_uri_error_prototype_name() {
    guest_flag(r#"URIError.prototype.name === "URIError" && URIError.prototype.hasOwnProperty("name");"#);
}

#[test]
fn micro_public_uri_error_construction() {
    guest_flag(r#"var p = URIError.prototype;
var a = URIError(); var b = new URIError(); var c = URIError("x"); var d = new URIError("x");
var ok = Object.getPrototypeOf(a) === p && Object.getPrototypeOf(b) === p;
ok = ok && Object.getPrototypeOf(c) === p && Object.getPrototypeOf(d) === p;
ok = ok && !a.hasOwnProperty("message") && !b.hasOwnProperty("message");
ok = ok && !c.hasOwnProperty("message") && !d.hasOwnProperty("message");
ok = ok && Object.getPrototypeOf(p) === Object.getPrototypeOf(new Error("boom"));
ok && a !== b && c !== d;"#);
}

#[test]
fn micro_public_uri_error_shadowed() {
    guest_flag(r#"var URIError = 1; URIError === 1;"#);
}

#[test]
fn public_uri_error_shares_internal_prototype() {
    guest_flag(r#"var created = new URIError();
var ok = false;
try { decodeURIComponent("%"); } catch (e) {
    ok = Object.getPrototypeOf(e) === Object.getPrototypeOf(created);
}
ok;"#);
}

#[test]
fn micro_eval_error_prototype_name() {
    guest_flag(r#"EvalError.prototype.name === "EvalError" && EvalError.prototype.hasOwnProperty("name");"#);
}
#[test]
fn micro_reference_error_prototype_name() {
    guest_flag(r#"ReferenceError.prototype.name === "ReferenceError" && ReferenceError.prototype.hasOwnProperty("name");"#);
}
#[test]
fn micro_eval_error_length_one() {
    guest_flag("EvalError.length === 1;");
}
#[test]
fn micro_reference_error_length_one() {
    guest_flag("ReferenceError.length === 1;");
}
#[test]
fn micro_eval_error_construction() {
    guest_flag(r#"var p = EvalError.prototype;
var a = EvalError(); var b = new EvalError(); var c = EvalError("x"); var d = new EvalError("x");
var ok = Object.getPrototypeOf(a) === p && Object.getPrototypeOf(b) === p;
ok = ok && Object.getPrototypeOf(c) === p && Object.getPrototypeOf(d) === p;
ok = ok && Object.getPrototypeOf(d).name === "EvalError";
ok = ok && !a.hasOwnProperty("message") && !d.hasOwnProperty("message");
ok = ok && Object.getPrototypeOf(p) === Object.getPrototypeOf(new Error("boom"));
ok && a !== b && c !== d;"#);
}
#[test]
fn micro_reference_error_construction() {
    guest_flag(r#"var p = ReferenceError.prototype;
var a = ReferenceError(); var b = new ReferenceError(); var c = ReferenceError("x"); var d = new ReferenceError("x");
var ok = Object.getPrototypeOf(a) === p && Object.getPrototypeOf(b) === p;
ok = ok && Object.getPrototypeOf(c) === p && Object.getPrototypeOf(d) === p;
ok = ok && Object.getPrototypeOf(d).name === "ReferenceError";
ok = ok && !a.hasOwnProperty("message") && !d.hasOwnProperty("message");
ok = ok && Object.getPrototypeOf(p) === Object.getPrototypeOf(new Error("boom"));
ok && a !== b && c !== d;"#);
}
#[test]
fn micro_eval_and_reference_error_prototypes_are_distinct() {
    // The frontend admits only the `new Error("boom")` construction.
    guest_flag(
        r#"var evalProto = Object.getPrototypeOf(new EvalError());
var referenceProto = Object.getPrototypeOf(new ReferenceError());
var errorProto = Object.getPrototypeOf(new Error("boom"));
evalProto !== referenceProto && Object.getPrototypeOf(evalProto) === errorProto &&
    Object.getPrototypeOf(referenceProto) === errorProto;"#,
    );
}

#[test]
fn micro_eval_error_shadowed() {
    guest_flag("var EvalError = 1; EvalError === 1;");
}
#[test]
fn micro_reference_error_shadowed() {
    guest_flag("var ReferenceError = 1; ReferenceError === 1;");
}
#[test]
fn micro_type_error_prototype_message() {
    guest_flag(r#"TypeError.prototype.message==="";"#);
}
#[test]
fn micro_type_error_new_name() {
    guest_flag(r#"new TypeError().name==="TypeError";"#);
}
#[test]
fn micro_type_error_new_message() {
    guest_flag(r#"new TypeError().message==="";"#);
}
#[test]
fn micro_type_error_new_prototype() {
    guest_flag("Object.getPrototypeOf(new TypeError())===TypeError.prototype;");
}
#[test]
fn error_351_type_error_instance_brand() {
    static OBS: object_observer::Observer = object_observer::Observer {
        type_error: true,
        units: None,
        force_gc: false,
        uncaught: false,
        realm_header: false,
        collect_phases: false,
        collect_stop: None,
    };
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            let mut machine = probe_vm(
                "new TypeError();",
                &OBS,
                "javascript-es5-object-observer",
            );
            assert_eq!(machine.run().unwrap(), vm::VmStatus::Halted);
            let stack = machine.stack();
            let at = stack.iter().rposition(|v| *v == vm::Value::string("P7_OBSERVED")).unwrap();
            assert_eq!(stack[at + 2], vm::Value::Float(3.0), "ordinary Object kind");
            assert_eq!(stack[at + 3], vm::Value::Float(7.0), "legacy class, extensible, and real Error brand");
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn micro_type_error_new_distinct() {
    guest_flag("new TypeError()!==new TypeError();");
}
#[test]
fn micro_type_error_call_name() {
    guest_flag(r#"TypeError().name==="TypeError";"#);
}
#[test]
fn micro_type_error_call_prototype() {
    guest_flag("Object.getPrototypeOf(TypeError())===TypeError.prototype;");
}
#[test]
fn micro_type_error_throw_catch() {
    guest_flag(r#"try { throw new TypeError(); } catch (e) { e.name === "TypeError"; }"#);
}
#[test]
fn micro_type_error_no_own_message() {
    guest_flag(r#"var e=new TypeError();e.hasOwnProperty("message")===false;"#);
}

#[test]
fn array_push_empty() { guest_flag("var a=[];a.push()===0&&a.length===0;"); }
#[test]
fn array_push_one() { guest_flag("var a=[];a.push(7)===1&&a.length===1&&a[0]===7;"); }
#[test]
fn array_push_many_order() { guest_flag("var a=[1];a.push(2,3,4)===4&&a[1]===2&&a[2]===3&&a[3]===4;"); }
#[test]
fn array_push_alias() { guest_flag("var a=[];var b=a;a.push(9);b.length===1&&b[0]===9;"); }
#[test]
fn array_push_alias_after_mutation_reads_length() { guest_flag("var a=[];a.push(1);var b=a;b[0]===1 && b.length===1;"); }
#[test]
fn array_push_after_shrink() { guest_flag("var a=[1,2,3];a.length=1;a.push(4)===2&&a[1]===4&&a[2]===undefined;"); }
#[test]
fn array_push_tagged_values() { char_at_units("var a=[];var o={x:1};a.push('A',o,true);(a.length===3&&a[0]==='A'&&a[1]===o&&a[2]===true)?a[0]:'N';", &[65]); }
#[test]
fn array_push_at_extent_limit() { guest_flag("var a=[];a.length=13105;a.push()===13105&&a.length===13105;"); }
#[test]
fn array_push_prototype_namespace() { guest_flag("var a=[];var p=Object.getPrototypeOf(a);p.hasOwnProperty('push')&&p.push===a.push&&typeof p.push==='function'&&p.push.length===1&&!a.hasOwnProperty('push');"); }
#[test]
fn array_pop_empty() { guest_flag("var a=[];a.pop()===undefined&&a.length===0;"); }
#[test]
fn array_pop_packed() { guest_flag("var a=[1,2];a.pop()===2&&a.length===1&&a[1]===undefined;"); }
#[test]
fn array_pop_tagged_identity() { guest_flag("var a=[];var o={};a.push(o);a.pop()===o&&a.length===0;"); }
#[test]
fn array_pop_after_grow() { guest_flag("var a=[1];a.length=3;a.pop()===undefined&&a.length===2;"); }
#[test]
fn array_pop_no_resurrect() { guest_flag("var a=[1,2];a.pop();a.length=2;a[1]===undefined;"); }
#[test]
fn array_pop_alias_after_mutation_reads_length() { guest_flag("var a=[7,8];a.pop();var b=a;b.length===1 && b[1]===undefined;"); }
#[test]
fn array_pop_prototype_namespace() { guest_flag("var a=[];var p=Object.getPrototypeOf(a);p.hasOwnProperty('pop')&&p.pop===a.pop&&typeof p.pop==='function'&&p.pop.length===0&&!a.hasOwnProperty('pop');"); }

#[test]
fn array_shift_empty_one() { guest_flag("var a=[];var b=[7];a.shift()===undefined&&a.length===0&&b.shift()===7&&b.length===0&&b[0]===undefined;"); }
#[test]
fn array_shift_many_order() { guest_flag("var a=[1,2,3,4];a.shift()===1&&a.length===3&&a[0]===2&&a[1]===3&&a[2]===4&&a[3]===undefined;"); }
#[test]
fn array_shift_tagged_identity() { char_at_units("var a=[];var o={};a.push(o,'X',true);(a.shift()===o&&a[0]==='X'&&a[1]===true&&a.length===2)?a[0]:'N';", &[88]); }
#[test]
fn array_shift_grown_holes_no_resurrect() { guest_flag("var a=[1,2];a.length=5;a.shift();var ok=a.length===4&&a[0]===2&&a[1]===undefined&&a[3]===undefined;a.length=1;a.length=4;ok&&a[1]===undefined&&a[3]===undefined;"); }
#[test]
fn array_shift_alias_shape() { guest_flag("var a=[1,2];var b=a;a['shift']();var n=b.length;var x=b[0];b[0]=9;n===1&&x===2&&a.length===1&&a[0]===9;"); }
#[test]
fn array_shift_borrowed_type_error() { guest_flag("var f=[].shift;var o={shift:f};var ok=false;try{o.shift();}catch(e){ok=e.name==='TypeError';}ok;"); }
#[test]
fn array_unshift_zero_args() { guest_flag("var a=[4];a.unshift()===1&&a.length===1&&a[0]===4;"); }
#[test]
fn array_unshift_one_empty() { guest_flag("var a=[];a.unshift(7)===1&&a.length===1&&a[0]===7;"); }
#[test]
fn array_unshift_many_order() { guest_flag("var a=[3,4];a.unshift(1,2)===4&&a.length===4&&a[0]===1&&a[1]===2&&a[2]===3&&a[3]===4;"); }
#[test]
fn array_unshift_tagged_identity() { char_at_units("var a=[];var o={};a.push(o,'Z');var p={};var n=a.unshift('A',p);(n===4&&a[0]==='A'&&a[1]===p&&a[2]===o&&a[3]==='Z')?a[3]:'N';", &[90]); }
#[test]
fn array_unshift_grown_holes() { guest_flag("var a=[1];a.length=3;a.unshift(7,8)===5&&a.length===5&&a[0]===7&&a[1]===8&&a[2]===1&&a[3]===undefined&&a[4]===undefined;"); }
#[test]
fn array_unshift_alias_shape() { guest_flag("var a=[2];var b=a;a['unshift'](1);var n=b.length;var x=b[1];b[0]=3;n===2&&x===2&&a.length===2&&a[0]===3&&a[1]===2;"); }
#[test]
fn array_unshift_borrowed_type_error() { guest_flag("var f=[].unshift;var o={unshift:f};var ok=false;try{o.unshift(1);}catch(e){ok=e.name==='TypeError';}ok;"); }
#[test]
fn array_unshift_prototype_namespace_limits() { guest_flag("var a=[1];var p=Object.getPrototypeOf(a);var ok=p.hasOwnProperty('shift')&&p.hasOwnProperty('unshift')&&p.shift===a.shift&&p.unshift===a.unshift&&p.shift.length===0&&p.unshift.length===1&&!a.hasOwnProperty('shift')&&!a.hasOwnProperty('unshift');a.length=13105;ok&&a.shift()===1&&a.length===13104&&a[13103]===undefined&&a.unshift()===13104;"); }
#[test]
fn array_reverse_packed_order() { guest_flag("var a=[1,2,3];var r=a.reverse();r===a&&a.length===3&&a[0]===3&&a[1]===2&&a[2]===1;"); }
#[test]
fn array_reverse_empty_and_one() { guest_flag("var e=[];var er=e.reverse();var one=[7];var orr=one.reverse();er===e&&e.length===0&&orr===one&&one[0]===7&&one.length===1;"); }
#[test]
fn array_reverse_odd_middle() { guest_flag("var a=[1,2,3,4,5];var r=a.reverse();r===a&&a.length===5&&a[0]===5&&a[1]===4&&a[2]===3&&a[3]===2&&a[4]===1;"); }
#[test]
fn array_reverse_object_identity() { guest_flag("var x={id:1};var y={id:2};var a=[x,y];var r=a.reverse();r===a&&a.length===2&&a[0]===y&&a[1]===x&&y.id===2&&x.id===1;"); }
#[test]
fn array_reverse_string_identity() { guest_flag("var a=['a','b','c'];var r=a.reverse();r===a&&a.length===3&&a[0]==='c'&&a[1]==='b'&&a[2]==='a';"); }
#[test]
fn array_reverse_borrowed_type_error() { guest_flag("var o={0:1,1:2,length:2};var f=[].reverse;var ok=false;try{f.call(o);}catch(e){ok=e.name==='TypeError';}ok&&o[0]===1&&o[1]===2&&o.length===2;"); }
#[test]
fn array_reverse_length_cell_unchanged() { guest_flag("var a=[1,2,3];a.length=5;var r=a.reverse();var i=0;var v0=a[i];i=4;r===a&&a.length===5&&v0===3&&a[i]===undefined;"); }
#[test]
fn array_reverse_returns_same() { guest_flag("var a=[1,2,3];var r=a['reverse']();r===a;"); }
#[test]
fn array_reverse_holes_present_bits() { guest_flag("var a=[1,2];a.length=4;var before=Object.keys(a).length;var own=a.hasOwnProperty('2')&&a.hasOwnProperty('3');var r=a.reverse();var i=0;var v0=a[i];i=1;var v1=a[i];i=2;var h2=a[i]===undefined;i=3;var h3=a[i]===undefined;r===a&&a.length===4&&before===4&&Object.keys(a).length===4&&own&&a.hasOwnProperty('2')&&a.hasOwnProperty('3')&&v0===2&&v1===1&&h2&&h3;"); }
#[test]
fn array_reverse_method_value() { guest_flag("var a=[1,2];var p=Object.getPrototypeOf(a);p.hasOwnProperty('reverse')&&p.reverse===a.reverse&&p.reverse.length===0&&!a.hasOwnProperty('reverse')&&a.reverse()===a&&a[0]===2&&a[1]===1;"); }
#[test]
fn array_reverse_string_increment() { guest_flag("var a=['2','9'];var r=a.reverse();var old=a[0]++;r===a&&old===9&&a[0]===10&&a[1]==='2'&&a.length===2;"); }

#[test]
fn array_reverse_packed_odd() { guest_flag("var a=[1,2,3];var r=a.reverse();r===a&&a.length===3&&a[0]===3&&a[1]===2&&a[2]===1;"); }

#[test]
fn array_reverse_packed_even() { guest_flag("var a=[1,2,3,4];var r=a.reverse();r===a&&a.length===4&&a[0]===4&&a[1]===3&&a[2]===2&&a[3]===1;"); }

#[test]
fn array_reverse_empty_and_single() { guest_flag("var e=[];var er=e.reverse();var one=[7];var orr=one.reverse();er===e&&e.length===0&&orr===one&&one[0]===7&&one.length===1;"); }

#[test]
fn array_reverse_tagged_values() { guest_flag("var s='Z';var o={id:7};var n=9;var a=[s,o,n];var r=a.reverse();r===a&&a[0]===n&&a[1]===o&&a[2]===s&&o.id===7;"); }

#[test]
fn array_reverse_alias_shape() { guest_flag("var a=[1,2,3];var f=a.reverse;var r=f.call(a);var i=0;var v0=a[i];i=1;var v1=a[i];i=2;var v2=a[i];r===a&&a.length===3&&v0===3&&v1===2&&v2===1;"); }

#[test]
fn array_reverse_borrowed_ordinary_object_type_error() { guest_flag("var o={0:1,1:2,length:2};var f=[].reverse;var ok=false;try{f.call(o);}catch(e){ok=e.name==='TypeError';}ok&&o[0]===1&&o[1]===2&&o.length===2;"); }

#[test]
fn array_reverse_extra_args_ignored() { guest_flag("var a=[1,2];var r=a.reverse(9,'x');r===a&&a.length===2&&a[0]===2&&a[1]===1;"); }

#[test]
fn array_reverse_length_property_untouched() { guest_flag("var a=[1,2,3];var r=a.reverse();r===a&&a.length===3&&a[0]===3&&a[2]===1;"); }

#[test]
fn array_reverse_method_value_and_length_zero() { guest_flag("var a=[1];var p=Object.getPrototypeOf(a);typeof p.reverse==='function'&&p.reverse.length===0&&p.reverse===a.reverse&&!a.hasOwnProperty('reverse');"); }

#[test]
fn array_reverse_n07_string_element_inc() { guest_flag("var a=['2','9'];var r=a.reverse();var old=a[0]++;r===a&&old===9&&a[0]===10&&a[1]==='2'&&a.length===2;"); }

/// `layout::ARRAY_HEADER_CELLS` is `pub(super)`; the packed literal path bakes
/// `header + index` into the cell address (`lower_packed_array_literal_read`).
const PACKED_ARRAY_HEADER_CELLS: i64 = 6;

fn packed_direct_cell_offset(args: &[vm::Expr]) -> Option<i64> {
    let [vm::Expr::Var(_), vm::Expr::Div(addr, scale)] = args else {
        return None;
    };
    let vm::Expr::Float(scale) = scale.as_ref() else {
        return None;
    };
    if *scale != 1.0 {
        return None;
    }
    let vm::Expr::Add(base, offset) = addr.as_ref() else {
        return None;
    };
    let vm::Expr::Var(_) = base.as_ref() else {
        return None;
    };
    let vm::Expr::Int(offset) = offset.as_ref() else {
        return None;
    };
    Some(*offset)
}

fn walk_expr(expr: &vm::Expr, visit: &mut dyn FnMut(&vm::Expr)) {
    visit(expr);
    use vm::Expr::*;
    match expr {
        Null | Unit | Int(_) | Float(_) | Bool(_) | String(_) | Bytes(_) | Var(_) | MoveVar(_)
        | MoveField { .. } | MoveIndex { .. } | FunctionRef(_, _) | ModuleFunctionRef(_, _)
        | UnresolvedFunctionRef { .. } => {}
        OptionalGet { container, key, .. } => {
            walk_expr(container, visit);
            walk_expr(key, visit);
        }
        OptionUnwrapOr { value, fallback, .. } => {
            walk_expr(value, visit);
            walk_expr(fallback, visit);
        }
        Call(_, _, args, _, _) | ModuleCall(_, _, args, _) | LocalCall(_, _, args, _) => {
            for arg in args {
                walk_expr(arg, visit);
            }
        }
        Closure(closure) => walk_expr(&closure.body, visit),
        ClosureCall(closure, args) => {
            walk_expr(&closure.body, visit);
            for arg in args {
                walk_expr(arg, visit);
            }
        }
        Add(left, right)
        | Sub(left, right)
        | Mul(left, right)
        | Div(left, right)
        | Mod(left, right)
        | And(left, right)
        | Or(left, right)
        | Eq(left, right)
        | Lt(left, right)
        | Gt(left, right) => {
            walk_expr(left, visit);
            walk_expr(right, visit);
        }
        Neg(inner) | Not(inner) | ToOwned(inner) | Borrow(inner) | BorrowMut(inner) => {
            walk_expr(inner, visit);
        }
        IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, visit);
            walk_expr(then_expr, visit);
            walk_expr(else_expr, visit);
        }
        Match {
            value,
            arms,
            default,
            ..
        } => {
            walk_expr(value, visit);
            for (_, arm) in arms {
                walk_expr(arm, visit);
            }
            walk_expr(default, visit);
        }
        Block { stmts, expr } => {
            for statement in stmts {
                walk_stmt(statement, visit);
            }
            walk_expr(expr, visit);
        }
        ResultConstruct { payload, .. } | ResultQuery { value: payload, .. } | ResultTry { value: payload, .. }
        | ResultConvert { value: payload, .. } => walk_expr(payload, visit),
        ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            walk_expr(value, visit);
            walk_expr(ok_expr, visit);
            walk_expr(err_expr, visit);
        }
        ResultMethod {
            receiver, argument, ..
        }
        | OptionMethod {
            receiver, argument, ..
        } => {
            walk_expr(receiver, visit);
            walk_expr(argument, visit);
        }
        OptionConstruct { payload, .. } => {
            if let Some(payload) = payload {
                walk_expr(payload, visit);
            }
        }
        OptionMatch {
            value,
            some_expr,
            none_expr,
            ..
        } => {
            walk_expr(value, visit);
            walk_expr(some_expr, visit);
            walk_expr(none_expr, visit);
        }
    }
}

fn walk_stmt(statement: &vm::Stmt, visit: &mut dyn FnMut(&vm::Expr)) {
    match statement {
        vm::Stmt::Let { expr, .. } | vm::Stmt::Assign { expr, .. } | vm::Stmt::Expr { expr, .. } => {
            walk_expr(expr, visit);
        }
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            walk_expr(condition, visit);
            for item in then_branch.iter().chain(else_branch) {
                walk_stmt(item, visit);
            }
        }
        vm::Stmt::For {
            init,
            condition,
            post,
            body,
            ..
        } => {
            walk_stmt(init, visit);
            walk_expr(condition, visit);
            walk_stmt(post, visit);
            for item in body {
                walk_stmt(item, visit);
            }
        }
        vm::Stmt::While { condition, body, .. } => {
            walk_expr(condition, visit);
            for item in body {
                walk_stmt(item, visit);
            }
        }
        vm::Stmt::ClosureLet { closure, .. } => walk_expr(&closure.body, visit),
        vm::Stmt::Noop { .. }
        | vm::Stmt::FuncDecl { .. }
        | vm::Stmt::Break { .. }
        | vm::Stmt::Continue { .. }
        | vm::Stmt::Drop { .. } => {}
    }
}

/// Guest `array_reverse_literal_index_after_direct`.
///
/// The script has no source function, so the two literal reads live in the
/// top-level script entry `ir.stmts` (the activation frame in `lower_with_realm`),
/// not in a helper. `__compat_es5_array_get` and the reverse mutator stay in
/// `function_impls` and are not walked: a helper-internal direct read would
/// not distinguish these guest sites.
fn assert_user_entry_packed_literal_reads(source: &str) {
    let ir = parse(source).unwrap_or_else(|error| panic!("lower {source}: {error:?}"));
    let array_get = helper(&ir, "__compat_es5_array_get");
    let cell_read = helper(&ir, "__compat_ref_read");
    let helper_fn = ir
        .functions
        .iter()
        .find(|function| function.index == array_get)
        .unwrap_or_else(|| panic!("array_get index {array_get} has no decl"));
    assert_eq!(helper_fn.name, "__compat_es5_array_get");
    assert!(
        ir.function_impls.contains_key(&array_get),
        "helper body exists and is excluded from the user-entry walk"
    );
    assert!(
        ir.functions
            .iter()
            .all(|function| !function.name.starts_with("__compat_es5_fn_")),
        "this guest source has no nested function; reads belong in ir.stmts"
    );
    let mut offsets = Vec::new();
    let mut array_get_calls = 0usize;
    let mut visit = |expr: &vm::Expr| {
        let vm::Expr::Call(index, _, args, _, _) = expr else {
            return;
        };
        if *index == array_get {
            array_get_calls += 1;
        }
        if *index == cell_read {
            if let Some(offset) = packed_direct_cell_offset(args) {
                offsets.push(offset);
            }
        }
    };
    for statement in &ir.stmts {
        walk_stmt(statement, &mut visit);
    }
    let element: Vec<i64> = offsets
        .iter()
        .copied()
        .filter(|offset| *offset >= PACKED_ARRAY_HEADER_CELLS)
        .collect();
    eprintln!(
        "REVERSE_IR_ENTRY guest=array_reverse_literal_index_after_direct entry=ir.stmts packed_element_offsets={element:?} array_get_calls={array_get_calls} helper={array_get} cell_read={cell_read} helper_body_walked=false"
    );
    assert_eq!(
        element,
        vec![
            PACKED_ARRAY_HEADER_CELLS,
            PACKED_ARRAY_HEADER_CELLS + 1
        ],
        "user entry a[0]/a[1] must be packed direct cell reads (header+index), not a folded or helper path; script cell offsets={offsets:?}"
    );
    assert_eq!(
        array_get_calls, 0,
        "user entry must not call __compat_es5_array_get; that is the shape-invalidated path"
    );
}

#[test]
fn array_reverse_literal_index_after_direct() {
    const SOURCE: &str = "var a=[1,2];var r=a.reverse();r===a&&a[0]===2&&a[1]===1;";
    assert_user_entry_packed_literal_reads(SOURCE);
    guest_flag(SOURCE);
}

fn number_namespace_float(source: &str) -> f64 {
    let vm::Value::Map(map) = execute(source) else { panic!("expected box: {source}"); };
    assert_eq!(map.get(&vm::Value::string("tag")), Some(&vm::Value::Int(3)), "{source}");
    let Some(vm::Value::Float(n)) = map.get(&vm::Value::string("number")) else {
        panic!("expected Float: {source}");
    };
    *n
}

fn number_namespace_ir(source: &str) -> Result<vm::FrontendIr, vm::ParseError> {
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

fn number_namespace_roots(ir: &vm::FrontendIr) -> Vec<i64> {
    let id = |name| ir.functions.iter().find(|f| f.name == name).unwrap().index;
    let bootstrap = id("__compat_es5_intrinsic_bootstrap");
    let allocate = id("__compat_es5_object_allocate");
    ir.function_impls[&bootstrap].body_stmts.iter()
        .filter_map(|stmt| match stmt {
            vm::Stmt::Assign { expr: vm::Expr::Call(id, _, args, _, _), .. } if *id == allocate => match args.as_slice() {
                [_, vm::Expr::Int(4), _, vm::Expr::Int(11), vm::Expr::Int(299), _, _, _, vm::Expr::Int(root)] => Some(*root),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn number_max_value_literal_and_bits() {
    let n = number_namespace_float("var n=Number.MAX_VALUE; n===1.7976931348623157e308 ? n : 0;");
    assert_eq!(n.to_bits(), 0x7fef_ffff_ffff_ffff);
}

#[test]
fn number_min_value_subnormal_bits() {
    let n = number_namespace_float("var n=Number.MIN_VALUE; n>0 && n===5e-324 ? n : 0;");
    assert_eq!(n.to_bits(), 1);
}

#[test]
fn number_nan_value() {
    assert!(number_namespace_float("var n=Number.NaN; n!==n ? n : 0;").is_nan());
}

#[test]
fn number_negative_infinity_signed_reciprocal() {
    let n = number_namespace_float("var n=Number.NEGATIVE_INFINITY; var r=1/n; n===-Infinity && r===0 ? r : 1;");
    assert_eq!(n.to_bits(), 0x8000_0000_0000_0000);
}

#[test]
fn number_positive_infinity_signed_reciprocal() {
    let n = number_namespace_float("var n=Number.POSITIVE_INFINITY; var r=1/n; n===Infinity && r===0 ? r : 1;");
    assert_eq!(n.to_bits(), 0);
}

#[test]
fn number_epsilon_exact() {
    let n = number_namespace_float("var n=Number.EPSILON; 1+n>1 ? n : 0;");
    assert_eq!(n.to_bits(), 0x3cb0_0000_0000_0000);
}

#[test]
fn number_safe_integer_endpoints() {
    guest_flag("var hi=Number.MAX_SAFE_INTEGER; var lo=Number.MIN_SAFE_INTEGER; hi===9007199254740991 && lo===-9007199254740991 && hi!==9007199254740992 && lo!==-9007199254740992;");
}

#[test]
fn number_alias_dot_and_computed_get() {
    guest_flag("var n=Number; var key='MAX_VALUE'; var other={MAX_VALUE:17}; n===Number && n.MAX_VALUE===n[key] && n[key]===1.7976931348623157e308 && other[key]===17;");
}

#[test]
fn number_own_descriptor_and_enumeration() {
    guest_flag(r#"
        var n=Number;
        var keys=['MAX_VALUE','MIN_VALUE','NaN','NEGATIVE_INFINITY','POSITIVE_INFINITY','EPSILON','MAX_SAFE_INTEGER','MIN_SAFE_INTEGER'];
        var i=0; var ok=true;
        while(i<8){
            var key=keys[i]; var d=Object.getOwnPropertyDescriptor(n,key); var v=n[key];
            ok=ok && n.hasOwnProperty(key) && d.writable===false && d.enumerable===false && d.configurable===false
                && (key==='NaN' ? d.value!==d.value : d.value===v);
            i++;
        }
        ok && !n.hasOwnProperty('missing') && Object.getOwnPropertyDescriptor(n,'missing')===undefined && Object.keys(n).length===0;
    "#);
}

#[test]
fn number_length_row() {
    guest_flag(r#"
        var d=Object.getOwnPropertyDescriptor(Number,'length');
        Number.length===1 && d.value===1 && d.writable===false
            && d.enumerable===false && d.configurable===false;
    "#);
}

#[test]
fn number_readonly_write_and_delete() {
    guest_flag(r#"
        function strictCheck(n){
            'use strict';
            var write=false; var remove=false;
            try{n.MAX_VALUE=3;}catch(e){write=e.name==='TypeError';}
            try{delete n.MAX_VALUE;}catch(e){remove=e.name==='TypeError';}
            return write && remove;
        }
        var n=Number; var before=n.MAX_VALUE;
        n.MAX_VALUE=7;
        var removed=delete n['MAX_VALUE'];
        !removed && n.hasOwnProperty('MAX_VALUE') && n.MAX_VALUE===before && strictCheck(n);
    "#);
}

#[test]
fn number_shadow_does_not_select_manifest() {
    let source = "function local(){var Number={MAX_VALUE:17}; return Number.MAX_VALUE;} function arg(Number){return Number.MAX_VALUE;} local()===17 && arg({MAX_VALUE:23})===23;";
    let ir = number_namespace_ir(source).expect("shadowed Number is an ordinary name");
    assert!(number_namespace_roots(&ir).is_empty());
    guest_flag(source);
}

#[test]
fn number_script_shadow() {
    let expression = "Number.MAX_VALUE===17;";
    let source = format!("var Number={{MAX_VALUE:17}}; {expression}");
    let shadowed = number_namespace_ir(&source).expect("script-local Number is an ordinary name");
    let unshadowed = number_namespace_ir(expression).expect("unshadowed Number selects the manifest");
    assert!(number_namespace_roots(&shadowed).is_empty());
    assert_eq!(number_namespace_roots(&unshadowed), [11]);
    guest_flag(&source);
}

#[test]
fn number_function_body_only_selects_manifest() {
    let source = "function read(){return Number.MAX_VALUE;} read();";
    let ir = number_namespace_ir(source).expect("body-only demand must be frozen before bootstrap");
    assert_eq!(number_namespace_roots(&ir), [11]);
    assert_eq!(number_namespace_float(source).to_bits(), 0x7fef_ffff_ffff_ffff);
}

#[test]
fn number_borrowed_call_type_error() {
    guest_flag(r#"
        var o={f:Number}; var ok=false;
        try{o.f(1);}catch(e){ok=e.name==='TypeError';}
        ok;
    "#);
}

#[test]
fn number_namespace_excluded_calls() {
    // `new Number(1)` is not a compile refusal: this slice throws the realm
    // TypeError at run time, and only the catchable `name` is part of it.
    guest_flag(r#"var c=false;try{new Number(1);}catch(e){c=e.name==="TypeError";}c;"#);
    // The carrier is the ES5.1 15.7.1 `Number(value)` function: the direct
    // call and an alias both convert. `Number.prototype` is the private Number
    // prototype, whose `constructor` row names the same carrier; `toString` is
    // the admitted Number-prototype method, the formatters stay absent.
    guest_flag(r#"
        var n=Number;
        var x=n.EPSILON;
        Number(1)===1 && n("12")===12
            && typeof n.parseInt==='function' && typeof n.parseFloat==='function'
            && typeof n.prototype==='object' && n.prototype.constructor===n
            && x.toExponential===undefined && x.toFixed===undefined && x.toPrecision===undefined
            && typeof x.toString==='function';
    "#);
}

fn number_parse_layout(ir: &vm::FrontendIr) -> (i64, Vec<(i64, i64, bool)>) {
    let id = |name: &str| {
        ir.functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .index
    };
    let bootstrap = id("__compat_es5_intrinsic_bootstrap");
    let allocate = id("__compat_es5_object_allocate");
    let enter = id("__compat_es5_root_enter");
    let body = &ir.function_impls[&bootstrap].body_stmts;
    let mut permanent = None;
    let mut capacity = 0;
    for stmt in body {
        if let vm::Stmt::Assign {
            index,
            expr: vm::Expr::Call(fn_id, _, args, _, _),
            ..
        } = stmt
        {
            if *fn_id == enter {
                if let vm::Expr::Int(cap) = args[1] {
                    if cap > 8 {
                        permanent = Some(*index);
                        capacity = cap;
                    }
                }
            }
        }
    }
    let permanent = permanent.expect("permanent root frame");
    let parses = body
        .iter()
        .filter_map(|stmt| {
            let vm::Stmt::Assign {
                expr: vm::Expr::Call(fn_id, _, args, _, _),
                ..
            } = stmt
            else {
                return None;
            };
            if *fn_id != allocate {
                return None;
            }
            match args.as_slice() {
                [_, vm::Expr::Int(4), _, vm::Expr::Int(rows), vm::Expr::Int(code), _, _, vm::Expr::Var(frame), _]
                    if *code == 530 || *code == 531 =>
                {
                    Some((*code, *rows, *frame == permanent))
                }
                _ => None,
            }
        })
        .collect();
    (capacity, parses)
}

#[test]
fn number_parse_int_static_only() {
    let source = r#"Number.parseInt("42")===42 && Number.parseInt("  11")===11;"#;
    let ir = number_namespace_ir(source).expect("static Number.parseInt");
    let (capacity, parses) = number_parse_layout(&ir);
    assert_eq!(number_namespace_roots(&ir), [11]);
    assert_eq!(capacity, 12, "static parseInt must not publish a bare global root");
    assert_eq!(parses, vec![(530, 1, false), (531, 1, false)]);
    guest_flag(source);
}

#[test]
fn number_parse_float_static_only() {
    let source = r#"Number.parseFloat("3.5e1")===35;"#;
    let ir = number_namespace_ir(source).expect("static Number.parseFloat");
    let (capacity, parses) = number_parse_layout(&ir);
    assert_eq!(capacity, 12, "static parseFloat must not publish a bare global root");
    assert!(parses.iter().all(|(_, _, permanent)| !permanent));
    guest_flag(source);
}

#[test]
fn number_parse_int_identity() {
    let source = "var number_parse_int_identity=Number.parseInt===parseInt; number_parse_int_identity;";
    let ir = number_namespace_ir(source).expect("identity selects both");
    let (_, parses) = number_parse_layout(&ir);
    assert_eq!(parses.iter().filter(|(code, _, permanent)| *code == 530 && *permanent).count(), 1);
    guest_flag(source);
}

#[test]
fn number_parse_float_identity() {
    let source = "var number_parse_float_identity=Number.parseFloat===parseFloat; number_parse_float_identity;";
    let ir = number_namespace_ir(source).expect("identity selects both");
    let (_, parses) = number_parse_layout(&ir);
    assert_eq!(parses.iter().filter(|(code, _, permanent)| *code == 531 && *permanent).count(), 1);
    guest_flag(source);
}

#[test]
fn number_parse_assigned_property_independent() {
    guest_flag(r#"
        var f=Number.parseInt;
        Number.parseInt=17;
        f("10")===10 && Number.parseInt===17 && parseInt("8")===8;
    "#);
}

#[test]
fn number_parse_computed_and_extracted() {
    guest_flag(r#"
        var p=Number.parseInt;
        Number["parseInt"]("0x10")===16 && p("7")===7 && Number["parseFloat"]("1.5")===1.5;
    "#);
}

#[test]
fn number_parse_shadow() {
    let expression = "Number.parseInt===1 && Number.parseFloat===2;";
    let source = format!("var Number={{parseInt:1,parseFloat:2}}; {expression}");
    let shadowed = number_namespace_ir(&source).expect("script-local Number");
    assert!(number_namespace_roots(&shadowed).is_empty());
    guest_flag(&source);
}

#[test]
fn number_parse_arity() {
    guest_flag(r#"Number.parseInt.length===2 && Number.parseFloat.length===1 && parseInt.length===2 && parseFloat.length===1;"#);
}

#[test]
fn number_parse_radix_prefix() {
    guest_flag(r#"
        parseInt("0x1f",16)===31 && parseInt("08",10)===8
            && Number.parseInt("0x1f",16)===31 && Number.parseInt("08",10)===8
            && Number.parseInt("0x10",10)===0;
    "#);
}

#[test]
fn number_parse_nan_and_sign() {
    guest_flag(r#"var n=Number.parseInt("abc"); isNaN(n) && isNaN(Number.parseFloat("nope"));"#);
    let z = number_namespace_float(r#"var z=Number.parseFloat("-0"); z===0 ? z : 1;"#);
    assert_eq!(z.to_bits(), (-0.0f64).to_bits());
}

#[test]
fn number_parse_conversion_order() {
    guest_flag(r#"
        var input={n:0,toString:function(){'use strict'; this.n=this.n+1; return '42';},valueOf:function(){'use strict'; this.n=this.n+100; return 7;}};
        var once=Number.parseInt(input,10)===42 && input.n===1;
        var input2={n:0,toString:function(){'use strict'; this.n=this.n+1; return '9';}};
        var radix={seen:0,valueOf:function(){'use strict'; throw 'radix';},toString:function(){'use strict'; this.seen=1; return '10';}};
        var thrown=false;
        try{Number.parseInt(input2, radix);}catch(e){thrown=e==='radix';}
        once && thrown && input2.n===1 && radix.seen===0;
    "#);
}

#[test]
fn number_parse_borrowed_call_type_error() {
    guest_flag(r#"
        var o={f:Number.parseInt}; var ok=false;
        try{o.f("1");}catch(e){ok=e.name==='TypeError';}
        ok;
    "#);
}

#[test]
fn number_parse_namespace_budget() {
    let object = number_namespace_ir("Object;").expect("control");
    let number = number_namespace_ir("Number;").expect("Number only");
    let both = number_namespace_ir("parseInt;parseFloat;Number;").expect("shared globals");
    let (object_cap, object_parses) = number_parse_layout(&object);
    let (number_cap, number_parses) = number_parse_layout(&number);
    let (_, both_parses) = number_parse_layout(&both);
    assert_eq!(object_cap, 11);
    assert!(object_parses.is_empty(), "no-Number control must not allocate parse functions");
    assert_eq!(number_cap, 12, "static methods must not add a bare global root");
    assert_eq!(number_parses, vec![(530, 1, false), (531, 1, false)]);
    assert_eq!(number_namespace_roots(&number), [11]);
    assert_eq!(both_parses, vec![(530, 1, true), (531, 1, true)]);
}

fn boolean_call_roots(ir: &vm::FrontendIr) -> Vec<i64> {
    let Some(bootstrap) = ir.functions.iter().find(|f| f.name == "__compat_es5_intrinsic_bootstrap") else {
        return Vec::new();
    };
    let Some(allocate) = ir.functions.iter().find(|f| f.name == "__compat_es5_object_allocate") else {
        return Vec::new();
    };
    ir.function_impls[&bootstrap.index]
        .body_stmts
        .iter()
        .filter_map(|stmt| match stmt {
            vm::Stmt::Assign { expr: vm::Expr::Call(id, _, args, _, _), .. } if *id == allocate.index => {
                match args.as_slice() {
                    [_, vm::Expr::Int(4), _, vm::Expr::Int(1), vm::Expr::Int(305), _, _, _, vm::Expr::Int(root)] => {
                        Some(*root)
                    }
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

#[test]
fn boolean_call_no_args_false() {
    guest_flag("Boolean()===false&&typeof Boolean()==='boolean';");
}

#[test]
fn boolean_call_falsy_values() {
    guest_flag(
        "Boolean(0)===false&&Boolean(NaN)===false&&Boolean('')===false&&Boolean(null)===false&&Boolean(undefined)===false;",
    );
}

#[test]
fn boolean_call_truthy_values() {
    guest_flag(
        "Boolean(1)===true&&Boolean('0')===true&&Boolean('false')===true&&Boolean({})===true&&Boolean([])===true&&Boolean(function(){return 1;})===true;",
    );
}

#[test]
fn boolean_call_extra_args_ignored() {
    guest_flag("Boolean(0,1,'x')===false;");
}

#[test]
fn boolean_call_arity_and_typeof() {
    guest_flag("Boolean.length===1&&typeof Boolean==='function';");
}

#[test]
fn boolean_call_borrowed() {
    guest_flag("var b=Boolean;b(0)===false&&b(1)===true;");
}

#[test]
fn boolean_call_nested_truthiness() {
    guest_flag("var branch=Boolean(0)?1:2;branch===2&&Boolean(Boolean(0))===false&&Boolean(Boolean(1))===true;");
}

#[test]
fn boolean_call_object_to_bool_once() {
    guest_flag(
        "var o={v:0,t:0,valueOf:function(){'use strict'; this.v=this.v+1; return 1;},toString:function(){'use strict'; this.t=this.t+1; return 'x';}};Boolean(o)===true&&o.v===0&&o.t===0;",
    );
}

#[test]
fn boolean_shadow_local() {
    let source = "function f(){var Boolean=function(){return 7;};return Boolean();}f()===7;";
    guest_flag(source);
    let ir = parse(source).expect("shadowed local Boolean");
    let roots = boolean_call_roots(&ir);
    assert!(roots.is_empty(), "shadowed Boolean must not install code 305: {roots:?}");
    eprintln!("BOOLEAN_SHADOW_LOCAL roots={roots:?}");
}

#[test]
fn boolean_shadow_script() {
    let source = "var Boolean=function(){return 9;};Boolean()===9;";
    guest_flag(source);
    let ir = parse(source).expect("shadowed script Boolean");
    let roots = boolean_call_roots(&ir);
    assert!(roots.is_empty(), "script-shadowed Boolean must not install code 305: {roots:?}");
    eprintln!("BOOLEAN_SHADOW_SCRIPT roots={roots:?}");
}

#[test]
fn boolean_new_form_is_runtime_type_error() {
    let source = "var c=false;try{new Boolean(1);}catch(e){c=e.name===\"TypeError\";}c;";
    guest_flag(source);
    eprintln!(
        "BOOLEAN_NEW_FORM runtime=TypeError source={source:?} basis=pinned engine rejects construction with a catchable TypeError, not a compile refusal"
    );
}

#[test]
fn array_index_of_packed_first() {
    guest_flag("[1,2,3].indexOf(1)===0;");
}

#[test]
fn array_index_of_packed_middle() {
    guest_flag("[1,2,3].indexOf(2)===1;");
}

#[test]
fn array_index_of_miss() {
    guest_flag("[1,2,3].indexOf(9)===-1&&typeof [1,2,3].indexOf(9)==='number';");
}

#[test]
fn array_index_of_nan_never_matches() {
    guest_flag("[NaN].indexOf(NaN)===-1&&[1,NaN,2].indexOf(NaN)===-1;");
}

#[test]
fn array_index_of_negative_zero() {
    guest_flag("[0].indexOf(-0)===0&&[-0].indexOf(0)===0;");
}

#[test]
fn array_index_of_from_index_clamp() {
    guest_flag("var a=[10,20,30,40,50];a.indexOf(50,-2)===4&&a.indexOf(30,-2)===-1&&a.indexOf(10,-10)===0&&a.indexOf(10,10)===-1&&a.indexOf(50,5)===-1&&a.indexOf(30,1.9)===2&&a.indexOf(20,2.1)===-1&&a.indexOf(30,2.9)===2;");
}

/// fromIndex valueOf pushes a match at the new tail. Micro re-reads length
/// after ToInt32Clamp, so the forward search must hit that new index.
/// `owner:a` is a parse error (numeric arrays cannot be object values);
/// `id(a)` is Dynamic, so the object can hold the array without a capture.
#[test]
fn array_index_of_from_index_mutates_array() {
    guest_flag("var id=function(x){return x;};var a=[1,2];var p={n:0,owner:id(a),valueOf:function(){'use strict';this.n=this.n+1;var owner=this.owner;owner.push(9);return 0;}};a.indexOf(9,p)===2&&p.n===1;");
    eprintln!("ARRAY_INDEXOF_MUTATE hit=2 valueOf_called=1 from_index=0");
}

#[test]
fn array_last_index_of_packed_last() {
    guest_flag("[1,2,1].lastIndexOf(1)===2;");
}

#[test]
fn array_last_index_of_duplicate_reverse_order() {
    guest_flag("[1,2,1,2].lastIndexOf(1)===2&&[7,7,7].lastIndexOf(7)===2;");
}

#[test]
fn array_last_index_of_miss() {
    guest_flag("[1,2,3].lastIndexOf(9)===-1;");
}

#[test]
fn array_last_index_of_from_index_clamp() {
    guest_flag("var a=[1,2,1,2];a.lastIndexOf(1,0)===0&&a.lastIndexOf(2,0)===-1&&a.lastIndexOf(2,-1)===-1&&a.lastIndexOf(2,-2)===1&&a.lastIndexOf(1,-2)===2&&a.lastIndexOf(2,2.9)===1&&a.lastIndexOf(2,0.9)===-1;");
}

/// fromIndex valueOf unshifts a match at the new head. Micro re-reads the
/// array after ToInt32Clamp and searches down from the clamped index, so the
/// new head is visible. Return 1 so a fromIndex echo cannot pass as a hit.
/// `id(a)` is required: object values cannot be numeric arrays.
#[test]
fn array_last_index_of_mutates_array() {
    guest_flag("var id=function(x){return x;};var a=[1,2];var p={n:0,owner:id(a),valueOf:function(){'use strict';this.n=this.n+1;var owner=this.owner;owner.unshift(7);return 1;}};a.lastIndexOf(7,p)===0&&p.n===1;");
    eprintln!("ARRAY_LASTINDEXOF_MUTATE hit=0 valueOf_called=1 from_index=1");
}

/// fromIndex valueOf pops the tail, records that return, then returns 1.
/// Micro must re-read length and reclamp the reverse start. Old index 1 is
/// past the new length; an unreclamp scan reads that hole as undefined and
/// reports 1. `id(a)` holds the array. `p.removed===2` proves pop ran; a
/// later guest `a.length` read is not this witness.
#[test]
fn array_last_index_of_shrink_reclamps_from_index() {
    let source = "var id=function(x){return x;};var a=[1,2];var p={n:0,removed:0,owner:id(a),valueOf:function(){'use strict';this.n=this.n+1;var owner=this.owner;this.removed=owner.pop();return 1;}};var hit=a.lastIndexOf(undefined,p);var shown=999;if(p.n===1&&p.removed===2){shown=hit;}shown;";
    let shown = number_namespace_float(source);
    if shown == 999.0 {
        eprintln!("ARRAY_LASTINDEXOF_SHRINK hit=unmeasured valueOf_called=mismatch pop_returned=mismatch sentinel=999");
        panic!("valueOf count or pop return was not the required witness");
    }
    let hit = shown as i64;
    eprintln!("ARRAY_LASTINDEXOF_SHRINK hit={hit} valueOf_called=1 pop_returned=2");
    assert_eq!(shown, -1.0, "measured hit={hit}; unreclamp would report 1");
}

#[test]
fn array_index_of_extra_args_ignored() {
    guest_flag("[1,2,3].indexOf(2,0,99)===1&&[1,2,1].lastIndexOf(1,2,99)===2;");
}

#[test]
fn array_index_of_borrowed_type_error() {
    guest_flag("var o={0:1,length:1};var f=[].indexOf;var ok=false;try{f.call(o,1);}catch(e){ok=e.name==='TypeError';}ok&&o[0]===1&&o.length===1;");
}

/// `f.call(o,1)` dies in Function.prototype.call's count==1 gate and never
/// reaches the Array brand check. A one-argument call does reach it: a plain
/// object is rejected and left unchanged, and the same shape on [undefined]
/// returns 0.
#[test]
fn array_index_of_brand_one_arg_call() {
    let source = "var f=[].indexOf;var o={0:1,length:1};var rejected=false;try{f.call(o);}catch(e){rejected=e.name==='TypeError';}var hit=f.call([undefined]);var shown=999;if(rejected&&o[0]===1&&o.length===1){shown=hit;}shown;";
    let shown = number_namespace_float(source);
    if shown == 999.0 {
        eprintln!("ARRAY_INDEXOF_BRAND reject=mismatch object_unchanged=mismatch success_hit=unmeasured sentinel=999");
        panic!("brand rejection or unchanged object was not witnessed");
    }
    let hit = shown as i64;
    eprintln!("ARRAY_INDEXOF_BRAND reject=TypeError object_unchanged=true success_hit={hit}");
    assert_eq!(shown, 0.0, "f.call([undefined]) measured {hit}, expected 0");
}

#[test]
fn array_index_of_arity_and_method_value() {
    guest_flag("var a=[1];var p=Object.getPrototypeOf(a);typeof a.indexOf==='function'&&a.indexOf.length===1&&typeof a.lastIndexOf==='function'&&a.lastIndexOf.length===1&&p.indexOf===a.indexOf&&p.lastIndexOf===a.lastIndexOf;");
}

#[test]
fn array_slice_basic() {
    guest_flag("var r=[1,2,3].slice(1);r.length===2&&r[0]===2&&r[1]===3;");
    eprintln!("ARRAY_SLICE_BASIC len=2 e0=2 e1=3");
}

#[test]
fn array_slice_negative_indices() {
    guest_flag("var a=[1,2,3];var r=a.slice(-1);var s=a.slice(-2,-1);r.length===1&&r[0]===3&&s.length===1&&s[0]===2;");
    eprintln!("ARRAY_SLICE_CLAMP neg1=3 neg2to1=2");
}

#[test]
fn array_slice_clamp_over_range() {
    guest_flag("var a=[1,2,3];var hi=a.slice(5);var all=a.slice(0,99);hi.length===0&&all.length===3&&all[0]===1&&all[1]===2&&all[2]===3;");
    eprintln!("ARRAY_SLICE_CLAMP over=empty to99=1,2,3");
}

#[test]
fn array_slice_fractional_and_nan() {
    guest_flag("var a=[1,2,3];var f=a.slice(1.9);var n=a.slice(NaN);f.length===2&&f[0]===2&&f[1]===3&&n.length===3&&n[0]===1&&n[1]===2&&n[2]===3;");
    eprintln!("ARRAY_SLICE_CLAMP frac=2,3 nan=1,2,3");
}

#[test]
fn array_slice_undefined_end() {
    guest_flag("var a=[1,2,3];var u=a.slice(1,undefined);var m=a.slice(1);u.length===2&&u[0]===2&&u[1]===3&&m.length===2&&m[0]===2&&m[1]===3;");
    eprintln!("ARRAY_SLICE_UNDEFINED_END same_as_missing=2,3");
}

#[test]
fn array_slice_null_end() {
    guest_flag("var r=[1,2,3].slice(1,null);r.length===0;");
    eprintln!("ARRAY_SLICE_NULL_END len=0");
}

#[test]
fn array_slice_returns_new_array() {
    guest_flag("var a=[1,2,3];var r=a.slice();r!==a&&r.length===3&&r[0]===1&&r[1]===2&&r[2]===3&&a.length===3&&a[0]===1&&a[1]===2&&a[2]===3;");
    eprintln!("ARRAY_SLICE_IDENTITY new=true source_unchanged=1,2,3");
}

#[test]
fn array_slice_tagged_identity() {
    guest_flag("var x={id:1};var a=[x];var r=a.slice();r!==a&&r[0]===x&&r[0]===a[0]&&a.length===1;");
    eprintln!("ARRAY_SLICE_TAGGED same_ref=true new_array=true");
}

/// Hole suffix is copied as `undefined` through `array_get`. This locks the
/// measured prefix semantics; it is not an ES5 HasProperty miss.
#[test]
fn array_slice_hole_suffix() {
    guest_flag("var a=[1];a.length=4;var r=a.slice(0,4);r.length===4&&r[0]===1&&r[1]===undefined&&r[2]===undefined&&r[3]===undefined;");
    eprintln!("ARRAY_SLICE_HOLE len=4 e0=1 e1=undefined e2=undefined e3=undefined");
}

/// start's valueOf pops, then returns -1. Clamp uses the old length 4, so
/// start stays 3. The omitted end stays that old length 4. Micro then re-reads
/// length 3 and sets final = min(4, 3) = 3. Start is not re-clamped, so the
/// copy length is max(3 - 3, 0) = 0. A missing re-read or a re-clamp would
/// return one element.
#[test]
fn array_slice_from_index_mutates_array() {
    guest_flag("var id=function(x){return x;};var a=[1,2,3,4];var p={n:0,removed:0,owner:id(a),valueOf:function(){'use strict';this.n=this.n+1;this.removed=this.owner.pop();return -1;}};var r=a.slice(p);r.length===0&&p.n===1&&p.removed===4;");
    eprintln!("ARRAY_SLICE_MUTATE result_len=0 valueOf_called=1 pop_returned=4 start_clamped_to_old_len=3 final_min_new_len=3");
}

#[test]
fn array_slice_borrowed_type_error() {
    guest_flag("var o={0:1,length:1};var f=[].slice;var ok=false;try{f.call(o);}catch(e){ok=e.name==='TypeError';}ok&&o[0]===1&&o.length===1;");
    eprintln!("ARRAY_SLICE_BRAND reject=TypeError object_unchanged=true");
}

#[test]
fn array_slice_method_value_and_length() {
    guest_flag("var a=[1];var p=Object.getPrototypeOf(a);typeof a.slice==='function'&&a.slice.length===2&&p.slice===a.slice;");
    eprintln!("ARRAY_SLICE_METHOD typeof=function length=2");
}

/// Logical length 70000 is already a public length. slice(0, 1) must return
/// [1]. slice(-1) clamps against that same length and copies the hole at
/// 69999 as undefined. A 65536 exact-int decode of the length cell or of the
/// clamped index host-asserts before either result exists.
#[test]
fn array_slice_logical_length_70000() {
    guest_flag("var a=[1];a.length=70000;var r=a.slice(0,1);var s=a.slice(-1);r.length===1&&r[0]===1&&s.length===1&&s[0]===undefined;");
    eprintln!("ARRAY_SLICE_LOGICAL_70000 slice01=1 len=1 slice_neg1=undefined len=1");
}

/// Both endpoints are valueOf objects. Start runs first, pushes 9, and returns
/// -1. End runs once, sees length 5 and index 4 === 9, and returns 100. Both
/// clamps still use the length captured before either conversion (4): start
/// becomes 3 and end becomes 4, so the copy is [4] and the pushed 9 is absent.
/// The push is visible during end's valueOf; this test does not require it to
/// remain on the source after slice returns.
#[test]
fn array_slice_explicit_end_value_of() {
    guest_flag("var id=function(x){return x;};var a=[1,2,3,4];var box={n:0,elem:0};var p1={n:0,box:box,owner:id(a),valueOf:function(){'use strict';this.n=this.n+1;var box=this.box;box.n=box.n*10+1;this.owner.push(9);return -1;}};var p2={n:0,seen:0,box:box,owner:id(a),valueOf:function(){'use strict';this.n=this.n+1;var box=this.box;box.n=box.n*10+2;box.elem=this.owner[4]===9?1:0;this.seen=this.owner.length;return 100;}};var r=a.slice(p1,p2);box.n===12&&p1.n===1&&p2.n===1&&box.elem===1&&p2.seen===5&&r.length===1&&r[0]===4;");
    eprintln!("ARRAY_SLICE_TWO_COERCIONS order=SE start_calls=1 end_calls=1 end_saw_len=5 end_saw_index4=9 start_clamped_to_old_len=3 end_clamped_to_old_len=4 result=4 excluded_pushed=9");
}

/// Documented overshoot inputs each reduced by 25: slice(-1)→slice(-26) and
/// slice(-2,-1)→slice(-27,-26). A six-call script of every shifted input did
/// not finish within 500s, so this suite test locks the two still-negative
/// clamps. slice(-20), slice(-25,74), slice(-23.1) and slice(NaN) passed in
/// separate focused runs.
#[test]
fn array_slice_case_shift_25() {
    guest_flag("var a=[1,2,3];var n26=a.slice(-26);var n27=a.slice(-27,-26);n26.length===3&&n26[0]===1&&n26[1]===2&&n26[2]===3&&n27.length===0&&a.length===3;");
    eprintln!("ARRAY_SLICE_CASE_SHIFT_25 neg26=1,2,3 neg27to26=empty");
}

#[test]
fn array_concat_basic() {
    guest_flag("var r=[1].concat([2,3]);r.length===3&&r[0]===1&&r[1]===2&&r[2]===3;");
    eprintln!("ARRAY_CONCAT_BASIC len=3 e0=1 e1=2 e2=3");
}

#[test]
fn array_concat_non_array_argument() {
    guest_flag("var r=[1].concat('ab');r.length===2&&r[0]===1&&r[1]==='ab'&&typeof r[1]==='string';");
    eprintln!("ARRAY_CONCAT_STRING len=2 e0=1 e1=ab not_split=true");
}

#[test]
fn array_concat_one_level_only() {
    // `[[1]].concat(2)` is the required shape. This scope rejects a nested
    // array literal, so the inner array is returned from a call (Dynamic) and
    // stored in a tagged slot. Flattening would make r[0] the number 1.
    guest_flag("var id=function(x){return x;};var inner=id([1]);var a=[{}];a[0]=inner;var r=a.concat(2);r.length===2&&r[0]===inner&&r[0].length===1&&r[0][0]===1&&r[1]===2;");
    eprintln!("ARRAY_CONCAT_ONE_LEVEL len=2 e0=[1] e0_len=1 e1=2 same_inner=true literal_rejected=nested_array");
}

#[test]
fn array_concat_nested_not_flattened() {
    // `[1].concat([2,[3]])` is the required shape. The inner array is a call
    // result stored in a tagged slot; flattening it would make r[2] the number 3.
    guest_flag("var id=function(x){return x;};var inner=id([3]);var mid=[2,{}];mid[1]=inner;var r=[1].concat(mid);r.length===3&&r[0]===1&&r[1]===2&&r[2]===inner&&r[2].length===1&&r[2][0]===3;");
    eprintln!("ARRAY_CONCAT_NESTED len=3 e0=1 e1=2 e2=[3] e2_len=1 same_inner=true literal_rejected=nested_array");
}

#[test]
fn array_concat_no_args_returns_new() {
    guest_flag("var a=[1];var r=a.concat();r!==a&&r.length===1&&r[0]===1&&a.length===1&&a[0]===1;");
    eprintln!("ARRAY_CONCAT_NO_ARGS new=true len=1 e0=1 source_unchanged=1");
}

#[test]
fn array_concat_multiple_args_order() {
    guest_flag("var o={a:1};var r=[0].concat([1],2,[3,4]);var s=[1].concat(o);r.length===5&&r[0]===0&&r[1]===1&&r[2]===2&&r[3]===3&&r[4]===4&&s.length===2&&s[0]===1&&s[1]===o;");
    eprintln!("ARRAY_CONCAT_ORDER len=5 values=0,1,2,3,4 object_arg_kept=true");
}

#[test]
fn array_concat_tagged_identity() {
    guest_flag("var x={id:1};var a=[x];var r=a.concat([x]);r[0]===a[0]&&r[0]===x&&r[1]===x&&r!==a&&a.length===1&&a[0]===x;");
    eprintln!("ARRAY_CONCAT_TAGGED same_ref=true new_array=true source_unchanged=true");
}

#[test]
fn array_concat_hole_suffix() {
    guest_flag("var a=[1];a.length=4;var r=a.concat();r!==a&&r.length===4&&r[0]===1&&r[1]===undefined&&r[2]===undefined&&r[3]===undefined&&a.length===4&&a[0]===1;");
    eprintln!("ARRAY_CONCAT_HOLE len=4 e0=1 e1=undefined e2=undefined e3=undefined");
}

#[test]
fn array_concat_empty_receiver() {
    guest_flag("var a=[];var b=[];var r=a.concat([1]);var s=a.concat(b);var t=a.concat();r.length===1&&r[0]===1&&r!==a&&s!==a&&s!==b&&s.length===0&&t!==a&&t.length===0&&a.length===0;");
    eprintln!("ARRAY_CONCAT_EMPTY with_one=1 with_empty_new=true no_args_new=true");
}

#[test]
fn array_concat_borrowed_type_error() {
    guest_flag("var o={0:1,length:1};var f=[].concat;var ok=false;try{f.call(o);}catch(e){ok=e.name==='TypeError';}ok&&o[0]===1&&o.length===1;");
    eprintln!("ARRAY_CONCAT_BRAND reject=TypeError object_unchanged=true");
}

#[test]
fn array_concat_method_value_and_length() {
    guest_flag("var a=[1];var p=Object.getPrototypeOf(a);typeof a.concat==='function'&&a.concat.length===1&&p.concat===a.concat;");
    eprintln!("ARRAY_CONCAT_METHOD typeof=function length=1");
}

#[test]
fn array_index_of_tagged_identity() {
    guest_flag("var x={id:1};var y={id:1};var a=[x,'ab',y];a.indexOf(x)===0&&a.indexOf(y)===2&&a.indexOf({id:1})===-1&&a.indexOf('ab')===1&&a.indexOf('a'+'b')===1&&a.lastIndexOf(y)===2&&a.lastIndexOf(x)===0;");
}

/// Hole suffix is read as `undefined` through `array_get`. This locks the
/// measured prefix semantics; it is not an ES5 HasProperty miss. Node returns
/// -1 for the same source; this test does not consult that oracle.
#[test]
fn array_index_of_hole_suffix_undefined() {
    guest_flag("var a=[1];a.length=4;a.indexOf(undefined)===1&&a.lastIndexOf(undefined)===3&&[1,2,3].indexOf(undefined)===-1;");
    eprintln!("ARRAY_INDEXOF_HOLE suffix_indexOf=1 suffix_lastIndexOf=3 dense_miss=-1 prefix_not_es5_hasproperty node_oracle=-1");
}

#[test]
fn range_error_call_zero_args() {
    guest_flag(r#"typeof RangeError()==="object"&&RangeError().name==="RangeError";"#);
}

#[test]
fn range_error_new_zero_args() {
    guest_flag(r#"typeof new RangeError()==="object";"#);
}

#[test]
fn range_error_name() {
    guest_flag(
        r#"new RangeError().name==="RangeError"&&RangeError().name==="RangeError"&&RangeError.prototype.name==="RangeError";"#,
    );
}

#[test]
fn range_error_message_empty() {
    guest_flag(
        r#"var e=new RangeError();e.hasOwnProperty("message")===false&&e.message===""&&RangeError.prototype.message===""&&RangeError().message==="";"#,
    );
}

#[test]
fn range_error_has_own_property() {
    guest_flag(
        r#"new RangeError().hasOwnProperty("message")===false&&RangeError().hasOwnProperty("name")===false;"#,
    );
    eprintln!("RANGE_ERROR_HAS_OWN new_message=false call_name=false");
}

#[test]
fn range_error_typeof_function() {
    guest_flag(r#"typeof RangeError==="function"&&RangeError.length===1;"#);
    eprintln!("RANGE_ERROR_TYPEOF function length=1");
}

#[test]
fn range_error_throw_catch() {
    guest_flag(r#"try { throw new RangeError(); } catch (e) { e.name==="RangeError"&&e.message===""; }"#);
}

fn range_error_roots(ir: &vm::FrontendIr) -> Vec<(i64, i64, i64, i64)> {
    let Some(bootstrap) = ir.functions.iter().find(|f| f.name == "__compat_es5_intrinsic_bootstrap") else {
        return Vec::new();
    };
    let Some(allocate) = ir.functions.iter().find(|f| f.name == "__compat_es5_object_allocate") else {
        return Vec::new();
    };
    let Some(enter) = ir.functions.iter().find(|f| f.name == "__compat_es5_root_enter") else {
        return Vec::new();
    };
    let body = &ir.function_impls[&bootstrap.index].body_stmts;
    let Some(permanent) = body.iter().find_map(|stmt| match stmt {
        vm::Stmt::Assign { index, expr: vm::Expr::Call(id, _, _, _, _), .. } if *id == enter.index => Some(*index),
        _ => None,
    }) else {
        return Vec::new();
    };
    body.iter()
        .filter_map(|stmt| match stmt {
            vm::Stmt::Assign { expr: vm::Expr::Call(id, _, args, _, _), .. } if *id == allocate.index => {
                match args.as_slice() {
                    [_, vm::Expr::Int(kind), _, vm::Expr::Int(rows), vm::Expr::Int(code), _, _, vm::Expr::Var(frame), vm::Expr::Int(root)]
                        if *frame == permanent && (*code == 308 || (*kind == 3 && *rows == 2 && *root > 3)) =>
                    {
                        Some((*root, *kind, *rows, *code))
                    }
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

#[test]
fn range_error_distinct_from_type_error() {
    guest_flag(
        r#"RangeError!==TypeError&&new RangeError()!==new TypeError()&&RangeError.prototype!==TypeError.prototype&&Object.getPrototypeOf(new RangeError())!==TypeError.prototype&&Object.getPrototypeOf(new TypeError())!==RangeError.prototype;"#,
    );
    eprintln!("RANGE_ERROR_DISTINCT same_realm_identity=getPrototypeOf not instanceof");
}

#[test]
fn range_error_shadow_local() {
    let source = "function f(){var RangeError=function(){return 7;};return RangeError();}f()===7;";
    guest_flag(source);
    let ir = parse(source).expect("shadowed local RangeError");
    let roots = range_error_roots(&ir);
    assert!(roots.is_empty(), "shadowed RangeError must not install code 308: {roots:?}");
    eprintln!("RANGE_ERROR_SHADOW_LOCAL roots={roots:?}");
}

#[test]
fn range_error_shadow_script() {
    let source = "var RangeError=function(){return 9;};RangeError()===9;";
    guest_flag(source);
    let ir = parse(source).expect("shadowed script RangeError");
    let roots = range_error_roots(&ir);
    assert!(roots.is_empty(), "script-shadowed RangeError must not install code 308: {roots:?}");
    eprintln!("RANGE_ERROR_SHADOW_SCRIPT roots={roots:?}");
}

#[test]
fn range_error_arity_zero_only() {
    for source in ["RangeError(\"x\");", "new RangeError(\"x\");"] {
        let err = parse(source).expect_err("frontend rejects a message argument");
        let text = format!("{err:?}");
        assert!(
            text.contains("unsupported JavaScript ES5 P0 primitive expression"),
            "{source} -> {text}"
        );
        eprintln!("RANGE_ERROR_ARITY frontend_reject source={source:?}");
    }
    guest_flag(
        r#"var thrown=false;var name="";try { RangeError.call({}, "x"); } catch (e) { thrown=true; name=e.name; } thrown&&name==="TypeError";"#,
    );
    eprintln!("RANGE_ERROR_ARITY call_receiver_x=TypeError existing_call_count_boundary message_not_accepted");
}

#[test]
fn range_error_prototype_chain_evidence() {
    guest_flag(
        "Object.getPrototypeOf(new RangeError())===RangeError.prototype&&Object.getPrototypeOf(RangeError())===RangeError.prototype;",
    );
    let selected = parse("RangeError;").expect("RangeError realm");
    let quiet = parse("Object;").expect("Object realm");
    let roots = range_error_roots(&selected);
    let quiet_roots = range_error_roots(&quiet);
    assert!(quiet_roots.is_empty(), "unnamed RangeError must not install: {quiet_roots:?}");
    assert!(
        roots.iter().any(|(_, _, _, code)| *code == 308),
        "public constructor missing: {roots:?}"
    );
    assert!(
        roots.iter().all(|(root, _, _, code)| *code != 257 && *root != 3),
        "must not reuse TypeError roots: {roots:?}"
    );
    let other = parse("RangeError;").expect("second realm");
    let other_roots = range_error_roots(&other);
    assert_eq!(roots, other_roots, "each compilation installs its own tail, not a shared object");
    let instance_of = parse("new RangeError() instanceof RangeError;").expect_err("instanceof is outside the admitted subset");
    let instance_text = format!("{instance_of:?}");
    assert!(
        instance_text.contains("unsupported JavaScript ES5 P0 binary operator"),
        "{instance_text}"
    );
    eprintln!(
        "RANGE_ERROR_RESOURCE roots={roots:?} quiet={} second_parse_same_indices=true instanceof_refused=true micro_divergence=not_ERROR_DEF prototype_holds_name_and_empty_message instance_has_no_own_message code=308",
        quiet_roots.len()
    );
}

struct RangeErrorGcObserver;
impl vm::SourcePlugin for RangeErrorGcObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("range-error-gc");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S};
        let observer = object_observer::Observer {
            force_gc: true,
            ..Default::default()
        };
        let mut ir = vm::SourcePlugin::parse_source(&observer, source, context)?;
        let observe = helper(&ir, "__compat_es5_heap_observe");
        let make = helper(&ir, "__compat_ref_make");
        let carrier = ir
            .stmts
            .iter()
            .find_map(|stmt| match stmt {
                S::Let { index, expr: E::Call(target, _, _, _, _), .. } if *target == make => Some(*index),
                _ => None,
            })
            .expect("real carrier");
        ir.stmts.push(S::Expr {
            expr: vm::Expr::Call(
                observe,
                Vec::new(),
                vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                None,
                None,
            ),
            line: 1,
        });
        Ok(ir)
    }
}

/// Force a collection on every node begin, then require the prototype that is
/// kept only by the function's `prototype` data row to still answer `name`.
#[test]
fn range_error_gc_keeps_prototype() {
    static OBS: std::sync::LazyLock<RangeErrorGcObserver> = std::sync::LazyLock::new(|| RangeErrorGcObserver);
    let source = r#"new RangeError().name==="RangeError"&&RangeError.prototype.name==="RangeError"&&RangeError!==TypeError;"#;
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = probe_vm(source, &*OBS, "range-error-gc");
            let status = machine.run();
            print_probe_fuel(&machine, PROBE_FUEL);
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let stack = machine.stack();
            let at = stack
                .iter()
                .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
                .expect("observer marker");
            let vm::Value::Map(map) = &stack[at + 1] else {
                panic!("guest box missing: {stack:?}");
            };
            assert_eq!(map.get(&vm::Value::string("tag")), Some(&vm::Value::Int(2)));
            assert_eq!(map.get(&vm::Value::string("flag")), Some(&vm::Value::Bool(true)));
            let collections = match stack.last() {
                Some(vm::Value::Int(n)) => *n,
                Some(vm::Value::Float(n)) => *n as i64,
                other => panic!("collection count missing: {other:?}"),
            };
            assert!(collections > 0, "forced collection did not run: {collections}");
            eprintln!(
                "RANGE_ERROR_GC collections={collections} new_name=RangeError prototype_name=RangeError distinct_from_TypeError=true"
            );
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

fn repeat_status(source: &str) -> Result<vm::VmStatus, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(
                &source,
            ) {
                Ok(machine) => machine,
                Err(error) => return Err(format!("ParseError: {error:?}")),
            };
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(PROBE_FUEL);
            match machine.run() {
                Ok(status) => Ok(status),
                Err(error) => Err(format!("{error:?}")),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn string_repeat_basic() {
    guest_flag(r#""ab".repeat(3)==="ababab";"#);
    eprintln!("STRING_REPEAT_BASIC count=3 value=ababab");
}

#[test]
fn string_repeat_zero() {
    guest_flag(r#""ab".repeat(0)==="";"#);
    eprintln!("STRING_REPEAT_ZERO sat=0 value=empty");
}

#[test]
fn string_repeat_negative_range_error() {
    guest_flag(
        r#"var hit=0;try{"ab".repeat(-1);}catch(e){if(e.name==="RangeError")hit=1;}hit===1;"#,
    );
    eprintln!("STRING_REPEAT_NEGATIVE name=RangeError");
}

#[test]
fn string_repeat_one_returns_same() {
    guest_flag(r#"var s="ab";s.repeat(1)===s;"#);
    eprintln!("STRING_REPEAT_SAME n=1 identity=true");
}

#[test]
fn string_repeat_empty_source() {
    guest_flag(r#"var s="";s.repeat(5)===s&&s.repeat(5)==="";"#);
    eprintln!("STRING_REPEAT_EMPTY identity=true");
}

#[test]
fn string_repeat_fractional_and_string_arg() {
    guest_flag(r#""ab".repeat(1.9)==="ab"&&"ab".repeat("2")==="abab";"#);
    eprintln!("STRING_REPEAT_SAT fractional=1 string_arg=2 values=ab,abab");
}

#[test]
fn string_repeat_nan_zero() {
    guest_flag(r#""ab".repeat(NaN)===""&&"ab".repeat(undefined)==="";"#);
    eprintln!("STRING_REPEAT_SAT nan=0 undefined=0");
}

#[test]
fn string_repeat_infinity() {
    let status = repeat_status(r#""ab".repeat(Infinity);"#);
    eprintln!("STRING_REPEAT_INFINITY status={status:?}");
    let Err(error) = status else {
        panic!("oversize repeat halted: {status:?}");
    };
    assert!(
        error.contains("HostError") && error.contains("assertion failed"),
        "{error}"
    );
    eprintln!("STRING_REPEAT_INFINITY oversize_failure_kind=HostError");
}

#[test]
fn string_repeat_negative_zero_and_minus_one() {
    guest_flag(
        r#"var hit=0;try{"ab".repeat(-1);}catch(e){if(e.name==="RangeError")hit=1;}hit===1&&"ab".repeat(-0)==="";"#,
    );
    eprintln!("STRING_REPEAT_SAT neg_zero=0 minus_one=RangeError");
}

#[test]
fn string_repeat_surrogates_preserved() {
    char_at_units("\"😀\".repeat(2);", &[0xD83D, 0xDE00, 0xD83D, 0xDE00]);
    eprintln!("STRING_REPEAT_SURROGATE units=D83D,DE00,D83D,DE00");
}

#[test]
fn string_repeat_nullish_receiver_type_error() {
    boxed(
        r#"var f="ab".repeat;var hit=0;try{f.call(null,2);}catch(e){if(e.name==="TypeError")hit=1;}hit===1;"#,
        2,
        0.0,
        true,
    );
    eprintln!("STRING_REPEAT_NULLISH name=TypeError");
}

#[test]
fn string_repeat_method_value_and_length() {
    boxed(
        r#"var f="ab".repeat;typeof f==="function"&&f.length===1;"#,
        2,
        0.0,
        true,
    );
    eprintln!("STRING_REPEAT_METHOD typeof=function length=1");
}

fn string_ns_carrier_roots(ir: &vm::FrontendIr) -> Vec<i64> {
    let id = |name: &str| ir.functions.iter().find(|f| f.name == name).unwrap().index;
    let bootstrap = id("__compat_es5_intrinsic_bootstrap");
    let allocate = id("__compat_es5_object_allocate");
    ir.function_impls[&bootstrap]
        .body_stmts
        .iter()
        .filter_map(|stmt| match stmt {
            vm::Stmt::Assign { expr: vm::Expr::Call(fn_id, _, args, _, _), .. } if *fn_id == allocate => {
                match args.as_slice() {
                    [_, vm::Expr::Int(4), _, vm::Expr::Int(3), vm::Expr::Int(312), _, _, _, vm::Expr::Int(root)] => Some(*root),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn string_ns_method_codes(ir: &vm::FrontendIr) -> Vec<i64> {
    let id = |name: &str| ir.functions.iter().find(|f| f.name == name).unwrap().index;
    let bootstrap = id("__compat_es5_intrinsic_bootstrap");
    let allocate = id("__compat_es5_object_allocate");
    let mut codes = Vec::new();
    for stmt in &ir.function_impls[&bootstrap].body_stmts {
        if let vm::Stmt::Assign { expr: vm::Expr::Call(fn_id, _, args, _, _), .. } = stmt
            && *fn_id == allocate
            && let Some(vm::Expr::Int(code)) = args.get(4)
            && matches!(*code, 310 | 311 | 312)
        {
            codes.push(*code);
        }
    }
    codes.sort();
    codes
}

#[test]
fn string_ns_carrier_shape() {
    // The carrier is not callable, but its `prototype` row is the private
    // String prototype the realm already owns (upstream `object`).
    guest_flag(
        r#"typeof String==="function" && typeof String.fromCharCode==="function" && typeof String.fromCodePoint==="function" && String.length===1 && String.fromCharCode.length===1 && String.fromCodePoint.length===1 && typeof String.prototype==="object";"#,
    );
    eprintln!("STRING_NS_SHAPE typeof=function method=function length=1 prototype=object roots=1");
}

#[test]
fn string_from_char_code_basic() {
    guest_flag(r#"String.fromCharCode(65,66)==="AB";"#);
    eprintln!("STRING_FROM_CHAR_CODE_BASIC AB");
}

#[test]
fn string_from_char_code_no_args() {
    guest_flag(r#"String.fromCharCode()==="";"#);
    eprintln!("STRING_FROM_CHAR_CODE_EMPTY");
}

#[test]
fn string_from_char_code_int32_and_mask() {
    // Micro JS_ToInt32 then &= 0xffff. 65601 is 0x10041, so the low 16 bits are 0x0041, not 0x0001.
    char_at_units("String.fromCharCode(65601);", &[0x0041]);
    char_at_units("String.fromCharCode(65537);", &[0x0001]);
    char_at_units("String.fromCharCode(-1);", &[0xFFFF]);
    char_at_units("String.fromCharCode(0x1F600);", &[0xF600]);
    char_at_units("String.fromCharCode(1.9);", &[0x0001]);
    char_at_units("String.fromCharCode(2147483648);", &[0x0000]);
    eprintln!("STRING_FROM_CHAR_CODE_MASK 65601=0041 not-0001; 65537=0001; -1=FFFF; 0x1F600=F600; 1.9=0001; 2147483648=0000");
}

#[test]
fn string_from_char_code_fractional() {
    char_at_units("String.fromCharCode(1.9);", &[0x0001]);
    eprintln!("STRING_FROM_CHAR_CODE_FRACTIONAL 1.9=0001");
}

#[test]
fn string_from_code_point_surrogate_pair() {
    char_at_units("String.fromCodePoint(0x1F600);", &[0xD83D, 0xDE00]);
    eprintln!("STRING_FROM_CODE_POINT_PAIR units=2 codes=D83D,DE00");
}

#[test]
fn string_from_code_point_bmp() {
    char_at_units("String.fromCodePoint(0x4E2D);", &[0x4E2D]);
    eprintln!("STRING_FROM_CODE_POINT_BMP 4E2D units=1");
}

#[test]
fn string_from_code_point_lone_surrogate_allowed() {
    char_at_units("String.fromCodePoint(0xD800);", &[0xD800]);
    char_at_units("String.fromCodePoint(0xDFFF);", &[0xDFFF]);
    eprintln!("STRING_FROM_CODE_POINT_LONE D800 units=1; DFFF units=1");
}

#[test]
fn string_from_code_point_multi_args() {
    char_at_units("String.fromCodePoint(0x1F600,0x1F601);", &[0xD83D, 0xDE00, 0xD83D, 0xDE01]);
    eprintln!("STRING_FROM_CODE_POINT_MULTI units=4");
}

#[test]
fn string_from_code_point_range_error() {
    guest_flag(
        r#"var hi=false; var lo=false; try{String.fromCodePoint(0x110000);}catch(e){hi=e.name==="RangeError";} try{String.fromCodePoint(-1);}catch(e){lo=e.name==="RangeError";} hi&&lo;"#,
    );
    eprintln!("STRING_FROM_CODE_POINT_RANGE 0x110000=RangeError -1=RangeError");
}

#[test]
fn string_from_code_point_function_body_range_error() {
    guest_flag(
        r#"function f(){return String.fromCodePoint(-1);} var ok=false; try{f();}catch(e){ok=e.name==="RangeError";} ok;"#,
    );
    eprintln!("STRING_FROM_CODE_POINT_RANGE_FN function-body guest RangeError");
}

#[test]
fn string_from_code_point_bracket_literal_range_error() {
    guest_flag(
        r#"var ok=false; try{String["fromCodePoint"](-1);}catch(e){ok=e.name==="RangeError";} ok;"#,
    );
    eprintln!("STRING_FROM_CODE_POINT_RANGE_BRACKET literal-bracket guest RangeError");
}

#[test]
fn string_ns_call_and_new_runtime_type_error() {
    // `new String("x")` is a runtime TypeError in this slice: the construction
    // form is not admitted, so the ordinary guest `new` path refuses it. The
    // call form is ES5.1 15.5.1 `String(value)`: the carrier and an alias both
    // return "1" for the number 1, so the old TypeError expectation was stale.
    guest_flag(
        r#"var c=false;try{new String("x");}catch(e){c=e.name==="TypeError";}c;"#,
    );
    guest_flag(
        r#"var s=String;String(1)==="1"&&s(1)==="1"&&String(true)==="true"&&String()==="";"#,
    );
    eprintln!("STRING_NS_CALL direct=1 alias=1 new=runtime TypeError");
}

#[test]
fn string_ns_shadow_local() {
    let source = "function local(){var String={fromCharCode:function(){return \"Z\";}}; return String.fromCharCode(65);} local()===\"Z\";";
    let ir = number_namespace_ir(source).unwrap();
    assert!(string_ns_carrier_roots(&ir).is_empty(), "{:?}", string_ns_carrier_roots(&ir));
    guest_flag(source);
    eprintln!("STRING_NS_SHADOW_LOCAL roots=[]");
}

#[test]
fn string_ns_shadow_script() {
    let source = "var String={fromCharCode:function(){return \"Z\";}}; String.fromCharCode(65)===\"Z\";";
    let ir = number_namespace_ir(source).unwrap();
    assert!(string_ns_carrier_roots(&ir).is_empty(), "{:?}", string_ns_carrier_roots(&ir));
    guest_flag(source);
    eprintln!("STRING_NS_SHADOW_SCRIPT roots=[]");
}

#[test]
fn string_ns_unshadowed_root() {
    let ir = number_namespace_ir("String.fromCharCode(65);").unwrap();
    let roots = string_ns_carrier_roots(&ir);
    assert_eq!(roots, vec![11], "{roots:?}");
    guest_flag(r#"String.fromCharCode(65)==="A";"#);
    eprintln!("STRING_NS_ROOT unshadowed={roots:?} shadowed_absent=true");
}

/// Allocator install is the emitted `__compat_es5_new_range_error` function
/// and the code-308 constructor row. Both are gated by the same demand flag.
fn range_error_allocator_installed(ir: &vm::FrontendIr) -> bool {
    let named = ir
        .functions
        .iter()
        .any(|function| function.name == "__compat_es5_new_range_error");
    let coded = range_error_roots(ir)
        .iter()
        .any(|(_, _, _, code)| *code == 308);
    named || coded
}

fn assert_range_error_allocator_absent(label: &str, source: &str) {
    let ir = number_namespace_ir(source).unwrap_or_else(|err| {
        let text = format!("{err:?}");
        eprintln!("{label} range_error_installed=unlowered diagnostic={text}");
        panic!("{label} did not produce IR: {text}");
    });
    let installed = range_error_allocator_installed(&ir);
    eprintln!("{label} range_error_installed={installed}");
    assert!(
        !installed,
        "{label} still installed the RangeError allocator"
    );
}

#[test]
fn string_ns_alias_from_char_code() {
    guest_flag(r#"var s=String; s.fromCharCode(65)==="A";"#);
    eprintln!("STRING_NS_ALIAS_STATIC alias=A");
}

#[test]
fn string_ns_alias_from_code_point() {
    guest_flag("var s=String; s.fromCodePoint(0x4E2D)===\"\\u4E2D\";");
    eprintln!("STRING_NS_ALIAS_STATIC alias=4E2D");
}

#[test]
fn string_ns_neg_shadow() {
    let positive = number_namespace_ir("String.fromCodePoint(-1);").expect("resolved fromCodePoint");
    assert!(
        range_error_allocator_installed(&positive),
        "detector must see the allocator on a resolved String.fromCodePoint"
    );
    assert_range_error_allocator_absent(
        "STRING_NS_NEG_SHADOW",
        "function f(){var String={fromCodePoint:function(){return 1;}};return String.fromCodePoint(-1);} f();",
    );
}

#[test]
fn string_ns_neg_unrelated() {
    assert_range_error_allocator_absent(
        "STRING_NS_NEG_UNRELATED",
        "var o={fromCodePoint:function(){return 2;}}; o.fromCodePoint(-1);",
    );
}

#[test]
fn string_ns_neg_computed() {
    assert_range_error_allocator_absent(
        "STRING_NS_NEG_COMPUTED",
        "var k=\"fromCodePoint\"; String[k](-1);",
    );
}

fn array_ns_allocations(ir: &vm::FrontendIr) -> Vec<(i64, i64, i64)> {
    let bootstrap = helper(ir, "__compat_es5_intrinsic_bootstrap");
    let allocate = helper(ir, "__compat_es5_object_allocate");
    ir.function_impls[&bootstrap]
        .body_stmts
        .iter()
        .filter_map(|stmt| match stmt {
            vm::Stmt::Assign { expr: vm::Expr::Call(id, _, args, _, _), .. } if *id == allocate => {
                match args.as_slice() {
                    [_, vm::Expr::Int(4), _, vm::Expr::Int(rows), vm::Expr::Int(code @ (315 | 316)), _, _, _, vm::Expr::Int(entry)] => {
                        Some((*code, *rows, *entry))
                    }
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn assert_array_ns_proto_ir(source: &str) {
    let ir = number_namespace_ir(source).expect("Array prototype access must lower to IR");
    let constructor = ir
        .functions
        .iter()
        .any(|f| f.name == "__compat_es5_array_constructor");
    let range_error = range_error_allocator_installed(&ir);
    let range_error_roots = range_error_roots(&ir);
    assert_eq!(
        (constructor, range_error, range_error_roots),
        (false, false, vec![]),
        "{source} must not install Array construction or the RangeError realm"
    );
    assert_eq!(array_ns_allocations(&ir), vec![(315, 3, 11), (316, 1, 1)]);
}

#[test]
fn array_ns_proto_direct() {
    assert_array_ns_proto_ir("Array.prototype;");
    guest_flag("typeof Array.prototype === 'object' && Array.prototype.constructor === Array;");
}

#[test]
fn array_ns_proto_nested_is_array() {
    assert_array_ns_proto_ir("Array.prototype.isArray;");
    // The static `isArray` lives on the carrier, never on the prototype, so
    // the read is an ordinary `undefined` (upstream), not a TypeError.
    guest_flag("Array.prototype.isArray === undefined;");
}

#[test]
fn array_ns_proto_bracket() {
    assert_array_ns_proto_ir(r#"Array["prototype"];"#);
    guest_flag(r#"typeof Array["prototype"] === "object" && Array["isArray"]([]) === true;"#);
}

#[test]
fn array_ns_proto_bound_value() {
    assert_array_ns_proto_ir("var p = Array.prototype; p;");
    guest_flag("var p = Array.prototype; p === Array.prototype && typeof p === 'object';");
}

#[test]
fn array_ns_carrier_shape() {
    let source = r#"typeof Array==="function" && typeof Array.isArray==="function" && Array.length===1 && Array.isArray.length===1 && typeof Array.prototype==="object" && Array.hasOwnProperty("prototype");"#;
    number_namespace_ir(source).expect("Array namespace is admitted");
    guest_flag(source);
    eprintln!("ARRAY_NS_SHAPE typeof=function method=function length=1 method_length=1 prototype=object own_prototype=true");
}

#[test]
fn array_ns_call_and_new_construct() {
    guest_flag("var a=Array(1,2);var b=new Array(1);Array.isArray(a)&&a.length===2&&a[0]===1&&a[1]===2&&Array.isArray(b)&&b.length===1&&b[0]===undefined;");
}

#[test]
fn array_ctor_empty_fresh() {
    guest_flag("var a=Array();var b=Array();a!==b&&a.length===0&&Array.isArray(a);");
}

#[test]
fn array_ctor_mixed_elements() {
    guest_flag(r#"var o={v:7};var a=Array(o,"x",7);a.length===3&&a[0]===o&&a[1]==="x"&&a[2]===7;"#);
}

#[test]
fn array_ctor_actual_order() {
    guest_flag("var n=0;var a=Array((n=n*10+1),(n=n*10+2));n===12&&a[0]===1&&a[1]===12;");
}

#[test]
fn array_ctor_alias() {
    guest_flag("var f=Array;var a=f(7,8);Array.isArray(a)&&a[1]===8;");
}

fn assert_array_ctor_length_only(source: &str) {
    static OBS: std::sync::LazyLock<object_observer::Observer> = std::sync::LazyLock::new(|| {
        object_observer::Observer {
            units: Some(3),
            ..Default::default()
        }
    });
    let stack = observed_for(source, &*OBS, "javascript-es5-object-observer", 0, true, PROBE_FUEL);
    let at = stack.iter().rposition(|v| *v == vm::Value::string("P7_OBSERVED")).unwrap();
    // Header offsets 2 and 5 hold logical length and physical present + 1.
    assert_eq!(stack[at + 2], vm::Value::Float(1.0), "logical length");
    assert_eq!(stack[at + 5], vm::Value::Float(1.0), "no materialized element");
}

#[test]
fn array_ctor_single_number() {
    assert_array_ctor_length_only("Array(1);");
}

#[test]
fn array_ctor_alias_single_number() {
    assert_array_ctor_length_only("var f=Array;f(1);");
}

#[test]
fn array_ctor_lexical_shadow() {
    guest_flag("function f(){var Array=function(){return 7;};return Array();}f()===7;");
}

#[test]
fn array_ctor_new_empty_and_list() {
    guest_flag("var a=new Array();var b=new Array(4,6);a.length===0&&b.length===2&&b[1]===6;");
}

#[test]
fn array_ctor_new_length_and_proto() {
    guest_flag(r#"var id=function(x){return x;};var a=id(new Array(3));var b=[];a.length===3&&a[2]===undefined&&a.hasOwnProperty("2")&&Object.getPrototypeOf(a)===Object.getPrototypeOf(b);"#);
}

#[test]
fn array_ctor_to_int32() {
    guest_flag("var a=new Array(1.5);var b=Array(4294967297);var c=Array(-0);a.length===1&&b.length===1&&c.length===0;");
}

#[test]
fn array_ctor_single_non_number() {
    guest_flag(r#"var o={v:2};var a=Array("2");var b=new Array(o);a.length===1&&a[0]==="2"&&b.length===1&&b[0]===o;"#);
}

#[test]
fn array_ctor_abrupt_actual() {
    guest_flag("var n=0;function fail(){throw 9;}var ok=false;try{new Array(fail(),(n=1));}catch(e){ok=e===9;}ok&&n===0;");
}

#[test]
fn array_ctor_gc_retains_elements() {
    guest_flag(r#"var a=Array(""+"kept",{v:7});var t={};var i=0;while(i<40){t={k:i};i=i+1;}a[0]==="kept"&&a[1].v===7;"#);
}

#[test]
fn array_ctor_new_alias_constructs() {
    // `var A=Array` is a builtin alias: the pinned engine constructs through
    // the same route as the direct spelling, so this is guest code, not a
    // compile refusal.
    guest_flag(
        r#"var A=Array;var a=new A(1,2);a.length===2&&Array.isArray(a);"#,
    );
}

#[test]
fn array_ctor_length_bounds() {
    guest_flag(r#"
        var a=Array(1073741823);
        function invalidLength(e) {
            return e.name==="RangeError" && e.message==="invalid array length"
                && e.hasOwnProperty("message")
                && Object.getOwnPropertyDescriptor(e,"message").enumerable===false;
        }
        var lo=false;var hi=false;
        try{Array(-1);}catch(e){lo=invalidLength(e);}
        try{new Array(1073741824);}catch(e){hi=invalidLength(e);}
        lo&&hi&&a.length===1073741823;
    "#);
}

#[test]
fn array_ctor_new_uncaught() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source("new Array(-1);").unwrap();
            machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
            machine.set_fuel(PROBE_FUEL);
            let error = machine.run().expect_err("new Array(-1) must throw");
            assert!(format!("{error:?}").contains("uncaught JavaScript throw"), "{error:?}");
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn array_ns_shadow_local() {
    let source = "function local(){var Array={isArray:function(){return 7;}};return Array.isArray([]);} local()===7;";
    let ir = number_namespace_ir(source).unwrap();
    assert_eq!(array_ns_allocations(&ir), vec![]);
    guest_flag(source);
    eprintln!("ARRAY_NS_SHADOW_LOCAL roots=[]");
}

#[test]
fn array_ns_shadow_script() {
    let source = "var Array={isArray:function(){return 7;}}; Array.isArray([])===7;";
    let ir = number_namespace_ir(source).unwrap();
    assert_eq!(array_ns_allocations(&ir), vec![]);
    guest_flag(source);
    eprintln!("ARRAY_NS_SHADOW_SCRIPT roots=[]");
}

#[test]
fn array_ns_coexists_with_string() {
    let source = r#"String.fromCharCode(65)==="A" && Array.isArray([])===true;"#;
    let ir = number_namespace_ir(source).unwrap();
    let string_roots = string_ns_carrier_roots(&ir);
    assert_eq!(string_roots, vec![11]);
    let allocations = array_ns_allocations(&ir);
    assert_eq!(allocations, vec![(315, 3, 12), (316, 1, 1)]);
    guest_flag(source);
    eprintln!("ARRAY_NS_COEXISTS string_root={string_roots:?} array_allocations={allocations:?}");
}

#[test]
fn array_is_array_true_cases() {
    guest_flag("var a=[1];a.length=4;Array.isArray([])===true && Array.isArray([1])===true && a.length===4 && Array.isArray(a)===true;");
    eprintln!("ARRAY_IS_ARRAY_TRUE []=true [1]=true extended_length_4=true");
}

#[test]
fn array_is_array_tagged() {
    guest_flag(r#"var a=["ab"]; a[0]==="ab" && Array.isArray(a)===true;"#);
    eprintln!("ARRAY_IS_ARRAY_TAGGED string_element_array=true");
}

#[test]
fn array_is_array_false_cases() {
    guest_flag(r#"Array.isArray({0:1,length:1})===false && Array.isArray("ab")===false && Array.isArray(null)===false && Array.isArray(undefined)===false && Array.isArray(5)===false && Array.isArray(function(){})===false;"#);
    eprintln!("ARRAY_IS_ARRAY_FALSE array_like=false string_ab=false null=false undefined=false number_5=false function=false");
}

#[test]
fn array_is_array_no_args() {
    guest_flag("Array.isArray()===false;");
    eprintln!("ARRAY_IS_ARRAY_NO_ARGS result=false");
}

#[test]
fn array_is_array_extra_args_ignored() {
    guest_flag("Array.isArray([],1,2)===true && Array.isArray(1,[])===false;");
    eprintln!("ARRAY_IS_ARRAY_EXTRA array_first=true array_second=false");
}

#[test]
fn array_is_array_borrowed() {
    guest_flag("var f=Array.isArray; f([1])===true && f.call([1])===false;");
    eprintln!("ARRAY_IS_ARRAY_BORROWED detached_array=true array_receiver_no_args=false");
}

#[test]
fn array_for_each_args_order() {
    guest_flag("var id=function(x){return x;};var a=id([1,2]);var seen={n:0,order:0,owner:a};a.forEach(function(v,i,r){'use strict';if(r===this.owner){this.n=this.n+1;this.order=this.order*100+v*10+i;}},seen);seen.n===2&&seen.order===1021;");
    eprintln!("ARRAY_FOREACH_ARGS calls=2 pairs=1:0,2:1 same_array=true");
}

#[test]
fn array_for_each_returns_undefined() {
    guest_flag("typeof [1].forEach(function(){return 42;})==='undefined';");
}

#[test]
fn array_for_each_this_arg() {
    guest_flag("var state={count:0};[1,2].forEach(function(v){'use strict';this.count=this.count+v;},state);var omitted=[1].every(function(){'use strict';return typeof this==='undefined';});state.count===3&&omitted;");
    eprintln!("ARRAY_FOREACH_THIS calls=2 object_sum=3 omitted_strict_this=undefined");
}

#[test]
fn array_for_each_not_function_type_error() {
    guest_flag("var n=0;try{[1].forEach(1);}catch(e){if(e.name==='TypeError'&&e.message==='not a function')n=n+1;}try{[].forEach();}catch(e){if(e.name==='TypeError'&&e.message==='not a function')n=n+1;}n===2;");
}

#[test]
fn array_for_each_shrink_stops() {
    guest_flag("var id=function(x){return x;};var a=id([1,2]);var state={owner:a,n:0};a.forEach(function(v,i){'use strict';this.n=this.n+1;if(i===0)this.owner.pop();},state);state.n===1&&a.length===1;");
    eprintln!("ARRAY_FOREACH_SHRINK before=2 after=1 calls=1");
}

#[test]
fn array_for_each_grow_not_extending() {
    // The previous parent-round contract misread MicroQuickJS's js_array_every:
    // it snapshots the entry length and consults the live length only to stop on shrink.
    guest_flag("var id=function(x){return x;};var a=id([1]);var state={owner:a,n:0,sum:0};a.forEach(function(v,i){'use strict';this.n=this.n+1;this.sum=this.sum+v;if(i===0)this.owner.push(9);},state);state.n===1&&state.sum===1&&a.length===2;");
    eprintln!("ARRAY_FOREACH_GROW before=1 after=2 calls=1 appended_value=9 sum=1");
}

#[test]
fn array_every_true_and_false() {
    guest_flag("[1,2].every(function(v){return v;})===true&&[1,0].every(function(v){return v;})===false&&[].every(function(){throw 42;})===true;");
}

#[test]
fn array_every_early_exit_count() {
    guest_flag("var state={count:0};var r=[1,0,1].every(function(v){'use strict';this.count=this.count+1;return v;},state);r===false&&state.count===2;");
    eprintln!("ARRAY_EVERY_EARLY values=1,0,1 calls=2 result=false");
}

#[test]
fn array_some_true_and_false() {
    guest_flag("[0,1].some(function(v){return v;})===true&&[0,0].some(function(v){return v;})===false&&[].some(function(){throw 42;})===false;");
}

#[test]
fn array_some_early_exit_count() {
    guest_flag("var state={count:0};var r=[0,1,0].some(function(v){'use strict';this.count=this.count+1;return v;},state);r===true&&state.count===2;");
    eprintln!("ARRAY_SOME_EARLY values=0,1,0 calls=2 result=true");
}

#[test]
fn array_callbacks_callback_error_propagates() {
    guest_flag("var token={code:42};var ok=false;try{[1].forEach(function(){'use strict';throw this;},token);}catch(e){ok=e===token;}ok;");
}

#[test]
fn array_callbacks_borrowed_type_error() {
    // `.call` still rejects extra arguments. Calling through an own row
    // exercises the Array adapter itself with a non-Array receiver.
    guest_flag("var f=[].every;var o={0:1,length:1,f:f};var callRejected=false;var brandRejected=false;try{f.call(o,function(){throw 42;});}catch(e){callRejected=e.name==='TypeError';}try{o.f(function(){throw 42;});}catch(e){brandRejected=e.name==='TypeError';}callRejected&&brandRejected&&o[0]===1&&o.length===1;");
}

#[test]
fn array_callbacks_method_value_and_length() {
    guest_flag("var a=[1];var e=a.every;var s=a.some;var f=a.forEach;var ok=true;if(typeof e!=='function'||e.length!==1)ok=false;if(typeof s!=='function'||s.length!==1)ok=false;if(typeof f!=='function'||f.length!==1)ok=false;ok;");
}

#[test]
fn array_callbacks_truthiness_not_conversion() {
    guest_flag("var a=[0,'',null];var no=a.some(function(v){return v;});var yes=[1].every(function(){return {valueOf:function(){throw 1;},toString:function(){throw 2;}};});no===false&&yes===true;");
}

#[test]
fn array_callbacks_large_logical_length() {
    guest_flag("var a=[1];a.length=70000;a.some(function(v,i){return i===0&&v===1;})===true;");
}

#[test]
fn array_map_basic() {
    guest_flag("var r=[1,2,3].map(function(v){return v*2;});r.length===3&&r[0]===2&&r[1]===4&&r[2]===6;");
    eprintln!("ARRAY_MAP_BASIC length=3 values=2,4,6");
}

#[test]
fn array_map_returns_new_array() {
    guest_flag("var a=[1,2];var r=a.map(function(v){return v+1;});r!==a&&Array.isArray(r)&&a.length===2&&a[0]===1&&a[1]===2;");
    eprintln!("ARRAY_MAP_NEW distinct=true array=true source_unchanged=true");
}

#[test]
fn array_map_shrink_result_shape() {
    guest_flag("var id=function(x){return x;};var a=id([1,2,3]);var s={calls:0};var r=a.map(function(v,i,owner){'use strict';this.calls=this.calls+1;if(i===0){owner.pop();owner.pop();}return v*2;},s);a.length===1&&s.calls===1&&r.length===3&&r[0]===2&&r[1]===undefined&&r[2]===undefined&&r.hasOwnProperty('1')&&r.hasOwnProperty('2')&&!r.hasOwnProperty('3');");
    eprintln!("ARRAY_MAP_SHRINK source_length=1 calls=1 result_length=3 values=2,undefined,undefined own_1=true own_2=true own_3=false");
}

#[test]
fn array_map_callback_args() {
    guest_flag("var id=function(x){return x;};var a=id([4,5]);var s={owner:a,calls:0};var r=a.map(function(v,i,owner){'use strict';if(owner===this.owner)this.calls=this.calls+1;return v*10+i;},s);s.calls===2&&r[0]===40&&r[1]===51;");
    eprintln!("ARRAY_MAP_ARGS pairs=4:0,5:1 original_array=true this_arg=true");
}

#[test]
fn array_map_not_function_type_error() {
    guest_flag("var ok=false;try{[1].map(1);}catch(e){ok=e.name==='TypeError'&&e.message==='not a function';}ok;");
    eprintln!("ARRAY_MAP_TYPE_ERROR name=TypeError message=not a function");
}

#[test]
fn array_map_error_propagates() {
    guest_flag("var token={};var ok=false;try{[1].map(function(){'use strict';throw this;},token);}catch(e){ok=e===token;}ok;");
    eprintln!("ARRAY_MAP_THROW same_payload=true");
}

#[test]
fn array_filter_basic() {
    guest_flag("var r=[4,3,2,1].filter(function(v){return v%2===0;});r.length===2&&r[0]===4&&r[1]===2;");
    eprintln!("ARRAY_FILTER_BASIC length=2 values=4,2");
}

#[test]
fn array_filter_truthiness() {
    guest_flag("var r=[10,20,30,40,50].filter(function(v){if(v===10)return 0;if(v===20)return '';if(v===30)return null;if(v===40)return {};return 'a';});r.length===2&&r[0]===40&&r[1]===50;");
    eprintln!("ARRAY_FILTER_TRUTHINESS dropped=0,empty_string,null kept=object,nonempty_string values=40,50");
}

#[test]
fn array_filter_keeps_element_identity() {
    guest_flag("var token={value:7};var r=[token].filter(function(v,i,a){a.pop();return 1;});r.length===1&&r[0]===token&&r[0].value===7;");
    eprintln!("ARRAY_FILTER_IDENTITY predicate=1 result_length=1 original_element=true after_source_pop=true");
}

#[test]
fn array_filter_returns_new_array() {
    guest_flag("var a=[1,2];var r=a.filter(function(){return true;});r!==a&&Array.isArray(r)&&a.length===2&&a[0]===1&&a[1]===2;");
    eprintln!("ARRAY_FILTER_NEW distinct=true array=true source_unchanged=true");
}

#[test]
fn array_filter_empty_result() {
    guest_flag("var a=[1,2];var r=a.filter(function(){});r!==a&&r.length===0&&r[0]===undefined&&!r.hasOwnProperty('0');");
    eprintln!("ARRAY_FILTER_EMPTY distinct=true length=0 own_0=false");
}

#[test]
fn array_map_filter_grow_not_extending() {
    guest_flag("var id=function(x){return x;};var a=id([2]);var b=id([3]);var m={calls:0};var f={calls:0};var cb=function(v,i,owner){'use strict';this.calls=this.calls+1;if(i===0)owner.push(9);return v*2;};var r=a.map(cb,m);var s=b.filter(cb,f);a.length===2&&b.length===2&&m.calls===1&&f.calls===1&&r.length===1&&r[0]===4&&s.length===1&&s[0]===3;");
    eprintln!("ARRAY_MAP_GROW entry=1 source_length=2 calls=1 result_length=1 value=4");
    eprintln!("ARRAY_FILTER_GROW entry=1 source_length=2 calls=1 result_length=1 value=3");
}

#[test]
fn array_reduce_with_initial() {
    guest_flag("[1,2,3].reduce(function(a,v){return a+v;},10)===16;");
    eprintln!("ARRAY_REDUCE_INITIAL result=16");
}

#[test]
fn array_reduce_without_initial() {
    guest_flag("[1,2,3].reduce(function(a,v){return a+v;})===6;");
    eprintln!("ARRAY_REDUCE_NO_INITIAL result=6");
}

#[test]
fn array_reduce_single_no_initial_zero_calls() {
    guest_flag("var token={calls:0};var cb=function(acc){acc.calls=acc.calls+1;};[5].reduce(function(){throw 1;})===5&&[5].reduceRight(function(){throw 1;})===5&&[token].reduce(cb)===token&&[token].reduceRight(cb)===token&&token.calls===0;");
    eprintln!("ARRAY_REDUCE_SINGLE result=5 calls=0 both_directions=true");
}

#[test]
fn array_reduce_empty_no_initial_type_error() {
    guest_flag("var cb=function(){throw 42;};var left=false;var right=false;try{[].reduce(cb);}catch(e){left=e.name==='TypeError'&&e.message==='empty array';}try{[].reduceRight(cb);}catch(e){right=e.name==='TypeError'&&e.message==='empty array';}left&&right;");
    eprintln!("ARRAY_REDUCE_EMPTY name=TypeError message=empty array");
}

#[test]
fn array_reduce_empty_with_initial() {
    guest_flag("var cb=function(){throw 42;};[].reduce(cb,7)===7&&[].reduceRight(cb,7)===7;");
    eprintln!("ARRAY_REDUCE_EMPTY_INITIAL result=7 calls=0 both_directions=true");
}

#[test]
fn array_reduce_callback_args() {
    guest_flag("var id=function(x){return x;};var a=id([4,5]);var s={owner:a,calls:0};var r=a.reduce(function(acc,v,i,owner){'use strict';if(typeof this==='undefined'&&owner===acc.owner&&v===i+4&&i===acc.calls)acc.calls=acc.calls+1;return acc;},s,{ignored:true});r===s&&s.calls===2;");
    eprintln!("ARRAY_REDUCE_ARGS pairs=4:0,5:1 original_array=true strict_this=undefined extra_arg_ignored=true");
}

#[test]
fn array_reduce_right_order() {
    guest_flag("[1,2,3].reduceRight(function(a,v,i,owner){if(i===2)owner.push(9);return a+i;},'')==='210';");
    eprintln!("ARRAY_REDUCE_RIGHT_ORDER initial=explicit indexes=2,1,0 growth_ignored=true");
}

#[test]
fn array_reduce_right_without_initial() {
    guest_flag("[1,2,3].reduceRight(function(a,v,i){return a+':'+i+'='+v;})==='3:1=2:0=1';");
    eprintln!("ARRAY_REDUCE_RIGHT_NO_INITIAL seed=3 indexes=1,0");
}

#[test]
fn array_reduce_not_function_type_error() {
    guest_flag("var left=false;var right=false;try{[1].reduce(1);}catch(e){left=e.name==='TypeError'&&e.message==='not a function';}try{[].reduceRight();}catch(e){right=e.name==='TypeError'&&e.message==='not a function';}left&&right;");
    eprintln!("ARRAY_REDUCE_NOT_FUNCTION name=TypeError message=not a function checked_before_empty=true");
}

#[test]
fn array_reduce_error_propagates() {
    guest_flag("var token={};var ok=false;try{[1].reduce(function(acc){throw acc;},token);}catch(e){ok=e===token;}ok;");
    eprintln!("ARRAY_REDUCE_THROW same_payload=true");
}

#[test]
fn array_reduce_shrink_stops() {
    guest_flag("var id=function(x){return x;};var left=id([1,2,3]);var right=id([1,2,3]);var a=left.reduce(function(acc,v,i,owner){owner.pop();owner.pop();return acc+i;},'');var b=right.reduceRight(function(acc,v,i,owner){owner.pop();owner.pop();return acc+i;},'');a==='0'&&b==='2'&&left.length===1&&right.length===1;");
    eprintln!("ARRAY_REDUCE_SHRINK left_indexes=0 right_indexes=2 calls_each=1 final_length_each=1");
}

#[test]
fn array_reduce_explicit_undefined_initial() {
    guest_flag("[5].reduce(function(acc,v,i){if(acc===undefined&&v===5&&i===0)return 9;},undefined)===9;");
    eprintln!("ARRAY_REDUCE_UNDEFINED_INITIAL index=0 result=9");
}

#[test]
fn array_join_default_separator() {
    char_at_units("[1,2,3].join();", &[49, 44, 50, 44, 51]);
    eprintln!("ARRAY_JOIN_DEFAULT result=1,2,3");
}

#[test]
fn array_join_custom_separator() {
    char_at_units(r#"["a","b"].join("-");"#, &[97, 45, 98]);
    eprintln!("ARRAY_JOIN_CUSTOM result=a-b");
}

#[test]
fn array_join_undefined_separator_is_default() {
    guest_flag(r#"var a=["a","b"];a.join(undefined)==="a,b"&&a.join()===a.join(undefined);"#);
    eprintln!("ARRAY_JOIN_UNDEFINED_SEPARATOR explicit=omitted result=a,b");
}

#[test]
fn array_join_null_undefined_elements() {
    guest_flag(r#"[null,undefined,"x"].join("|")==="||x"&&[null,undefined,1].join("|")==="||1";"#);
    eprintln!("ARRAY_JOIN_NULLISH string_tail=||x number_tail=||1");
}

#[test]
fn array_join_empty_array() {
    char_at_units(r#"[].join("-");"#, &[]);
    eprintln!("ARRAY_JOIN_EMPTY length=0 result_units=0");
}

#[test]
fn array_join_single_empty_string() {
    char_at_units(r#"[""].join("-");"#, &[]);
    eprintln!("ARRAY_JOIN_SINGLE_EMPTY length=1 result_units=0");
}

#[test]
fn array_join_numbers() {
    char_at_units(r#"[1,2,3].join("-");"#, &[49, 45, 50, 45, 51]);
    eprintln!("ARRAY_JOIN_NUMBERS result=1-2-3");
}

#[test]
fn array_join_logical_length_tail() {
    char_at_units(r#"var a=["x"];a.length=4;a.join("-");"#, &[120, 45, 45, 45]);
    eprintln!("ARRAY_JOIN_LENGTH logical_length=4 result=x---");
}

#[test]
fn array_join_borrowed_type_error() {
    guest_flag(r#"var a=[];var o={0:"x",length:1,j:a.join,s:a.toString};var j=false;var s=false;try{o.j();}catch(e){j=e.name==="TypeError";}try{o.s();}catch(e){s=e.name==="TypeError";}j&&s&&o[0]==="x"&&o.length===1;"#);
    eprintln!("ARRAY_JOIN_BRAND join=TypeError toString=TypeError object_unchanged=true");
}

#[test]
fn array_to_string_equals_join_comma() {
    guest_flag(r#"var a=["a","b"];var p=Object.getPrototypeOf(a);a.toString()==="a,b"&&a.toString("-")===a.join()&&a.join.length===1&&a.toString.length===0&&p.hasOwnProperty("join")&&p.hasOwnProperty("toString");"#);
    eprintln!("ARRAY_JOIN_TO_STRING result=a,b join_arity=1 toString_arity=0 ignores_argument=true");
}

#[test]
fn array_join_rereads_after_coercion() {
    char_at_units(r#"var id=function(x){return x;};var a=[true,"x","y","z"];var sep={owner:id(a),toString:function(){'use strict';this.owner.pop();this.owner.pop();return "\uD83D\uDE00";}};var first={owner:id(a),toString:function(){'use strict';this.owner.pop();return "t";}};a[0]=first;a.join(sep);"#, &[116, 0xD83D, 0xDE00, 0xD83D, 0xDE00, 0xD83D, 0xDE00]);
    eprintln!("ARRAY_JOIN_REREAD initial_length=4 after_separator=2 after_element=1 units=7");
}

#[test]
fn array_splice_no_args_empty_result() {
    guest_flag("var a=[1];var r=a.splice();r!==a&&Array.isArray(r)&&r.length===0&&a.length===1&&a[0]===1&&a.splice.length===2;");
}

#[test]
fn array_splice_start_only_deletes_to_end() {
    guest_flag("var id=function(x){return x;};var a=id([1,2,3]);var r=a.splice(1);r.length===2&&r[0]===2&&r[1]===3&&a.length===1&&a[0]===1;");
}

#[test]
fn array_splice_delete_count() {
    guest_flag("var a=[1,2,3,4,5];var r=a.splice(1,2);r.join() === '2,3' && a.join() === '1,4,5';");
    eprintln!("ARRAY_SPLICE_DELETE removed=[2,3] source=[1,4,5]");
}

#[test]
fn array_splice_insert_only() {
    guest_flag("var a=[1,2,3];var alias=a;var r=a.splice(1,0,7,8);r.length===0&&alias.join()==='1,7,8,2,3';");
}

#[test]
fn array_splice_replace() {
    guest_flag("var a=[1,2,3,4];var r=a.splice(1,2,'x');var s=a.splice(1,1,'y','z');r.join()==='2,3'&&s.join()==='x'&&a.join()==='1,y,z,4';");
}

#[test]
fn array_splice_negative_start_clamp() {
    guest_flag("var a=[1,2,3];var r=a.splice(-2,1);var s=a.splice(-99,1);r.join()==='2'&&s.join()==='1'&&a.join()==='3';");
}

#[test]
fn array_splice_start_beyond_length() {
    guest_flag("var a=[1];var r=a.splice(Infinity,99,2);r.length===0&&a.join()==='1,2';");
}

#[test]
fn array_splice_nan_and_fractional_start() {
    guest_flag("var a=[1,2,3,4];var r=a.splice(NaN,1.9);var s=a.splice('1.9',true);var t=a.splice(0,undefined);var u=a.splice(0,-1);r.join()==='1'&&s.join()==='3'&&t.length===0&&u.length===0&&a.join()==='2,4';");
}

#[test]
fn array_splice_returns_removed_elements() {
    guest_flag("var obj={x:1};var a=[0,obj,2];var r=a.splice(1,1);r[0].x=7;r!==a&&Array.isArray(r)&&r[0]===obj&&obj.x===7&&a.join()==='0,2';");
}

#[test]
fn array_splice_logical_length_tail() {
    guest_flag("var a=[1,2,3];a.length=8;var r=a.splice(1,1);var s=a.splice(-2,2);r[0]===2&&a[1]===3&&a[2]===undefined&&a.length===5&&s.length===2&&s.hasOwnProperty('1')&&s[1]===undefined;");
}

#[test]
fn array_splice_length_modified_type_error() {
    guest_flag("var id=function(x){return x;};var a=id([1,2,3]);var state={order:0};var start={owner:a,state:state,valueOf:function(){'use strict';this.state.order=1;this.owner.pop();return 0;}};var count={state:state,valueOf:function(){'use strict';this.state.order=this.state.order+2;return 1;}};var ok=false;try{a.splice(start,count,9);}catch(e){ok=e.name==='TypeError'&&e.message==='array length was modified';}ok&&state.order===3&&a.length===2&&a[0]===1&&a[1]===2;");
    guest_flag("var id=function(x){return x;};var a=id([4,5]);var count={owner:a,valueOf:function(){'use strict';this.owner.push(6);return 1;}};var ok=false;try{a.splice(0,count,9);}catch(e){ok=e.name==='TypeError'&&e.message==='array length was modified';}ok&&a.length===3&&a[0]===4&&a[1]===5&&a[2]===6;");
}

#[test]
fn array_splice_borrowed_type_error() {
    guest_flag("var o={0:1,length:1,splice:[].splice};var ok=false;try{o.splice({valueOf:function(){throw 42;}},1,9);}catch(e){ok=e.name==='TypeError';}ok&&o[0]===1&&o.length===1;");
}

#[test]
fn array_splice_shrink_updates_length_read() {
    guest_flag("var a=[1,2,3];var r=a.splice(1,1);r[0]===2&&a.length===2&&a[0]===1&&a[1]===3;");
}

#[test]
fn array_splice_replace_same_length_index_read() {
    guest_flag("var a=[1,2,3];var r=a.splice(1,1,'x');r[0]===2&&a.length===3&&a[0]===1&&a[1]==='x'&&a[2]===3;");
}

#[test]
fn array_splice_grow_then_new_index_read() {
    guest_flag("var a=[1,2,3];var r=a.splice(1,0,9,8);r.length===0&&a.length===5&&a[1]===9&&a[2]===8&&a[3]===2;");
}

#[test]
fn array_splice_put_respects_physical_extent() {
    // A tagged prefix through index 13104 cannot fit alongside the realm's
    // other live storage. Start with a tagged array so splice reaches
    // array_put's own expansion path for both a small hole and the limit.
    guest_flag("var a=['a'];a.length=13105;var r=a.splice(1,1,'x');r.length===1&&r[0]===undefined&&a[0]==='a'&&a[1]==='x'&&a.length===13105&&a[13104]===undefined;");
    let error = repeat_status("var a=['a'];a.length=13105;a.splice(13105,0,'x');")
        .expect_err("a tagged store past the physical extent must fail");
    assert!(
        error.contains("HostError") && error.contains("assertion failed"),
        "{error}"
    );
}

#[test]
fn uri_manifest_host_costs_and_codes() {
    let ir = number_namespace_ir("Number; Boolean; RangeError; [1]; \"ab\".repeat(1); String.fromCharCode(65); String.fromCodePoint(0x1F600); Array.isArray([]);").unwrap();
    let codes = string_ns_method_codes(&ir);
    assert_eq!(codes, vec![310, 311, 312], "{codes:?}");
    let roots = string_ns_carrier_roots(&ir);
    assert_eq!(roots.len(), 1, "{roots:?}");
    let allocate = helper(&ir, "__compat_es5_object_allocate");
    let mut host_codes = Vec::new();
    for function in ir.function_impls.values() {
        for stmt in &function.body_stmts {
            walk_stmt(stmt, &mut |expr| {
                if let vm::Expr::Call(fn_id, _, args, _, _) = expr
                    && *fn_id == allocate
                    && let Some(vm::Expr::Int(code)) = args.get(4)
                {
                    host_codes.push(*code);
                }
            });
        }
    }
    for code in std::iter::once(299).chain(304..=327) {
        assert_eq!(host_codes.iter().filter(|&&value| value == code).count(), 1, "code {code} in {host_codes:?}");
    }
    for code in 300..=303 {
        assert!(!host_codes.contains(&code), "reserved guest code {code} in {host_codes:?}");
    }
    eprintln!("ARRAY_SORT_CODES sort=327 count_each=1 reserved_free=300..=303 unchanged=299,304..=326 string_root={roots:?}");
}

/// Repeat precheck budget read from the installed helper, not from a copied literal.
fn repeat_bridge_ceiling(ir: &vm::FrontendIr) -> i64 {
    let body = format!(
        "{:?}",
        ir.function_impls[&helper(ir, "__compat_es5_string_repeat")]
    );
    let bounds = debug_lt_ints(&body);
    let ceiling = bounds.into_iter().max().unwrap_or_else(|| {
        let at = body.find("Lt(").unwrap_or(0);
        let end = (at + 160).min(body.len());
        panic!("repeat ceiling missing near {}", &body[at..end]);
    });
    let floats = debug_lt_floats(&body);
    assert!(
        floats.iter().any(|n| *n == ceiling as f64),
        "float ceiling {floats:?} does not include {ceiling}"
    );
    assert!(ceiling > 1_000 && ceiling % 2 == 0, "repeat ceiling {ceiling}");
    ceiling
}

fn debug_lt_ints(text: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut rest = text;
    let key = "Lt(Int(";
    while let Some(at) = rest.find(key) {
        rest = &rest[at + key.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<i64>() {
            out.push(n);
        }
    }
    out
}

fn debug_lt_floats(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut rest = text;
    let key = "Lt(Float(";
    while let Some(at) = rest.find(key) {
        rest = &rest[at + key.len()..];
        let token: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        if let Ok(n) = token.parse::<f64>() {
            out.push(n);
        }
    }
    out
}

fn repeat_realm_cells(ir: &vm::FrontendIr) -> i64 {
    let body = format!("{:?}", ir.function_impls[&helper(ir, "__compat_es5_node_begin")]);
    let mut best = 0i64;
    let mut rest = body.as_str();
    let key = "Int(";
    while let Some(at) = rest.find(key) {
        rest = &rest[at + key.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<i64>() {
            best = best.max(n);
        }
    }
    assert!(best > 1_000, "realm budget missing");
    best
}

struct RepeatProbe {
    class: String,
    fuel_used: u64,
    fuel_left: u64,
    frames: String,
    flag: Option<bool>,
}

fn repeat_probe(source: &str, fuel: u64) -> RepeatProbe {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| panic!("compile {source:?} {error:?}"));
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(fuel);
            let status = machine.run();
            let left = machine.get_fuel().unwrap_or(0);
            let class = match &status {
                Ok(vm::VmStatus::Halted) => "halt".to_owned(),
                Ok(other) => format!("other_status:{other:?}"),
                Err(err) => {
                    let text = format!("{err:?}");
                    if text.contains("assertion failed") {
                        "host_assert".to_owned()
                    } else if text.contains("RangeError") {
                        "range".to_owned()
                    } else {
                        format!("other:{text}")
                    }
                }
            };
            let flag = machine.stack().last().and_then(|value| {
                let vm::Value::Map(map) = value else {
                    return None;
                };
                match map.get(&vm::Value::string("flag")) {
                    Some(vm::Value::Bool(bit)) => Some(*bit),
                    _ => None,
                }
            });
            let frames = repeat_frame_names(&machine);
            RepeatProbe {
                class,
                fuel_used: fuel.saturating_sub(left),
                fuel_left: left,
                frames,
                flag,
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

fn repeat_frame_names(machine: &vm::Vm) -> String {
    let program = machine.program();
    let debug = program.debug.as_ref();
    let names: Vec<String> = machine
        .execution_frames()
        .iter()
        .map(|frame| {
            let Some(id) = frame.prototype_id else {
                return "current".to_owned();
            };
            match program
                .callable_prototypes
                .get(id as usize)
                .map(|prototype| &prototype.target)
            {
                Some(vm::CallableTarget::ScriptFunction(index)) => debug
                    .and_then(|info| info.functions.get(*index as usize))
                    .map(|function| function.name.clone())
                    .unwrap_or_else(|| format!("script:{index}")),
                Some(vm::CallableTarget::HostImport(index)) => format!("host:{index}"),
                None => format!("proto:{id}"),
            }
        })
        .collect();
    format!("ip={} frames={names:?}", machine.ip())
}

fn box_field<'a>(value: &'a vm::Value, key: &str) -> &'a vm::Value {
    let vm::Value::Map(map) = value else {
        panic!("{key} carrier {value:?}");
    };
    map.get(&vm::Value::string(key))
        .unwrap_or_else(|| panic!("missing {key} in {map:?}"))
}

struct RepeatEmptyInfinity;

impl vm::SourcePlugin for RepeatEmptyInfinity {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("repeat-empty-infinity");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            context,
        )?;
        let make = helper(&ir, "__compat_ref_make");
        let read = helper(&ir, "__compat_ref_read");
        let checked = helper(&ir, "__compat_es5_checked_base");
        let carrier = ir
            .stmts
            .iter()
            .find_map(|stmt| match stmt {
                S::Let {
                    index,
                    expr: E::Call(target, _, _, _, _),
                    ..
                } if *target == make => Some(*index),
                _ => None,
            })
            .expect("real carrier");
        let receiver = ir
            .local_bindings
            .iter()
            .find(|(name, _)| name == "s")
            .map(|(_, slot)| *slot)
            .expect("binding s");
        let result = ir
            .stmts
            .iter()
            .rev()
            .find_map(|stmt| match stmt {
                S::Expr {
                    expr: E::Var(index),
                    ..
                } => Some(*index),
                _ => None,
            })
            .expect("repeat completion");
        let base = u16::try_from(ir.locals).expect("observer slot");
        ir.locals += 1;
        ir.stmts.push(S::Expr {
            expr: E::String("REPEAT_EMPTY_INFINITY".into()),
            line: 1,
        });
        ir.stmts.push(S::Expr {
            expr: E::Var(receiver),
            line: 1,
        });
        ir.stmts.push(S::Expr {
            expr: E::Var(result),
            line: 1,
        });
        ir.stmts.push(S::Let {
            index: base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: call(
                checked,
                vec![
                    E::Var(carrier),
                    get(result, "ref_slot"),
                    get(result, "generation"),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(S::Expr {
            expr: call(
                read,
                vec![E::Var(carrier), num(plus(E::Var(base), E::Int(2)))],
            ),
            line: 1,
        });
        Ok(ir)
    }
}

struct RepeatHeapEnd;

impl vm::SourcePlugin for RepeatHeapEnd {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("repeat-heap-end");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            context,
        )?;
        let make = helper(&ir, "__compat_ref_make");
        let read = helper(&ir, "__compat_ref_read");
        let carrier = ir
            .stmts
            .iter()
            .find_map(|stmt| match stmt {
                S::Let {
                    index,
                    expr: E::Call(target, _, _, _, _),
                    ..
                } if *target == make => Some(*index),
                _ => None,
            })
            .expect("real carrier");
        ir.stmts.push(S::Expr {
            expr: call(read, vec![E::Var(carrier), E::Float(1.0)]),
            line: 1,
        });
        Ok(ir)
    }
}

/// Empty receiver plus +Infinity returns that receiver: Halted, zero units,
/// same ref_slot/generation. Identity is the slot pair, not `===`.
#[test]
fn string_repeat_empty_infinity() {
    static OBS: RepeatEmptyInfinity = RepeatEmptyInfinity;
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            let mut machine = probe_vm(r#"var s="";s.repeat(Infinity);"#, &OBS, "repeat-empty-infinity");
            let status = machine.run();
            assert!(
                matches!(status, Ok(vm::VmStatus::Halted)),
                "REPEAT_EMPTY_INFINITY status={status:?}"
            );
            let stack = machine.stack().to_vec();
            let marker = vm::Value::string("REPEAT_EMPTY_INFINITY");
            let at = stack
                .iter()
                .rposition(|value| value == &marker)
                .expect("identity marker");
            let receiver = &stack[at + 1];
            let result = &stack[at + 2];
            let units = &stack[at + 3];
            let slot = box_field(receiver, "ref_slot");
            let generation = box_field(receiver, "generation");
            assert_eq!(box_field(receiver, "tag"), &vm::Value::Int(4));
            assert_eq!(box_field(result, "tag"), &vm::Value::Int(4));
            assert_eq!(box_field(result, "ref_slot"), slot);
            assert_eq!(box_field(result, "generation"), generation);
            assert!(matches!(slot, vm::Value::Int(n) if *n > 0), "{slot:?}");
            assert!(
                matches!(generation, vm::Value::Int(n) if *n > 0),
                "{generation:?}"
            );
            assert!(
                matches!(units, vm::Value::Float(n) if *n == 0.0)
                    || matches!(units, vm::Value::Int(0)),
                "units={units:?}"
            );
            eprintln!(
                "REPEAT_EMPTY_INFINITY status=Halted units=0 ref_slot={slot:?} generation={generation:?} same_as_receiver=true"
            );
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Negative saturation is tested before the empty-string return. -Infinity is
/// the non-finite negative endpoint, so the guest sees RangeError, not the
/// original receiver and not a host assert.
#[test]
fn string_repeat_empty_negative_infinity() {
    guest_flag(
        r#"var hit=0;try{"".repeat(-Infinity);}catch(e){if(e.name==="RangeError")hit=1;}hit===1;"#,
    );
    eprintln!("REPEAT_EMPTY_NEG_INFINITY name=RangeError before_empty=true");
}

/// Large finite counts must saturate to the signed 32-bit endpoints, not wrap
/// modulo 2^32. Wrap of 2^32 is 0 (empty success); wrap of 2^31 and 2^32-1 is
/// negative (RangeError); wrap of -(2^31)-1 is +2^31-1 (host assert).
#[test]
fn string_repeat_saturation_endpoints() {
    let ir = parse(r#""ab".repeat(1);"#).expect("repeat helper");
    let body = format!(
        "{:?}",
        ir.function_impls[&helper(&ir, "__compat_es5_string_repeat")]
    );
    assert!(body.contains("2147483647"), "positive endpoint missing");
    assert!(body.contains("-2147483648"), "negative endpoint missing");
    guest_flag(
        r#"var hit=0;try{"".repeat(-2147483649);}catch(e){if(e.name==="RangeError")hit=hit+1;}try{"ab".repeat(-2147483649);}catch(e){if(e.name==="RangeError")hit=hit+1;}try{"ab".repeat(-2147483648);}catch(e){if(e.name==="RangeError")hit=hit+1;}hit===3&&"ab".repeat(0)==="";"#
    );
    let mut seen = Vec::new();
    for (label, source, expected) in [
        ("pow32", r#""ab".repeat(4294967296);"#, "host_assert"),
        ("pow31", r#""ab".repeat(2147483648);"#, "host_assert"),
        ("int32_max", r#""ab".repeat(2147483647);"#, "host_assert"),
        ("pow32_minus1", r#""ab".repeat(4294967295);"#, "host_assert"),
        ("empty_pow32", "\"\".repeat(4294967296);", "halt"),
    ] {
        let probe = repeat_probe(source, PROBE_FUEL);
        eprintln!(
            "REPEAT_SAT_ENDPOINTS case={label} class={} fuel_left={} {}",
            probe.class, probe.fuel_left, probe.frames
        );
        assert_eq!(probe.class, expected, "{label} {}", probe.frames);
        assert!(probe.fuel_left > 0, "{label} exhausted fuel");
        seen.push(format!("{label}={}", probe.class));
    }
    eprintln!(
        "REPEAT_SAT_ENDPOINTS {} neg_2147483649=RangeError int32_min=RangeError empty_neg=RangeError zero_control=empty wrap32_excluded=true",
        seen.join(" ")
    );
}

fn repeat_publish_ok(units: i64, fuel: u64) -> bool {
    let probe = repeat_probe(&format!(r#""a".repeat({units}).length==={units};"#), fuel);
    eprintln!(
        "REPEAT_CAPACITY_PROBE units={units} class={} flag={:?} fuel_used={} fuel_left={}",
        probe.class, probe.flag, probe.fuel_used, probe.fuel_left
    );
    assert!(probe.fuel_left > 0, "units {units} exhausted fuel");
    match probe.class.as_str() {
        "halt" => {
            assert_eq!(probe.flag, Some(true), "units {units} published the wrong length");
            true
        }
        "host_assert" => false,
        other => panic!("units {units} unexpected {other}"),
    }
}

/// Two-unit products B-2 and B pass the bridge precheck and enter concat; B+2
/// is rejected by that assert before the loop. A separate one-unit search then
/// records the realm's actual publish boundary. B itself is not that boundary:
/// the string node needs the units plus the raw and string headers.
#[test]
fn string_repeat_capacity_two_layer() {
    const FUEL: u64 = 200_000_000;
    let ir = parse(r#""ab".repeat(2);"#).expect("repeat helper");
    let ceiling = repeat_bridge_ceiling(&ir);
    let realm = repeat_realm_cells(&ir);
    eprintln!("REPEAT_CAPACITY_TWO_LAYER B={ceiling} realm_cells={realm}");
    let under = repeat_probe(&format!(r#""ab".repeat({});"#, (ceiling - 2) / 2), FUEL);
    let at = repeat_probe(&format!(r#""ab".repeat({});"#, ceiling / 2), FUEL);
    let over = repeat_probe(&format!(r#""ab".repeat({});"#, (ceiling + 2) / 2), FUEL);
    for (label, probe) in [("B-2", &under), ("B", &at), ("B+2", &over)] {
        eprintln!(
            "REPEAT_CAPACITY_PHASE product={label} class={} fuel_used={} fuel_left={} {}",
            probe.class, probe.fuel_used, probe.fuel_left, probe.frames
        );
        assert_eq!(probe.class, "host_assert", "{label}");
        assert!(probe.fuel_left > 0, "{label} exhausted fuel");
    }
    let loop_gap = at.fuel_used.abs_diff(under.fuel_used);
    let reject_gap = at.fuel_used.saturating_sub(over.fuel_used);
    assert!(
        reject_gap > 100_000 && reject_gap > loop_gap.saturating_mul(8),
        "B+2 did not reject before concat: under={} at={} over={} loop_gap={loop_gap} reject_gap={reject_gap}",
        under.fuel_used, at.fuel_used, over.fuel_used
    );
    static HEAP: RepeatHeapEnd = RepeatHeapEnd;
    let heap_end = std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            let mut machine = probe_vm(r#""a".repeat(2);"#, &HEAP, "repeat-heap-end");
            assert!(matches!(machine.run(), Ok(vm::VmStatus::Halted)));
            let end = match machine.stack().last() {
                Some(vm::Value::Float(n)) => *n as i64,
                Some(vm::Value::Int(n)) => *n,
                other => panic!("heap_end {other:?}"),
            };
            reset(&mut machine);
            end
        })
        .unwrap()
        .join()
        .unwrap();
    let hint = (realm - heap_end - 6).clamp(2, realm);
    eprintln!("REPEAT_CAPACITY_HINT heap_end={heap_end} hint={hint}");
    let (mut lo, mut hi) = if repeat_publish_ok(hint, FUEL) {
        let mut success = hint;
        let mut step = 1i64;
        let mut fail = hint + 1;
        while fail < realm && repeat_publish_ok(fail, FUEL) {
            success = fail;
            step = step.saturating_mul(2);
            fail = success.saturating_add(step).min(realm);
        }
        assert!(fail > success, "no publish failure below realm {realm}");
        (success, fail)
    } else {
        assert!(repeat_publish_ok(2, FUEL), "two-unit publish failed");
        (2, hint)
    };
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if repeat_publish_ok(mid, FUEL) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    assert!(lo < ceiling, "published length {lo} is not below bridge ceiling {ceiling}");
    eprintln!(
        "REPEAT_CAPACITY_TWO_LAYER B={ceiling} realm_cells={realm} product_B_precheck=pass product_B_publish=fail product_Bp2=reject_before_concat fuel_Bm2={} fuel_B={} fuel_Bp2={} max_publish_units={lo} adjacent_fail_units={hi} documented_node_floor=B+6",
        under.fuel_used, at.fuel_used, over.fuel_used
    );
}

#[test]
fn typed_array_bpe_constructor_values() {
    guest_flag(r#"
        var matched = 0;
        if (Uint8Array.BYTES_PER_ELEMENT === 1) matched = matched + 1;
        if (Uint8ClampedArray.BYTES_PER_ELEMENT === 1) matched = matched + 1;
        if (Int8Array.BYTES_PER_ELEMENT === 1) matched = matched + 1;
        if (Int16Array.BYTES_PER_ELEMENT === 2) matched = matched + 1;
        if (Uint16Array.BYTES_PER_ELEMENT === 2) matched = matched + 1;
        if (Int32Array.BYTES_PER_ELEMENT === 4) matched = matched + 1;
        if (Uint32Array.BYTES_PER_ELEMENT === 4) matched = matched + 1;
        if (Float32Array.BYTES_PER_ELEMENT === 4) matched = matched + 1;
        if (Float64Array.BYTES_PER_ELEMENT === 8) matched = matched + 1;
        matched === 9;
    "#);
}

#[test]
fn typed_array_bpe_prototype_values() {
    guest_flag(r#"
        var matched = 0;
        if (Uint8Array.prototype.BYTES_PER_ELEMENT === 1) matched = matched + 1;
        if (Uint8ClampedArray.prototype.BYTES_PER_ELEMENT === 1) matched = matched + 1;
        if (Int8Array.prototype.BYTES_PER_ELEMENT === 1) matched = matched + 1;
        if (Int16Array.prototype.BYTES_PER_ELEMENT === 2) matched = matched + 1;
        if (Uint16Array.prototype.BYTES_PER_ELEMENT === 2) matched = matched + 1;
        if (Int32Array.prototype.BYTES_PER_ELEMENT === 4) matched = matched + 1;
        if (Uint32Array.prototype.BYTES_PER_ELEMENT === 4) matched = matched + 1;
        if (Float32Array.prototype.BYTES_PER_ELEMENT === 4) matched = matched + 1;
        if (Float64Array.prototype.BYTES_PER_ELEMENT === 8) matched = matched + 1;
        matched === 9;
    "#);
}

#[test]
fn typed_array_bpe_instance_inheritance() {
    guest_flag(r#"
        var v = new Uint16Array(new ArrayBuffer(8));
        v.BYTES_PER_ELEMENT === 2 && v.hasOwnProperty("BYTES_PER_ELEMENT") === false;
    "#);
}

#[test]
fn typed_array_bpe_descriptors() {
    // Pinned mqjs exposes no descriptor API at all (`Object.getOwnPropertyDescriptor`
    // is not a function there), and it stores no attribute bits either: `JSProperty`
    // carries only `prop_type` (mquickjs.c:266-283), deletion never refuses
    // (mquickjs.c:3414-3415), and each `TA_DEF` row is exactly one
    // `JS_PROP_DOUBLE_DEF("BYTES_PER_ELEMENT", bpe, 0)` entry (mqjs_stdlib.c:268-276).
    // All 18 rows must therefore be own, enumerable, writable and configurable.
    // One call site plus a loop keeps the probe exact while keeping the compiled
    // guest program small; a descriptor read is not used and is not needed.
    guest_flag(r#"
        function probe(target, width) {
            "use strict";
            var key = "BYTES_PER_ELEMENT";
            var own = target.hasOwnProperty(key);
            var exact = target[key] === width;
            var keys = Object.keys(target);
            var visible = false;
            for (var i = 0; i < keys.length; i = i + 1) {
                if (keys[i] === key) visible = true;
            }
            target[key] = 99;
            var wrote = target[key] === 99;
            var removed = delete target[key];
            var gone = removed && !target.hasOwnProperty(key) && !(key in target);
            return own && exact && visible && wrote && gone;
        }
        var targets = [
            Uint8Array, Uint8Array.prototype,
            Uint8ClampedArray, Uint8ClampedArray.prototype,
            Int8Array, Int8Array.prototype,
            Int16Array, Int16Array.prototype,
            Uint16Array, Uint16Array.prototype,
            Int32Array, Int32Array.prototype,
            Uint32Array, Uint32Array.prototype,
            Float32Array, Float32Array.prototype,
            Float64Array, Float64Array.prototype
        ];
        var widths = [1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 4, 8, 8];
        var count = 0;
        for (var i = 0; i < targets.length; i = i + 1) {
            if (probe(targets[i], widths[i])) count = count + 1;
        }
        count === 18;
    "#);
}

#[test]
fn typed_array_methods_subarray_wide_chains() {
    guest_flag(r#"
        var b = new ArrayBuffer(32);
        var a = new Int32Array(b, 4);
        var s = a.subarray(1, 3);
        if (!(s.byteOffset === 8 && s.length === 2 && s.byteLength === 8 && s.buffer === b)) throw 'Int32 window';
        var t = s.subarray(1);
        if (!(t.byteOffset === 12 && t.length === 1 && t.byteLength === 4)) throw 'Int32 nested window';

        var f = new ArrayBuffer(32);
        var d = new Float64Array(f, 8);
        var u = d.subarray(1, 3);
        if (!(u.byteOffset === 16 && u.length === 2 && u.byteLength === 16 && u.buffer === f)) throw 'Float64 window';
        var v = u.subarray(1);
        v.byteOffset === 24 && v.length === 1 && v.byteLength === 8;
    "#);
}

#[test]
fn typed_array_methods_borrowed_receivers() {
    guest_flag(r#"
        if (Uint8Array.prototype.join.call({0: 1, length: 1}) !== '1') throw 'ordinary object join';
        var joinNull = false;
        var toStringNull = false;
        var setArray = false;
        try { Uint8Array.prototype.join.call(null); } catch (e) { joinNull = e.name === 'TypeError'; }
        try { Uint8Array.prototype.toString.call(null); } catch (f) { toStringNull = f.name === 'TypeError'; }
        try { Uint8Array.prototype.set.call([1, 2], 0); } catch (g) { setArray = g.name === 'TypeError'; }
        joinNull && toStringNull && setArray;
    "#);
}

#[test]
fn typed_array_methods_join_borrowed_tagged_array() {
    guest_flag(r#"
        // The string element makes this literal a tagged Array.
        var a = [4, 'x', null, 5];
        Uint8Array.prototype.join.call(a, '-') === '4-x--5';
    "#);
}

// ------------------------------------------------------ Error brand (stage 1)

#[test]
fn es51_error_brand_stage1_surface() {
    guest_flag(
        r#"// Recorded baseline surface of `new Error("boom")`: the own message row is
// present, `Object.keys` enumerates none of the branded rows, the realm
// prototype still holds `name === "Error"` and `message === ""`, and `name`
// resolves through the real prototype chain.
var e = new Error("boom");
var p = Object.getPrototypeOf(e);
e.message === "boom"
    && e.hasOwnProperty("message")
    && Object.keys(e).length === 0
    && p.name === "Error"
    && p.hasOwnProperty("name")
    && p.message === ""
    && e.name === "Error";"#,
    );
}
