//! ES5 Math.random forwarding through a deterministic, exact host import.
use std::{collections::VecDeque, sync::Arc};

use pd_vm_compat_frontends::{
    javascript_es5_compile_options, javascript_es5_flavor,
    runtime::{array, cli::reference_array_cli_effective_catalog, scalar_error},
};
use vm::{
    CallOutcome, CallReturn, HostApiCatalog, HostFunctionRegistry, HostFunctionSchema,
    HostImportSchema, HostParamSchema, HostState, HostTypeSchema, StaticHostFunction, Value, Vm,
    VmError, VmResult, VmStatus, compiler::CertifiedCompilation,
};

const RANDOM: &str = "compat_math::random";
const MAX_UNIT_BITS: u64 = 0x3fef_ffff_ffff_ffff;
const HOST_ERROR: &str = "deterministic Math.random host failure";

struct RandomProbe {
    replies: VecDeque<VmResult<Value>>,
    calls: usize,
}

impl HostState for RandomProbe {
    const KEY: &'static str = "test.es5.math_random";

    fn initialize() -> Result<Self, String> {
        Err("random probe must be configured on this VM".into())
    }
}

fn next_reply(machine: &mut Vm, args: &[Value]) -> VmResult<Value> {
    assert!(args.is_empty(), "random host received guest arguments");
    let context = machine.host_context();
    let mut probe = context
        .host_state_mut::<RandomProbe>(RANDOM, "write")
        .expect("configured random probe");
    probe.calls += 1;
    probe.replies.pop_front().expect("unexpected random call")
}

fn numeric_host(machine: &mut Vm, args: &[Value]) -> VmResult<CallOutcome> {
    let value = next_reply(machine, args).expect("numeric host reply");
    assert!(matches!(value, Value::Float(_) | Value::Int(_)));
    Ok(CallOutcome::Return(CallReturn::one(value)))
}

fn fault_host(machine: &mut Vm, args: &[Value]) -> VmResult<CallOutcome> {
    // Deliberately bypass a generated adapter's return conversion: the
    // declared ABI is Number, but the fault must reach the real VM call.
    next_reply(machine, args).map(|value| CallOutcome::Return(CallReturn::one(value)))
}

fn random_schema(params: Vec<HostParamSchema>, result: HostTypeSchema) -> HostFunctionSchema {
    HostFunctionSchema::with_return(RANDOM, params, result)
}

fn catalog_with_random(
    entries: impl IntoIterator<Item = HostFunctionSchema>,
) -> Arc<HostApiCatalog> {
    let production = reference_array_cli_effective_catalog();
    let mut builder = HostApiCatalog::builder();
    for resource in production.resources() {
        builder.resource(resource.clone());
    }
    for structure in production.structs() {
        builder.named_struct(structure.clone());
    }
    for function in production.functions().iter().filter(|f| f.name != RANDOM) {
        builder.function(function.clone());
    }
    for entry in entries {
        builder.function(entry);
    }
    Arc::new(builder.build().expect("valid fixture catalog"))
}

fn compile(
    source: &str,
    catalog: Option<Arc<HostApiCatalog>>,
) -> Result<CertifiedCompilation, vm::SourcePathError> {
    let mut options = javascript_es5_compile_options();
    if let Some(catalog) = catalog {
        options = options.with_host_api_catalog(catalog);
    }
    vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
}

