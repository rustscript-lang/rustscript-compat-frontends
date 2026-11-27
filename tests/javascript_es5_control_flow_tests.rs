//! P6-A coherent conditional statements and conditional returns.
//! Expectations follow ES5.1 12.1–12.5 and 14 UpdateEmpty, not a reconstructed value.
use pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog;
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};
#[path = "javascript_es5/arity_observer.rs"]
mod observer;

fn field(value: &Value, key: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed result: {value:?}")
    };
    map.get(&Value::string(key)).expect("box field").clone()
}

fn execute(source: &str, native: bool, observer: Option<observer::Observer>) -> Vec<Value> {
    let catalog = reference_array_cli_effective_catalog();
    let mut options = vm::CompileSourceFileOptions::new().with_host_api_catalog(catalog.clone());
    let flavor = if let Some(observer) = observer {
        options = options.with_source_plugin(Box::leak(Box::new(observer)));
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-arity-observe"))
    } else {
        options = options.with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
        javascript_es5_flavor()
    };
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .unwrap_or_else(|error| panic!("compile {source:?}: {error:?}"));
    let mut machine = compiled
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("arrays");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owners");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("snapshots");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("math");
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("error adapter");
    registry.allow_builtin("math::floor").expect("floor");
    registry
        .allow_builtin("math::is_finite")
        .expect("is_finite");
    registry
        .allow_builtin("math::powf")
        .expect("String adapter capability");
    registry
        .allow_builtin("math::trunc")
        .expect("String adapter capability");
    // The ordinary realm's bootstrap publishes its fixed ASCII strings through
    // the Core bytes initializer, so this capability is part of every emitted
    // realm program (the Object.keys harness allows the same builtin).
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).expect("registry");
    machine.set_fuel(10_000_000);
    if native {
        let program = machine.program();
        let frames: Vec<usize> = program
            .callable_prototypes
            .iter()
            .map(|prototype| prototype.frame_local_count)
            .collect();
        let max_frame = frames.iter().copied().max().unwrap_or(0);
        let sum_frame: u64 = frames
            .iter()
            .map(|count| u64::try_from(*count).unwrap_or(0))
            .sum();
        eprintln!(
            "P6A_NATIVE program_code={} local_count={} prototypes={} max_frame_local_count={} sum_frame_local_count={} counts=allocated_not_live",
            program.code.len(),
            program.local_count,
            frames.len(),
            max_frame,
            sum_frame
        );
        eprintln!("P6A_NATIVE phase=compile_aot source={source}");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        machine.compile_aot().expect("native compile");
        eprintln!("P6A_NATIVE phase=compiled source={source}");
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }
    assert_eq!(
        machine.run().unwrap_or_else(|error| panic!(
            "run {source:?}: {error:?}; ip={}; stack={:?}; frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        )),
        VmStatus::Halted
    );
    if native {
        let count = machine.aot_exec_count();
        let info = machine.dump_aot_info();
        eprintln!("P6A_NATIVE actual_vm_aot_exec_count={count}\n{info}");
        assert!(
            count > 0 && info.contains("lowering=ssa"),
            "live native authority missing: {info}"
        );
        assert!(
            machine
                .capture_certification_stats()
                .is_some_and(|facts| facts.valid),
            "live source certification must stay valid"
        );
    }
    let stack = machine.stack().to_vec();
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    stack
}

fn read(source: &str, name: &str) -> Value {
    let values = execute(
        source,
        false,
        Some(observer::Observer {
            reads: vec![name.into()],
            ..Default::default()
        }),
    );
    values.last().cloned().expect("binding read")
}

fn assert_number(value: &Value, expected: f64) {
    assert_eq!(field(value, "tag"), Value::Int(3), "{value:?}");
    let Value::Float(actual) = field(value, "number") else {
        panic!("Float payload")
    };
    assert_eq!(
        actual.to_bits(),
        expected.to_bits(),
        "{actual} != {expected}"
    );
}

fn assert_bool(value: &Value, expected: bool) {
    assert_eq!(field(value, "tag"), Value::Int(2));
    assert_eq!(field(value, "flag"), Value::Bool(expected));
}

fn assert_undefined(value: &Value) {
    assert_eq!(field(value, "tag"), Value::Int(0), "{value:?}");
}

fn refuse(source: &str, fragment: &str) {
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(reference_array_cli_effective_catalog());
    let error = match vm::compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        options,
    ) {
        Ok(_) => panic!("must refuse: {source}"),
        Err(error) => format!("{error:?}"),
    };
    assert!(
        error.contains(fragment) && error.contains("span: Some"),
        "{source}: {error}"
    );
}

/// Raw script completion is the trailing (present, value) pair emitted for a
/// runtime conditional. Empty is present=false; explicit Undefined is present=true.
fn script_completion(source: &str) -> (bool, Value) {
    let values = execute(source, false, None);
    let n = values.len();
    assert!(n >= 2, "completion pair missing for {source:?}: {values:?}");
    let present = match &values[n - 2] {
        Value::Bool(flag) => *flag,
        other => panic!("present flag for {source:?}: {other:?}"),
    };
    (present, values[n - 1].clone())
}

fn node_value(source: &str) -> serde_json::Value {
    let script = r#"
const vm = require('vm');
let source = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => { source += chunk; });
process.stdin.on('end', () => {
  const result = vm.runInNewContext(source, Object.create(null), { timeout: 1000 });
  let kind = typeof result;
  if (kind === 'number' && Object.is(result, -0)) kind = '-0';
  else if (kind === 'number' && Number.isNaN(result)) kind = 'nan';
  process.stdout.write(JSON.stringify({ kind, value: result }));
});
"#;
    let mut child = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node oracle");
    std::io::Write::write_all(child.stdin.as_mut().expect("stdin"), source.as_bytes())
        .expect("write");
    let output = child.wait_with_output().expect("node oracle");
    assert!(
        output.status.success(),
        "node oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("node json")
}

#[test]
fn declaration_only_literal_if_keeps_selected_length() {
    assert_number(
        &read("if(true){var a=[1];}else{var a=[1,2,3];}var n=a[0];", "n"),
        1.0,
    );
}

#[test]
fn runtime_conditions_select_one_branch() {
    for (expr, taken) in [
        ("1", true),
        ("0", false),
        ("-0", false),
        ("0/0", false),
        ("1/0", true),
        ("true", true),
        ("false", false),
        ("null", false),
        ("undefined", false),
        ("''", false),
        ("'x'", true),
        ("'\\uD800'", true),
        ("({})", true),
        ("(function(){'use strict';return 1;})", true),
    ] {
        let source = format!("var r=0;if({expr}){{r=1;}}else{{r=2;}}");
        assert_number(&read(&source, "r"), if taken { 1.0 } else { 2.0 });
    }
    assert_number(
        &read(
            "function f(){'use strict';return false;}var r=0;if(f()){r=1;}else{r=2;}",
            "r",
        ),
        2.0,
    );
    assert_number(
        &read("var o={flag:true};var r=0;if(o.flag){r=1;}else{r=2;}", "r"),
        1.0,
    );
}

#[test]
fn condition_effects_run_once_and_only_selected_branch_runs() {
    assert_number(
        &read("var n=0;var r=0;if((n=n+1)){r=1;}else{r=2;}", "n"),
        1.0,
    );
    assert_number(
        &read("var n=0;var r=0;if((n=n+1)){r=1;}else{r=2;}", "r"),
        1.0,
    );
    assert_number(&read("var a=0;var b=0;if(1)if(0)a=1;else b=1;", "a"), 0.0);
    assert_number(&read("var a=0;var b=0;if(1)if(0)a=1;else b=1;", "b"), 1.0);
    assert_number(&read("var r=1;if(0){r=2;}", "r"), 1.0);
    assert_number(&read("var x=1;if(1){x=2;var y=x;}else{var y=9;}", "y"), 2.0);
    assert_number(
        &read("var r=0;if(1){if(0){r=1;}else{r=2;}}else{r=3;}", "r"),
        2.0,
    );
    assert_number(&read("if(0){var x=1;}else{var x=2;}", "x"), 2.0);
}

#[test]
fn unselected_var_is_hoisted_and_initializer_does_not_run() {
    let values = execute(
        "if(0){var x=1;}var t=typeof x;",
        false,
        Some(observer::Observer {
            string: Some(("t".into(), 9)),
            ..Default::default()
        }),
    );
    let units: Vec<u16> = "undefined".encode_utf16().collect();
    let tail = &values[values.len() - units.len() - 1..];
    assert_eq!(tail[0], Value::Float(units.len() as f64));
    for (actual, expected) in tail[1..].iter().zip(units) {
        assert_eq!(actual, &Value::Float(f64::from(expected)));
    }
}

#[test]
fn script_completion_follows_update_empty() {
    // present=false is Empty. present=true with an undefined box is explicit Undefined.
    // Node's Script result conflates those two; it still checks the observable value.
    // Node's public Script result is the last statement completion. It does not
    // apply ES5 14 UpdateEmpty across a trailing empty statement, and it
    // conflates Empty with explicit Undefined. Those two discriminators are
    // checked on our raw pair; Node checks the value only where it can see it.
    let cases = [
        ("if(false){1;}", false, None),
        ("if(true){undefined;}", true, Some("undefined")),
        ("1;if(false){2;}", true, None),
        ("1;if(true){;}", true, None),
        ("1;if(true){undefined;}", true, Some("undefined")),
        ("if(true){1;2;}", true, Some("number")),
        ("if(true){1;;}", true, Some("number")),
        ("if(true){var x=1;}", false, None),
        ("1;var x=2;if(false){3;}", true, None),
        ("if(true){}else{4;}", false, None),
        ("if(false){}else{4;}", true, Some("number")),
        ("if(false){5;}else{}", false, None),
    ];
    for (source, present, node_kind) in cases {
        let (actual_present, value) = script_completion(source);
        assert_eq!(actual_present, present, "{source}");
        if let Some(kind) = node_kind {
            let oracle = node_value(source);
            assert_eq!(oracle["kind"], kind, "{source} node={oracle}");
            if present && kind == "number" {
                let expected = oracle["value"].as_f64().expect("node number");
                assert_number(&value, expected);
            }
            if present && kind == "undefined" {
                assert_undefined(&value);
            }
        }
    }
    let (present, value) = script_completion("1;if(false){2;}");
    assert!(present);
    assert_number(&value, 1.0);
    let (present, value) = script_completion("if(true){1;2;}");
    assert!(present);
    assert_number(&value, 2.0);
    let (empty, _) = script_completion("if(false){1;}");
    let (explicit, explicit_value) = script_completion("if(true){undefined;}");
    assert!(!empty && explicit);
    assert_undefined(&explicit_value);
}

#[test]
fn exact_es51_empty_if_vectors_keep_spec_completion() {
    let manifest: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/javascript_es5/p6a-conditional-vectors.json"
    ))
    .expect("oracle manifest");
    let vectors = manifest["vectors"].as_array().expect("vectors");
    assert_eq!(vectors.len(), 8, "exact parent vector count");
    for row in vectors {
        let id = row["id"].as_str().expect("id");
        let source = row["source"].as_str().expect("source");
        let (present, value) = script_completion(source);
        assert!(present, "{id} {source}");
        let expect = &row["es51_boundary_value"];
        match expect["kind"].as_str().expect("kind") {
            "Number" => assert_number(&value, expect["value"].as_f64().expect("number")),
            "Undefined" => assert_undefined(&value),
            other => panic!("{id}: unknown es51 kind {other}"),
        }
        if row["node_relation"] == "agreement" {
            let oracle = node_value(source);
            let node_kind = match expect["kind"].as_str().expect("kind") {
                "Number" => "number",
                "Undefined" => "undefined",
                other => panic!("{id}: unknown es51 kind {other}"),
            };
            assert_eq!(oracle["kind"], node_kind, "{id} node={oracle}");
            if node_kind == "number" {
                assert_eq!(oracle["value"], expect["value"], "{id} node={oracle}");
            }
        }
    }
}

#[test]
fn conditional_boolean_and_closed_function_payloads() {
    assert_bool(
        &read(
            "function f(c){'use strict';if(c){return true;}return false;}var r=f(1);",
            "r",
        ),
        true,
    );
    assert_bool(
        &read(
            "function f(c){'use strict';if(c){return true;}return false;}var r=f(0);",
            "r",
        ),
        false,
    );
    assert_number(
        &read(
            "function f(c){'use strict';if(c){return function(){'use strict';return 4;};}return function(){'use strict';return 5;};}var g=f(1);var r=g();",
            "r",
        ),
        4.0,
    );
    assert_number(
        &read(
            "function f(c){'use strict';if(c){return function(){'use strict';return 4;};}return function(){'use strict';return 5;};}var g=f(0);var r=g();",
            "r",
        ),
        5.0,
    );
}

#[test]
fn conditional_returns_propagate_and_skip_later_effects() {
    assert_number(
        &read(
            "function f(c){'use strict';if(c){return 1;}else{return 2;}}var r=f(1);",
            "r",
        ),
        1.0,
    );
    assert_number(
        &read(
            "function f(c){'use strict';if(c){return 1;}else{return 2;}}var r=f(0);",
            "r",
        ),
        2.0,
    );
    assert_number(
        &read(
            "function f(c){'use strict';if(c){{return 7;}}return 8;}var r=f(1);",
            "r",
        ),
        7.0,
    );
    assert_number(
        &read(
            "function f(c){'use strict';if(c){{return 7;}}return 8;}var r=f('');",
            "r",
        ),
        8.0,
    );
    assert_number(
        &read(
            "function f(c){'use strict';var a=[0];if(c){return 1;a[0]=9;}a[0]=2;return a[0];}var n=f(1);",
            "n",
        ),
        1.0,
    );
    assert_number(
        &read(
            "function f(c){'use strict';var a=[0];if(c){return 1;a[0]=9;}a[0]=2;return a[0];}var n=f(0);",
            "n",
        ),
        2.0,
    );
    assert_number(
        &read(
            "function f(a,b){'use strict';if(a){return b;}return a;}var n=0;var r=f((n=n+1),(n=n+1));",
            "r",
        ),
        2.0,
    );
    assert_number(
        &read(
            "function f(a,b){'use strict';if(a){return b;}return a;}var n=0;var r=f((n=n+1),(n=n+1));",
            "n",
        ),
        2.0,
    );
}

#[test]
fn fallthrough_bare_return_and_explicit_undefined_stay_distinct() {
    let values = execute(
        "function bare(c){'use strict';if(c){return;}}function fall(c){'use strict';if(c){return 1;}}function explicit(c){'use strict';if(c){return undefined;}}var b=bare(1);var f=fall(0);var e=explicit(1);",
        false,
        Some(observer::Observer {
            reads: vec!["b".into(), "f".into(), "e".into()],
            completion: true,
            ..Default::default()
        }),
    );
    // Three calls push (kind, has_value) before the three binding reads.
    let n = values.len();
    assert!(n >= 9, "{values:?}");
    let probe = &values[n - 9..n - 3];
    assert_eq!(
        probe,
        &[
            Value::Int(1),
            Value::Bool(true),
            Value::Int(0),
            Value::Bool(false),
            Value::Int(1),
            Value::Bool(true),
        ],
        "kind/has_value probe: {values:?}"
    );
    assert_undefined(&values[n - 3]);
    assert_undefined(&values[n - 2]);
    assert_undefined(&values[n - 1]);
}

#[test]
fn managed_conditional_payloads_survive_collection() {
    let values = execute(
        "function f(c){'use strict';if(c){return '\\uD800\\0';}return 'other';}var s=f(1);",
        false,
        Some(observer::Observer {
            string: Some(("s".into(), 2)),
            force_gc: true,
            collect: true,
            ..Default::default()
        }),
    );
    assert_eq!(
        &values[values.len() - 3..],
        &[Value::Float(2.0), Value::Float(55296.0), Value::Float(0.0)]
    );
    assert_number(
        &read(
            "function f(c){'use strict';var o={n:6,m:function(){'use strict';return this.n;}};if(c){return o.m();}return 0;}var r=f(1);",
            "r",
        ),
        6.0,
    );
    assert_bool(
        &read(
            "function f(a,c){'use strict';if(c){return a;}return [9];}var original=[4];var r=f(original,1);var same=r===original;",
            "same",
        ),
        true,
    );
    assert_bool(
        &read(
            "function f(a,c){'use strict';if(c){return a;}return [9];}var original=[4];var r=f(original,0);var same=r===original;",
            "same",
        ),
        false,
    );
}

#[test]
fn unselected_branch_cannot_fabricate_array_length() {
    refuse("if(1){var a=[1,2];}else{var a=[3];}a[0];", "exact length");
    assert_number(
        &read(
            "function f(){'use strict';var n=0;if(0){var a=[1,2,3];n=a[0];}else{var a=[9];n=a[0];}return n;}var r=f();",
            "r",
        ),
        9.0,
    );
}

#[test]
fn dead_unsupported_statements_still_refuse() {
    // P6-C: a dead `while` is now a real positively lowering iteration. It is
    // never executed, so the script completes with the prior value.
    assert_number(&read("var r=0;if(0){while(1){}}r;", "r"), 0.0);
    // Dead unsupported bodies are still refused, never silently skipped.
    refuse("if(0){for(var x in y){;}}", "unsupported");
    refuse("if(false){with(x){;}}", "unsupported");
}

#[test]
fn representative_paths_execute_native_ssa() {
    native_worker(|| {
        let script = execute("var r=0;if(1){r=4;}else{r=5;}r;", true, None);
        let script_len = script.len();
        assert!(
            script_len >= 2,
            "native script completion missing: {script:?}"
        );
        assert_eq!(script[script_len - 2], Value::Bool(true), "{script:?}");
        assert_number(&script[script_len - 1], 4.0);
        let returned = execute(
            "function f(c){'use strict';if(c){return 7;}return 8;}var r=f(1);",
            true,
            Some(observer::Observer {
                reads: vec!["r".into()],
                ..Default::default()
            }),
        );
        assert_number(returned.last().expect("native return"), 7.0);
        let values = execute(
            "function f(c){'use strict';var o={n:3};if(c){return o;}return {n:4};}var r=f(1);var n=r.n;",
            true,
            Some(observer::Observer {
                reads: vec!["n".into()],
                force_gc: true,
                collect: true,
                ..Default::default()
            }),
        );
        assert_number(values.last().expect("native payload"), 3.0);
    });
}

