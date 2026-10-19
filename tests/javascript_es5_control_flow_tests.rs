//! P6-A coherent conditional statements and conditional returns.
//! Expectations follow ES5.1 12.1–12.5 and 14 UpdateEmpty, not a reconstructed value.
use pd_vm_compat_frontends::{
    JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor, reference_array_effective_catalog,
};
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
    let catalog = reference_array_effective_catalog();
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
    registry.allow_builtin("math::floor").expect("floor");
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
        .with_host_api_catalog(reference_array_effective_catalog());
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
    refuse("if(1){throw 1;}", "throw");
    refuse("if(0){while(1){}}", "unsupported");
    refuse("function f(){'use strict';return 1;throw 1;}", "throw");
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
        .with_host_api_catalog(reference_array_effective_catalog());
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
    let catalog = reference_array_effective_catalog();
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
