//! Bit-exact, call-shape and conversion receipt for `Math.sign`.
//!
//! The single guest program is the pinned MicroQuickJS oracle program (mqjs
//! `6d4d7eb6b99011d5aab6e88a9b99e8fd4d86c6e9`) executed byte-for-byte on both
//! engines: `-0` stays `-0`, `+0` stays `+0`, NaN stays NaN, both infinities
//! narrow to `±1`, `-2.5` narrows to `-1` and `2.5` to `1`, a missing argument
//! is NaN, and `length` is the callable arity 1. Direct calls, an extracted
//! `var S = Math.sign` alias and the borrowed `o.f = S; o.f(...)` shape are all
//! covered, and `this` is ignored. The host checks only the final boxed Number:
//! tag 3 with exact IEEE-754 bits for finite values, infinities and signed
//! zeros; NaN is only a semantic category, never a payload. Additional
//! one-expression receipts pin `+0`, `-0`, `+1`, `-1` and the two NaN routes.
//!
//! The accepted subset (`P`) is exactly ordinary JS ES5 through the certified,
//! JIT-disabled interpreter: `Math.sign` with zero or one argument whose
//! conversion is a direct primitive or the pinned `'-2.5'` string example.
//! Object conversion order is `valueOf` then `toString`, a throwing hook keeps
//! its original payload, two object-returning hooks produce `TypeError`, and a
//! throwing first-argument expression or conversion prevents every later
//! conversion. Extra arguments are evaluated but never converted: a shared
//! tail object increments one counter from both hooks, and that counter must
//! stay 0 after direct, extracted and borrowed calls.
//!
//! Out of scope: full string ToNumber, `.call`/`.apply`, property descriptors
//! (`enumerable`/`writable`/`configurable`/`delete`), the `length` descriptor
//! (only callable arity is observed), TypeScript, JIT/AOT/native execution and
//! the full finite domain. `CANDIDATE_BASE` records the production base under
//! test; a first green run at that base accepts existing behavior instead of
//! manufacturing a feature RED.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

const FUEL_LIMIT: u64 = 16_000_000;
const CANDIDATE_BASE: &str = "a70b05a150de24398b048a460a4e0b1ad36b6996";

fn resource_floor_path() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        let path = std::path::PathBuf::from(dir);
        if !path.as_os_str().is_empty() {
            return if path.is_absolute() {
                path
            } else {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
            };
        }
    }
    if let Some(dir) = std::env::var_os("TMPDIR") {
        let path = std::path::PathBuf::from(dir);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    let fallback = std::env::temp_dir();
    if fallback.as_os_str().is_empty() {
        std::env::current_dir().expect("current directory for free floor")
    } else {
        fallback
    }
}

#[cfg(target_os = "linux")]
fn free_floor() {
    let path = resource_floor_path();
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
    let output = std::process::Command::new("df")
        .args(["-B1"])
        .arg(&path)
        .output()
        .expect("df");
    let df = String::from_utf8(output.stdout).expect("df utf8");
    let available = df
        .lines()
        .last()
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|field| field.parse::<u64>().ok())
        .expect("df available bytes");
    eprintln!(
        "MATH_SIGN_BUDGET path={} mem_available_kib={kb} fs_available_bytes={available}",
        path.display()
    );
    assert!(
        kb.saturating_mul(1024) >= 8_589_934_592,
        "memory free floor below 8589934592 bytes: {kb}kB"
    );
    assert!(
        available >= 8_589_934_592,
        "filesystem free floor below 8589934592 bytes at {}: {available}",
        path.display()
    );
}