fn native_worker(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(f)
        .expect("native worker")
        .join()
        .expect("native assertions");
}

fn conditional_manifest() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "fixtures/javascript_es5/p6a-conditional-vectors.json"
    ))
    .expect("oracle manifest")
}

fn utf16_units(row: &serde_json::Value) -> Vec<u16> {
    row["utf16"]
        .as_array()
        .expect("utf16")
        .iter()
        .map(|unit| u16::try_from(unit.as_u64().expect("unit")).expect("utf16 unit"))
        .collect()
}

fn assert_observed_type(source: &str, units: &[u16], also_bool: Option<&str>, native: bool) {
    let values = execute(
        source,
        native,
        Some(observer::Observer {
            reads: also_bool.iter().map(|name| (*name).to_string()).collect(),
            string: Some(("s".into(), units.len())),
            force_gc: native,
            collect: native,
            ..Default::default()
        }),
    );
    let string_len = units.len() + 1;
    let prefix = usize::from(also_bool.is_some());
    assert!(
        values.len() >= string_len + prefix,
        "type payload missing for {source:?}: {values:?}"
    );
    if also_bool.is_some() {
        assert_bool(&values[values.len() - string_len - 1], true);
    }
    let tail = &values[values.len() - string_len..];
    assert_eq!(
        tail[0],
        Value::Float(units.len() as f64),
        "{source:?}: {values:?}"
    );
    for (actual, expected) in tail[1..].iter().zip(units) {
        assert_eq!(
            actual,
            &Value::Float(f64::from(*expected)),
            "{source:?}: {values:?}"
        );
    }
}

#[test]
fn typeof_cache_branch_isolation_interpreter() {
    let manifest = conditional_manifest();
    let prior = manifest["vectors"].as_array().expect("empty-if vectors");
    assert_eq!(prior.len(), 8);
    assert_eq!(prior[3]["id"], "if-selected-empty");
    assert_eq!(prior[3]["node_relation"], "version-difference");
    assert_eq!(prior[4]["id"], "if-unselected-no-else");
    assert_eq!(prior[4]["node_relation"], "version-difference");
    assert_eq!(prior[5]["id"], "if-selected-empty-else");
    assert_eq!(prior[5]["node_relation"], "version-difference");
    assert_eq!(manifest["normative_sections"][0], "12.1");
    let rows = manifest["typeof_cache_vectors"]
        .as_array()
        .expect("typeof vectors");
    assert_eq!(rows.len(), 18);
    let original_function = "function f(c){'use strict';var x=1;var pre=typeof x;if(c){return typeof x;}else{return typeof x;}}var s=f(0);s";
    let original_grouped = "function arm(c){'use strict';var x=1;if(c){return typeof x;}else{return typeof x;}}function nest(c,d){'use strict';var x=true;if(c){if(d){return typeof x;}else{return typeof x;}}else{return typeof x;}}function dominated(c){'use strict';var x='z';var pre=typeof x;if(c){return typeof x;}else{return typeof x;}}function memberArm(flag){'use strict';var x=1;var o={flag:flag};if(o.flag){return typeof x;}else{return typeof x;}}var c=0;var x=1;if(c){var a=typeof x;}else{var b=typeof x;}var j=typeof x;var r0=arm(0);var r1=arm(1);var n0=nest(1,0);var d0=dominated(0);var m0=memberArm(0);var ok=r0==='number'&&r1==='number'&&n0==='boolean'&&d0==='string'&&b==='number'&&j==='number'&&m0==='number';var s=r0;s";
    assert_eq!(
        rows.iter()
            .find(|row| row["id"] == "dominating-function")
            .expect("original function")["source"],
        original_function
    );
    assert_eq!(
        rows.iter()
            .find(|row| row["id"] == "native-grouped")
            .expect("original grouped")["source"],
        original_grouped
    );
    for row in rows {
        let source = row["source"].as_str().expect("source");
        let units = utf16_units(row);
        let text: String = units
            .iter()
            .map(|unit| char::from_u32(u32::from(*unit)).unwrap())
            .collect();
        assert_eq!(
            text,
            row["es51_type"].as_str().expect("es51 type"),
            "{}",
            row["id"]
        );
        assert_observed_type(source, &units, row["also_bool"].as_str(), false);
    }
}

#[test]
fn typeof_cache_branch_isolation_native_ssa() {
    native_worker(|| {
        let manifest = conditional_manifest();
        let row = manifest["typeof_cache_vectors"]
            .as_array()
            .expect("typeof vectors")
            .iter()
            .find(|row| row["id"] == "native-grouped")
            .expect("grouped source");
        let source = row["source"].as_str().expect("source");
        assert!(
            source.contains("var pre=typeof x"),
            "native grouped source must keep the original dominating typeof"
        );
        assert_observed_type(source, &utf16_units(row), row["also_bool"].as_str(), true);
    });
}

fn compiled_code_len(source: &str) -> usize {
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(reference_array_cli_effective_catalog());
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("compile {source:?}: {error:?}"));
    compiled
        .into_vm()
        .expect("certified compilation should create a source-certified VM")
        .program()
        .code
        .len()
}

fn assert_type_text(source: &str, text: &str) {
    let units: Vec<u16> = text.encode_utf16().collect();
    assert_observed_type(source, &units, None, false);
    let node = node_value(source);
    assert_eq!(node["kind"], "string", "{source}: {node}");
    assert_eq!(node["value"], text, "{source}: {node}");
}

#[test]
fn typeof_sparse_primitive_controls() {
    let manifest = conditional_manifest();
    assert_eq!(manifest["vectors"].as_array().expect("empty-if").len(), 8);
    assert_eq!(
        manifest["typeof_cache_vectors"]
            .as_array()
            .expect("typeof")
            .len(),
        18
    );
    let once = "var n=0;var s=typeof (n=n+1);s";
    let units: Vec<u16> = "number".encode_utf16().collect();
    let values = execute(
        once,
        false,
        Some(observer::Observer {
            reads: vec!["n".into()],
            string: Some(("s".into(), units.len())),
            ..Default::default()
        }),
    );
    let string_len = units.len() + 1;
    assert!(values.len() > string_len, "{values:?}");
    assert_number(&values[values.len() - string_len - 1], 1.0);
    let tail = &values[values.len() - string_len..];
    assert_eq!(tail[0], Value::Float(units.len() as f64));
    for (actual, expected) in tail[1..].iter().zip(&units) {
        assert_eq!(actual, &Value::Float(f64::from(*expected)));
    }
    let node_type = node_value("var n=0;typeof (n=n+1)");
    assert_eq!(node_type["kind"], "string");
    assert_eq!(node_type["value"], "number");
    let node_count = node_value("var n=0;var s=typeof (n=n+1);n");
    assert_eq!(node_count["kind"], "number");
    assert_eq!(node_count["value"], 1);

    assert_type_text("var s=typeof 1;s", "number");
    assert_type_text("var s=typeof true;s", "boolean");
    assert_type_text("var s=typeof 'z';s", "string");
    assert_type_text("var s=typeof null;s", "object");
    assert_type_text("var s=typeof undefined;s", "undefined");

    let merge = "function f(c){'use strict';var x=1;if(c){x=true;}else{x='z';}return typeof x;}";
    assert_type_text(&format!("{merge}var s=f(1);s"), "boolean");
    assert_type_text(&format!("{merge}var s=f(0);s"), "string");

    let generic = "function box(v){'use strict';return typeof v;}function id(v){'use strict';return v;}var o={flag:true};var u;";
    for (suffix, text) in [
        ("var s=box(true);s", "boolean"),
        ("var s=box(1);s", "number"),
        ("var s=box('z');s", "string"),
        ("var s=box(o);s", "object"),
        ("var s=box(box);s", "function"),
        ("var s=box(null);s", "object"),
        ("var s=box(u);s", "undefined"),
        ("var s=typeof o.flag;s", "boolean"),
        ("var s=typeof id(true);s", "boolean"),
        ("var s=typeof id(1);s", "number"),
        ("var s=typeof id('z');s", "string"),
        ("var s=typeof id(o);s", "object"),
        ("var s=typeof id(box);s", "function"),
        ("var s=typeof id(null);s", "object"),
        ("var s=typeof id(u);s", "undefined"),
    ] {
        assert_type_text(&format!("{generic}{suffix}"), text);
    }

    assert_type_text(
        "var known=typeof 1;var x=1;var c=1;if(c){x=true;}else{x='z';}var s=typeof x;s",
        "boolean",
    );
    assert_type_text(
        "var known=typeof 1;var x=1;var c=0;if(c){x=true;}else{x='z';}var s=typeof x;s",
        "string",
    );
    assert_type_text(
        "var known=typeof 1;var o={flag:true};var s=typeof o.flag;s",
        "boolean",
    );
    assert_type_text(
        "var c=0;var x=1;if(c){x=true;var a=typeof 1;}else{x='z';var b=typeof true;}var s=typeof x;s",
        "string",
    );
    assert_type_text(
        "var c=1;var x=1;if(c){x=true;var a=typeof 1;}else{x='z';var b=typeof true;}var s=typeof x;s",
        "boolean",
    );
    let guard = "function f(c){'use strict';var x=1;var pre=typeof x;if(c){return typeof true;}else{return typeof x;}}";
    assert_type_text(&format!("{guard}var s=f(0);s"), "number");
    assert_type_text(&format!("{guard}var s=f(1);s"), "boolean");

    assert_type_text(
        "var a=typeof 1;var b=typeof 1;var c=typeof 1;var s=b;s",
        "number",
    );
    let code_once = compiled_code_len("var s=typeof 1;s");
    let code_twice = compiled_code_len("var a=typeof 1;var b=typeof 1;b");
    eprintln!("P6A_SPARSE code_once={code_once} code_twice={code_twice}");
    assert!(
        code_twice < code_once.saturating_mul(2),
        "repeated typeof multiplied emission: once={code_once} twice={code_twice}"
    );
}

const UTF16_INIT_PREFIX: &str = "__compat_es5_utf16_init_";

fn lowered_ir(source: &str) -> vm::FrontendIr {
    let catalog = reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, &context)
        .unwrap_or_else(|error| panic!("lower {source:?}: {error:?}"))
}

fn function_index(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing helper {name}"))
        .index
}

fn utf16_helpers(ir: &vm::FrontendIr) -> Vec<&vm::FunctionDecl> {
    ir.functions
        .iter()
        .filter(|function| function.name.starts_with(UTF16_INIT_PREFIX))
        .collect()
}

fn walk_expr(
    expr: &vm::Expr,
    visit: &mut dyn FnMut(&vm::Expr),
    visit_stmt: &mut dyn FnMut(&vm::Stmt),
) {
    visit(expr);
    match expr {
        vm::Expr::Call(_, _, args, _, _)
        | vm::Expr::ModuleCall(_, _, args, _)
        | vm::Expr::LocalCall(_, _, args, _) => {
            for arg in args {
                walk_expr(arg, visit, visit_stmt);
            }
        }
        vm::Expr::ClosureCall(closure, args) => {
            walk_expr(&closure.body, visit, visit_stmt);
            for arg in args {
                walk_expr(arg, visit, visit_stmt);
            }
        }
        vm::Expr::Closure(closure) => walk_expr(&closure.body, visit, visit_stmt),
        vm::Expr::Add(left, right)
        | vm::Expr::Sub(left, right)
        | vm::Expr::Mul(left, right)
        | vm::Expr::Div(left, right)
        | vm::Expr::Mod(left, right)
        | vm::Expr::And(left, right)
        | vm::Expr::Or(left, right)
        | vm::Expr::Eq(left, right)
        | vm::Expr::Lt(left, right)
        | vm::Expr::Gt(left, right) => {
            walk_expr(left, visit, visit_stmt);
            walk_expr(right, visit, visit_stmt);
        }
        vm::Expr::Neg(inner)
        | vm::Expr::Not(inner)
        | vm::Expr::ToOwned(inner)
        | vm::Expr::Borrow(inner)
        | vm::Expr::BorrowMut(inner) => walk_expr(inner, visit, visit_stmt),
        vm::Expr::OptionalGet { container, key, .. } => {
            walk_expr(container, visit, visit_stmt);
            walk_expr(key, visit, visit_stmt);
        }
        vm::Expr::OptionUnwrapOr {
            value, fallback, ..
        } => {
            walk_expr(value, visit, visit_stmt);
            walk_expr(fallback, visit, visit_stmt);
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, visit, visit_stmt);
            walk_expr(then_expr, visit, visit_stmt);
            walk_expr(else_expr, visit, visit_stmt);
        }
        vm::Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            walk_expr(value, visit, visit_stmt);
            for (_, arm) in arms {
                walk_expr(arm, visit, visit_stmt);
            }
            walk_expr(default, visit, visit_stmt);
        }
        vm::Expr::Block { stmts, expr } => {
            walk_stmts(stmts, visit, visit_stmt);
            walk_expr(expr, visit, visit_stmt);
        }
        vm::Expr::ResultConstruct { payload, .. }
        | vm::Expr::ResultQuery { value: payload, .. } => {
            walk_expr(payload, visit, visit_stmt);
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            walk_expr(value, visit, visit_stmt);
            walk_expr(ok_expr, visit, visit_stmt);
            walk_expr(err_expr, visit, visit_stmt);
        }
        vm::Expr::ResultTry { value, .. } => walk_expr(value, visit, visit_stmt),
        vm::Expr::Null
        | vm::Expr::Int(_)
        | vm::Expr::Float(_)
        | vm::Expr::Bool(_)
        | vm::Expr::String(_)
        | vm::Expr::Bytes(_)
        | vm::Expr::FunctionRef(_, _)
        | vm::Expr::ModuleFunctionRef(_, _)
        | vm::Expr::UnresolvedFunctionRef { .. }
        | vm::Expr::Var(_)
        | vm::Expr::MoveVar(_)
        | vm::Expr::MoveField { .. }
        | vm::Expr::MoveIndex { .. } => {}
        // Future Core expression variants carry no control-flow edges or
        // call-root structure in these structural observations.
        _ => {}
    }
}

fn walk_stmts(
    stmts: &[vm::Stmt],
    visit: &mut dyn FnMut(&vm::Expr),
    visit_stmt: &mut dyn FnMut(&vm::Stmt),
) {
    for stmt in stmts {
        visit_stmt(stmt);
        match stmt {
            vm::Stmt::Let { expr, .. }
            | vm::Stmt::Assign { expr, .. }
            | vm::Stmt::Expr { expr, .. } => walk_expr(expr, visit, visit_stmt),
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                walk_expr(condition, visit, visit_stmt);
                walk_stmts(then_branch, visit, visit_stmt);
                walk_stmts(else_branch, visit, visit_stmt);
            }
            vm::Stmt::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                walk_stmts(std::slice::from_ref(init.as_ref()), visit, visit_stmt);
                walk_expr(condition, visit, visit_stmt);
                walk_stmts(std::slice::from_ref(post.as_ref()), visit, visit_stmt);
                walk_stmts(body, visit, visit_stmt);
            }
            vm::Stmt::While {
                condition, body, ..
            } => {
                walk_expr(condition, visit, visit_stmt);
                walk_stmts(body, visit, visit_stmt);
            }
            vm::Stmt::ClosureLet { closure, .. } => walk_expr(&closure.body, visit, visit_stmt),
            vm::Stmt::Noop { .. }
            | vm::Stmt::FuncDecl { .. }
            | vm::Stmt::Break { .. }
            | vm::Stmt::Continue { .. }
            | vm::Stmt::Drop { .. } => {}
        }
    }
}

fn int_leaves(expr: &vm::Expr) -> Vec<i64> {
    let mut leaves = Vec::new();
    let mut visit_stmt = |_: &vm::Stmt| {};
    walk_expr(
        expr,
        &mut |inner| {
            if let vm::Expr::Int(value) = inner {
                leaves.push(*value);
            }
        },
        &mut visit_stmt,
    );
    leaves
}

struct WriteScan {
    targets: Vec<u16>,
    payloads: Vec<i64>,
}

fn scan_region(stmts: &[vm::Stmt], extra: Option<&vm::Expr>, write: u16) -> WriteScan {
    let mut scan = WriteScan {
        targets: Vec::new(),
        payloads: Vec::new(),
    };
    let mut visit = |expr: &vm::Expr| {
        if let vm::Expr::Call(target, _, args, _, _) = expr {
            scan.targets.push(*target);
            if *target == write
                && let Some(payload) = args.get(2)
            {
                scan.payloads.extend(int_leaves(payload));
            }
        }
    };
    let mut visit_stmt = |_: &vm::Stmt| {};
    walk_stmts(stmts, &mut visit, &mut visit_stmt);
    if let Some(expr) = extra {
        walk_expr(expr, &mut visit, &mut visit_stmt);
    }
    scan
}

fn caller_scan(ir: &vm::FrontendIr) -> WriteScan {
    let write = function_index(ir, "__compat_ref_write");
    let helpers: std::collections::HashSet<u16> = utf16_helpers(ir)
        .into_iter()
        .map(|function| function.index)
        .collect();
    let mut scan = scan_region(&ir.stmts, None, write);
    for (index, implementation) in &ir.function_impls {
        if helpers.contains(index) {
            continue;
        }
        let nested = scan_region(
            &implementation.body_stmts,
            Some(&implementation.body_expr),
            write,
        );
        scan.targets.extend(nested.targets);
        scan.payloads.extend(nested.payloads);
    }
    scan
}

fn helper_scan(ir: &vm::FrontendIr, index: u16) -> WriteScan {
    let write = function_index(ir, "__compat_ref_write");
    let implementation = ir
        .function_impls
        .get(&index)
        .unwrap_or_else(|| panic!("missing utf16 initializer impl {index}"));
    scan_region(
        &implementation.body_stmts,
        Some(&implementation.body_expr),
        write,
    )
}

