//! Ordinary-call acceptance for MicroQuickJS `Math.min` / `Math.max`.
//!
//! `tests/javascript_es5_stdlib_tests.rs` does not compile on this base: six
//! pre-existing `match vm::Expr` sites omit variants added by the pinned Core
//! revision (E0004). Editing those unrelated observers is outside this slice,
//! so this dedicated integration target reuses the same public
//! prepare/execute shape as `javascript_es5_math_extensions_tests.rs` and the
//! existing `math-vectors-08` fixture.

use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name))
        .unwrap_or_else(|| panic!("boxed field {name}"))
        .clone()
}

fn prepare(source: &str) -> vm::Vm {
    eprintln!("MINMAX_SOURCE {source}");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("MINMAX_COMPILED");
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    for name in [
        "abs",
        "acos",
        "asin",
        "atan",
        "atan2",
        "ceil",
        "copysign",
        "cos",
        "exp",
        "exp2",
        "floor",
        "is_finite",
        "is_infinite",
        "is_nan",
        "ln",
        "log10",
        "log2",
        "powf",
        "signum",
        "sin",
        "sqrt",
        "tan",
        "trunc",
    ] {
        registry
            .allow_builtin(format!("math::{name}"))
            .unwrap_or_else(|error| panic!("numeric builtin math::{name}: {error:?}"));
    }
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("retained numeric formatter UTF-8 conversion");
    registry.bind_vm_cached(&mut machine).expect("binding");
    machine.set_fuel(10_000_000);
    machine
}

fn execute(source: &str) -> Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = prepare(&source);
            let before = machine.get_fuel();
            assert!(
                matches!(before, Some(fuel) if fuel <= 10_000_000),
                "interpreter fuel {before:?}"
            );
            let status = machine.run();
            let after = machine.get_fuel();
            eprintln!(
                "MINMAX_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(
                status.unwrap_or_else(|error| panic!("{source}: {error:?}")),
                VmStatus::Halted,
                "{source}"
            );
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            assert!(
                matches!((before, after), (Some(left), Some(right)) if right <= left && right <= 10_000_000)
            );
            assert!(machine
                .capture_certification_stats()
                .is_some_and(|stats| stats.valid));
            let resources = machine.host_context().resource_count();
            assert!(resources <= 1, "at most one real B1 realm");
            let value = machine.stack().last().expect("completion").clone();
            machine.reset_for_reuse().expect("reset");
            assert_eq!(machine.host_context().resource_count(), 0);
            value
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

fn number(source: &str) -> f64 {
    let result = execute(source);
    assert_eq!(field(&result, "tag"), Value::Int(3));
    match field(&result, "number") {
        Value::Float(number) => number,
        Value::Int(number) => number as f64,
        other => panic!("Number: {other:?}"),
    }
}

/// Ordinary-call discriminators for `Math.min` / `Math.max`. The exact script
/// body is also fed to the pinned MicroQuickJS `mqjs` binary, so both engines
/// compare the same completion value. `n` is zero only when every probe passes;
/// each of the twelve bits isolates one behavior: function length, last-argument dominance at
/// three and four arguments, extracted and foreign-object borrowed calls,
/// first-argument NaN continuing to the second conversion, and a second
/// argument throwing a sentinel before the already-evaluated last argument is
/// converted.
const MIN_MAX_PLAIN_CALL_PROBES: &str = r#"'use strict';
var n = 0;
if (!(Math.max.length === 2)) n = n | 1;
if (!(Math.min.length === 2)) n = n | 2;
if (!(Math.max(1, 2, 9) === 9)) n = n | 4;
if (!(Math.max(1, 2, 3, 9) === 9)) n = n | 8;
if (!(Math.min(1, 2, -9) === -9)) n = n | 16;
if (!(Math.min(1, 2, 3, -9) === -9)) n = n | 32;
var extractedMax = Math.max;
var extractedMin = Math.min;
if (!(extractedMax(1, 2, 9) === 9 && extractedMin(1, 2, 3, -9) === -9)) n = n | 64;
var borrowed = { max: Math.max, min: Math.min };
if (!(borrowed.max(1, 2, 3, 9) === 9 && borrowed.min(1, 2, -9) === -9)) n = n | 128;
var nanSeen = { n: 0 };
var firstNaN = { valueOf: function () { return 0 / 0; } };
var secondNine = { owner: nanSeen, valueOf: function () { this.owner.n = this.owner.n + 1; return 9; } };
var maxNaN = Math.max(firstNaN, secondNine);
if (!(maxNaN !== maxNaN && nanSeen.n === 1)) n = n | 256;
var trace = { n: 0, calls: 0 };
var thrower = { valueOf: function () { throw 37; } };
var tail = { owner: trace, valueOf: function () { this.owner.calls = this.owner.calls + 1; return 5; } };
var caught = 0;
try { Math.max(1, thrower, (trace.n = 7, tail)); } catch (maxError) { caught = maxError; }
if (!(caught === 37 && trace.n === 7 && trace.calls === 0)) n = n | 512;
var traceMin = { n: 0, calls: 0 };
var throwerMin = { valueOf: function () { throw 41; } };
var tailMin = { owner: traceMin, valueOf: function () { this.owner.calls = this.owner.calls + 1; return -9; } };
var caughtMin = 0;
try { Math.min(1, throwerMin, (traceMin.n = 9, tailMin)); } catch (minError) { caughtMin = minError; }
if (!(caughtMin === 41 && traceMin.n === 9 && traceMin.calls === 0)) n = n | 1024;
var nanSeenMin = { n: 0 };
var firstNaNMin = { valueOf: function () { return 0 / 0; } };
var secondNeg = { owner: nanSeenMin, valueOf: function () { this.owner.n = this.owner.n + 1; return -9; } };
var minNaN = Math.min(firstNaNMin, secondNeg);
if (!(minNaN !== minNaN && nanSeenMin.n === 1)) n = n | 2048;
n;
"#;

#[test]
fn p8_math_min_max_plain_call_interpreter() {
    assert_eq!(number(MIN_MAX_PLAIN_CALL_PROBES), 0.0, "probe bitmask");
    eprintln!(
        "P8_MIN_MAX_PLAIN_CALL bitmask=0 max_length=2 min_length=2 last_arg=true extracted=true borrowed=true nan_continues=true throw_order=true aot=0 jit=0 jit_enabled=false"
    );
}

#[test]
fn p8_vectors_b08_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-08.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=08 first=240 count=30 aot=0 trace=0");
}
