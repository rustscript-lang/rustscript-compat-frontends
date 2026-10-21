//! P5-A production expressions; oracle rows are original ES5 source, not IR.
use pd_vm_compat_frontends::{
    JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor, reference_array_effective_catalog,
};
use vm::{HostFunctionRegistry, Value, VmStatus};
// Reuse only the existing test-only observer, never an owning test target.
#[path = "javascript_es5/arity_observer.rs"]
mod observer;

fn field(value: &Value, key: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed result: {value:?}")
    };
    map.get(&Value::string(key)).expect("box field").clone()
}

fn execute(source: &str, native: bool, observation: Option<observer::Observer>) -> Vec<Value> {
    let start = std::time::Instant::now();
    let catalog = reference_array_effective_catalog();
    let mut options = vm::CompileSourceFileOptions::new().with_host_api_catalog(catalog.clone());
    let flavor = if let Some(observation) = observation {
        options = options.with_source_plugin(Box::leak(Box::new(observation)));
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-arity-observe"))
    } else {
        options = options.with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
        javascript_es5_flavor()
    };
    eprintln!("P5_PHASE source-compile source={source:?}");
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .unwrap_or_else(|error| panic!("compile {source:?}: {error:?}"));
    let mut machine = compiled.into_vm().expect("source VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("realm init");
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
    eprintln!(
        "P5_PHASE source-compiled seconds={}",
        start.elapsed().as_secs_f64()
    );
    if native {
        let phase = std::time::Instant::now();
        eprintln!("P5_PHASE native-compile");
        machine.compile_aot().expect("native compile");
        eprintln!(
            "P5_PHASE native-compiled seconds={}",
            phase.elapsed().as_secs_f64()
        );
    }
    let phase = std::time::Instant::now();
    assert_eq!(
        machine.run().unwrap_or_else(|e| panic!(
            "run {source:?}: {e:?}; ip={}; stack={:?}; frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        )),
        VmStatus::Halted
    );
    eprintln!(
        "P5_PHASE executed seconds={}",
        phase.elapsed().as_secs_f64()
    );
    if native {
        let count = machine.aot_exec_count();
        let info = machine.dump_aot_info();
        eprintln!("P5_OPERATOR_NATIVE actual_vm_aot_exec_count={count}\n{info}");
        assert!(count > 0 && info.contains("lowering=ssa"), "{info}");
    }
    let stack = machine.stack().to_vec();
    let resources = machine.host_context().resource_count();
    if resources > 0 {
        assert_eq!(resources, 1);
        let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
            .expect("owner stats");
        assert_eq!((stats.pending, stats.registered), (0, 1));
    }
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    eprintln!("P5_RESET resources=0 owner_directory=absent");
    stack
}
fn run(source: &str) -> Value {
    execute(source, false, None)
        .last()
        .expect("source result")
        .clone()
}
fn assert_bool(value: &Value, expected: bool) {
    assert_eq!(field(value, "tag"), Value::Int(2));
    assert_eq!(field(value, "flag"), Value::Bool(expected));
}
fn assert_number(value: &Value, expected: f64) {
    assert_eq!(field(value, "tag"), Value::Int(3));
    let Value::Float(actual) = field(value, "number") else {
        panic!("Float payload")
    };
    if expected.is_nan() {
        assert!(actual.is_nan());
    } else {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
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
    eprintln!("P5_PROFILE_REFUSAL source={source:?} error={error}");
}

#[test]
fn original_source_vectors_interpreted() {
    for line in include_str!("fixtures/javascript_es5/operators.jsonl").lines() {
        let row: serde_json::Value = serde_json::from_str(line).expect("source vector");
        let source = row["source"].as_str().expect("source");
        let actual = run(source);
        let expected = &row["expect"];
        if let Some(b) = expected.as_bool() {
            assert_bool(&actual, b);
        } else if let Some(n) = expected.as_f64() {
            assert_number(&actual, n);
        } else {
            assert_number(
                &actual,
                match expected.as_str().expect("special number") {
                    "-0" => -0.0,
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    other => panic!("unknown encoding {other}"),
                },
            );
        }
        eprintln!("P5_VECTOR id={} actual={actual:?}", row["id"]);
    }
}

#[test]
fn conditional_binding_kinds_are_conservative_in_all_source_positions() {
    for source in [
        "var x=1; false && (x=false); x+1;",
        "var x=1; true || (x=false); x+1;",
        "function f(){'use strict';var x=1; false && (x=false); x+1;} f();",
        "function f(){'use strict';return;var x=1; false && (x=false); x+1;} f();",
    ] {
        refuse(source, "requires numeric operands");
    }
    refuse(
        "var f; false && (f=function(){'use strict';}); f();",
        "require a function binding",
    );
    refuse("var o; false && (o={}); o.a;", "require an object receiver");
}

#[test]
fn selected_original_box_is_not_reconstructed() {
    let values = execute(
        "var a=-0;var r=a&&7;",
        false,
        Some(observer::Observer {
            reads: vec!["a".into(), "r".into()],
            ..Default::default()
        }),
    );
    let [Value::Map(a), Value::Map(r)] = &values[values.len() - 2..] else {
        panic!("original maps")
    };
    assert!(
        std::sync::Arc::ptr_eq(a, r),
        "selected left keeps its original boxed map"
    );
    let values = execute(
        "var a={s:'live'};var r=a||{};",
        false,
        Some(observer::Observer {
            reads: vec!["a".into(), "r".into()],
            force_gc: true,
            collect: true,
            ..Default::default()
        }),
    );
    let [Value::Map(a), Value::Map(r)] = &values[values.len() - 2..] else {
        panic!("original maps")
    };
    assert!(
        std::sync::Arc::ptr_eq(a, r),
        "managed selected left keeps its original boxed map"
    );
}

#[test]
fn conditional_array_lengths_never_become_false_exact_bounds() {
    for source in [
        "var a=[1]; false && (a=[]); a[0];",
        "var a=[]; true || (a=[7]); a[0];",
        "var a=[1]; false && (a=[]); a[0]=7;",
        "function f(){'use strict';return;var a=[]; false && (a=[7]); a[0];} f();",
    ] {
        refuse(source, "requires an exact length");
    }
    assert_number(&run("var a=[1]; false && (a=[7]); a[0];"), 1.0);
    assert_number(&run("var a=[1]; true && (a=[7]); a[0];"), 7.0);
    assert_bool(&run("var a=[1]; false && (a=[]); !a;"), false);
}

#[test]
fn syntactic_rhs_is_validated_even_when_skipped_or_post_return() {
    for source in [
        "false && missing;",
        "true || missing;",
        "!missing;",
        "function f(){'use strict';return;false && missing;} f();",
        "if(false){var x=true || missing;}",
    ] {
        refuse(source, "unknown local 'missing'");
    }
    for source in [
        "false && (1%2);",
        "true || (1%2);",
        "function f(){'use strict';return;false && (1%2);} f();",
    ] {
        refuse(source, "binary operator");
    }
    refuse("1 ? 2 : 3;", "primitive expression");
    // P6-A admits this runtime if. Completion and branch behavior are in
    // javascript_es5_control_flow_tests; this only retires the obsolete refusal.
    vm::compile_source_with_flavor_and_options(
        "if(1){var x=1;}",
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
            .with_host_api_catalog(reference_array_effective_catalog()),
    )
    .expect("runtime if is admitted");
    refuse("false && (1=='1');", "binary operator");
}

#[test]
fn scalar_logical_sources_remain_heap_free() {
    // Construct and execute without a realm catalog or runtime initialization.
    let compiled = vm::compile_source_with_flavor_and_options(
        "var x=0; !(x=x+1) || (0 && 7);",
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new().with_source_plugin(&JAVASCRIPT_ES5_PLUGIN),
    )
    .expect("heap-free compile");
    let mut machine = vm::Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("heap-free execution"),
        VmStatus::Halted
    );
    assert_eq!(machine.host_context().resource_count(), 0);
    assert_number(machine.stack().last().expect("result"), 0.0);
}

fn native_worker(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(f)
        .expect("native worker")
        .join()
        .expect("native assertions");
}

#[test]
fn runtime_tags_and_selected_operands_native_ssa() {
    native_worker(|| {
        let source = "function n(x){'use strict';return !x;} function a(x,y){'use strict';return x&&y;} function o(x,y){'use strict';return x||y;} var bt=n(true);var bf=n(false);var nz=n(-0);var pz=n(0);var nan=n(0/0);var num=n(7);var es=n('');var ns=n('\\uD800\\0');var z=a(-0,7);var q=a(0/0,7);var t=o(0,true);var object={s:'live'};var r=a(object,object);var same=r===object;var x=0;false&&(x=1);true&&(x=2);var s='\\uD800\\0';var keep=s||'replacement';";
        let names = [
            "bt", "bf", "nz", "pz", "nan", "num", "es", "ns", "z", "q", "t", "same", "x",
        ];
        let values = execute(
            source,
            true,
            Some(observer::Observer {
                reads: names.into_iter().map(str::to_owned).collect(),
                string: Some(("keep".into(), 2)),
                force_gc: true,
                ..Default::default()
            }),
        );
        assert_eq!(
            &values[values.len() - 3..],
            &[Value::Float(2.0), Value::Float(55296.0), Value::Float(0.0)]
        );
        let tail = &values[values.len() - names.len() - 3..values.len() - 3];
        for (i, expected) in [false, true, true, true, true, false, true, false]
            .into_iter()
            .enumerate()
        {
            assert_bool(&tail[i], expected);
        }
        assert_number(&tail[8], -0.0);
        assert_number(&tail[9], f64::NAN);
        assert_bool(&tail[10], true);
        assert_bool(&tail[11], true);
        assert_number(&tail[12], 2.0);
    });
}

#[test]
fn typeof_branch_cache_utf16_and_return_alias_survive_forced_collection() {
    for source in [
        "var x=1;false&&typeof x;var s=typeof x;",
        "var x=1;true||typeof x;var s=typeof x;",
        "var x=1;false&&(true&&typeof x);var s=typeof x;",
        "var x=1;var s=true&&(false||typeof x);typeof x;",
        "var x=1;var s=true&&typeof x;typeof x;",
        "function f(x){'use strict';var r=true&&typeof x;typeof x;return r;}var s=f(1);",
    ] {
        let values = execute(
            source,
            false,
            Some(observer::Observer {
                string: Some(("s".into(), 6)),
                force_gc: true,
                collect: true,
                ..Default::default()
            }),
        );
        let expected: Vec<_> = std::iter::once(Value::Float(6.0))
            .chain("number".encode_utf16().map(|u| Value::Float(f64::from(u))))
            .collect();
        assert_eq!(&values[values.len() - 7..], expected.as_slice());
    }
}

#[test]
fn selected_utf16_payload_is_original() {
    let values = execute(
        "var original='\\uD800\\0';var s=original||'unused';original=null;",
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
}

#[test]
fn skipped_rhs_payload_allocations_and_type_name_cache_do_not_run() {
    let stats = |source| {
        let values = execute(
            source,
            false,
            Some(observer::Observer {
                stats: true,
                ..Default::default()
            }),
        );
        values[values.len() - 3..].to_vec()
    };
    let baseline = stats("var s='anchor';");
    assert_eq!(stats("var s='anchor';false&&'unused';"), baseline);
    assert_eq!(stats("var s='anchor';true||'unused';"), baseline);
    assert_eq!(stats("var s='anchor';false&&typeof s;"), baseline);
}

#[test]
fn selected_transitive_aliases_survive_and_last_alias_release_reclaims() {
    let source = "function f(a){'use strict';return a&&a;}var original={child:{s:'payload'}};var r=f(original);original=null;var junk={s:'pressure'};junk=null;var same=r.child.s==='payload';";
    let values = execute(
        source,
        false,
        Some(observer::Observer {
            reads: vec!["same".into()],
            force_gc: true,
            collect: true,
            stats: true,
            ..Default::default()
        }),
    );
    assert_bool(&values[values.len() - 4], true);
    let live = values[values.len() - 3].clone();
    let released = execute(
        "function f(a){'use strict';return a&&a;}var original={child:{s:'payload'}};var r=f(original);original=null;r=null;",
        false,
        Some(observer::Observer {
            force_gc: true,
            collect: true,
            stats: true,
            ..Default::default()
        }),
    );
    assert!(matches!((live,&released[released.len()-3]),(Value::Int(a),Value::Int(b)) if a>*b));
}