fn assert_initializer_shape(ir: &vm::FrontendIr, function: &vm::FunctionDecl) {
    assert_eq!(function.arity, 2, "{}", function.name);
    assert!(!function.exported, "{}", function.name);
    assert_eq!(function.arg_schemas.len(), 2, "{}", function.name);
    assert_eq!(
        function.arg_schemas[1],
        Some(vm::TypeSchema::Int),
        "{}",
        function.name
    );
    assert_eq!(
        function.return_schema,
        Some(vm::TypeSchema::Int),
        "{}",
        function.name
    );
    let implementation = ir
        .function_impls
        .get(&function.index)
        .unwrap_or_else(|| panic!("missing impl {}", function.name));
    assert_eq!(implementation.param_slots.len(), 2, "{}", function.name);
    match &implementation.body_expr {
        vm::Expr::Var(slot) => {
            assert_eq!(*slot, implementation.param_slots[1], "{}", function.name)
        }
        other => panic!("{} must return the raw base, got {other:?}", function.name),
    }
    let begin = function_index(ir, "__compat_es5_node_begin");
    let publish = function_index(ir, "__compat_es5_node_publish");
    let collect = function_index(ir, "__compat_es5_heap_collect");
    let write = function_index(ir, "__compat_ref_write");
    let body = helper_scan(ir, function.index);
    assert!(
        body.targets.iter().all(|target| *target == write),
        "{} may call only ref_write, got {:?}",
        function.name,
        body.targets
    );
    for forbidden in [begin, publish, collect] {
        assert!(
            !body.targets.contains(&forbidden),
            "{} reentered allocation or collection",
            function.name
        );
    }
}

fn root_capacity(source: &str) -> i64 {
    let ir = lowered_ir(source);
    let enter = function_index(&ir, "__compat_es5_root_enter");
    let capacities: Vec<i64> = ir
        .stmts
        .iter()
        .filter_map(|statement| match statement {
            vm::Stmt::Let {
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } if *target == enter => match args.get(1) {
                Some(vm::Expr::Int(capacity)) => Some(*capacity),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(capacities.len(), 1, "{source}");
    capacities[0]
}

fn node_begin_count(source: &str) -> usize {
    let ir = lowered_ir(source);
    let begin = function_index(&ir, "__compat_es5_node_begin");
    caller_scan(&ir)
        .targets
        .into_iter()
        .filter(|target| *target == begin)
        .count()
}

#[test]
fn utf16_outline_ir_red_reuses_identical_units() {
    let ir = lowered_ir("var short=\"xy\"; var a=\"abcd\"; var b=\"abcd\"; a;");
    let helpers = utf16_helpers(&ir);
    assert_eq!(
        helpers.len(),
        1,
        "identical 4-unit sequences share one initializer"
    );
    let helper = helpers[0];
    assert_initializer_shape(&ir, helper);
    let calls = caller_scan(&ir)
        .targets
        .into_iter()
        .filter(|target| *target == helper.index)
        .count();
    assert_eq!(
        calls, 2,
        "each long site still evaluates the shared initializer"
    );
    let body = helper_scan(&ir, helper.index);
    for unit in [
        i64::from(b'a'),
        i64::from(b'b'),
        i64::from(b'c'),
        i64::from(b'd'),
        4,
    ] {
        assert!(
            body.payloads.contains(&unit),
            "helper dropped unit {unit}: {:?}",
            body.payloads
        );
    }
    let caller = caller_scan(&ir);
    for unit in [
        i64::from(b'a'),
        i64::from(b'b'),
        i64::from(b'c'),
        i64::from(b'd'),
    ] {
        assert!(
            !caller.payloads.contains(&unit),
            "outlined unit {unit} stayed inline"
        );
    }
    for unit in [i64::from(b'x'), i64::from(b'y'), 2] {
        assert!(
            caller.payloads.contains(&unit),
            "short fallback lost inline unit {unit}: {:?}",
            caller.payloads
        );
    }
    let begin = function_index(&ir, "__compat_es5_node_begin");
    let publish = function_index(&ir, "__compat_es5_node_publish");
    assert_eq!(
        caller
            .targets
            .iter()
            .filter(|target| **target == begin)
            .count(),
        3,
        "begin_node stays at each evaluation site"
    );
    assert_eq!(
        caller
            .targets
            .iter()
            .filter(|target| **target == publish)
            .count(),
        3,
        "publish_node stays at each evaluation site"
    );
}

#[test]
fn utf16_outline_ir_red_distinct_surrogates() {
    let ir = lowered_ir(
        "var high=\"ab\\uD800c\"; var low=\"ab\\uDC00c\"; var repl=\"ab\\uFFFDc\"; high;",
    );
    let helpers = utf16_helpers(&ir);
    assert_eq!(
        helpers.len(),
        3,
        "lone surrogates and U+FFFD stay distinct keys"
    );
    let mut seen = Vec::new();
    for helper in &helpers {
        assert_initializer_shape(&ir, helper);
        let payloads = helper_scan(&ir, helper.index).payloads;
        let marker = [0xD800_i64, 0xDC00, 0xFFFD]
            .into_iter()
            .find(|unit| payloads.contains(unit))
            .unwrap_or_else(|| panic!("{} lost its distinctive unit: {payloads:?}", helper.name));
        assert!(
            !seen.contains(&marker),
            "sequence {marker:#x} collapsed into another helper"
        );
        seen.push(marker);
    }
    assert_eq!(seen.len(), 3);
}

#[test]
fn utf16_outline_ir_red_pool_cap_inline_fallback() {
    let mut source = String::new();
    for ordinal in 0..65u16 {
        let unit = 0x40 + ordinal;
        source.push_str(&format!("var s{ordinal}=\"abc\\u{unit:04X}\";"));
    }
    source.push_str("s64;");
    let ir = lowered_ir(&source);
    let helpers = utf16_helpers(&ir);
    assert_eq!(helpers.len(), 64, "pool cap is 64 distinct initializers");
    let caller = caller_scan(&ir);
    let first = 0x40_i64;
    let overflow = 0x40_i64 + 64;
    let helper_payloads: Vec<i64> = helpers
        .iter()
        .flat_map(|helper| helper_scan(&ir, helper.index).payloads)
        .collect();
    assert!(
        helper_payloads.contains(&first),
        "first eligible sequence was not outlined"
    );
    assert!(
        !caller.payloads.contains(&first),
        "pooled sequence stayed inline"
    );
    assert!(
        caller.payloads.contains(&overflow),
        "65th sequence must keep the original inline initializer"
    );
    assert!(
        !helper_payloads.contains(&overflow),
        "pool-full sequence must not gain a 65th helper"
    );
}

#[test]
fn utf16_outline_ir_red_cross_activation_share() {
    let ir =
        lowered_ir("function f(){'use strict'; return \"wxyz\";} var s=\"wxyz\"; var r=f(); r;");
    let helpers = utf16_helpers(&ir);
    assert_eq!(
        helpers.len(),
        1,
        "the pool is compiler-global, not per activation"
    );
    let index = helpers[0].index;
    let script = scan_region(&ir.stmts, None, function_index(&ir, "__compat_ref_write"));
    assert!(
        script.targets.contains(&index),
        "script site did not call the shared initializer"
    );
    let mut function_calls = 0usize;
    for (function_index, implementation) in &ir.function_impls {
        if *function_index == index {
            continue;
        }
        let nested = scan_region(
            &implementation.body_stmts,
            Some(&implementation.body_expr),
            0,
        );
        function_calls += nested
            .targets
            .iter()
            .filter(|target| **target == index)
            .count();
    }
    assert_eq!(
        function_calls, 1,
        "nested source function must reuse the same index"
    );
}

#[test]
fn utf16_outline_ctrl_short_stays_inline() {
    let ir = lowered_ir("var empty=\"\"; var one=\"a\"; var two=\"ab\"; var three=\"abc\"; empty;");
    assert!(
        utf16_helpers(&ir).is_empty(),
        "sequences shorter than 4 units stay inline"
    );
    let caller = caller_scan(&ir);
    for unit in [
        0,
        1,
        2,
        3,
        i64::from(b'a'),
        i64::from(b'b'),
        i64::from(b'c'),
    ] {
        assert!(
            caller.payloads.contains(&unit),
            "inline short initializer lost payload {unit}: {:?}",
            caller.payloads
        );
    }
}

#[test]
fn utf16_outline_ctrl_begin_count_matches_sites() {
    let short = node_begin_count("var a=\"ab\"; var b=\"cd\"; a;");
    let long = node_begin_count("var a=\"abcd\"; var b=\"efgh\"; a;");
    assert_eq!(short, 2, "two short literals still begin two nodes");
    assert_eq!(long, short, "outlining must not remove per-site begin_node");
}

#[test]
fn utf16_outline_ctrl_root_capacity_ignores_outline() {
    let short = root_capacity("var a=\"ab\"; var b=\"ab\"; a;");
    let long = root_capacity("var a=\"abcd\"; var b=\"abcd\"; a;");
    assert_eq!(
        short, long,
        "initializer reuse must not add a guest root entry"
    );
}

#[test]
fn utf16_outline_ctrl_alias_surrogate_gc() {
    let units: [u16; 4] = [u16::from(b'a'), u16::from(b'b'), 0xD800, u16::from(b'c')];
    let values = execute(
        "var a=\"ab\\uD800c\"; var b=a; var same=a===b; var s=a;",
        false,
        Some(observer::Observer {
            reads: vec!["same".into()],
            string: Some(("s".into(), units.len())),
            force_gc: true,
            collect: true,
            stats: true,
            ..Default::default()
        }),
    );
    let stats = 3;
    let string_len = units.len() + 1;
    assert!(values.len() > stats + string_len, "{values:?}");
    let same_at = values.len() - stats - string_len - 1;
    assert_bool(&values[same_at], true);
    let tail = &values[same_at + 1..same_at + 1 + string_len];
    assert_eq!(tail[0], Value::Float(units.len() as f64));
    for (actual, expected) in tail[1..].iter().zip(&units) {
        assert_eq!(actual, &Value::Float(f64::from(*expected)));
    }
    let live = &values[values.len() - stats];
    let occupied = &values[values.len() - stats + 1];
    let collections = &values[values.len() - stats + 2];
    assert!(matches!(live, Value::Int(n) if *n >= 1), "{values:?}");
    assert!(matches!(occupied, Value::Int(n) if *n >= 1), "{values:?}");
    assert!(
        matches!(collections, Value::Int(n) if *n >= 1),
        "{values:?}"
    );
}

#[test]
fn utf16_outline_ir_red_string_key_shares_literal() {
    let ir = lowered_ir("var o={\"abcd\":7}; var n=o[\"abcd\"]; var s=\"abcd\"; s;");
    assert_eq!(
        utf16_helpers(&ir).len(),
        1,
        "object key and literal share one Vec<u16> key"
    );
}

#[test]
fn utf16_outline_ctrl_string_key() {
    assert_number(&read("var o={\"abcd\":7}; var n=o[\"abcd\"]; n;", "n"), 7.0);
}

#[test]
fn utf16_outline_ctrl_untaken_side_effect() {
    let source = "function f(c){'use strict'; var n=0; if(c){ n=1; return \"abcd\"; } return n;}";
    assert_number(&read(&format!("{source} var r=f(0);"), "r"), 0.0);
    assert_type_text(&format!("{source} var s=f(1);s"), "abcd");
    let node = node_value(&format!("{source} f(0)"));
    assert_eq!(node["kind"], "number");
    assert_eq!(node["value"], 0);
}

#[test]
fn utf16_outline_ctrl_returned_guard() {
    let guard = "function f(c){'use strict'; var x=1; var pre=typeof x; if(c){return typeof true;} else {return typeof x;}}";
    assert_type_text(&format!("{guard} var s=f(0);s"), "number");
    assert_type_text(&format!("{guard} var s=f(1);s"), "boolean");
    let returned = "function f(c){'use strict'; if(c){return \"abcd\";} return \"wxyz\";}";
    assert_type_text(&format!("{returned} var s=f(0);s"), "wxyz");
    assert_type_text(&format!("{returned} var s=f(1);s"), "abcd");
}

fn peel_set<'a>(
    expr: &'a vm::Expr,
    set: u16,
    key: &str,
    value_ok: impl Fn(&vm::Expr) -> bool,
) -> Option<&'a vm::Expr> {
    let vm::Expr::Call(index, type_args, args, None, None) = expr else {
        return None;
    };
    if *index != set || !type_args.is_empty() || args.len() != 3 {
        return None;
    }
    let vm::Expr::String(actual) = &args[1] else {
        return None;
    };
    if actual != key || !value_ok(&args[2]) {
        return None;
    }
    Some(&args[0])
}

fn is_canonical_undefined(expr: &vm::Expr) -> bool {
    let map_new = vm::BuiltinFunction::MapNew.call_index();
    let set = vm::BuiltinFunction::Set.call_index();
    let Some(inner) = peel_set(expr, set, "generation", |value| {
        matches!(value, vm::Expr::Int(0))
    }) else {
        return false;
    };
    let Some(inner) = peel_set(inner, set, "ref_slot", |value| {
        matches!(value, vm::Expr::Int(0))
    }) else {
        return false;
    };
    let Some(inner) = peel_set(inner, set, "flag", |value| {
        matches!(value, vm::Expr::Bool(false))
    }) else {
        return false;
    };
    let Some(inner) = peel_set(
        inner,
        set,
        "number",
        |value| matches!(value, vm::Expr::Float(number) if number.to_bits() == 0),
    ) else {
        return false;
    };
    let Some(inner) = peel_set(inner, set, "tag", |value| matches!(value, vm::Expr::Int(0))) else {
        return false;
    };
    matches!(
        inner,
        vm::Expr::Call(index, type_args, args, None, None)
            if *index == map_new && type_args.is_empty() && args.is_empty()
    )
}

fn scan_ir(ir: &vm::FrontendIr, skip_impl: Option<u16>, visit: &mut dyn FnMut(&vm::Expr)) {
    let mut visit_stmt = |_: &vm::Stmt| {};
    walk_stmts(&ir.stmts, visit, &mut visit_stmt);
    for (index, body) in &ir.function_impls {
        if skip_impl == Some(*index) {
            continue;
        }
        walk_stmts(&body.body_stmts, visit, &mut visit_stmt);
        walk_expr(&body.body_expr, visit, &mut visit_stmt);
    }
}

fn count_canonical(ir: &vm::FrontendIr, skip_impl: Option<u16>) -> usize {
    let mut count = 0;
    scan_ir(ir, skip_impl, &mut |expr| {
        if is_canonical_undefined(expr) {
            count += 1;
        }
    });
    count
}

fn count_direct_calls(ir: &vm::FrontendIr, target: u16, skip_impl: Option<u16>) -> usize {
    let mut count = 0;
    scan_ir(ir, skip_impl, &mut |expr| {
        if matches!(
            expr,
            vm::Expr::Call(index, type_args, args, None, None)
                if *index == target && type_args.is_empty() && args.is_empty()
        ) {
            count += 1;
        }
    });
    count
}

fn count_tag_sets(ir: &vm::FrontendIr, tag: i64) -> usize {
    let set = vm::BuiltinFunction::Set.call_index();
    let mut count = 0;
    scan_ir(ir, None, &mut |expr| {
        if peel_set(
            expr,
            set,
            "tag",
            |value| matches!(value, vm::Expr::Int(actual) if *actual == tag),
        )
        .is_some()
        {
            count += 1;
        }
    });
    count
}

fn undefined_factories(ir: &vm::FrontendIr) -> Vec<&vm::FunctionDecl> {
    ir.functions
        .iter()
        .filter(|function| {
            function.arity == 0
                && function.args.is_empty()
                && !function.exported
                && function.symbol.is_none()
                && ir.function_impls.get(&function.index).is_some_and(|body| {
                    body.param_slots.is_empty()
                        && body.capture_copies.is_empty()
                        && body.body_stmts.is_empty()
                        && is_canonical_undefined(&body.body_expr)
                })
        })
        .collect()
}

#[test]
fn undefined_box_ir_red_shares_canonical_sites() {
    let source = "\"use strict\"; var missing; function omitted(x){return x;} function bare(){return;} function selected(flag){if(flag){return;} return 1;} missing;";
    let ir = lowered_ir(source);
    let factories = undefined_factories(&ir);
    assert_eq!(
        factories.len(),
        1,
        "structural RED: canonical undefined constructions must share exactly one zero-arg no-capture no-local helper"
    );
    let factory = factories[0];
    let leftover = count_canonical(&ir, Some(factory.index));
    let replacements = count_direct_calls(&ir, factory.index, Some(factory.index));
    eprintln!(
        "P6A_UNDEFINED_BOX structural leftover={leftover} replacements={replacements} factories={}",
        factories.len()
    );
    assert_eq!(
        leftover, 0,
        "matched sites must be calls, not leftover inline records"
    );
    assert!(
        replacements >= 2,
        "complete input must replace multiple real sites, found {replacements}"
    );
    let max_other = ir
        .functions
        .iter()
        .filter(|function| function.index != factory.index)
        .map(|function| function.index)
        .max()
        .expect("existing function set");
    assert!(
        factory.index > max_other,
        "helper index {} must be allocated after the complete function set {max_other}",
        factory.index
    );
    assert!(
        factory.index < vm::BuiltinFunction::Len.call_index(),
        "helper index must stay below the reserved builtin floor"
    );
    let body = ir.function_impls.get(&factory.index).expect("factory body");
    let mut map_new = 0;
    let mut visit_stmt = |_: &vm::Stmt| {};
    walk_expr(
        &body.body_expr,
        &mut |expr| {
            if matches!(
                expr,
                vm::Expr::Call(index, type_args, args, None, None)
                    if *index == vm::BuiltinFunction::MapNew.call_index()
                        && type_args.is_empty()
                        && args.is_empty()
            ) {
                map_new += 1;
            }
        },
        &mut visit_stmt,
    );
    assert_eq!(map_new, 1, "factory body must keep one fresh MapNew chain");
    assert_eq!(
        count_canonical(&ir, Some(factory.index)),
        0,
        "matched sites must be calls, not leftover inline records"
    );
    let replacements = count_direct_calls(&ir, factory.index, Some(factory.index));
    eprintln!(
        "P6A_UNDEFINED_BOX structural replacements={replacements} factory_index={} factory_mapnew={map_new}",
        factory.index
    );
    assert!(
        replacements >= 2,
        "complete input must replace multiple real sites, found {replacements}"
    );
    let mut completion_value_calls = 0;
    let set = vm::BuiltinFunction::Set.call_index();
    scan_ir(&ir, Some(factory.index), &mut |expr| {
        if peel_set(expr, set, "value", |value| {
            matches!(
                value,
                vm::Expr::Call(index, type_args, args, None, None)
                    if *index == factory.index && type_args.is_empty() && args.is_empty()
            )
        })
        .is_some()
            && peel_set(expr, set, "has_value", |_| true).is_none()
        {
            // The value field itself is the call; the outer completion still
            // has has_value one level out. Count value-calls whose parent
            // chain is recovered below.
            completion_value_calls += 1;
        }
    });
    let mut completion_preserved = 0;
    scan_ir(&ir, Some(factory.index), &mut |expr| {
        let Some(value_receiver) = peel_set(expr, set, "has_value", |_| true) else {
            return;
        };
        if peel_set(value_receiver, set, "value", |value| {
            matches!(
                value,
                vm::Expr::Call(index, type_args, args, None, None)
                    if *index == factory.index && type_args.is_empty() && args.is_empty()
            )
        })
        .is_some()
        {
            completion_preserved += 1;
        }
    });
    eprintln!(
        "P6A_UNDEFINED_BOX structural completion_value_calls={completion_value_calls} completion_preserved={completion_preserved}"
    );
    assert!(
        completion_preserved >= 1,
        "a canonical undefined nested in a Completion must be replaced without rewriting the outer completion"
    );
}

#[test]
fn undefined_box_ir_other_tags_stay_inline() {
    let ir = lowered_ir("var n=0; var t=true; var f=false; var z=null; var s=\"xy\"; n;");
    for (tag, label) in [(1, "null"), (2, "boolean"), (3, "number")] {
        let found = count_tag_sets(&ir, tag);
        assert!(
            found >= 1,
            "{label} tag {tag} must stay an inline record, found {found}"
        );
    }
    let factories = undefined_factories(&ir);
    assert!(
        factories.len() <= 1,
        "other-tag input must not grow a second undefined factory"
    );
}

#[test]
fn undefined_box_sem_missing_var() {
    assert_undefined(&read("var missing; missing;", "missing"));
    assert_undefined(&read("var a; var b; a=1; b;", "b"));
    assert_number(&read("var a; var b; a=1; b;", "a"), 1.0);
}

#[test]
fn undefined_box_sem_omitted_argument() {
    let source = "\"use strict\"; function f(x){return x;} var missing=f(); var present=f(1);";
    assert_undefined(&read(&format!("{source} missing;"), "missing"));
    assert_number(&read(&format!("{source} present;"), "present"), 1.0);
}

#[test]
fn undefined_box_sem_returned_and_siblings() {
    let source = "\"use strict\"; function bare(){return;} function selected(flag){if(flag){return;} return 1;} var u=bare(); var taken=selected(1); var untaken=selected(0);";
    assert_undefined(&read(&format!("{source} u;"), "u"));
    assert_undefined(&read(&format!("{source} taken;"), "taken"));
    assert_number(&read(&format!("{source} untaken;"), "untaken"), 1.0);
}

#[test]
fn undefined_box_sem_payloads_and_reference() {
    assert_bool(&read("var t=true; t;", "t"), true);
    assert_bool(&read("var f=false; f;", "f"), false);
    assert_number(&read("var z=0; z;", "z"), 0.0);
    assert_number(&read("var n=1; n;", "n"), 1.0);
    assert_number(&read("var o={\"k\":1}; var n=o[\"k\"]; n;", "n"), 1.0);
}

#[test]
fn undefined_box_sem_gc_cleanup() {
    let values = execute(
        "var missing; var s=\"abcd\"; missing;",
        false,
        Some(observer::Observer {
            reads: vec!["missing".into()],
            force_gc: true,
            collect: true,
            stats: true,
            ..Default::default()
        }),
    );
    let stats = 3;
    assert!(values.len() > stats, "{values:?}");
    assert_undefined(&values[values.len() - stats - 1]);
    let collections = &values[values.len() - 1];
    assert!(
        matches!(collections, Value::Int(n) if *n >= 1),
        "forced collection did not run: {values:?}"
    );
}

// ---------------------------------------------------------------------------
// Item 151: bounded witness for call-root retirement in lower_call_expr.
//
// Production call lowering prepares and roots the callee, the receiver and
// every evaluated actual, but the current code releases those owned roots
// only inside the dispatch ResultMatch arms. Key/Get/argument failure skips
// dispatch, so the prepared roots are never retired on that path. These
// helpers bind the actual IR identities of one member call (receiver and
// callee slots, the dispatch vector and its first actual, the result slot)
// and assert that consumed roots are released by direct statements after the
// dispatch gate while the result root survives. Nothing here observes the
// guest VM: the source tests only parse and lower.
// ---------------------------------------------------------------------------

const CRW_CARRIER: u16 = 1;
const CRW_FRAME: u16 = 10;
const CRW_RECEIVER: u16 = 31;
const CRW_CALLEE: u16 = 32;
const CRW_VECTOR: u16 = 33;
const CRW_RESULT: u16 = 34;

/// Fixture-only flat helper indices. The source tests resolve the frozen
/// production helper names through function_index instead.
const CRW_DISPATCH: u16 = 41;
const CRW_ROOT_WRITE: u16 = 42;
const CRW_MAP_NEW: u16 = 43;
const CRW_SET: u16 = 44;
const CRW_PUSH: u16 = 45;
/// Fixture-only stand-in for the factored logical undefined helper. It is a
/// phase-control identity, deliberately unrelated to any production index.
const CRW_UNDEFINED: u16 = 46;
/// Fixture-only zero-argument helper that is not the undefined declaration.
const CRW_OTHER_HELPER: u16 = 47;

/// One root-table write emitted by write_root_value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootWriteValue {
    /// `root_write(..., slot)` keeps or refreshes the boxed edge.
    Slot(u16),
    /// A verified logical clear: either the exact `value::undefined()` record
    /// or an exact empty call to the declared undefined helper. The edge is
    /// logically dropped.
    Undefined,
    /// Neither shape. Extraction fails instead of guessing a clear.
    Unknown,
}

