//! Public Micro `Math` namespace on the certified, JIT-disabled interpreter.
//!
//! One realm object is observed through the ordinary ES5 entry: repeated bare
//! `Math` reads, a `var m=Math` alias, a rewritten script binding (`Math=7`),
//! formal and function-local `Math` shadows, and the original object's
//! not-callable / not-constructible `TypeError` with argument evaluation order.
//! Every identity claim is a guest `===` preceded by an explicit
//! `typeof m==="object"` plus `m!==null` check; the host only reads the final
//! boxed Number completion as the exact IEEE-754 `+0` bit pattern and never
//! inspects a Rust realm object in its place.
//!
//! The execution witness is the pinned MicroQuickJS commit
//! `6d4d7eb6b99011d5aab6e88a9b99e8fd4d86c6e9`. Its answers are: the saved
//! `m` and `n` are both the non-null object `Math`; after `Math=7`, `m` keeps
//! the old object and the bare name reads 7; `function f(Math){return Math}`
//! returns its argument; `var Math=8` inside a function wins locally; `m()`
//! and `new m()` each throw `TypeError`; the call evaluates both actuals
//! before the throw, and `new` also evaluates its actual before the throw
//! (the pinned order). The namespace selector passed unchanged at
//! `CANDIDATE_BASE`, so this file accepts existing production behavior rather
//! than reproducing a feature failure.
//!
//! Out of scope: `globalThis` descriptors/attribute bridges, catch-binding
//! details and same-name catch rules, `Object.prototype.toString` branding
//! (Micro prints `[object Object]`, the candidate `[object Math]`, and the
//! candidate has no first-class `globalThis` attribute bridge), property
//! descriptors, member counts, each member's numeric value, `.call`/`.apply`,
//! plain TypeScript, JIT/AOT/native execution, cross-realm identity, and the
//! whole of ES5.1. A member value is never used here as evidence that the
//! namespace is complete.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

/// Production base under test. This file adds tests only.
const CANDIDATE_BASE: &str = "a70b05a150de24398b048a460a4e0b1ad36b6996";

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
    eprintln!("MATH_NS_BUDGET mem_available_kib={kb}");
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
    let started = std::time::Instant::now();
    eprintln!("MATH_NS_CANDIDATE {CANDIDATE_BASE}");
    eprintln!("MATH_NS_SOURCE {source}");
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
    eprintln!("MATH_NS_COMPILED ms={}", started.elapsed().as_millis());
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
            let resources = machine.host_context().resource_count();
            eprintln!(
                "MATH_NS_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} resources={resources}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count()
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

/// The completion must be a boxed Number (tag 3) whose payload is exactly
/// IEEE-754 `+0`. Comparing the raw bits rejects NaN, `-0`, and every other
/// magnitude instead of letting a lossy cast collapse them to the zero code.
fn completion_zero(source: &str) {
    let value = execute(source);
    assert_eq!(
        field(&value, "tag"),
        Value::Int(3),
        "boxed Number completion for {source}"
    );
    match field(&value, "number") {
        Value::Float(number) => assert_eq!(
            number.to_bits(),
            0x0000_0000_0000_0000u64,
            "exact +0 completion bits for {source}: {number:?}"
        ),
        other => panic!("boxed Number Float payload for {source}: {other:?}"),
    }
}

const NAMESPACE_SOURCE: &str = r#"
var m=Math;
var n=Math;
var a=m;
var bad=0;
if(!(typeof m==="object"&&m!==null))bad=bad+1;
if(!(m===n))bad=bad+2;
if(!(a===Math))bad=bad+4;
if(!(Math===Math))bad=bad+8;
Math=7;
if(!(Math===7))bad=bad+16;
if(m===Math)bad=bad+32;
if(m===7)bad=bad+64;
if(!(a===m))bad=bad+128;
function f(Math){return Math;}
var o={};
if(!(f(o)===o))bad=bad+256;
if(!(f(7)===7))bad=bad+512;
if(!(Math===7))bad=bad+1024;
function g(){var Math=8;return Math;}
if(!(g()===8))bad=bad+2048;
if(!(Math===7))bad=bad+4096;
var t1=0;
try{m();}catch(e1){t1=e1.name==="TypeError"?1:2;}
if(t1!==1)bad=bad+8192;
var t2=0;
try{new m();}catch(e2){t2=e2.name==="TypeError"?1:2;}
if(t2!==1)bad=bad+16384;
var c=0;
var ct=0;
try{m(c=c*10+1,c=c*10+2);}catch(e3){ct=e3.name==="TypeError"?1:2;}
if(c!==12)bad=bad+32768;
if(ct!==1)bad=bad+65536;
var d=0;
var dt=0;
try{new m(d=d*10+1);}catch(e4){d=d+100;dt=e4.name==="TypeError"?1:2;}
if(d!==101)bad=bad+131072;
if(dt!==1)bad=bad+262144;
bad;
"#;

/// A single interpreter program checks same-realm bare `Math` reads, alias
/// identity, rebinding, and formal-parameter and function-local shadowing. It
/// also checks that the saved object is neither callable nor constructible
/// and that actual arguments are evaluated in order before the throw.
/// It contains one bare `Math` requirement; that requirement selects the
/// production namespace bootstrap (25 methods and 8 constants), not a
/// member-minimal allocation, because every unshadowed `Math` read installs
/// the namespace. The first check proves the saved binding is a non-null
/// object before any identity comparison, so a pre-seeded `Math=undefined` or
/// `Math=42` cannot satisfy the alias rows. The expected completion is exactly
/// boxed `+0`: any flipped identity, alias, rebinding, shadow, throw class, or
/// evaluation order has its own nonzero code.
#[test]
fn math_namespace_identity_alias_rebinding_shadow_and_throws() {
    completion_zero(NAMESPACE_SOURCE);
    eprintln!("MATH_NS_CODES identity_alias_rebinding shadow throws order completion=positive+0");
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

/// A formal `Math` and a function-local `var Math` both bind before the
/// builtin is consulted, so neither may request the namespace. The observer
/// catalog removes only `compat_math::random`, whose exact zero-argument
/// Number signature is pinned from the production catalog first. The shadow
/// source compiles under the filtered catalog; a separate unshadowed `Math`
/// source fails there with the exact random-signature diagnostic. The shadow
/// source never spells an unshadowed `Math` anywhere else.
#[test]
fn math_namespace_shadow_parameter_and_local_declaration_are_not_the_builtin() {
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
    assert_eq!(
        full.functions().len(),
        filtered.functions().len() + 1,
        "the observer catalog must remove exactly one function"
    );

    let shadow = "function f(Math){return Math;}function g(){var Math=8;return Math;}f(1)+g();";
    compile_only(shadow, std::sync::Arc::clone(&filtered))
        .expect("a formal or function-local Math binding must not request the builtin namespace");

    let unshadowed = "Math;";
    let error = compile_only(unshadowed, filtered)
        .expect_err("unshadowed Math must require the exact random signature");
    assert!(
        error.contains("Math.random requires exact compat_math::random() -> Number"),
        "unshadowed Math diagnostic: {error}"
    );
    eprintln!("MATH_NS_SHADOW source={shadow} unshadowed_error={error} candidate={CANDIDATE_BASE}");
}
