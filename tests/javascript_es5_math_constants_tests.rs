//! Public Micro Math constants on the certified, JIT-disabled interpreter.
//!
//! Each selector reads one public `Math.NAME` through the real ES5 entry and
//! asserts the boxed Number payload bit for bit. Property shape is deliberately
//! out of scope: enumerability, writability, deletion, the Math namespace
//! bridge and `.call/.apply` are not asserted here, and a numeric value is
//! never used as evidence for any property attribute.
//!
//! The first minimal selector (`Math.PI;`) passed unchanged at
//! `CANDIDATE_BASE`, so this file accepts existing production behavior. It is
//! not a feature-failure reproduction. A test-only negative control swapped one
//! expected PI bit, failed exactly at the host `to_bits` comparison, and was
//! reverted; the committed constants are the fixed production values.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

/// Production base under test. This file adds tests only, so the constants
/// observed here are the candidate at this revision.
const CANDIDATE_BASE: &str = "a70b05a150de24398b048a460a4e0b1ad36b6996";

const E_BITS: u64 = 0x4005bf0a8b145769;
const LN10_BITS: u64 = 0x40026bb1bbb55516;
const LN2_BITS: u64 = 0x3fe62e42fefa39ef;
const LOG2E_BITS: u64 = 0x3ff71547652b82fe;
const LOG10E_BITS: u64 = 0x3fdbcb7b1526e50e;
const PI_BITS: u64 = 0x400921fb54442d18;
const SQRT1_2_BITS: u64 = 0x3fe6a09e667f3bcd;
const SQRT2_BITS: u64 = 0x3ff6a09e667f3bcd;

fn free_floor() {
    let text = std::fs::read_to_string("/proc/meminfo").expect("meminfo");
    let kb = text
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .expect("MemAvailable")
        .split_whitespace()
        .next()
        .expect("kb")
        .parse::<u64>()
        .expect("kb number");
    assert!(
        kb.saturating_mul(1024) >= 8_589_934_592,
        "free floor below 8589934592 bytes: {kb}kB"
    );
}

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name))
        .unwrap_or_else(|| panic!("boxed field {name}"))
        .clone()
}

fn prepare(source: &str) -> vm::Vm {
    free_floor();
    eprintln!("MATH_CONST_CANDIDATE {CANDIDATE_BASE}");
    eprintln!("MATH_CONST_SOURCE {source}");
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
    eprintln!("MATH_CONST_COMPILED");
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
                "MATH_CONST_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} stack={:?}",
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

/// The completion must be a boxed Number (tag 3) carrying a Float payload.
/// The exact bit pattern is compared on the host, never through a decimal
/// approximation, equality tolerance, or NaN class.
fn boxed_number_bits(source: &str) -> u64 {
    let value = execute(source);
    assert_eq!(
        field(&value, "tag"),
        Value::Int(3),
        "boxed Number tag for {source}"
    );
    let Value::Float(number) = field(&value, "number") else {
        panic!("boxed Number Float payload for {source}: {value:?}");
    };
    number.to_bits()
}

fn expect_source_bits(source: &str, expected: u64) {
    let got = boxed_number_bits(source);
    eprintln!(
        "MATH_CONST_BITS source={source} got={got:#018x} expected={expected:#018x} candidate={CANDIDATE_BASE}"
    );
    assert_eq!(
        got, expected,
        "public Math read bits for {source} got={got:#018x} expected={expected:#018x}"
    );
}

fn expect_bits(name: &str, expected: u64) {
    let source = format!("Math.{name};");
    expect_source_bits(&source, expected);
}

#[test]
fn math_constant_bits_e() {
    expect_bits("E", E_BITS);
}

#[test]
fn math_constant_bits_ln10() {
    expect_bits("LN10", LN10_BITS);
}

#[test]
fn math_constant_bits_ln2() {
    expect_bits("LN2", LN2_BITS);
}

#[test]
fn math_constant_bits_log2e() {
    expect_bits("LOG2E", LOG2E_BITS);
}

#[test]
fn math_constant_bits_log10e() {
    expect_bits("LOG10E", LOG10E_BITS);
}

#[test]
fn math_constant_bits_pi() {
    expect_bits("PI", PI_BITS);
}

#[test]
fn math_constant_bits_sqrt1_2() {
    expect_bits("SQRT1_2", SQRT1_2_BITS);
}

#[test]
fn math_constant_bits_sqrt2() {
    expect_bits("SQRT2", SQRT2_BITS);
}

/// A local alias of the namespace must return the same exact PI bits.
#[test]
fn math_constant_control_alias_pi() {
    expect_source_bits("var m=Math; m.PI;", PI_BITS);
}

/// A runtime String key must return the same exact PI bits.
#[test]
fn math_constant_control_dynamic_key_pi() {
    expect_source_bits("var k='PI'; Math[k];", PI_BITS);
}

/// The requirement scan must include reads inside a function body, so the
/// public Math namespace exists and LN2 is readable from the call result.
#[test]
fn math_constant_control_function_body_ln2() {
    expect_source_bits("function f(){return Math.LN2;} f();", LN2_BITS);
}

fn catalog_without(function_name: &str) -> std::sync::Arc<vm::HostApiCatalog> {
    let full = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let mut builder = vm::HostApiCatalog::builder();
    for resource in full.resources() {
        builder.resource(resource.clone());
    }
    for schema in full.structs() {
        builder.named_struct(schema.clone());
    }
    for function in full.functions() {
        if function.name != function_name {
            builder.function(function.clone());
        }
    }
    std::sync::Arc::new(builder.build().expect("filtered catalog must validate"))
}

fn compile_only(source: &str, catalog: std::sync::Arc<vm::HostApiCatalog>) -> Result<(), String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let options = vm::CompileSourceFileOptions::new()
                .with_host_api_catalog(catalog)
                .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
            vm::compile_source_with_flavor_and_options_with_capture_certification(
                source.as_str(),
                javascript_es5_flavor(),
                options,
            )
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

/// A formal named `Math` shadows the builtin, so its body must not request the
/// Math namespace. The catalog observer removes only
/// `compat_math::random`: the unshadowed control must refuse with the exact
/// random-signature diagnostic, while the shadowed body must compile. The
/// production catalog's exact zero-argument Number random signature is pinned
/// first, so this control cannot pass by weakening or deleting that demand.
#[test]
fn math_constant_control_shadow_parameter_not_builtin() {
    let full = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let candidates = full.functions_named("compat_math::random");
    assert!(
        matches!(candidates.as_slice(), [candidate]
            if candidate.params.is_empty()
                && candidate.return_type == vm::HostTypeSchema::Number),
        "production compat_math::random must be an exact () -> Number signature: {candidates:?}"
    );
    let filtered = catalog_without("compat_math::random");
    assert!(
        filtered.function("compat_math::random").is_none(),
        "the observer catalog must omit compat_math::random"
    );
    let positive = compile_only("Math.PI;", std::sync::Arc::clone(&filtered))
        .expect_err("unshadowed Math must require the exact random signature");
    assert!(
        positive.contains("Math.random requires exact compat_math::random() -> Number"),
        "unshadowed Math diagnostic: {positive}"
    );
    eprintln!(
        "MATH_CONST_SHADOW unshadowed_error={positive} shadowed_compiles=true candidate={CANDIDATE_BASE}"
    );
    compile_only("function f(Math){return Math.LN2;} f({LN2:1});", filtered)
        .expect("a formal named Math must not request the builtin namespace");
}