#[derive(Clone, Copy, Debug)]
struct RootWriteEvent {
    frame: u16,
    entry: i64,
    value: RootWriteValue,
}

#[derive(Clone, Copy, Debug)]
struct DispatchSite {
    callee: u16,
    receiver: u16,
    vector: u16,
    /// Root writes already collected in this flow when the dispatch node was
    /// visited. Later writes in the same statement belong to the dispatch
    /// arms and cannot root this call.
    writes_before_dispatch: usize,
}

#[derive(Clone, Copy, Debug)]
struct CallRootSpec {
    dispatch: u16,
    root_write: u16,
    /// Declared flat index of the logical `__compat_es5_undefined` helper.
    /// Post-factoring clears are exact empty calls to that declaration, so the
    /// identity comes from the IR's own `FunctionDecl`, never from a guess.
    undefined: Option<u16>,
    require_first_actual: bool,
}

#[derive(Clone, Debug, Default)]
struct CallRootFlow {
    writes: Vec<RootWriteEvent>,
    dispatches: Vec<DispatchSite>,
    /// (vector slot, actual slot) of one argument push.
    pushes: Vec<(u16, u16)>,
    malformed: usize,
}

#[derive(Clone, Debug)]
struct CallRootWitness {
    receiver_slot: u16,
    callee_slot: u16,
    receiver_entry: i64,
    callee_entry: i64,
    first_actual_slot: Option<u16>,
    result_slot: u16,
    result_entry: i64,
    /// (root entry, slot) pairs that the call consumed and must retire.
    required: Vec<(i64, u16)>,
    /// Entries cleared by direct statements after the dispatch gate.
    common_clears: Vec<i64>,
    /// Entries cleared inside the gate or the dispatch arms.
    gated_clears: Vec<i64>,
    dispatch_count: usize,
}

fn crw_call(target: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(target, Vec::new(), args, None, None)
}

fn crw_var(slot: u16) -> vm::Expr {
    vm::Expr::Var(slot)
}

fn crw_undefined() -> vm::Expr {
    let mut record = crw_call(CRW_MAP_NEW, Vec::new());
    for (key, field) in [
        ("tag", vm::Expr::Int(0)),
        ("number", vm::Expr::Float(0.0)),
        ("flag", vm::Expr::Bool(false)),
        ("ref_slot", vm::Expr::Int(0)),
        ("generation", vm::Expr::Int(0)),
    ] {
        record = crw_call(
            CRW_SET,
            vec![record, vm::Expr::String(key.to_owned()), field],
        );
    }
    record
}

fn crw_root_write_value(frame: u16, entry: u16, value: vm::Expr) -> vm::Expr {
    crw_call(
        CRW_ROOT_WRITE,
        vec![
            crw_var(CRW_CARRIER),
            crw_var(frame),
            vm::Expr::Int(i64::from(entry)),
            value,
        ],
    )
}

fn crw_root_write_stmt(ack: u16, frame: u16, entry: u16, value: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: crw_root_write_value(frame, entry, value),
        line: 0,
    }
}

fn crw_clear_stmt(ack: u16, frame: u16, entry: u16) -> vm::Stmt {
    crw_root_write_stmt(ack, frame, entry, crw_undefined())
}

/// Post-factoring clear: one exact zero-argument call to the logical helper.
fn crw_helper_clear_stmt(ack: u16, frame: u16, entry: u16) -> vm::Stmt {
    crw_root_write_stmt(ack, frame, entry, crw_call(CRW_UNDEFINED, Vec::new()))
}

fn crw_dispatch_stmt(slot: u16, callee: u16, receiver: u16, vector: u16) -> vm::Stmt {
    vm::Stmt::Let {
        index: slot,
        declared_schema: None,
        expr: crw_call(
            CRW_DISPATCH,
            vec![
                crw_var(CRW_CARRIER),
                crw_var(callee),
                crw_var(receiver),
                crw_var(vector),
            ],
        ),
        line: 0,
    }
}

fn crw_gate(then_branch: Vec<vm::Stmt>) -> vm::Stmt {
    vm::Stmt::IfElse {
        condition: crw_var(9),
        then_branch,
        else_branch: Vec::new(),
        line: 0,
    }
}

fn crw_candidate_block(
    pre_dispatch: Vec<vm::Stmt>,
    gated_release: Vec<vm::Stmt>,
    after_gate: Vec<vm::Stmt>,
) -> (Vec<vm::Stmt>, vm::Expr) {
    let mut gate = pre_dispatch;
    gate.push(crw_dispatch_stmt(35, CRW_CALLEE, CRW_RECEIVER, CRW_VECTOR));
    gate.extend(gated_release);
    gate.push(crw_root_write_stmt(52, CRW_FRAME, 3, crw_var(CRW_RESULT)));
    let mut stmts = vec![
        crw_root_write_stmt(50, CRW_FRAME, 0, crw_var(CRW_RECEIVER)),
        crw_root_write_stmt(51, CRW_FRAME, 1, crw_var(CRW_CALLEE)),
        crw_gate(gate),
    ];
    stmts.extend(after_gate);
    (stmts, crw_var(CRW_RESULT))
}

fn crw_expr_block(block: (Vec<vm::Stmt>, vm::Expr)) -> vm::Stmt {
    let (stmts, tail) = block;
    vm::Stmt::Expr {
        expr: vm::Expr::Block {
            stmts,
            expr: Box::new(tail),
        },
        line: 0,
    }
}

fn crw_push_block_stmt(vector: u16, nested: vm::Expr) -> vm::Stmt {
    vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: vector,
        expr: crw_call(CRW_PUSH, vec![crw_var(vector), nested]),
        line: 0,
    }
}

/// Exact `value::undefined()` shape: a five-field record whose only numeric
/// payloads are zero. Anything else is not a logical clear.
fn undefined_record(expr: &vm::Expr) -> bool {
    let mut fields = Vec::new();
    let mut current = expr;
    loop {
        match current {
            vm::Expr::Call(_, _, args, _, _) if args.len() == 3 => {
                let (vm::Expr::String(key), value) = (&args[1], &args[2]) else {
                    return false;
                };
                fields.push((key.as_str(), value));
                current = &args[0];
            }
            vm::Expr::Call(_, _, args, _, _) if args.is_empty() => break,
            _ => return false,
        }
    }
    if fields.len() != 5 {
        return false;
    }
    let mut tag = false;
    let mut number = false;
    let mut flag = false;
    let mut ref_slot = false;
    let mut generation = false;
    for (key, value) in fields {
        match (key, value) {
            ("tag", vm::Expr::Int(0)) if !tag => tag = true,
            ("number", vm::Expr::Float(amount)) if !number && *amount == 0.0 => number = true,
            ("flag", vm::Expr::Bool(false)) if !flag => flag = true,
            ("ref_slot", vm::Expr::Int(0)) if !ref_slot => ref_slot = true,
            ("generation", vm::Expr::Int(0)) if !generation => generation = true,
            _ => return false,
        }
    }
    tag && number && flag && ref_slot && generation
}

/// Exact call to the verified logical undefined helper: no type arguments, no
/// runtime arguments and no per-call annotation. Anything else — including an
/// arbitrary zero-argument call or the same helper called with arguments —
/// stays unknown instead of being read as a clear.
fn undefined_helper_call(spec: &CallRootSpec, expr: &vm::Expr) -> bool {
    let Some(undefined) = spec.undefined else {
        return false;
    };
    if let vm::Expr::Call(target, type_args, args, resolved, node) = expr {
        return *target == undefined
            && type_args.is_empty()
            && args.is_empty()
            && resolved.is_none()
            && node.is_none();
    }
    false
}

fn root_write_event(spec: &CallRootSpec, args: &[vm::Expr]) -> Option<RootWriteEvent> {
    let [
        vm::Expr::Var(_),
        vm::Expr::Var(frame),
        vm::Expr::Int(entry),
        value,
    ] = args
    else {
        return None;
    };
    let value = match value {
        vm::Expr::Var(slot) => RootWriteValue::Slot(*slot),
        other if undefined_record(other) => RootWriteValue::Undefined,
        other if undefined_helper_call(spec, other) => RootWriteValue::Undefined,
        _ => RootWriteValue::Unknown,
    };
    Some(RootWriteEvent {
        frame: *frame,
        entry: *entry,
        value,
    })
}

fn dispatch_site(args: &[vm::Expr]) -> Option<DispatchSite> {
    let [
        vm::Expr::Var(_),
        vm::Expr::Var(callee),
        vm::Expr::Var(receiver),
        vm::Expr::Var(vector),
    ] = args
    else {
        return None;
    };
    Some(DispatchSite {
        callee: *callee,
        receiver: *receiver,
        vector: *vector,
        writes_before_dispatch: 0,
    })
}

/// Bounded local flow: control flow and ResultMatch arms are followed, but a
/// nested Expr::Block is a separate evaluation region with its own roots and
/// its own call identity, so descent stops there.
fn scan_flow_expr(spec: &CallRootSpec, expr: &vm::Expr, flow: &mut CallRootFlow) {
    match expr {
        vm::Expr::Call(target, _, args, _, _) if *target == spec.dispatch => {
            match dispatch_site(args) {
                Some(mut site) => {
                    site.writes_before_dispatch = flow.writes.len();
                    flow.dispatches.push(site);
                }
                None => flow.malformed += 1,
            }
        }
        vm::Expr::Call(target, _, args, _, _) if *target == spec.root_write => {
            match root_write_event(spec, args) {
                Some(event) => flow.writes.push(event),
                None => flow.malformed += 1,
            }
        }
        vm::Expr::Call(_, _, args, _, _)
        | vm::Expr::ModuleCall(_, _, args, _)
        | vm::Expr::LocalCall(_, _, args, _) => {
            for arg in args {
                scan_flow_expr(spec, arg, flow);
            }
        }
        vm::Expr::ClosureCall(_, args) => {
            for arg in args {
                scan_flow_expr(spec, arg, flow);
            }
        }
        vm::Expr::Block { .. } => {}
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            scan_flow_expr(spec, value, flow);
            scan_result_arm(spec, ok_expr, flow);
            scan_result_arm(spec, err_expr, flow);
        }
        vm::Expr::ResultConstruct { payload, .. }
        | vm::Expr::ResultQuery { value: payload, .. } => scan_flow_expr(spec, payload, flow),
        vm::Expr::ResultTry { value, .. } => scan_flow_expr(spec, value, flow),
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            scan_flow_expr(spec, condition, flow);
            scan_flow_expr(spec, then_expr, flow);
            scan_flow_expr(spec, else_expr, flow);
        }
        vm::Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            scan_flow_expr(spec, value, flow);
            for (_, arm) in arms {
                scan_flow_expr(spec, arm, flow);
            }
            scan_flow_expr(spec, default, flow);
        }
        vm::Expr::Add(left, right)
        | vm::Expr::Sub(left, right)
        | vm::Expr::Mul(left, right)
        | vm::Expr::Div(left, right)
        | vm::Expr::Mod(left, right)
        | vm::Expr::And(left, right)
        | vm::Expr::Or(left, right)
        | vm::Expr::Eq(left, right)
        | vm::Expr::Lt(left, right)
        | vm::Expr::Gt(left, right) => {
            scan_flow_expr(spec, left, flow);
            scan_flow_expr(spec, right, flow);
        }
        vm::Expr::Neg(inner)
        | vm::Expr::Not(inner)
        | vm::Expr::ToOwned(inner)
        | vm::Expr::Borrow(inner)
        | vm::Expr::BorrowMut(inner) => scan_flow_expr(spec, inner, flow),
        vm::Expr::OptionalGet { container, key, .. } => {
            scan_flow_expr(spec, container, flow);
            scan_flow_expr(spec, key, flow);
        }
        vm::Expr::OptionUnwrapOr {
            value, fallback, ..
        } => {
            scan_flow_expr(spec, value, flow);
            scan_flow_expr(spec, fallback, flow);
        }
        vm::Expr::Closure(_)
        | vm::Expr::Null
        | vm::Expr::Int(_)
        | vm::Expr::Float(_)
        | vm::Expr::Bool(_)
        | vm::Expr::String(_)
        | vm::Expr::Bytes(_)
        | vm::Expr::FunctionRef(_, _)
        | vm::Expr::ModuleFunctionRef(_, _)
        | vm::Expr::UnresolvedFunctionRef { .. }
        | vm::Expr::Var(_)
        | vm::Expr::MoveVar(_)
        | vm::Expr::MoveField { .. }
        | vm::Expr::MoveIndex { .. } => {}
        // Future Core expression variants carry no control-flow edges or
        // call-root structure in these structural observations.
        _ => {}
    }
}