fn prepare(
    source: &str,
    host: StaticHostFunction,
    replies: impl IntoIterator<Item = VmResult<Value>>,
) -> Vm {
    let schema = random_schema(vec![], HostTypeSchema::Number);
    let catalog = catalog_with_random([schema.clone()]);
    let import = HostImportSchema::from_function(&catalog, &schema);
    let mut machine = compile(source, Some(catalog.clone()))
        .unwrap_or_else(|e| panic!("compile {source:?}: {e:?}"))
        .into_vm()
        .expect("certified VM");
    let imports: Vec<_> = machine
        .program()
        .host_import_schemas()
        .iter()
        .flatten()
        .filter(|schema| schema.name == RANDOM)
        .collect();
    assert_eq!(imports, [&import], "exact compiled random import: {source}");

    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        array::typescript_array_host_module(),
        array::typescript_owned_array_host_module(),
        array::typescript_array_snapshot_host_module(),
        scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
    }
    registry
        .register_exact_static(RANDOM, 0, import, host)
        .unwrap();
    registry.authorize_registered_builtin_import(RANDOM);
    for builtin in [
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
        "math::powf",
        "math::sin",
        "math::sqrt",
        "math::tan",
        "math::trunc",
        "math::log2",
        "math::log10",
        "math::exp2",
        "math::signum",
        "math::copysign",
        "math::is_finite",
        "math::is_infinite",
        "bytes::from_utf8",
    ] {
        registry.allow_builtin(builtin).unwrap();
    }
    registry
        .bind_vm_cached(&mut machine)
        .expect("bind exact catalog");
    array::initialize_typescript_owned_arrays(&mut machine, Default::default()).unwrap();
    machine
        .host_context()
        .set_host_state(RandomProbe {
            replies: replies.into_iter().collect(),
            calls: 0,
        })
        .unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    machine
}

fn check_interpreter(machine: &Vm) {
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(machine.capture_certification_stats().unwrap().valid);
}

fn check_calls(machine: &mut Vm, expected: usize) {
    let context = machine.host_context();
    let probe = context.host_state::<RandomProbe>().unwrap();
    assert_eq!(probe.calls, expected, "random host call count");
    assert!(probe.replies.is_empty(), "unconsumed random replies");
}

fn field<'a>(boxed: &'a Value, key: &str) -> &'a Value {
    let Value::Map(map) = boxed else {
        panic!("expected boxed completion: {boxed:?}")
    };
    map.get(&Value::string(key)).expect("boxed field")
}

fn finish(machine: &mut Vm) {
    machine.reset_for_reuse().expect("reset VM");
    assert_eq!(machine.host_context().resource_count(), 0);
}

fn run_success(source: &str, replies: &[Value], expected_calls: usize, check: impl FnOnce(&Value)) {
    eprintln!("Math.random source={source:?} replies={replies:?} calls={expected_calls}");
    let mut machine = prepare(source, numeric_host, replies.iter().cloned().map(Ok));
    assert_eq!(
        machine
            .run()
            .unwrap_or_else(|e| panic!("run {source:?}: {e:?}")),
        VmStatus::Halted
    );
    check_interpreter(&machine);
    check_calls(&mut machine, expected_calls);
    check(machine.stack().last().expect("guest completion"));
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = array::owned_array_runtime_stats(&mut machine).expect("real realm");
    assert_eq!((stats.pending, stats.registered), (0, 1));
    finish(&mut machine);
}

fn assert_number_box(completion: &Value, expected: f64) {
    assert_eq!(field(completion, "tag"), &Value::Int(3));
    let Value::Float(actual) = field(completion, "number") else {
        panic!("expected Float payload in {completion:?}");
    };
    assert_eq!(actual.to_bits(), expected.to_bits(), "Float bits");
}

#[test]
fn p8_math_random_forwarding_interpreter() {
    let max_unit = f64::from_bits(MAX_UNIT_BITS);
    for (source, answer) in [
        ("Math.random();", 0.0),
        ("var f = Math.random; f();", max_unit),
    ] {
        run_success(source, &[Value::Float(answer)], 1, |v| {
            assert_number_box(v, answer)
        });
    }

    // The borrowed call uses its own reply without coercing the receiver.
    // The inner call sees the completed argument trace and gets the next
    // reply before the outer call. Extra argument values are never coerced.
    run_success(
        r#"
        var n = 0;
        var receiver = {
            random: Math.random,
            valueOf: function () { throw 71; },
            toString: function () { throw 72; }
        };
        if (receiver.random() !== 0.5) { throw 78; }
        var extra = {
            valueOf: function () { throw 73; },
            toString: function () { throw 74; }
        };
        var inner;
        var outer = Math.random(
            (n = n * 10 + 1, extra),
            (n = n * 10 + 2, extra),
            (n = n * 10 + 3, inner = n === 123 ? Math.random() : -1, extra)
        );
        if (n !== 123) { throw 75; }
        if (inner !== 0.25) { throw 76; }
        outer;
    "#,
        &[Value::Float(0.5), Value::Float(0.25), Value::Float(0.75)],
        3,
        |v| assert_number_box(v, 0.75),
    );

    run_success(
        r#"
        'use strict';
        var receiver = {
            n: 0,
            get random() { this.n = 1; return Math.random; },
            fail: function () { this.n = this.n * 10 + 2; throw 9; }
        };
        var caught;
        try { receiver.random(receiver.fail(), receiver.n = 999); } catch (e) { caught = e; }
        if (receiver.n !== 12) { throw 77; }
        if (Math.random.length !== 0) { throw 79; }
        if (Math.exp(0) !== 1) { throw 80; }
        caught;
    "#,
        &[],
        0,
        |v| assert_number_box(v, 9.0),
    );
}