#[cfg(not(target_os = "linux"))]
fn free_floor() {
    eprintln!(
        "MATH_SIGN_BUDGET platform={} skipped=true path={}",
        std::env::consts::OS,
        resource_floor_path().display()
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
    eprintln!("MATH_SIGN_CANDIDATE {CANDIDATE_BASE}");
    eprintln!("MATH_SIGN_SOURCE bytes={}", source.len());
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    eprintln!("MATH_SIGN_CATALOG ms={}", started.elapsed().as_millis());
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("MATH_SIGN_COMPILED ms={}", started.elapsed().as_millis());
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
        .expect("numeric string conversion");
    registry.bind_vm_cached(&mut machine).expect("binding");
    // The ordered tail-conversion guest adds object conversions on top of the
    // realm prelude's variable startup fuel; keep a bounded but safe budget.
    machine.set_fuel(FUEL_LIMIT);
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
                matches!(before, Some(fuel) if fuel <= FUEL_LIMIT),
                "interpreter fuel {before:?}"
            );
            let status = machine.run();
            let after = machine.get_fuel();
            let resources = machine.host_context().resource_count();
            eprintln!(
                "MATH_SIGN_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} resources={resources}",
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
                matches!((before, after), (Some(left), Some(right)) if right <= left && right <= FUEL_LIMIT)
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

fn number_bits(source: &str) -> (i64, u64) {
    let result = execute(source);
    let tag = match field(&result, "tag") {
        Value::Int(tag) => tag,
        other => panic!("tag: {other:?}\n{source}"),
    };
    let bits = match field(&result, "number") {
        Value::Float(number) => number.to_bits(),
        Value::Int(number) => (number as f64).to_bits(),
        other => panic!("number: {other:?}\n{source}"),
    };
    eprintln!(
        "MATH_SIGN_NUMBER tag={tag} bits={bits:#x} value={}",
        f64::from_bits(bits)
    );
    (tag, bits)
}

fn only_method(source: &str, method: &str) {
    for other in [
        "abs", "acos", "asin", "atan", "atan2", "ceil", "cos", "exp", "floor", "log", "max", "min",
        "pow", "random", "round", "sin", "sqrt", "tan", "sign", "imul", "clz32", "fround", "trunc",
        "log2", "log10",
    ] {
        if other != method {
            assert!(
                !source.contains(&format!("Math.{other}")),
                "{method} script mentions Math.{other}"
            );
        }
    }
    assert!(!source.contains(".call"), "{method}");
    assert!(!source.contains(".apply"), "{method}");
    for banned in ["0b", "0o", "-0x", "\u{00a0}"] {
        assert!(
            !source.contains(banned),
            "{method} excluded literal {banned}"
        );
    }
}

fn expect_bits(method: &str, source: &str, expected: f64) {
    only_method(source, method);
    let (tag, bits) = number_bits(source);
    assert_eq!(tag, 3, "{source}");
    assert!(
        !expected.is_nan(),
        "NaN receipts use expect_nan_class, not payload bits"
    );
    assert_eq!(bits, expected.to_bits(), "{source}");
}

fn expect_nan_class(method: &str, source: &str) {
    only_method(source, method);
    let (tag, bits) = number_bits(source);
    assert_eq!(tag, 3, "{source}");
    assert!(
        f64::from_bits(bits).is_nan(),
        "{source} NaN class bits={bits:#x}"
    );
}

/// Pinned MicroQuickJS oracle program, executed byte-for-byte by both engines.
/// Each failed guest invariant throws `MATH_SIGN_CASE_<n>`; `length` is only
/// callable arity, never a descriptor claim.
const SIGN_CASE: &str = r#"var n = 0;
var z = 1 / (-1 / 0);
var seq = 0;
var markG = {};
var markM = {};
var markL = {};
var markX = {};
var tail = { mark: 11, th: 0 };
var later = { hits: 0 };
var p = { ts: 0 };
var b = { hits: 0 };
var g = { mark: markG };
var m = { mark: markM };
var both = { hits: 0 };
var L = { mark: markL };
var X = { mark: markX };
function retObj() { "use strict"; this.hits = this.hits + 1; return {}; }
function retStr() { "use strict"; return '-2.5'; }
function throwThis() { "use strict"; throw this.mark; }
function laterValue() { "use strict"; this.hits = this.hits + 1; return 0; }
p.valueOf = retStr;
p.toString = throwThis;
b.valueOf = retObj;
b.toString = retStr;
m.valueOf = throwThis;
both.valueOf = retObj;
both.toString = retObj;
function tailValue() { "use strict"; this.th = this.th + 1; return 0; }
function tailText() { "use strict"; this.th = this.th + 1; return '-2.5'; }
tail.valueOf = tailValue;
tail.toString = tailText;
L.boom = throwThis;
X.valueOf = throwThis;
later.valueOf = laterValue;
Object.defineProperty(g, 'valueOf', { get: throwThis });
var S = Math.sign;
var D = Math.sign((seq = seq * 10 + 1, -2.5), (seq = seq * 10 + 2, tail));
if (!(S.length === 1)) n = 1;
if (n === 0 && !(D === -1 && seq === 12)) n = 2;
if (n === 0 && !(1 / S(z) === -1 / 0)) n = 3;
if (n === 0 && !(1 / S(0) === 1 / 0)) n = 4;
if (n === 0 && !(S(-2.5) === -1 && S(2.5) === 1)) n = 5;
if (n === 0 && !(S(-1 / 0) === -1 && S(1 / 0) === 1)) n = 6;
if (n === 0 && !(S() !== S())) n = 7;
if (n === 0 && !(S(0 / 0) !== S(0 / 0))) n = 8;
var o = { mark: markL };
o.m = S;
o.valueOf = throwThis;
if (n === 0 && !(o.m(-2.5) === -1 && o.m(2.5) === 1 && 1 / o.m(z) === -1 / 0)) n = 9;
var ex = S((seq = seq * 10 + 3, -2.5), (seq = seq * 10 + 4, tail));
var bo = o.m((seq = seq * 10 + 5, -2.5), (seq = seq * 10 + 6, tail));
if (n === 0 && !(ex === -1 && bo === -1 && seq === 123456)) n = 10;
var k1 = { ord: 0 };
function co1() { "use strict"; this.ord = this.ord * 10 + 3; return -2.5; }
k1.valueOf = co1;
var t1 = Math.sign((k1.ord = 1, k1), (k1.ord = k1.ord * 10 + 2, tail));
if (n === 0 && !(t1 === -1 && k1.ord === 123)) n = 11;
if (n === 0 && !(tail.th === 0)) throw 'TAIL_COUNT';
if (n === 0 && !(S(p) === -1)) n = 12;
if (n === 0 && !(S(b) === -1 && b.hits === 1)) n = 13;
var cg = 0;
try { S(g); } catch (e1) { if (e1 === markG) cg = 1; }
if (n === 0 && !(cg === 1)) n = 14;
var cm = 0;
try { S(m); } catch (e2) { if (e2 === markM) cm = 1; }
if (n === 0 && !(cm === 1)) n = 15;
var ct = 0;
try { S(both); } catch (e3) { if (e3 && e3.name === 'TypeError') ct = 1; }
if (n === 0 && !(ct === 1 && both.hits === 2)) n = 16;
var sl = 0;
var lh = 0;
try { S((sl = sl * 10 + 1, L.boom()), (sl = sl * 10 + 2, 1)); } catch (e4) { if (e4 === markL) lh = 1; }
if (n === 0 && !(lh === 1 && sl === 1)) n = 17;
var xh = 0;
var xl = 0;
try { S((xl = xl * 10 + 1, X), (xl = xl * 10 + 2, later)); } catch (e5) { if (e5 === markX) xh = 1; }
if (n === 0 && !(xh === 1 && xl === 12 && later.hits === 0)) n = 18;
if (n !== 0) throw 'MATH_SIGN_CASE_' + n;
S(-2.5);
"#;

#[test]
fn p8_math_sign_interpreter() {
    // Main receipt: the entire guest invariant program completes with the
    // exact boxed -1 that distinguishes sign from abs/identity, and with
    // bounded fuel and no AOT/JIT-native execution.
    only_method(SIGN_CASE, "sign");
    let (tag, bits) = number_bits(SIGN_CASE);
    assert_eq!(tag, 3, "completion must be a boxed Number");
    assert_eq!(bits, (-1.0f64).to_bits(), "sign(-2.5) exact bits");

    // Host-side category receipts, each one ordinary ES5 expression.
    expect_bits("sign", "var S = Math.sign; S(-2.5);", -1.0);
    expect_bits("sign", "var z = 1 / (-1 / 0); Math.sign(z);", -0.0);
    expect_bits("sign", "Math.sign(0);", 0.0);
    expect_bits("sign", "Math.sign(1 / 0);", 1.0);
    expect_bits("sign", "Math.sign(-1 / 0);", -1.0);
    expect_nan_class("sign", "Math.sign();");
    expect_nan_class("sign", "Math.sign(0 / 0);");
}