/// ResultMatch arms are inspected through their own statement list rather
/// than through arbitrary block descent.
fn scan_result_arm(spec: &CallRootSpec, arm: &vm::Expr, flow: &mut CallRootFlow) {
    match arm {
        vm::Expr::Block { stmts, expr } => {
            for stmt in stmts {
                scan_flow_stmt(spec, stmt, flow);
            }
            scan_flow_expr(spec, expr, flow);
        }
        other => scan_flow_expr(spec, other, flow),
    }
}

fn scan_flow_stmt(spec: &CallRootSpec, stmt: &vm::Stmt, flow: &mut CallRootFlow) {
    match stmt {
        vm::Stmt::Let { expr, .. } | vm::Stmt::Expr { expr, .. } => {
            scan_flow_expr(spec, expr, flow)
        }
        vm::Stmt::Assign { index, expr, .. } => {
            if let vm::Expr::Call(_, _, args, _, _) = expr
                && let [vm::Expr::Var(vector), vm::Expr::Var(actual)] = args.as_slice()
                && vector == index
            {
                flow.pushes.push((*vector, *actual));
            }
            scan_flow_expr(spec, expr, flow);
        }
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            scan_flow_expr(spec, condition, flow);
            for nested in then_branch {
                scan_flow_stmt(spec, nested, flow);
            }
            for nested in else_branch {
                scan_flow_stmt(spec, nested, flow);
            }
        }
        vm::Stmt::For {
            init,
            condition,
            post,
            body,
            ..
        } => {
            scan_flow_stmt(spec, init, flow);
            scan_flow_expr(spec, condition, flow);
            scan_flow_stmt(spec, post, flow);
            for nested in body {
                scan_flow_stmt(spec, nested, flow);
            }
        }
        vm::Stmt::While {
            condition, body, ..
        } => {
            scan_flow_expr(spec, condition, flow);
            for nested in body {
                scan_flow_stmt(spec, nested, flow);
            }
        }
        vm::Stmt::Noop { .. }
        | vm::Stmt::FuncDecl { .. }
        | vm::Stmt::Break { .. }
        | vm::Stmt::Continue { .. }
        | vm::Stmt::ClosureLet { .. }
        | vm::Stmt::Drop { .. } => {}
    }
}

fn all_writes(flows: &[CallRootFlow]) -> impl Iterator<Item = &RootWriteEvent> {
    flows.iter().flat_map(|flow| flow.writes.iter())
}

fn entry_for_slot(flows: &[CallRootFlow], slot: u16) -> Result<Option<i64>, String> {
    let mut found: Option<i64> = None;
    for flow in flows {
        for event in &flow.writes {
            if event.value != RootWriteValue::Slot(slot) {
                continue;
            }
            match found {
                Some(entry) if entry != event.entry => {
                    return Err(format!(
                        "call-root witness: slot {slot} is rooted in two different entries"
                    ));
                }
                _ => found = Some(event.entry),
            }
        }
    }
    Ok(found)
}

fn last_write_for_entry(flows: &[CallRootFlow], entry: i64) -> Option<RootWriteValue> {
    flows
        .iter()
        .flat_map(|flow| flow.writes.iter())
        .rfind(|event| event.entry == entry)
        .map(|event| event.value)
}

/// One root write that is a direct statement of the examined list, without
/// descending into conditional branches.
fn direct_root_write_events(spec: &CallRootSpec, stmt: &vm::Stmt) -> Vec<RootWriteEvent> {
    let expr = match stmt {
        vm::Stmt::Let { expr, .. }
        | vm::Stmt::Assign { expr, .. }
        | vm::Stmt::Expr { expr, .. } => expr,
        _ => return Vec::new(),
    };
    match expr {
        vm::Expr::Call(target, _, args, _, _) if *target == spec.root_write => {
            root_write_event(spec, args).into_iter().collect()
        }
        _ => Vec::new(),
    }
}

fn block_call_witness(
    spec: &CallRootSpec,
    stmts: &[vm::Stmt],
    tail: &vm::Expr,
) -> Result<Option<CallRootWitness>, String> {
    let mut per_stmt = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        let mut flow = CallRootFlow::default();
        scan_flow_stmt(spec, stmt, &mut flow);
        per_stmt.push(flow);
    }
    let mut tail_flow = CallRootFlow::default();
    scan_flow_expr(spec, tail, &mut tail_flow);

    let sites: Vec<(usize, DispatchSite)> = per_stmt
        .iter()
        .enumerate()
        .flat_map(|(index, flow)| flow.dispatches.iter().map(move |site| (index, *site)))
        .collect();
    if sites.is_empty() {
        if !tail_flow.dispatches.is_empty() {
            return Err("call-root witness: dispatch lives in the result expression".to_owned());
        }
        return Ok(None);
    }
    let [(gate_index, site)] = sites.as_slice() else {
        return Err(format!(
            "call-root witness: expected one dispatch in the call block, found {}",
            sites.len()
        ));
    };
    if !tail_flow.dispatches.is_empty() {
        return Err("call-root witness: dispatch lives in the result expression".to_owned());
    }
    let (gate_index, site) = (*gate_index, *site);

    if per_stmt[..=gate_index]
        .iter()
        .any(|flow| flow.malformed > 0)
        || tail_flow.malformed > 0
    {
        return Err("call-root witness: malformed dispatch or root-write call".to_owned());
    }

    let pre = &per_stmt[..=gate_index];
    // Identity and liveness come from the root writes that existed when this
    // dispatch node was visited. Releases emitted by the ResultMatch arms sit
    // after that point in the same statement and must not root the call.
    let mut root_prefix = per_stmt[..=gate_index].to_vec();
    let gate_flow = root_prefix
        .last_mut()
        .expect("the gate statement is part of the prefix");
    if site.writes_before_dispatch > gate_flow.writes.len() {
        return Err(format!(
            "call-root witness: dispatch prefix {} exceeds {} collected root writes",
            site.writes_before_dispatch,
            gate_flow.writes.len()
        ));
    }
    gate_flow.writes.truncate(site.writes_before_dispatch);
    let mut all = per_stmt.clone();
    all.push(tail_flow.clone());

    let receiver_entry = match entry_for_slot(&root_prefix, site.receiver)? {
        Some(entry) => entry,
        None => return Ok(None),
    };
    if last_write_for_entry(&root_prefix, receiver_entry)
        != Some(RootWriteValue::Slot(site.receiver))
    {
        return Ok(None);
    }
    let callee_entry = entry_for_slot(&root_prefix, site.callee)?.ok_or_else(|| {
        format!(
            "call-root witness: callee slot {} is not rooted in the call block",
            site.callee
        )
    })?;
    if last_write_for_entry(&root_prefix, callee_entry) != Some(RootWriteValue::Slot(site.callee)) {
        return Err(format!(
            "call-root witness: callee slot {} is not rooted at the dispatch gate",
            site.callee
        ));
    }
    if all_writes(&all).any(|event| event.value == RootWriteValue::Unknown) {
        return Err(
            "call-root witness: a root write value is neither a slot nor a verified undefined clear"
                .to_owned(),
        );
    }
    let mut frames: Vec<u16> = all_writes(&all).map(|event| event.frame).collect();
    frames.sort_unstable();
    frames.dedup();
    if frames.len() > 1 {
        return Err("call-root witness: the call block mixes activation frames".to_owned());
    }

    let first_actual_slot = if spec.require_first_actual {
        let (_, actual) = all
            .iter()
            .flat_map(|flow| flow.pushes.iter())
            .find(|(vector, _)| *vector == site.vector)
            .ok_or_else(|| "call-root witness: dispatch vector has no argument push".to_owned())?;
        let actual = *actual;
        let entry = entry_for_slot(&root_prefix, actual)?.ok_or_else(|| {
            format!("call-root witness: first actual slot {actual} is not rooted in the call block")
        })?;
        if last_write_for_entry(&root_prefix, entry) != Some(RootWriteValue::Slot(actual)) {
            return Err(format!(
                "call-root witness: first actual slot {actual} is not rooted at the dispatch gate"
            ));
        }
        Some((actual, entry))
    } else {
        None
    };

    let result_slot = match tail {
        vm::Expr::Var(slot) => *slot,
        vm::Expr::ResultMatch { value_slot, .. } => *value_slot,
        _ => {
            return Err(
                "call-root witness: call result is neither a slot nor a Result match".to_owned(),
            );
        }
    };
    let result_entry = entry_for_slot(&all, result_slot)?
        .ok_or_else(|| format!("call-root witness: result slot {result_slot} is not rooted"))?;

    let mut required = vec![(receiver_entry, site.receiver), (callee_entry, site.callee)];
    if let Some((actual, entry)) = first_actual_slot {
        required.push((entry, actual));
    }

    let mut common_clears = Vec::new();
    for stmt in &stmts[gate_index + 1..] {
        for event in direct_root_write_events(spec, stmt) {
            if event.value == RootWriteValue::Undefined && !common_clears.contains(&event.entry) {
                common_clears.push(event.entry);
            }
        }
    }
    let mut gated_clears = Vec::new();
    for flow in pre.iter().chain(std::iter::once(&tail_flow)) {
        for event in &flow.writes {
            if event.value == RootWriteValue::Undefined && !gated_clears.contains(&event.entry) {
                gated_clears.push(event.entry);
            }
        }
    }

    Ok(Some(CallRootWitness {
        receiver_slot: site.receiver,
        callee_slot: site.callee,
        receiver_entry,
        callee_entry,
        first_actual_slot: first_actual_slot.map(|(slot, _)| slot),
        result_slot,
        result_entry,
        required,
        common_clears,
        gated_clears,
        dispatch_count: 1,
    }))
}

/// Global discovery of the outer member-call block. Candidates come from the
/// existing exhaustive walkers, so initializer blocks are covered; a block
/// whose dispatch receiver has no rooted write (a plain callee or a nested
/// argument call) is not a member-call candidate.
fn select_call_root_witness(
    spec: &CallRootSpec,
    stmts: &[vm::Stmt],
) -> Result<CallRootWitness, String> {
    let mut outcomes: Vec<Result<Option<CallRootWitness>, String>> = Vec::new();
    {
        let mut visit_stmt = |_: &vm::Stmt| {};
        let mut visit = |expr: &vm::Expr| {
            if let vm::Expr::Block {
                stmts: block_stmts,
                expr: tail,
            } = expr
            {
                outcomes.push(block_call_witness(spec, block_stmts, tail));
            }
        };
        walk_stmts(stmts, &mut visit, &mut visit_stmt);
    }
    let mut candidates = Vec::new();
    let mut malformed = Vec::new();
    for outcome in outcomes {
        match outcome {
            Ok(Some(witness)) => candidates.push(witness),
            Ok(None) => {}
            Err(error) => malformed.push(error),
        }
    }
    if !malformed.is_empty() {
        return Err(malformed.join("; "));
    }
    match candidates.len() {
        0 => {
            Err("call-root witness: no member-call block has a rooted dispatch receiver".to_owned())
        }
        1 => Ok(candidates.pop().expect("one candidate checked above")),
        count => Err(format!(
            "call-root witness: ambiguous, {count} member-call candidates"
        )),
    }
}

fn assert_call_roots_retired(witness: &CallRootWitness) -> Result<(), String> {
    if witness.required.is_empty() {
        return Err("call-root retirement: the required root set is empty".to_owned());
    }
    let missing: Vec<String> = witness
        .required
        .iter()
        .filter(|(entry, _)| !witness.common_clears.contains(entry))
        .map(|(entry, slot)| format!("slot {slot} entry {entry}"))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "consumed call roots are not retired by direct statements after the dispatch gate: {}",
            missing.join(", ")
        ));
    }
    let doubled: Vec<String> = witness
        .required
        .iter()
        .filter(|(entry, _)| witness.gated_clears.contains(entry))
        .map(|(entry, slot)| format!("slot {slot} entry {entry}"))
        .collect();
    if !doubled.is_empty() {
        return Err(format!(
            "consumed call roots are still released inside the gate or dispatch arm: {}",
            doubled.join(", ")
        ));
    }
    if witness.common_clears.contains(&witness.result_entry)
        || witness.gated_clears.contains(&witness.result_entry)
    {
        return Err(format!(
            "result root entry {} was cleared",
            witness.result_entry
        ));
    }
    Ok(())
}

#[test]
fn primitive_key151_call_root_retirement_witness_controls() {
    let spec = CallRootSpec {
        dispatch: CRW_DISPATCH,
        root_write: CRW_ROOT_WRITE,
        undefined: None,
        require_first_actual: false,
    };

    // Accepted: direct post-gate retirement of both consumed roots.
    let accepted = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_clear_stmt(60, CRW_FRAME, 0),
            crw_clear_stmt(61, CRW_FRAME, 1),
        ],
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(accepted)])
        .expect("nominal block extracts");
    assert_eq!(
        witness.required,
        vec![(0, CRW_RECEIVER), (1, CRW_CALLEE)],
        "receiver and callee are the consumed call roots"
    );
    assert_eq!(witness.result_entry, 3, "result slot stays rooted");
    assert_call_roots_retired(&witness).expect("common post-gate retirement is accepted");

    // Rejected: a consumed root is never retired.
    let missing = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![crw_clear_stmt(60, CRW_FRAME, 0)],
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(missing)])
        .expect("missing-clear block extracts");
    let error = assert_call_roots_retired(&witness)
        .expect_err("missing callee retirement must be rejected");
    assert!(error.contains("not retired"), "{error}");

    // Rejected: the only release sits inside the dispatch gate.
    let gated_only = crw_candidate_block(
        Vec::new(),
        vec![
            crw_clear_stmt(60, CRW_FRAME, 0),
            crw_clear_stmt(61, CRW_FRAME, 1),
        ],
        Vec::new(),
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(gated_only)])
        .expect("arm-only release block extracts");
    let error = assert_call_roots_retired(&witness).expect_err("arm-only release must be rejected");
    assert!(error.contains("not retired"), "{error}");

    // Rejected at extraction: a receiver rooted only after the dispatch is
    // not the receiver edge this dispatch consumed, even when the later root
    // and the common clears afterwards look complete.
    let late_rooted = (
        vec![
            crw_gate(vec![
                crw_dispatch_stmt(35, CRW_CALLEE, CRW_RECEIVER, CRW_VECTOR),
                crw_root_write_stmt(50, CRW_FRAME, 0, crw_var(CRW_RECEIVER)),
                crw_root_write_stmt(51, CRW_FRAME, 1, crw_var(CRW_CALLEE)),
                crw_root_write_stmt(52, CRW_FRAME, 3, crw_var(CRW_RESULT)),
            ]),
            crw_clear_stmt(60, CRW_FRAME, 0),
            crw_clear_stmt(61, CRW_FRAME, 1),
        ],
        crw_var(CRW_RESULT),
    );
    let error = select_call_root_witness(&spec, &[crw_expr_block(late_rooted)])
        .expect_err("post-dispatch rooting must not satisfy the dispatch receiver");
    assert!(error.contains("rooted dispatch receiver"), "{error}");

    // Rejected: a clear emitted before the gate cannot substitute.
    let before_gate = vec![
        crw_root_write_stmt(50, CRW_FRAME, 0, crw_var(CRW_RECEIVER)),
        crw_clear_stmt(51, CRW_FRAME, 0),
        crw_root_write_stmt(52, CRW_FRAME, 0, crw_var(CRW_RECEIVER)),
        crw_root_write_stmt(53, CRW_FRAME, 1, crw_var(CRW_CALLEE)),
        crw_gate(vec![
            crw_dispatch_stmt(35, CRW_CALLEE, CRW_RECEIVER, CRW_VECTOR),
            crw_root_write_stmt(54, CRW_FRAME, 3, crw_var(CRW_RESULT)),
        ]),
        crw_clear_stmt(55, CRW_FRAME, 1),
    ];
    let witness =
        select_call_root_witness(&spec, &[crw_expr_block((before_gate, crw_var(CRW_RESULT)))])
            .expect("pre-gate clear block extracts");
    let error = assert_call_roots_retired(&witness)
        .expect_err("a pre-gate clear must not count as retirement");
    assert!(error.contains("not retired"), "{error}");

    // Rejected: an unrelated root clear cannot stand in for a consumed one.
    let unrelated = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_clear_stmt(60, CRW_FRAME, 0),
            crw_clear_stmt(61, CRW_FRAME, 7),
        ],
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(unrelated)])
        .expect("unrelated-clear block extracts");
    let error =
        assert_call_roots_retired(&witness).expect_err("an unrelated root clear must be rejected");
    assert!(error.contains("not retired"), "{error}");

    // Rejected: the result root is cleared.
    let result_cleared = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_clear_stmt(60, CRW_FRAME, 0),
            crw_clear_stmt(61, CRW_FRAME, 1),
            crw_clear_stmt(62, CRW_FRAME, 3),
        ],
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(result_cleared)])
        .expect("result-clear block extracts");
    let error =
        assert_call_roots_retired(&witness).expect_err("a cleared result root must be rejected");
    assert!(error.contains("result root"), "{error}");

    // Rejected loudly: an unknown root-write value shape is not a clear.
    let unknown = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_root_write_stmt(60, CRW_FRAME, 2, vm::Expr::Int(7)),
            crw_clear_stmt(61, CRW_FRAME, 0),
            crw_clear_stmt(62, CRW_FRAME, 1),
        ],
    );
    let error = select_call_root_witness(&spec, &[crw_expr_block(unknown)])
        .expect_err("an unknown root-write value shape must fail extraction");
    assert!(error.contains("neither a slot"), "{error}");

    // A nested block with an unrooted receiver is not the member call.
    let nested = vm::Expr::Block {
        stmts: vec![crw_dispatch_stmt(36, 37, 38, 39)],
        expr: Box::new(crw_var(36)),
    };
    let outer = crw_candidate_block(
        vec![crw_push_block_stmt(CRW_VECTOR, nested)],
        Vec::new(),
        vec![
            crw_clear_stmt(60, CRW_FRAME, 0),
            crw_clear_stmt(61, CRW_FRAME, 1),
        ],
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(outer)])
        .expect("only the outer member call is a candidate");
    assert_eq!(
        (witness.receiver_slot, witness.callee_slot),
        (CRW_RECEIVER, CRW_CALLEE),
        "the nested call must not steal the outer identity"
    );
    assert_call_roots_retired(&witness).expect("outer retirement is accepted");

    // Two member-call candidates are ambiguous, never silently merged.
    let second = (
        vec![
            crw_root_write_stmt(70, CRW_FRAME, 4, crw_var(41)),
            crw_root_write_stmt(71, CRW_FRAME, 5, crw_var(42)),
            crw_gate(vec![
                crw_dispatch_stmt(37, 42, 41, 43),
                crw_root_write_stmt(72, CRW_FRAME, 6, crw_var(44)),
            ]),
            crw_clear_stmt(73, CRW_FRAME, 4),
            crw_clear_stmt(74, CRW_FRAME, 5),
        ],
        crw_var(44),
    );
    let ambiguous = [
        crw_expr_block(crw_candidate_block(
            Vec::new(),
            Vec::new(),
            vec![
                crw_clear_stmt(60, CRW_FRAME, 0),
                crw_clear_stmt(61, CRW_FRAME, 1),
            ],
        )),
        crw_expr_block(second),
    ];
    let error = select_call_root_witness(&spec, &ambiguous)
        .expect_err("two member-call candidates must be ambiguous");
    assert!(error.contains("ambiguous"), "{error}");
}