#[test]
fn p8_math_random_out_of_range_host_observation() {
    // These are test-host replies, not values promised by the production RNG.
    run_success(
        r#"
        if (Math.random() !== 1) { throw 81; }
        if (Math.random() !== -1) { throw 82; }
        var nan = Math.random();
        if (typeof nan !== 'number') { throw 83; }
        if (nan === nan) { throw 84; }
        if (Math.random() !== 1 / 0) { throw 85; }
        Math.random();
    "#,
        &[
            Value::Float(1.0),
            Value::Float(-1.0),
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY),
        ],
        5,
        |v| assert_number_box(v, f64::NEG_INFINITY),
    );
}

#[test]
fn p8_math_random_catalog_validation() {
    let exact = random_schema(vec![], HostTypeSchema::Number);
    let parameter = random_schema(
        vec![HostParamSchema::value("extra", HostTypeSchema::Number)],
        HostTypeSchema::Number,
    );
    let wrong_result = random_schema(vec![], HostTypeSchema::Bool);
    let invalid = [
        ("missing", catalog_with_random([])),
        (
            "overloaded",
            catalog_with_random([exact, parameter.clone()]),
        ),
        ("wrong arity", catalog_with_random([parameter])),
        ("wrong return", catalog_with_random([wrong_result])),
    ];
    for source in ["Math.random();", "Math.random.length;", "Math.exp(0);"] {
        let error = compile(source, None)
            .err()
            .expect("no catalog must refuse Math");
        // The managed realm's prelude is loaded before the Math-specific
        // catalog check. A completely absent catalog fails at that boundary.
        assert!(
            error
                .to_string()
                .contains("JS numeric-array realm requires the effective host catalog"),
            "{source}: {error}"
        );
        for (case, catalog) in &invalid {
            let error = compile(source, Some(catalog.clone()))
                .err()
                .unwrap_or_else(|| panic!("{case} catalog admitted {source}"));
            assert!(
                error
                    .to_string()
                    .contains("Math.random requires exact compat_math::random() -> Number"),
                "{case} {source}: {error}"
            );
        }
    }
}

#[test]
fn p8_math_random_fatal_paths() {
    let source = "try { Math.random(); } catch (e) { Math.random(); } 42;";
    let mut wrong_type = prepare(source, fault_host, [Ok(Value::string("not a number"))]);
    let status = wrong_type.run();
    assert!(
        matches!(status, Err(VmError::TypeMismatch("number"))),
        "{status:?}"
    );
    check_interpreter(&wrong_type);
    check_calls(&mut wrong_type, 1);
    finish(&mut wrong_type);

    let mut failed_host = prepare(
        source,
        fault_host,
        [Err(VmError::HostError(HOST_ERROR.into()))],
    );
    let status = failed_host.run();
    assert!(
        matches!(&status, Err(VmError::HostError(message)) if message == HOST_ERROR),
        "{status:?}"
    );
    check_interpreter(&failed_host);
    check_calls(&mut failed_host, 1);
    finish(&mut failed_host);
}

#[test]
fn p8_math_random_int_host_observation() {
    run_success("Math.random();", &[Value::Int(1)], 1, |v| {
        assert_number_box(v, 1.0)
    });
}
