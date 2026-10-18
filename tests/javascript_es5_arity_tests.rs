//! Strict closed-source argument semantics through the production JS frontend.
use pd_vm_compat_frontends::{javascript_es5_flavor, reference_array_effective_catalog};
use vm::{HostFunctionRegistry, Value, VmStatus};
#[path = "javascript_es5/arity_observer.rs"]
mod observer;

fn source_activation(machine: &vm::Vm) -> Option<(u32, usize)> {
    machine.execution_frames().into_iter().rev().find_map(|frame| {
        let id=frame.prototype_id?;
        let prototype=&machine.program().callable_prototypes[id as usize];
        (prototype.arity==5 && matches!(&prototype.schema,Some(vm::TypeSchema::Callable {params,..}) if matches!(params.get(3),Some(vm::TypeSchema::Array(_)))))
            .then_some((id,frame.local_base))
    })
}

fn execute(source: &str, native: bool, observer: Option<observer::Observer>) -> Vec<Value> {
    let catalog = reference_array_effective_catalog();
    let mut options = vm::CompileSourceFileOptions::new().with_host_api_catalog(catalog.clone());
    let expected_vector = observer
        .as_ref()
        .and_then(|observer| observer.vector_numbers.clone());
    let flavor = if let Some(observer) = observer {
        options = options.with_source_plugin(Box::leak(Box::new(observer)));
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-arity-observe"))
    } else {
        options = options.with_source_plugin(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        javascript_es5_flavor()
    };
    eprintln!("P4_PHASE source-compile");
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    let mut machine = compiled.into_vm().expect("source VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("realm initialization");
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("array adapters");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owner adapters");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("snapshot adapters");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("math adapters");
    registry.allow_builtin("math::floor").expect("floor");
    registry.bind_vm_cached(&mut machine).expect("binding");
    machine.set_fuel(10_000_000);
    if native {
        eprintln!("P4_PHASE native-compile");
        machine.compile_aot().expect("native compilation");
        eprintln!("P4_PHASE native-compiled");
    }
    eprintln!("P4_PHASE execute");
    if let Some(expected) = expected_vector {
        let disassembly = vm::disassemble_program(machine.program());
        let instructions: std::collections::BTreeMap<usize, &str> = disassembly
            .lines()
            .filter_map(|line| {
                line.split_once('\t')
                    .and_then(|(ip, _)| ip.parse().ok().map(|ip| (ip, line)))
            })
            .collect();
        let mut argument_reads = 0;
        let mut returns = 0;
        let mut remaining = 10_000_000_u64;
        let mut samples = 0_usize;
        let mut source_ips = std::collections::BTreeSet::new();
        let mut native_first = None;
        let mut native_last = 0;
        loop {
            assert!(remaining > 0, "original total fuel budget");
            remaining -= 1;
            machine.set_fuel(1);
            let before_ip = machine.ip();
            let before_count = machine.aot_exec_count();
            let before_activation = source_activation(&machine);
            let before_source = before_activation.is_some_and(|(id, base)| {
                machine
                    .execution_frames()
                    .last()
                    .is_some_and(|frame| frame.prototype_id == Some(id) && frame.local_base == base)
            });
            let argument_index = if argument_reads == 0
                && before_source
                && instructions
                    .get(&before_ip)
                    .is_some_and(|line| line.contains("builtin get/2"))
            {
                match machine.stack() {
                    [.., Value::Array(values), Value::Int(index)]
                        if values.len() == expected.len() =>
                    {
                        Some(usize::try_from(*index).expect("nonnegative guarded ordinal"))
                    }
                    _ => None,
                }
            } else {
                None
            };
            let status = machine.run().unwrap_or_else(|error| {
                panic!(
                    "vector step: {error:?}; ip={}; frames={:?}; locals={:?}",
                    machine.ip(),
                    machine.execution_frames(),
                    machine.locals()
                )
            });
            if let Some(index) = argument_index {
                let value = machine.stack().last().expect("real Get result");
                assert_eq!(field(value, "tag"), Value::Int(3));
                assert_eq!(field(value, "number"), Value::Float(expected[index]));
                let after_count = machine.aot_exec_count();
                eprintln!(
                    "P4_ARGUMENT_READ ordinal={index} ip={before_ip} next_ip={} native_before={before_count} native_after={after_count} result={value:?}",
                    machine.ip()
                );
                if native {
                    assert!(
                        after_count > before_count,
                        "argument Get itself needs actual native execution"
                    );
                }
                argument_reads += 1;
            }
            // Native fuel checkpoints are resumable chunks, not guaranteed
            // single-op stops. Observe the real source activation leaving.
            let returning = before_activation.is_some_and(|(id, base)| {
                !machine
                    .execution_frames()
                    .iter()
                    .any(|frame| frame.prototype_id == Some(id) && frame.local_base == base)
            });
            if returning {
                let after_count = machine.aot_exec_count();
                eprintln!(
                    "P4_RETURN_BOUNDARY ip={before_ip} next_ip={} native_before={before_count} native_after={after_count}",
                    machine.ip()
                );
                if native {
                    assert!(
                        after_count > before_count,
                        "source return handoff needs actual native execution"
                    );
                }
                returns += 1;
            }
            let frames = machine.execution_frames();
            for frame in &frames {
                let Some(id) = frame.prototype_id else {
                    continue;
                };
                let prototype = &machine.program().callable_prototypes[id as usize];
                let Some(vm::TypeSchema::Callable { params, .. }) = &prototype.schema else {
                    continue;
                };
                if prototype.arity != 5 || !matches!(params.get(3), Some(vm::TypeSchema::Array(_)))
                {
                    continue;
                }
                let physical = frame.local_base + usize::from(prototype.parameter_slots[3]);
                let Value::Array(values) = &machine.locals()[physical] else {
                    continue;
                };
                assert_eq!(values.len(), expected.len(), "real param_slots[3] count");
                for (value, number) in values.iter().zip(&expected) {
                    assert_eq!(field(value, "tag"), Value::Int(3));
                    assert_eq!(field(value, "number"), Value::Float(*number));
                }
                if samples == 0 {
                    eprintln!(
                        "P4_VECTOR physical_slot={physical} count={} capacity={} element_bytes={} storage_bytes={} values={values:?}",
                        values.len(),
                        values.capacity(),
                        std::mem::size_of::<Value>(),
                        values.capacity() * std::mem::size_of::<Value>()
                    );
                }
                samples += 1;
                if frames.last().and_then(|frame| frame.prototype_id) == Some(id) {
                    source_ips.insert(machine.ip());
                    native_first.get_or_insert(machine.aot_exec_count());
                    native_last = machine.aot_exec_count();
                }
            }
            if status == VmStatus::Halted {
                break;
            }
            assert_eq!(status, VmStatus::Yielded);
            assert_eq!(machine.last_yield_reason(), Some(vm::VmYieldReason::Fuel));
        }
        assert!(
            samples > 0 && !source_ips.is_empty(),
            "actual source-frame boundary samples"
        );
        assert!(
            argument_reads > 0 && returns > 0,
            "real argument-read and return boundaries"
        );
        if native {
            assert!(native_last > native_first.expect("native source frame"));
        }
        eprintln!(
            "P4_VECTOR_BOUNDARY native={native} samples={samples} source_ips={source_ips:?} native_first={native_first:?} native_last={native_last}"
        );
        eprintln!("P4_VECTOR_DISASSEMBLY\n{disassembly}");
    } else {
        assert_eq!(
            machine.run().unwrap_or_else(|error| panic!(
                "run: {error:?}; ip={:?}; stack={:?}; frames={:?}; locals={:?}; prototypes={:?}",
                machine.ip(),
                machine.stack(),
                machine.execution_frames(),
                machine.locals(),
                machine.program().callable_prototypes
            )),
            VmStatus::Halted
        );
    }
    if native {
        let count = machine.aot_exec_count();
        let info = machine.dump_aot_info();
        eprintln!("P4_ARGUMENT_NATIVE count={count}\n{info}");
        assert!(count > 0 && info.contains("lowering=ssa"), "{info}");
    }
    let values = machine.stack().to_vec();
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "open realm before reset"
    );
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("live owner directory");
    assert_eq!((stats.pending, stats.registered), (0, 1));
    machine
        .reset_for_reuse()
        .expect("argument-call realm reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    eprintln!("P4_RESET resources=0 owner_directory=absent");
    values
}

fn run(source: &str, native: bool) -> Value {
    execute(source, native, None)
        .last()
        .expect("source result")
        .clone()
}
fn observed(source: &str, native: bool, observer: observer::Observer) -> Vec<Value> {
    execute(source, native, Some(observer))
}

#[test]
fn unused_extras_and_overwritten_parameters_retire_transitively() {
    let values = observed(
        "function f(a){\"use strict\";a=null;return;}var r=f({n:{s:\"old\"}},{s:\"extra\"});",
        false,
        observer::Observer {
            reads: vec!["r".into()],
            collect: true,
            stats: true,
            ..Default::default()
        },
    );
    assert_eq!(field(&values[values.len() - 4], "tag"), Value::Int(0));
    assert_eq!(
        &values[values.len() - 3..values.len() - 1],
        &[Value::Int(1), Value::Int(13)]
    );
    assert!(matches!(values.last(),Some(Value::Int(collections)) if *collections>0));
}

#[test]
fn function_length_boundary_native_ssa() {
    let value = run(
        "function f(a,b){\"use strict\";return a;}f[\"length\"];",
        true,
    );
    assert_eq!(field(&value, "tag"), Value::Int(3));
    assert_eq!(field(&value, "number"), Value::Float(2.0));
}

#[test]
fn dynamic_scalar_arguments_native_ssa() {
    let values = observed(
        "function f(a){\"use strict\";return a;}var t=f(true);var b=f(false);var z=f(-0);var i=f(1e309);var u=f();",
        true,
        observer::Observer {
            reads: ["t", "b", "z", "i", "u"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            ..Default::default()
        },
    );
    let tail = &values[values.len() - 5..];
    assert_eq!(field(&tail[0], "flag"), Value::Bool(true));
    assert_eq!(field(&tail[1], "flag"), Value::Bool(false));
    assert_eq!(field(&tail[0], "tag"), Value::Int(2));
    assert_eq!(field(&tail[1], "tag"), Value::Int(2));
    let Value::Float(z) = field(&tail[2], "number") else {
        panic!("Float")
    };
    assert_eq!(z.to_bits(), (-0.0f64).to_bits());
    assert_eq!(field(&tail[3], "number"), Value::Float(f64::INFINITY));
    assert_eq!(field(&tail[4], "tag"), Value::Int(0));
}

#[test]
fn once_only_actuals_leave_real_source_state() {
    let values = observed(
        "var n=1;function f(a,b){\"use strict\";return a;}var r=f(n=n+1,n=n*10);",
        false,
        observer::Observer {
            reads: vec!["r".into(), "n".into()],
            ..Default::default()
        },
    );
    assert_eq!(
        field(&values[values.len() - 2], "number"),
        Value::Float(2.0)
    );
    assert_eq!(
        field(&values[values.len() - 1], "number"),
        Value::Float(20.0)
    );
}

#[test]
fn physical_vector_retains_every_extra_actual() {
    let values = observed(
        "function f(a){\"use strict\";return a;}var r=f(7,9,11);",
        false,
        observer::Observer {
            reads: vec!["r".into()],
            vector_numbers: Some(vec![7.0, 9.0, 11.0]),
            ..Default::default()
        },
    );
    assert_eq!(field(values.last().unwrap(), "number"), Value::Float(7.0));
}

fn physical_effects_case(native: bool) {
    let values = observed(
        "var n=1;function f(a){\"use strict\";return a;}var r=f(n=n+1,n=n*10,11);",
        native,
        observer::Observer {
            reads: vec!["r".into(), "n".into()],
            vector_numbers: Some(vec![2.0, 20.0, 11.0]),
            ..Default::default()
        },
    );
    assert_eq!(
        field(&values[values.len() - 2], "number"),
        Value::Float(2.0)
    );
    assert_eq!(
        field(&values[values.len() - 1], "number"),
        Value::Float(20.0)
    );
}

#[test]
fn physical_vector_effects_and_state() {
    physical_effects_case(false);
}

#[test]
fn physical_vector_boundary_native_ssa() {
    physical_effects_case(true);
}

fn saved_call_values(native: bool) {
    let values = observed(
        "var f=function(a,b){\"use strict\";return b;};var r=f(f=null,7);var o={n:7,m:function(a){\"use strict\";return this.n;}};var s=o.m(o=null);",
        native,
        observer::Observer {
            reads: ["r", "f", "s", "o"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            ..Default::default()
        },
    );
    let values = &values[values.len() - 4..];
    assert_eq!(field(&values[0], "number"), Value::Float(7.0));
    assert_eq!(field(&values[1], "tag"), Value::Int(1));
    assert_eq!(field(&values[2], "number"), Value::Float(7.0));
    assert_eq!(field(&values[3], "tag"), Value::Int(1));
}

#[test]
fn callee_receiver_actual_effects_leave_null_bindings() {
    saved_call_values(false);
}

#[test]
fn callee_receiver_argument_boundary_native_ssa() {
    saved_call_values(true);
}

#[test]
fn missing_argument_boundary_native_ssa() {
    let value = run("function f(a,b){\"use strict\";return b;}f(7);", true);
    assert_eq!(field(&value, "tag"), Value::Int(0));
}

#[test]
fn parameters_copy_shadow_forward_and_cook_names() {
    assert_true("var a=19;function f(a){\"use strict\";a=9;return a;}var r=f(a);a===19;");
    assert_true("function f(\\u0061){\"use strict\";return a;}var g=f;g(7)===7;");
    assert_true("function f(a){\"use strict\";function g(b){return b;}return g(a);}f(7)===7;");
    assert_true("function f(a){\"use strict\";return typeof a;}f(false)===\"boolean\";");
}

#[test]
fn managed_identity_survives_argument_and_return_handoffs() {
    assert_true("function f(a){\"use strict\";var b=a;return b;}var o={n:7};var r=f(o);r===o;");
    assert_true("function f(a){\"use strict\";return a;}var a=[7,8];var r=f(a);r===a;");
    assert_true(
        "function f(a){\"use strict\";return a;}var g=function(x,y){\"use strict\";return x;};var r=f(g);r===g;",
    );
}

#[test]
fn actual_tags_and_number_bits_are_not_static_facts() {
    for (source, tag) in [
        ("function f(a){\"use strict\";return a;}f(null);", 1),
        ("function f(a){\"use strict\";return a;}f(false);", 2),
        ("function f(a){\"use strict\";return a;}f();", 0),
    ] {
        assert_eq!(field(&run(source, false), "tag"), Value::Int(tag));
    }
    let value = run("function f(a){\"use strict\";return a;}f(-0);", false);
    let Value::Float(n) = field(&value, "number") else {
        panic!("Float payload")
    };
    assert_eq!(n.to_bits(), (-0.0f64).to_bits());
    let value = run("function f(a){\"use strict\";return a;}f(1e309);", false);
    assert_eq!(field(&value, "number"), Value::Float(f64::INFINITY));
}

fn managed_pressure(native: bool) {
    let source = "function f(a,b){\"use strict\";var c={q:true,r:false};var t=\"later\";return a;}var r=f(\"\\uD800\\0\",{n:7});";
    let forwarded = "function f(a){\"use strict\";function g(x){var local=[9];return x;}return g(a);}var r=f(\"\\uD800\\0\");";
    for source in [source, forwarded] {
        let values = observed(
            source,
            native,
            observer::Observer {
                reads: vec!["r".into()],
                string: Some(("r".into(), 2)),
                collect: true,
                force_gc: true,
                ..Default::default()
            },
        );
        assert_eq!(
            &values[values.len() - 3..],
            &[Value::Float(2.0), Value::Float(55296.0), Value::Float(0.0)]
        );
        assert_eq!(field(&values[values.len() - 4], "tag"), Value::Int(4));
    }
}

#[test]
fn early_managed_actual_survives_later_actual_and_callee_collections() {
    managed_pressure(false);
}

#[test]
fn managed_argument_boundary_native_ssa() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| managed_pressure(true))
        .expect("native worker")
        .join()
        .expect("native assertions");
}

#[test]
fn aliased_and_returned_function_lengths_use_own_metadata() {
    assert_true("var f=function(a,b){\"use strict\";return a;};var g=f;g[\"length\"]===2;");
    assert_true(
        "function f(a){\"use strict\";return a;}var g=function(x,y,z){\"use strict\";return x;};var r=f(g,7);r.length===3;",
    );
    assert_true("function f(){\"use strict\";}f.length===0;");
    assert_true("var o={length:7};o.length===7;");
}

fn refuse(source: &str, expected: &str) -> String {
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(reference_array_effective_catalog());
    let text = match vm::compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        options,
    ) {
        Ok(_) => panic!("must refuse: {source}"),
        Err(e) => format!("{e:?}"),
    };
    assert!(text.contains(expected), "expected {expected}: {text}");
    text
}

#[test]
fn maximum64_formals_and_actuals_execute() {
    let params = (0..64)
        .map(|n| format!("p{n}"))
        .collect::<Vec<_>>()
        .join(",");
    let actuals = (0..64).map(|n| n.to_string()).collect::<Vec<_>>().join(",");
    let source = format!("function f({params}){{\"use strict\";return p63;}}f({actuals});");
    let values = observed(
        &source,
        false,
        observer::Observer {
            vector_numbers: Some((0..64).map(f64::from).collect()),
            ..Default::default()
        },
    );
    assert_eq!(field(values.last().unwrap(), "number"), Value::Float(63.0));
}

#[test]
fn count65_rejections_preserve_exact_source_spans() {
    let params = (0..65)
        .map(|n| format!("p{n}"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("function f({params}){{\"use strict\";return p0;}}");
    let text = refuse(&source, "64 formal");
    assert!(
        text.contains(&format!("lo: 0, hi: {}", source.len())),
        "{text}"
    );
    let actuals = vec!["1"; 65].join(",");
    let post_return =
        format!("function g(){{\"use strict\";function f(){{}}return 1;f({actuals});}}");
    refuse(&post_return, "64 actual");
    for prefix in ["", "if(false){var x="] {
        let source = format!(
            "function f(){{\"use strict\";}}{prefix}f({actuals});{}",
            if prefix.is_empty() { "" } else { "}" }
        );
        let text = refuse(&source, "64 actual");
        let lo = source.rfind("f(").unwrap();
        let hi = lo + actuals.len() + 3;
        assert!(text.contains(&format!("lo: {lo}, hi: {hi}")), "{text}");
    }
}

#[test]
fn pure_validation_visits_all_actuals_in_live_dead_and_post_return_code() {
    for body in ["var r=f(1,missing);", "if(false){var r=f(1,missing);}"] {
        refuse(
            &format!("function f(a){{\"use strict\";return a;}}{body}"),
            "unknown local 'missing'",
        );
    }
    refuse(
        "function g(){\"use strict\";function f(a){return a;}return 1;f(1,missing);}",
        "unknown local 'missing'",
    );
    for body in ["var r=f(n=null,n+1);", "if(false){var r=f(n=null,n+1);}"] {
        refuse(
            &format!("var n=1;function f(a,b){{\"use strict\";return a;}}{body}"),
            "numeric",
        );
    }
}

#[test]
fn function_property_calls_refuse_in_live_dead_and_post_return_code() {
    for source in [
        "function f(a){\"use strict\";return a;}f.prototype();",
        "function f(a){\"use strict\";return a;}if(false){f.prototype();}",
        "function g(){\"use strict\";function f(a){return a;}return 1;f.prototype();}",
    ] {
        refuse(source, "intrinsic length");
    }
}

#[test]
fn numeric_function_length_calls_refuse_in_all_validation_paths() {
    for source in [
        "function f(a){\"use strict\";return a;}f.length();",
        "function f(a){\"use strict\";return a;}if(false){f.length();}",
        "function g(){\"use strict\";function f(a){return a;}return 1;f.length();}",
    ] {
        refuse(source, "function binding");
    }
}

#[test]
fn ordinary_object_methods_named_length_and_prototype_remain_supported() {
    assert_true("var o={length:function(a){\"use strict\";return a;}};o.length(7)===7;");
    assert_true("var o={prototype:function(a){\"use strict\";return a;}};o.prototype(7)===7;");
}

#[test]
fn unknown_parameter_operations_and_later_capabilities_refuse() {
    for expression in ["a+1", "a.n", "a()"] {
        refuse(
            &format!("function f(a){{\"use strict\";return {expression};}}"),
            "P0",
        );
        refuse(
            &format!("function f(a){{\"use strict\";return 1;var r={expression};}}"),
            "P0",
        );
        refuse(
            &format!("function f(a){{\"use strict\";if(false){{var r={expression};}}return 1;}}"),
            "P0",
        );
    }
    for source in [
        "function f(a,a){\"use strict\";}",
        "function f(eval){\"use strict\";}",
        "function f(arguments){\"use strict\";}",
    ] {
        refuse(source, "strict");
    }
    refuse("function f(a){\"use strict\";return arguments;}", "unknown");
    refuse("var x=1;function f(a){\"use strict\";return x;}", "capture");
    refuse("function f(a){\"use strict\";}new f(1);", "unsupported");
    refuse(
        "function f(a){\"use strict\";}f.prototype;",
        "intrinsic length",
    );
}

fn field(value: &Value, key: &str) -> Value {
    let Value::Map(fields) = value else {
        panic!("boxed value: {value:?}");
    };
    fields.get(&Value::string(key)).expect("field").clone()
}

fn assert_true(source: &str) {
    let value = run(source, false);
    assert_eq!(field(&value, "tag"), Value::Int(2));
    assert_eq!(field(&value, "flag"), Value::Bool(true));
}

#[test]
fn function_length_is_source_formal_count() {
    let value = run("function f(a,b){\"use strict\";return a;}f.length;", false);
    assert_eq!(field(&value, "tag"), Value::Int(3));
    assert_eq!(field(&value, "number"), Value::Float(2.0));
}

#[test]
fn guarded_missing_and_excess_actuals() {
    assert_true("function f(a,b){\"use strict\";return a;}f()===undefined;");
    assert_true("function f(a,b){\"use strict\";return b;}f()===undefined;");
    assert_true("function f(a,b){\"use strict\";return b;}f(7)===undefined;");
    assert_true("function f(a,b){\"use strict\";return b;}f(7,9,11)===9;");
}

#[test]
fn parameter_declaration_instantiation_order() {
    assert_true("function f(a){\"use strict\";var a;return a;}f(7)===7;");
    assert_true("function f(a){\"use strict\";var a=9;return a;}f(7)===9;");
    assert_true(
        "function f(a){\"use strict\";function a(){return 9;}var a;return typeof a;}f(7)===\"function\";",
    );
    assert_true("function f(undefined){\"use strict\";return undefined;}f(7)===7;");
}

#[test]
fn callee_and_receiver_saved_before_actual_effects() {
    assert_true("var f=function(a,b){\"use strict\";return b;};var r=f(f=null,7);r===7;");
    assert_true(
        "var o={n:7,m:function(a){\"use strict\";return this.n;}};var r=o.m(o=null);r===7;",
    );
}

#[test]
fn scalar_argument_boundary_native_ssa() {
    let value = run(
        "function f(a,b){\"use strict\";return a;}var n=1;var r=f(n=n+1,n=n*10);r===2;",
        true,
    );
    assert_eq!(field(&value, "tag"), Value::Int(2));
    assert_eq!(field(&value, "flag"), Value::Bool(true));
}

#[test]
fn strict_formal_returns_real_actual() {
    let value = run("function f(a,b){\"use strict\";return a;}f(7,9);", false);
    assert_eq!(field(&value, "tag"), Value::Int(3));
    assert_eq!(field(&value, "number"), Value::Float(7.0));
}