/// Phase controls for the factored logical clear. Only an exact empty call to
/// the resolved helper identity is a clear; every other call shape keeps the
/// strict Unknown rejection.
#[test]
fn primitive_key151_call_root_undefined_helper_phase_controls() {
    let spec = CallRootSpec {
        dispatch: CRW_DISPATCH,
        root_write: CRW_ROOT_WRITE,
        undefined: Some(CRW_UNDEFINED),
        require_first_actual: false,
    };

    // Accepted: both consumed roots are cleared through the verified helper
    // call after the dispatch gate.
    let helper_cleared = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_helper_clear_stmt(60, CRW_FRAME, 0),
            crw_helper_clear_stmt(61, CRW_FRAME, 1),
        ],
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(helper_cleared)])
        .expect("helper-clear block extracts");
    assert_eq!(
        witness.common_clears,
        vec![0, 1],
        "helper calls after the gate are logical clears"
    );
    assert_call_roots_retired(&witness).expect("helper clears retire the consumed roots");

    // Accepted: the literal five-field record keeps its meaning under a
    // helper-bound spec.
    let record_cleared = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_clear_stmt(60, CRW_FRAME, 0),
            crw_clear_stmt(61, CRW_FRAME, 1),
        ],
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(record_cleared)])
        .expect("literal-record block extracts");
    assert_call_roots_retired(&witness).expect("the exact record remains a clear");

    // Rejected: a helper clear inside the gate is not post-gate retirement.
    let gated_helper = crw_candidate_block(
        Vec::new(),
        vec![
            crw_helper_clear_stmt(60, CRW_FRAME, 0),
            crw_helper_clear_stmt(61, CRW_FRAME, 1),
        ],
        Vec::new(),
    );
    let witness = select_call_root_witness(&spec, &[crw_expr_block(gated_helper)])
        .expect("gated helper block extracts");
    let error = assert_call_roots_retired(&witness)
        .expect_err("gated helper clears must not count as retirement");
    assert!(error.contains("not retired"), "{error}");

    // Rejected loudly: a zero-argument call to another helper is not the
    // undefined declaration, even when the required clears are present.
    let wrong_helper = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_helper_clear_stmt(60, CRW_FRAME, 0),
            crw_helper_clear_stmt(61, CRW_FRAME, 1),
            crw_root_write_stmt(62, CRW_FRAME, 2, crw_call(CRW_OTHER_HELPER, Vec::new())),
        ],
    );
    let error = select_call_root_witness(&spec, &[crw_expr_block(wrong_helper)])
        .expect_err("a zero-argument call to another helper must stay unknown");
    assert!(error.contains("neither a slot"), "{error}");

    // Rejected loudly: the verified helper called with a runtime argument is
    // not the logical clear.
    let with_argument = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_helper_clear_stmt(60, CRW_FRAME, 0),
            crw_helper_clear_stmt(61, CRW_FRAME, 1),
            crw_root_write_stmt(
                62,
                CRW_FRAME,
                2,
                crw_call(CRW_UNDEFINED, vec![crw_var(CRW_RESULT)]),
            ),
        ],
    );
    let error = select_call_root_witness(&spec, &[crw_expr_block(with_argument)])
        .expect_err("the helper called with an argument must stay unknown");
    assert!(error.contains("neither a slot"), "{error}");

    // Rejected loudly: a type-argument capture of the same helper is not the
    // plain logical clear.
    let type_instantiated = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_helper_clear_stmt(60, CRW_FRAME, 0),
            crw_helper_clear_stmt(61, CRW_FRAME, 1),
            crw_root_write_stmt(
                62,
                CRW_FRAME,
                2,
                vm::Expr::Call(
                    CRW_UNDEFINED,
                    vec![vm::TypeSchema::Int],
                    Vec::new(),
                    None,
                    None,
                ),
            ),
        ],
    );
    let error = select_call_root_witness(&spec, &[crw_expr_block(type_instantiated)])
        .expect_err("a type-instantiated helper call must stay unknown");
    assert!(error.contains("neither a slot"), "{error}");

    // Identity control: the same helper calls stay unknown to a spec bound to
    // a different declaration, so acceptance comes from the verified identity.
    let other_spec = CallRootSpec {
        undefined: Some(CRW_OTHER_HELPER),
        ..spec
    };
    let helper_cleared = crw_candidate_block(
        Vec::new(),
        Vec::new(),
        vec![
            crw_helper_clear_stmt(60, CRW_FRAME, 0),
            crw_helper_clear_stmt(61, CRW_FRAME, 1),
        ],
    );
    let error = select_call_root_witness(&other_spec, &[crw_expr_block(helper_cleared)])
        .expect_err("helper calls must not clear when the identity differs");
    assert!(error.contains("neither a slot"), "{error}");
}

/// Resolve the factored `__compat_es5_undefined` logical helper from the
/// freshly captured IR. The identity is the declaration's own flat index and
/// its shape is validated; a missing, ambiguous or mismatched declaration is a
/// prerequisite failure, never a fallback classification.
fn undefined_helper_index(ir: &vm::FrontendIr) -> u16 {
    const NAME: &str = "__compat_es5_undefined";
    let mut declarations = ir.functions.iter().filter(|function| function.name == NAME);
    let declaration = declarations
        .next()
        .unwrap_or_else(|| panic!("missing logical helper {NAME}"));
    assert!(
        declarations.next().is_none(),
        "ambiguous logical helper {NAME}"
    );
    assert_eq!(declaration.arity, 0, "{NAME} must take no parameters");
    assert!(
        declaration.args.is_empty() && declaration.arg_schemas.is_empty(),
        "{NAME} must declare no parameters"
    );
    assert!(
        declaration.type_params.is_empty(),
        "{NAME} must not be generic"
    );
    assert_eq!(
        declaration.return_schema,
        Some(vm::TypeSchema::Named(
            "__CompatEs5Primitive".to_owned(),
            Vec::new()
        )),
        "{NAME} must return the es5 primitive schema"
    );
    declaration.index
}

#[test]
fn primitive_key151_call_pre_dispatch_throw_retires_consumed_roots() {
    eprintln!("CALL_ROOT_RETIREMENT_SOURCE source=pre_dispatch_throw frontend_parse=1 guest_vm=0");
    let source =
        r#"'use strict';var k={toString:function(){'use strict';throw 1;}};var o={};o[k]();"#;
    let ir = lowered_ir(source);
    eprintln!(
        "CALL_ROOT_RETIREMENT_IR_RAW source=pre_dispatch functions={:?} stmts={:?}",
        ir.functions, ir.stmts
    );
    let spec = CallRootSpec {
        dispatch: function_index(&ir, "__compat_es5_dispatch"),
        root_write: function_index(&ir, "__compat_es5_root_write"),
        undefined: Some(undefined_helper_index(&ir)),
        require_first_actual: false,
    };
    let witness =
        select_call_root_witness(&spec, &ir.stmts).expect("extract the primitive-key call witness");
    eprintln!(
        "CALL_ROOT_RETIREMENT_WITNESS receiver_slot={} receiver_entry={} callee_slot={} callee_entry={} result_slot={} result_entry={} required={:?} common={:?} gated={:?}",
        witness.receiver_slot,
        witness.receiver_entry,
        witness.callee_slot,
        witness.callee_entry,
        witness.result_slot,
        witness.result_entry,
        witness.required,
        witness.common_clears,
        witness.gated_clears
    );
    assert_eq!(
        witness.dispatch_count, 1,
        "one direct dispatch in the call block"
    );
    assert_ne!(witness.receiver_slot, witness.callee_slot);
    assert_eq!(
        witness.required.len(),
        2,
        "the primitive-key call consumes the receiver and the callee root"
    );
    assert!(
        !witness
            .required
            .iter()
            .any(|(_, slot)| *slot == witness.result_slot),
        "the result slot is not a consumed call root"
    );
    assert_call_roots_retired(&witness)
        .unwrap_or_else(|error| panic!("CALL_ROOTS_NOT_RETIRED: {error}"));
}

#[test]
fn primitive_key151_call_argument_throw_retires_consumed_roots() {
    eprintln!("CALL_ROOT_RETIREMENT_SOURCE source=argument_throw frontend_parse=1 guest_vm=0");
    let source = r#"'use strict';var o={'m':function(){'use strict';return 1;}};var key='m';function boom(){'use strict';throw 1;}o[key]({},boom());"#;
    let ir = lowered_ir(source);
    eprintln!(
        "CALL_ROOT_RETIREMENT_IR_RAW source=argument_throw functions={:?} stmts={:?}",
        ir.functions, ir.stmts
    );
    let spec = CallRootSpec {
        dispatch: function_index(&ir, "__compat_es5_dispatch"),
        root_write: function_index(&ir, "__compat_es5_root_write"),
        undefined: Some(undefined_helper_index(&ir)),
        require_first_actual: true,
    };
    let witness = select_call_root_witness(&spec, &ir.stmts)
        .expect("extract the argument-throw call witness");
    eprintln!(
        "CALL_ROOT_RETIREMENT_WITNESS receiver_slot={} receiver_entry={} callee_slot={} callee_entry={} first_actual={:?} result_slot={} result_entry={} required={:?} common={:?} gated={:?}",
        witness.receiver_slot,
        witness.receiver_entry,
        witness.callee_slot,
        witness.callee_entry,
        witness.first_actual_slot,
        witness.result_slot,
        witness.result_entry,
        witness.required,
        witness.common_clears,
        witness.gated_clears
    );
    assert_eq!(
        witness.dispatch_count, 1,
        "one direct dispatch in the call block"
    );
    assert_ne!(witness.receiver_slot, witness.callee_slot);
    let first_actual = witness
        .first_actual_slot
        .expect("the outer call pushes the fresh object actual");
    assert!(
        first_actual != witness.receiver_slot && first_actual != witness.callee_slot,
        "the first actual is its own rooted temporary"
    );
    assert_eq!(
        witness.required.len(),
        3,
        "receiver, callee and the first actual are consumed call roots"
    );
    assert!(
        !witness
            .required
            .iter()
            .any(|(_, slot)| *slot == witness.result_slot),
        "the result slot is not a consumed call root"
    );
    assert_call_roots_retired(&witness)
        .unwrap_or_else(|error| panic!("CALL_ROOTS_NOT_RETIRED: {error}"));
}

// ---------------------------------------------------------------------------
// Known-throw initializer root retirement for `null[{}] += 1`.
//
// The initializer publishes a fresh object for the computed key and roots an
// evaluation copy of it, then throws a TypeError whose pending payload is
// rooted separately. Only the IR names the actual helper identities, so the
// witness binds the publication and copy root entries from the produced
// calls. Success-arm writes and the pending payload are deliberately not
// treated as owned by the key evaluation.
// ---------------------------------------------------------------------------

fn initializer_primitive_schema() -> Option<vm::TypeSchema> {
    Some(vm::TypeSchema::Named(
        "__CompatEs5Primitive".to_owned(),
        Vec::new(),
    ))
}

struct InitializerRootSpec {
    type_error: u16,
    node_publish: u16,
    root_write: u16,
    undefined: u16,
    map_new: u16,
    set: u16,
}

#[derive(Debug)]
struct InitializerRootWitness {
    frame: u16,
    publish_slot: u16,
    publish_entry: i64,
    copy_slot: u16,
    copy_entry: i64,
    pending_slot: u16,
    pending_entry: i64,
    discarded_temp: Option<(i64, u16)>,
    common_clears: Vec<i64>,
}

fn initializer_helper(ir: &vm::FrontendIr, name: &str, arity: u8, args: &[&str]) -> u16 {
    let function = ir
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing helper {name}"));
    assert_eq!(function.arity, arity, "{name}");
    assert_eq!(function.args.as_slice(), args, "{name}");
    function.index
}

fn initializer_root_spec(ir: &vm::FrontendIr) -> InitializerRootSpec {
    InitializerRootSpec {
        type_error: initializer_helper(ir, "__compat_es5_new_type_error", 1, &["arg0"]),
        node_publish: initializer_helper(
            ir,
            "__compat_es5_node_publish",
            6,
            &["carrier", "slot", "generation", "base", "frame", "entry"],
        ),
        root_write: initializer_helper(
            ir,
            "__compat_es5_root_write",
            4,
            &["carrier", "frame", "entry", "value"],
        ),
        undefined: initializer_helper(ir, "__compat_es5_undefined", 0, &[]),
        map_new: vm::builtin_call_index("map_new").expect("catalog map_new identity"),
        set: vm::builtin_call_index("set").expect("catalog set identity"),
    }
}

/// The existing root-write parser only classifies argument values; the
/// dispatch field is unused here.
fn initializer_clear_spec(spec: &InitializerRootSpec) -> CallRootSpec {
    CallRootSpec {
        dispatch: 0,
        root_write: spec.root_write,
        undefined: Some(spec.undefined),
        require_first_actual: false,
    }
}

fn initializer_call_args(expr: &vm::Expr, target: u16) -> Option<&[vm::Expr]> {
    if let vm::Expr::Call(index, type_args, args, resolved, node) = expr
        && *index == target
        && type_args.is_empty()
        && resolved.is_none()
        && node.is_none()
    {
        Some(args)
    } else {
        None
    }
}

fn initializer_root_write(
    spec: &InitializerRootSpec,
    expr: &vm::Expr,
) -> Option<(u16, RootWriteEvent)> {
    let args = initializer_call_args(expr, spec.root_write)?;
    let [vm::Expr::Var(carrier), _, _, _] = args else {
        return None;
    };
    let event = root_write_event(&initializer_clear_spec(spec), args)?;
    Some((*carrier, event))
}

fn initializer_direct_root_write(
    spec: &InitializerRootSpec,
    stmt: &vm::Stmt,
) -> Option<(u16, RootWriteEvent)> {
    let expr = match stmt {
        vm::Stmt::Let { expr, .. }
        | vm::Stmt::Assign { expr, .. }
        | vm::Stmt::Expr { expr, .. } => expr,
        _ => return None,
    };
    initializer_root_write(spec, expr)
}

fn initializer_has_call(stmts: &[vm::Stmt], tail: &vm::Expr, target: u16) -> bool {
    let mut found = false;
    let mut visit_stmt = |_: &vm::Stmt| {};
    let mut visit = |expr: &vm::Expr| {
        if initializer_call_args(expr, target).is_some() {
            found = true;
        }
    };
    walk_stmts(stmts, &mut visit, &mut visit_stmt);
    walk_expr(tail, &mut visit, &mut visit_stmt);
    found
}

fn initializer_publisher(args: &[vm::Expr]) -> Option<(u16, u16, i64)> {
    let [
        vm::Expr::Var(carrier),
        vm::Expr::Var(_),
        vm::Expr::Var(_),
        vm::Expr::Var(_),
        vm::Expr::Var(frame),
        vm::Expr::Int(entry),
    ] = args
    else {
        return None;
    };
    Some((*carrier, *frame, *entry))
}

/// Follows statement lists only, so the caller can keep the list that holds
/// the initializer owner and scan its direct statements after that owner.
fn collect_initializer_owners<'a>(
    stmts: &'a [vm::Stmt],
    spec: &InitializerRootSpec,
    found: &mut Vec<(&'a [vm::Stmt], usize)>,
) {
    for (index, stmt) in stmts.iter().enumerate() {
        if let vm::Stmt::Let {
            expr:
                vm::Expr::Block {
                    stmts: body,
                    expr: tail,
                },
            ..
        } = stmt
            && initializer_has_call(body, tail, spec.type_error)
            && initializer_has_call(body, tail, spec.node_publish)
        {
            found.push((stmts, index));
            continue;
        }
        match stmt {
            vm::Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                collect_initializer_owners(then_branch, spec, found);
                collect_initializer_owners(else_branch, spec, found);
            }
            vm::Stmt::While { body, .. } => collect_initializer_owners(body, spec, found),
            vm::Stmt::For {
                init, post, body, ..
            } => {
                collect_initializer_owners(std::slice::from_ref(init.as_ref()), spec, found);
                collect_initializer_owners(std::slice::from_ref(post.as_ref()), spec, found);
                collect_initializer_owners(body, spec, found);
            }
            _ => {}
        }
    }
}

fn initializer_record<'a>(
    expr: &'a vm::Expr,
    spec: &InitializerRootSpec,
) -> Option<Vec<(&'a str, &'a vm::Expr)>> {
    let mut fields = Vec::new();
    let mut current = expr;
    loop {
        if let Some(args) = initializer_call_args(current, spec.set) {
            let [previous, vm::Expr::String(key), value] = args else {
                return None;
            };
            fields.push((key.as_str(), value));
            current = previous;
        } else if initializer_call_args(current, spec.map_new).is_some_and(|args| args.is_empty()) {
            return Some(fields);
        } else {
            return None;
        }
    }
}

fn initializer_throw_payload(record: Vec<(&str, &vm::Expr)>) -> Option<u16> {
    if record.len() != 4 {
        return None;
    }
    let mut fields: [Option<&vm::Expr>; 4] = [None; 4];
    for (key, field) in record {
        let slot = match key {
            "kind" => &mut fields[0],
            "value" => &mut fields[1],
            "has_value" => &mut fields[2],
            "target" => &mut fields[3],
            _ => return None,
        };
        if slot.replace(field).is_some() {
            return None;
        }
    }
    let [kind, value, has_value, target] = fields;
    let kind = kind?;
    let value = value?;
    let has_value = has_value?;
    let target = target?;
    if !matches!(kind, vm::Expr::Int(2))
        || !matches!(has_value, vm::Expr::Bool(true))
        || !matches!(target, vm::Expr::Int(0))
    {
        return None;
    }
    let vm::Expr::Var(payload) = value else {
        return None;
    };
    Some(*payload)
}

fn initializer_pending(
    err_binding: u16,
    err_expr: &vm::Expr,
    spec: &InitializerRootSpec,
    carrier: u16,
    frame: u16,
) -> Option<(u16, i64)> {
    let vm::Expr::Block { stmts, expr: tail } = err_expr else {
        return None;
    };
    let mut payloads = Vec::new();
    {
        let mut visit_stmt = |_: &vm::Stmt| {};
        let mut visit = |expr: &vm::Expr| {
            if let Some(record) = initializer_record(expr, spec)
                && let Some(payload) = initializer_throw_payload(record)
            {
                payloads.push(payload);
            }
        };
        walk_stmts(stmts, &mut visit, &mut visit_stmt);
        walk_expr(tail, &mut visit, &mut visit_stmt);
    }
    let [payload] = payloads.as_slice() else {
        return None;
    };
    let payload = *payload;
    let assigned = stmts.iter().any(|stmt| {
        matches!(
            stmt,
            vm::Stmt::Assign { index, expr: vm::Expr::Var(source), .. }
                if *index == payload && *source == err_binding
        )
    });
    if !assigned {
        return None;
    }
    let mut roots = Vec::new();
    {
        let mut visit_stmt = |_: &vm::Stmt| {};
        let mut visit = |expr: &vm::Expr| {
            if let Some((write_carrier, event)) = initializer_root_write(spec, expr)
                && write_carrier == carrier
                && event.frame == frame
                && event.value == RootWriteValue::Slot(payload)
            {
                roots.push(event);
            }
        };
        walk_stmts(stmts, &mut visit, &mut visit_stmt);
    }
    let [root] = roots.as_slice() else {
        return None;
    };
    Some((payload, root.entry))
}

fn initializer_root_witness(
    ir_stmts: &[vm::Stmt],
    spec: &InitializerRootSpec,
) -> Result<InitializerRootWitness, String> {
    let mut owners = Vec::new();
    collect_initializer_owners(ir_stmts, spec, &mut owners);
    if owners.len() != 1 {
        return Err(format!(
            "initializer root witness: expected one known-throw initializer block, found {}",
            owners.len()
        ));
    }
    let (list, owner_index) = owners[0];
    let (owner_slot, block_stmts, block_tail) = match &list[owner_index] {
        vm::Stmt::Let {
            index,
            expr: vm::Expr::Block { stmts, expr },
            ..
        } => (*index, stmts.as_slice(), expr.as_ref()),
        _ => return Err("initializer root witness: owner is not a block-valued let".to_owned()),
    };
    if !matches!(block_tail, vm::Expr::Var(_)) {
        return Err("initializer root witness: owner block tail is not a variable".to_owned());
    }

    let primitive = initializer_primitive_schema();
    let mut inputs: Vec<(u16, Vec<vm::Stmt>, Box<vm::Expr>)> = Vec::new();
    {
        let mut visit = |_: &vm::Expr| {};
        let mut visit_stmt = |stmt: &vm::Stmt| {
            let vm::Stmt::Let {
                index,
                declared_schema,
                expr: vm::Expr::Block { stmts, expr },
                ..
            } = stmt
            else {
                return;
            };
            if *declared_schema != primitive {
                return;
            }
            if initializer_has_call(stmts, expr, spec.type_error) {
                return;
            }
            if initializer_has_call(stmts, expr, spec.node_publish) {
                inputs.push((*index, stmts.clone(), expr.clone()));
            }
        };
        walk_stmts(block_stmts, &mut visit, &mut visit_stmt);
    }
    if inputs.len() != 1 {
        return Err(format!(
            "initializer root witness: expected one key-evaluation block, found {}",
            inputs.len()
        ));
    }
    let (input_slot, inner_stmts, inner_tail) =
        (inputs[0].0, inputs[0].1.as_slice(), inputs[0].2.as_ref());
    let vm::Expr::Var(output_slot) = inner_tail else {
        return Err("initializer root witness: key block tail is not a variable".to_owned());
    };
    let output_slot = *output_slot;

    let mut publishers = Vec::new();
    {
        let mut visit_stmt = |_: &vm::Stmt| {};
        let mut visit = |expr: &vm::Expr| {
            if let Some(args) = initializer_call_args(expr, spec.node_publish)
                && let Some(publisher) = initializer_publisher(args)
            {
                publishers.push(publisher);
            }
        };
        walk_stmts(inner_stmts, &mut visit, &mut visit_stmt);
        walk_expr(inner_tail, &mut visit, &mut visit_stmt);
    }
    let [publisher] = publishers.as_slice() else {
        return Err(format!(
            "initializer root witness: expected one node publication, found {}",
            publishers.len()
        ));
    };
    let (carrier, frame, publish_entry) = *publisher;

    let mut publish_roots = Vec::new();
    {
        let mut visit_stmt = |_: &vm::Stmt| {};
        let mut visit = |expr: &vm::Expr| {
            if let Some((write_carrier, event)) = initializer_root_write(spec, expr)
                && write_carrier == carrier
                && event.frame == frame
                && event.value == RootWriteValue::Slot(output_slot)
            {
                publish_roots.push(event);
            }
        };
        walk_stmts(inner_stmts, &mut visit, &mut visit_stmt);
        walk_expr(inner_tail, &mut visit, &mut visit_stmt);
    }
    let [publish_root] = publish_roots.as_slice() else {
        return Err(format!(
            "initializer root witness: expected one publication root, found {}",
            publish_roots.len()
        ));
    };
    if publish_root.entry != publish_entry {
        return Err(format!(
            "initializer root witness: publication entry {} is not root entry {}",
            publish_entry, publish_root.entry
        ));
    }

    let mut copy_roots = Vec::new();
    {
        let mut visit_stmt = |_: &vm::Stmt| {};
        let mut visit = |expr: &vm::Expr| {
            if let Some((write_carrier, event)) = initializer_root_write(spec, expr)
                && write_carrier == carrier
                && event.frame == frame
                && event.value == RootWriteValue::Slot(input_slot)
            {
                copy_roots.push(event);
            }
        };
        walk_stmts(block_stmts, &mut visit, &mut visit_stmt);
    }
    let [copy_root] = copy_roots.as_slice() else {
        return Err(format!(
            "initializer root witness: expected one key-copy root, found {}",
            copy_roots.len()
        ));
    };
    if copy_root.entry == publish_entry {
        return Err(
            "initializer root witness: the key copy shares the publication entry".to_owned(),
        );
    }

    let mut pendings = Vec::new();
    {
        let mut visit_stmt = |_: &vm::Stmt| {};
        let mut visit = |expr: &vm::Expr| {
            let vm::Expr::ResultMatch {
                err_binding: Some(err_binding),
                err_expr,
                ..
            } = expr
            else {
                return;
            };
            pendings.push((*err_binding, err_expr.as_ref().clone()));
        };
        walk_stmts(block_stmts, &mut visit, &mut visit_stmt);
    }
    if pendings.len() != 1 {
        return Err(format!(
            "initializer root witness: expected one throw result match, found {}",
            pendings.len()
        ));
    }
    let (err_binding, err_expr) = pendings.pop().expect("one pending checked above");
    let (pending_slot, pending_entry) =
        initializer_pending(err_binding, &err_expr, spec, carrier, frame).ok_or_else(|| {
            "initializer root witness: the throw arm does not publish or root its payload"
                .to_owned()
        })?;

    let mut common_clears = Vec::new();
    let mut discarded_temp = None;
    for stmt in &list[owner_index + 1..] {
        let Some((write_carrier, event)) = initializer_direct_root_write(spec, stmt) else {
            continue;
        };
        if write_carrier != carrier || event.frame != frame {
            continue;
        }
        match event.value {
            RootWriteValue::Slot(slot) if slot == owner_slot => {
                if discarded_temp.is_some() {
                    return Err(
                        "initializer root witness: two discarded destination roots".to_owned()
                    );
                }
                discarded_temp = Some((event.entry, slot));
            }
            RootWriteValue::Undefined => {
                if !common_clears.contains(&event.entry) {
                    common_clears.push(event.entry);
                }
            }
            RootWriteValue::Slot(_) => {}
            RootWriteValue::Unknown => {
                return Err(format!(
                    "initializer root witness: unknown post-owner root write at entry {}",
                    event.entry
                ));
            }
        }
    }

    Ok(InitializerRootWitness {
        frame,
        publish_slot: output_slot,
        publish_entry,
        copy_slot: input_slot,
        copy_entry: copy_root.entry,
        pending_slot,
        pending_entry,
        discarded_temp,
        common_clears,
    })
}

fn assert_initializer_roots_retired(witness: &InitializerRootWitness) -> Result<(), String> {
    let required = [
        (
            "fresh_key_publication",
            witness.publish_entry,
            witness.publish_slot,
        ),
        ("key_evaluation_copy", witness.copy_entry, witness.copy_slot),
    ];
    if witness.publish_entry == witness.copy_entry || witness.publish_slot == witness.copy_slot {
        return Err(format!(
            "initializer root witness: key roots are not distinct (entries {} and {})",
            witness.publish_entry, witness.copy_entry
        ));
    }
    if required
        .iter()
        .any(|(_, entry, _)| *entry == witness.pending_entry)
    {
        return Err(format!(
            "initializer root witness: pending payload entry {} is claimed as a key root",
            witness.pending_entry
        ));
    }
    if witness.common_clears.contains(&witness.pending_entry) {
        return Err(format!(
            "INITIALIZER_PENDING_ROOT_CLEARED: slot {} entry {} clears {:?}",
            witness.pending_slot, witness.pending_entry, witness.common_clears
        ));
    }
    let mut missing = Vec::new();
    for (label, entry, slot) in required {
        if !witness.common_clears.contains(&entry) {
            missing.push(format!("{label} slot {slot} entry {entry}"));
        }
    }
    if let Some((entry, slot)) = witness.discarded_temp
        && !witness.common_clears.contains(&entry)
    {
        missing.push(format!(
            "discarded_destination_temp slot {slot} entry {entry}"
        ));
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "KNOWN_THROW_DECL_ROOTS_NOT_RETIRED: frame {} missing [{}] clears {:?}",
            witness.frame,
            missing.join(", "),
            witness.common_clears
        ))
    }
}

fn initializer_witness(ir: &vm::FrontendIr) -> InitializerRootWitness {
    let spec = initializer_root_spec(ir);
    initializer_root_witness(&ir.stmts, &spec)
        .unwrap_or_else(|error| panic!("INITIALIZER_ROOT_SHAPE: {error}"))
}

#[test]
fn primitive_key151_known_throw_var_initializer_retires_owned_roots() {
    let source = r#"try{var result=null[{}]+=1;}catch(e){}"#;
    eprintln!(
        "INITIALIZER_RETIREMENT_SOURCE case=declarator frontend_parse=1 guest_vm=0 source={source:?}"
    );
    let ir = lowered_ir(source);
    let witness = initializer_witness(&ir);
    assert_initializer_roots_retired(&witness).unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn primitive_key151_known_throw_expression_statement_retires_owned_roots() {
    let source = r#"try{null[{}]+=1;}catch(e){}"#;
    eprintln!(
        "INITIALIZER_RETIREMENT_SOURCE case=expression frontend_parse=1 guest_vm=0 source={source:?}"
    );
    let ir = lowered_ir(source);
    let witness = initializer_witness(&ir);
    assert_initializer_roots_retired(&witness).unwrap_or_else(|error| panic!("{error}"));
}

// ---------------------------------------------------------------------------
// Pure controls: synthetic statements with identities unrelated to the
// production helper indices. They exercise the same extraction and assertion
// without any frontend parse.
// ---------------------------------------------------------------------------

const IRW_CARRIER: u16 = 5;
const IRW_FRAME: u16 = 23;
const IRW_TYPE_ERROR: u16 = 71;
const IRW_PUBLISH: u16 = 72;
const IRW_ROOT_WRITE: u16 = 73;
const IRW_UNDEFINED: u16 = 74;
const IRW_MAP_NEW: u16 = 75;
const IRW_SET: u16 = 76;
const IRW_OTHER: u16 = 77;
const IRW_PUBLISHED: u16 = 301;
const IRW_THROWN: u16 = 302;
const IRW_SCRUTINEE: u16 = 303;
const IRW_OK: u16 = 304;
const IRW_ERR: u16 = 305;
const IRW_ACK: u16 = 306;
const IRW_ARGS_BASE: u16 = 311;

struct IrwPlan {
    owner: u16,
    input: u16,
    output: u16,
    publish_entry: i64,
    copy_entry: i64,
    pending: u16,
    pending_entry: i64,
    discarded_entry: i64,
}

fn irw_spec() -> InitializerRootSpec {
    InitializerRootSpec {
        type_error: IRW_TYPE_ERROR,
        node_publish: IRW_PUBLISH,
        root_write: IRW_ROOT_WRITE,
        undefined: IRW_UNDEFINED,
        map_new: IRW_MAP_NEW,
        set: IRW_SET,
    }
}

fn irw_plan() -> IrwPlan {
    IrwPlan {
        owner: 101,
        input: 102,
        output: 103,
        publish_entry: 21,
        copy_entry: 22,
        pending: 104,
        pending_entry: 23,
        discarded_entry: 24,
    }
}

fn irw_other_plan() -> IrwPlan {
    IrwPlan {
        owner: 201,
        input: 202,
        output: 203,
        publish_entry: 41,
        copy_entry: 42,
        pending: 204,
        pending_entry: 43,
        discarded_entry: 44,
    }
}

fn irw_schema() -> Option<vm::TypeSchema> {
    initializer_primitive_schema()
}

fn irw_call(target: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(target, Vec::new(), args, None, None)
}

fn irw_var(slot: u16) -> vm::Expr {
    vm::Expr::Var(slot)
}

fn irw_let(slot: u16, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index: slot,
        declared_schema: None,
        expr,
        line: 0,
    }
}

fn irw_primitive_let(slot: u16, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index: slot,
        declared_schema: irw_schema(),
        expr,
        line: 0,
    }
}

fn irw_assign(index: u16, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index,
        expr,
        line: 0,
    }
}

fn irw_root_write_at(frame: u16, entry: i64, value: vm::Expr) -> vm::Expr {
    irw_call(
        IRW_ROOT_WRITE,
        vec![
            irw_var(IRW_CARRIER),
            irw_var(frame),
            vm::Expr::Int(entry),
            value,
        ],
    )
}

fn irw_root_write(entry: i64, value: vm::Expr) -> vm::Expr {
    irw_root_write_at(IRW_FRAME, entry, value)
}

fn irw_root_write_let(slot: u16, entry: i64, value: vm::Expr) -> vm::Stmt {
    irw_let(slot, irw_root_write(entry, value))
}

fn irw_clear_at(frame: u16, slot: u16, entry: i64) -> vm::Stmt {
    irw_let(
        slot,
        irw_root_write_at(frame, entry, irw_call(IRW_UNDEFINED, Vec::new())),
    )
}

fn irw_clear(slot: u16, entry: i64) -> vm::Stmt {
    irw_clear_at(IRW_FRAME, slot, entry)
}

fn irw_completion(value: vm::Expr) -> vm::Expr {
    let mut record = irw_call(IRW_MAP_NEW, Vec::new());
    for (key, field) in [
        ("kind", vm::Expr::Int(2)),
        ("value", value),
        ("has_value", vm::Expr::Bool(true)),
        ("target", vm::Expr::Int(0)),
    ] {
        record = irw_call(
            IRW_SET,
            vec![record, vm::Expr::String(key.to_owned()), field],
        );
    }
    record
}

fn irw_undefined_record() -> vm::Expr {
    let mut record = irw_call(IRW_MAP_NEW, Vec::new());
    for (key, field) in [
        ("tag", vm::Expr::Int(0)),
        ("number", vm::Expr::Float(0.0)),
        ("flag", vm::Expr::Bool(false)),
        ("ref_slot", vm::Expr::Int(0)),
        ("generation", vm::Expr::Int(0)),
    ] {
        record = irw_call(
            IRW_SET,
            vec![record, vm::Expr::String(key.to_owned()), field],
        );
    }
    record
}

fn irw_type_error() -> vm::Stmt {
    irw_let(
        IRW_THROWN,
        irw_call(IRW_TYPE_ERROR, vec![irw_var(IRW_CARRIER)]),
    )
}

fn irw_publish(plan: &IrwPlan) -> vm::Stmt {
    irw_let(
        IRW_PUBLISHED,
        irw_call(
            IRW_PUBLISH,
            vec![
                irw_var(IRW_CARRIER),
                irw_var(IRW_ARGS_BASE),
                irw_var(IRW_ARGS_BASE + 1),
                irw_var(IRW_ARGS_BASE + 2),
                irw_var(IRW_FRAME),
                vm::Expr::Int(plan.publish_entry),
            ],
        ),
    )
}

fn irw_publish_gate(plan: &IrwPlan) -> vm::Stmt {
    vm::Stmt::IfElse {
        condition: vm::Expr::Bool(true),
        then_branch: vec![
            irw_publish(plan),
            irw_assign(plan.output, irw_var(IRW_PUBLISHED)),
            irw_root_write_let(IRW_ACK, plan.publish_entry, irw_var(plan.output)),
        ],
        else_branch: Vec::new(),
        line: 0,
    }
}

fn irw_input(plan: &IrwPlan) -> vm::Stmt {
    irw_primitive_let(
        plan.input,
        vm::Expr::Block {
            stmts: vec![
                irw_primitive_let(plan.output, irw_call(IRW_UNDEFINED, Vec::new())),
                irw_publish_gate(plan),
            ],
            expr: Box::new(irw_var(plan.output)),
        },
    )
}

fn irw_arm(stmts: Vec<vm::Stmt>, tail: u16) -> vm::Expr {
    vm::Expr::Block {
        stmts,
        expr: Box::new(irw_var(tail)),
    }
}

fn irw_err_arm(plan: &IrwPlan) -> vm::Expr {
    irw_arm(
        vec![
            irw_assign(plan.pending, irw_var(IRW_ERR)),
            irw_root_write_let(IRW_ACK, plan.pending_entry, irw_var(plan.pending)),
            irw_let(IRW_ACK + 1, irw_completion(irw_var(plan.pending))),
        ],
        IRW_ACK + 1,
    )
}

fn irw_result(plan: &IrwPlan) -> vm::Stmt {
    irw_assign(
        plan.pending,
        vm::Expr::ResultMatch {
            value: Box::new(irw_var(IRW_SCRUTINEE)),
            value_slot: IRW_SCRUTINEE,
            ok_binding: Some(IRW_OK),
            ok_expr: Box::new(irw_arm(Vec::new(), IRW_OK)),
            err_binding: Some(IRW_ERR),
            err_expr: Box::new(irw_err_arm(plan)),
            scrutinee_schema: vm::TypeSchema::Unknown,
            span: vm::Span::new(0, 0, 0),
            semantic_id: None,
        },
    )
}

fn irw_owner(plan: &IrwPlan) -> vm::Stmt {
    irw_primitive_let(
        plan.owner,
        vm::Expr::Block {
            stmts: vec![
                irw_type_error(),
                irw_input(plan),
                irw_root_write_let(IRW_ACK, plan.copy_entry, irw_var(plan.input)),
                irw_result(plan),
            ],
            expr: Box::new(irw_var(plan.owner)),
        },
    )
}

fn irw_nested_owner(plan: &IrwPlan) -> vm::Stmt {
    irw_primitive_let(
        plan.owner,
        vm::Expr::Block {
            stmts: vec![
                irw_type_error(),
                vm::Stmt::IfElse {
                    condition: vm::Expr::Bool(true),
                    then_branch: vec![
                        irw_input(plan),
                        irw_root_write_let(IRW_ACK, plan.copy_entry, irw_var(plan.input)),
                    ],
                    else_branch: Vec::new(),
                    line: 0,
                },
                irw_result(plan),
            ],
            expr: Box::new(irw_var(plan.owner)),
        },
    )
}

fn irw_post(plan: &IrwPlan, discarded_root: bool, clears: &[i64]) -> Vec<vm::Stmt> {
    let mut stmts = Vec::new();
    if discarded_root {
        stmts.push(irw_root_write_let(
            IRW_ACK,
            plan.discarded_entry,
            irw_var(plan.owner),
        ));
    }
    for entry in clears {
        stmts.push(irw_clear(IRW_ACK, *entry));
    }
    stmts
}

fn irw_program(plan: &IrwPlan, post_owner: Vec<vm::Stmt>) -> Vec<vm::Stmt> {
    let mut stmts = vec![irw_owner(plan)];
    stmts.extend(post_owner);
    stmts
}

fn irw_reject(stmts: &[vm::Stmt], spec: &InitializerRootSpec) -> String {
    let witness = initializer_root_witness(stmts, spec).expect("control witness shape");
    assert_initializer_roots_retired(&witness).expect_err("control must be rejected")
}

#[test]
fn primitive_key151_initializer_retirement_witness_controls() {
    let spec = irw_spec();
    let base = irw_plan();
    let clears = [base.publish_entry, base.copy_entry, base.discarded_entry];

    let accepted = irw_program(&base, irw_post(&base, true, &clears));
    let witness = initializer_root_witness(&accepted, &spec).expect("control witness shape");
    assert_initializer_roots_retired(&witness).expect("common clears retire every required root");

    let mut nested_program = vec![irw_nested_owner(&base)];
    nested_program.extend(irw_post(&base, true, &clears));
    let nested_witness =
        initializer_root_witness(&nested_program, &spec).expect("nested key control shape");
    assert_initializer_roots_retired(&nested_witness)
        .expect("nested key evaluation retires every required root");

    let missing_producer = irw_program(
        &base,
        irw_post(&base, true, &[base.copy_entry, base.discarded_entry]),
    );
    let error = irw_reject(&missing_producer, &spec);
    assert!(
        error.starts_with("KNOWN_THROW_DECL_ROOTS_NOT_RETIRED"),
        "{error}"
    );
    assert!(error.contains("fresh_key_publication"), "{error}");
    assert!(
        error.contains(&format!("entry {}", base.publish_entry)),
        "{error}"
    );

    let missing_copy = irw_program(
        &base,
        irw_post(&base, true, &[base.publish_entry, base.discarded_entry]),
    );
    let error = irw_reject(&missing_copy, &spec);
    assert!(error.contains("key_evaluation_copy"), "{error}");

    let mut branch_only = irw_post(&base, true, &[base.copy_entry, base.discarded_entry]);
    branch_only.push(vm::Stmt::IfElse {
        condition: vm::Expr::Bool(true),
        then_branch: vec![irw_clear(IRW_ACK, base.publish_entry)],
        else_branch: Vec::new(),
        line: 0,
    });
    let error = irw_reject(&irw_program(&base, branch_only), &spec);
    assert!(error.contains("fresh_key_publication"), "{error}");

    let mut wrong_frame = irw_post(&base, true, &[base.copy_entry, base.discarded_entry]);
    wrong_frame.push(irw_clear_at(IRW_FRAME + 1, IRW_ACK, base.publish_entry));
    let error = irw_reject(&irw_program(&base, wrong_frame), &spec);
    assert!(error.contains("fresh_key_publication"), "{error}");

    let mut pending_cleared = irw_post(&base, true, &clears);
    pending_cleared.push(irw_clear(IRW_ACK, base.pending_entry));
    let error = irw_reject(&irw_program(&base, pending_cleared), &spec);
    assert!(
        error.starts_with("INITIALIZER_PENDING_ROOT_CLEARED"),
        "{error}"
    );
    assert!(
        !error.starts_with("KNOWN_THROW_DECL_ROOTS_NOT_RETIRED"),
        "{error}"
    );

    let discarded_unretired = irw_program(
        &base,
        irw_post(&base, true, &[base.publish_entry, base.copy_entry]),
    );
    let error = irw_reject(&discarded_unretired, &spec);
    assert!(error.contains("discarded_destination_temp"), "{error}");

    let other = irw_other_plan();
    let other_clears = [other.publish_entry, other.copy_entry, other.discarded_entry];
    let other_program = irw_program(&other, irw_post(&other, true, &other_clears));
    let other_witness =
        initializer_root_witness(&other_program, &spec).expect("second witness shape");
    assert_initializer_roots_retired(&other_witness).expect("identities are not fixture constants");
    assert_ne!(other_witness.publish_entry, base.publish_entry);
    assert_eq!(other_witness.pending_entry, other.pending_entry);

    let mut record_clear = irw_post(&base, true, &[base.publish_entry, base.discarded_entry]);
    record_clear.push(irw_let(
        IRW_ACK,
        irw_root_write(base.copy_entry, irw_undefined_record()),
    ));
    let record_program = irw_program(&base, record_clear);
    let record_witness =
        initializer_root_witness(&record_program, &spec).expect("record clear shape");
    assert_initializer_roots_retired(&record_witness).expect("the undefined record is a clear");

    let mut unknown_clear = irw_post(&base, true, &[base.copy_entry, base.discarded_entry]);
    unknown_clear.push(irw_let(
        IRW_ACK,
        irw_root_write(base.publish_entry, irw_call(IRW_OTHER, Vec::new())),
    ));
    let error = initializer_root_witness(&irw_program(&base, unknown_clear), &spec)
        .expect_err("an unknown value shape is not a clear");
    assert!(error.contains("unknown"), "{error}");
}

// ---------------------------------------------------------------------------
// P6-C/P6-D control-flow closure: loops, labelled abrupt completion, switch.
//
// Each test is one self-contained ES5 script whose final binding `m` is a bit
// mask; every bit is a distinct pinned (MicroQuickJS 6d4d7eb6) observation.
// The scripts were first run through the pinned `mqjs` and through the
// certified file pipeline, and the masks match. `do...while` semantics:
// `continue` inside a do-while still evaluates the test, so the next
// iteration runs (unlike `for`, where `continue` jumps to the update).
// ---------------------------------------------------------------------------

/// while/do-while entry, `for(;;)` exit, and `continue` on every loop form.
/// Mask bits: 1 while(false) never runs; 2 do-while(false) runs once;
/// 4 while accumulates 0,1,2; 8 `for(;;)` exits via break after 3;
/// 16 for-continue still runs the update (023); 32 post-loop n is 4;
/// 64 while-continue keeps the counter (134); 128 do-continue keeps 134;
/// 256 do-while runs its fourth iteration after the continue.
#[test]
fn while_do_and_for_entry_and_continue_semantics() {
    let source = "var m = 0;\
        var w = 0; while (false) { w++; } if (w === 0) m = m + 1;\
        var u = 0; do { u++; } while (false); if (u === 1) m = m + 2;\
        var w2 = 0, i2 = 0; while (i2 < 3) { w2 = w2 * 10 + i2; i2++; } if (w2 === 12) m = m + 4;\
        var a = 0; for (;;) { a++; if (a >= 3) break; } if (a === 3) m = m + 8;\
        var s = 0, n; for (n = 0; n < 4; n++) { if (n === 1) continue; s = s * 10 + n; } if (s === 23) m = m + 16;\
        if (n === 4) m = m + 32;\
        var k = 0, ws = 0; while (k < 4) { k++; if (k === 2) continue; ws = ws * 10 + k; } if (ws === 134) m = m + 64;\
        var d = 0, ds = 0; do { d++; if (d === 2) continue; ds = ds * 10 + d; } while (d < 4); if (ds === 134) m = m + 128;\
        if (d === 4) m = m + 256;\
        m;";
    assert_number(&read(source, "m"), 511.0);
}

/// Unlabelled `break`/`continue` only reach the innermost loop: the outer
/// loop keeps iterating and the inner loop restarts each time.
/// Mask bits: 512 three outer iterations each breaking the inner loop on j=1
/// (sequence 1,2,3); 1024 the six visited (i,j) pairs encode 134679.
#[test]
fn nested_unlabeled_break_and_continue_stay_innermost() {
    let source = "var m = 0;\
        var t = 0, i, j; for (i = 0; i < 3; i++) { for (j = 0; j < 3; j++) { if (j === 1) break; t = t * 10 + (i + 1); } } if (t === 123) m = m + 512;\
        var t2 = 0; for (i = 0; i < 3; i++) { for (j = 0; j < 3; j++) { if (j === 1) continue; t2 = t2 * 10 + (i * 3 + j + 1); } } if (t2 === 134679) m = m + 1024;\
        m;";
    assert_number(&read(source, "m"), 1536.0);
}

/// Labelled jumps: `continue outer` runs the outer `for` update (3 visits,
/// no repeated i), `break outer` leaves both loops after one inner visit,
/// `continue` from inside a switch reaches the outer loop, a labelled block
/// accepts `break`, and `while`/`do` labels keep their own iteration rules
/// (do-while continue evaluates the test and runs the next body).
/// Mask bits 1..128 all set; expected 255.
#[test]
fn labeled_continue_and_break_target_outer_loop() {
    let source = "var m = 0;\
        var s3 = 0, i, j; outer: for (i = 0; i < 3; i++) { for (j = 0; j < 3; j++) { if (j === 1) continue outer; s3 = s3 * 10 + (i + 1); } } if (s3 === 123) m = m + 1;\
        var s4 = 0; outer2: for (i = 0; i < 3; i++) { for (j = 0; j < 3; j++) { if (j === 1) break outer2; s4 = s4 * 10 + (i + 1); } } if (s4 === 1) m = m + 2;\
        var s5 = 0, c5 = 0, i5; outer3: for (i5 = 0; i5 < 4; i5++) { switch (i5) { case 1: continue outer3; default: s5 = s5 + i5; c5++; } } if (s5 === 5 && c5 === 3) m = m + 4;\
        var b7 = 0; lbl7: { b7 = 1; break lbl7; b7 = 2; } if (b7 === 1) m = m + 8;\
        var count = 0; outer4: for (i = 0; i < 3; i++) { count++; continue outer4; } if (count === 3 && i === 3) m = m + 16;\
        var s8 = 0, c8 = 0, i8, j8; a8: for (i8 = 0; i8 < 2; i8++) { b8: for (j8 = 0; j8 < 3; j8++) { if (j8 === 1) break b8; s8 = s8 * 100 + (i8 * 10 + j8 + 1); } c8++; } if (s8 === 111 && c8 === 2) m = m + 32;\
        var wl = 0, iw = 0; wloop: while (iw < 3) { iw++; wl = wl * 10 + iw; continue wloop; } if (wl === 123 && iw === 3) m = m + 64;\
        var dl = 0, id = 0; dloop: do { id++; if (id === 1) continue dloop; dl = 99; } while (id < 2); if (dl === 99 && id === 2) m = m + 128;\
        m;";
    assert_number(&read(source, "m"), 255.0);
}

/// switch selection and fallthrough: `case 1` falls into `case 2` before
/// break, `default` in the middle keeps executing later cases when nothing
/// matches (9 -> "dc"), `break` exits only the switch, `case` selectors run
/// in source order only until the first strict match, an unmatched switch
/// without default is a no-op, and loop `continue`/`break` cross the switch
/// boundary in their own directions. Mask bits 1..1024 all set; expected 2047.
#[test]
fn switch_fallthrough_default_and_loop_interplay() {
    let source = "var m = 0;\
        function sw(v) { 'use strict'; var r = 0; switch (v) { case 1: r = r * 10 + 1; case 2: r = r * 10 + 2; break; default: r = r * 10 + 4; case 3: r = r * 10 + 3; } return r; }\
        if (sw(1) === 12) m = m + 1;\
        if (sw(2) === 2) m = m + 2;\
        if (sw(3) === 3) m = m + 4;\
        if (sw(9) === 43) m = m + 8;\
        var n = 0; switch (2) { case (n = n * 10 + 1): break; case (n = n * 10 + 2) - 10: break; case (n = n * 10 + 3): break; } if (n === 12) m = m + 16;\
        var nd = 7; switch (5) { case 1: nd = 1; break; } if (nd === 7) m = m + 32;\
        var nb = 0, q; switch (1) { case 1: for (q = 0; q < 3; q++) { if (q === 1) break; nb = nb * 10 + q; } nb = nb * 10 + 8; break; } if (nb === 8) m = m + 64;\
        var after = 0; switch (1) { case 1: after = 1; break; default: after = 2; } after = after * 10 + 5; if (after === 15) m = m + 128;\
        var ft = 0; switch (0) { case 0: ft = ft * 10 + 1; default: ft = ft * 10 + 4; case 1: ft = ft * 10 + 2; break; case 2: ft = ft * 10 + 3; } if (ft === 142) m = m + 256;\
        var cu = 0, ic; for (ic = 0; ic < 4; ic++) { switch (ic) { case 1: continue; } cu++; } if (cu === 3 && ic === 4) m = m + 512;\
        var cb = 0; for (ic = 0; ic < 3; ic++) { switch (ic) { case 1: break; } cb++; } if (cb === 3 && ic === 3) m = m + 1024;\
        m;";
    assert_number(&read(source, "m"), 2047.0);
}

/// `case` matching is ES5 strict equality: "1" and 1 differ, null and
/// undefined differ, object identity is required, NaN never matches (not
/// even itself), -0 matches 0, and true/"1"/1 stay distinct. Later case
/// selectors never run after a match. Mask bits 1..2048 all set; expected 4095.
#[test]
fn switch_case_matching_is_strict_equality() {
    let source = "var m = 0;\
        function eq(v) { 'use strict'; var r = 0; switch (v) { case \"1\": r = 1; break; case 1: r = 2; break; case null: r = 3; break; default: r = 9; } return r; }\
        if (eq(\"1\") === 1) m = m + 1;\
        if (eq(1) === 2) m = m + 2;\
        if (eq(null) === 3) m = m + 4;\
        if (eq(undefined) === 9) m = m + 8;\
        var o1 = {}, o2 = {}, ro = 0; switch (o1) { case o2: ro = 1; break; case o1: ro = 2; break; default: ro = 9; } if (ro === 2) m = m + 16;\
        switch (o2) { case o1: ro = 1; break; case o2: ro = 2; break; default: ro = 9; } if (ro === 2) m = m + 32;\
        var rn = 0; switch (NaN) { case NaN: rn = 7; break; default: rn = 9; } if (rn === 9) m = m + 64;\
        var rz = 0; switch (-0) { case 0: rz = 1; break; default: rz = 9; } if (rz === 1) m = m + 128;\
        var rt = 0; switch (true) { case true: rt = 1; break; case \"1\": rt = 2; break; case 1: rt = 3; break; default: rt = 9; } if (rt === 1) m = m + 256;\
        var rs = 0; switch (\"1\") { case true: rs = 1; break; case \"1\": rs = 2; break; case 1: rs = 3; break; default: rs = 9; } if (rs === 2) m = m + 512;\
        var rq = 0; switch (1) { case true: rq = 1; break; case \"1\": rq = 2; break; case 1: rq = 3; break; default: rq = 9; } if (rq === 3) m = m + 1024;\
        var seen = 0; switch (1) { case (seen = seen * 10 + 1): break; case (seen = seen * 10 + 2): break; case (seen = seen * 10 + 3): break; } if (seen === 1) m = m + 2048;\
        m;";
    assert_number(&read(source, "m"), 4095.0);
}
