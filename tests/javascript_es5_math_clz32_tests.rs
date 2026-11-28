//! Bit-exact, call-shape and conversion receipt for `Math.clz32`.
//!
//! The single guest program is the pinned MicroQuickJS oracle program (mqjs
//! `6d4d7eb6b99011d5aab6e88a9b99e8fd4d86c6e9`) executed byte-for-byte on both
//! engines: a missing argument, `0`, NaN, `+Infinity` and `-Infinity` narrow to
//! `32`; `1` is `31`, `2` is `30`, `256` is `23` and `2^32` wraps to `32`;
//! `-1`, `0xffffffff` and `0x80000000` all have no leading zero, while
//! `0x7fffffff` has exactly one. `3.9` truncates to `3` and yields `30`;
//! `-0.5` truncates toward zero to `0` and yields `32` (not the `0` that a
//! floor or sign interpretation would produce), and `-3.9` wraps to
//! `0xfffffffd` with count `0`. Direct calls, an extracted
//! `var C = Math.clz32` alias and the borrowed `o.f = C; o.f(...)` shape are
//! all covered, `this` is ignored, and `length` is only observed as callable
//! arity 1, never as a descriptor.
//!
//! The accepted subset (`P`) is exactly ordinary JS ES5 through the certified,
//! JIT-disabled interpreter: `Math.clz32` with zero or one argument whose
//! conversion is a direct primitive or the pinned `'-0.5'` string example.
//! Object conversion order is `valueOf` then `toString`, a throwing getter or
//! method keeps its original payload, two object-returning hooks produce
//! `TypeError`, and a throwing first-argument expression or conversion
//! prevents every later conversion. Extra arguments are evaluated but never
//! converted: a shared tail object increments one counter from both hooks, and
//! that counter must stay 0 after direct, extracted and borrowed calls. The
//! host checks only the final boxed Number: tag 3 as `Value::Float` with exact
//! IEEE-754 bits, `23` for the full guest and `30` in a one-expression receipt
//! (both distinguish clz32 from sign, abs and identity). NaN exists only as
//! input; every `clz32` result is an exact non-negative integer.
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
        "MATH_CLZ32_BUDGET path={} mem_available_kib={kb} fs_available_bytes={available}",
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
        "MATH_CLZ32_BUDGET platform={} skipped=true path={}",
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
    eprintln!("MATH_CLZ32_CANDIDATE {CANDIDATE_BASE}");
    eprintln!("MATH_CLZ32_SOURCE bytes={}", source.len());
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    eprintln!("MATH_CLZ32_CATALOG ms={}", started.elapsed().as_millis());
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("MATH_CLZ32_COMPILED ms={}", started.elapsed().as_millis());
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
                "MATH_CLZ32_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} resources={resources}",
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
        other => panic!("Float number: {other:?}\n{source}"),
    };
    eprintln!(
        "MATH_CLZ32_NUMBER tag={tag} bits={bits:#x} value={}",
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
    assert_eq!(bits, expected.to_bits(), "{source}");
}

/// Pinned MicroQuickJS oracle program, executed byte-for-byte by both engines.
/// Each failed guest invariant throws `MATH_CLZ32_CASE_<n>`; `length` is only
/// callable arity, never a descriptor claim.
const CLZ32_CASE: &str = r#"var n = 0;
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
function retStr() { "use strict"; return '-0.5'; }
function throwThis() { "use strict"; throw this.mark; }
function laterValue() { "use strict"; this.hits = this.hits + 1; return 0; }
function primValue() { "use strict"; this.ts = this.ts + 1; return 256; }
p.valueOf = primValue;
p.toString = throwThis;
b.valueOf = retObj;
b.toString = retStr;
m.valueOf = throwThis;
both.valueOf = retObj;
both.toString = retObj;
function tailValue() { "use strict"; this.th = this.th + 1; return 0; }
function tailText() { "use strict"; this.th = this.th + 1; return '-0.5'; }
tail.valueOf = tailValue;
tail.toString = tailText;
L.boom = throwThis;
X.valueOf = throwThis;
later.valueOf = laterValue;
Object.defineProperty(g, 'valueOf', { get: throwThis });
var C = Math.clz32;
var D = Math.clz32((seq = seq * 10 + 1, 256), (seq = seq * 10 + 2, tail));
if (!(C.length === 1)) n = 1;
if (n === 0 && !(D === 23 && seq === 12)) n = 2;
if (n === 0 && !(C(0) === 32)) n = 3;
if (n === 0 && !(C() === 32)) n = 4;
if (n === 0 && !(C(0 / 0) === 32)) n = 5;
if (n === 0 && !(C(1 / 0) === 32 && C(-1 / 0) === 32)) n = 6;
if (n === 0 && !(C(1) === 31 && C(2) === 30)) n = 7;
if (n === 0 && !(C(256) === 23)) n = 8;
if (n === 0 && !(C(4294967296) === 32)) n = 9;
if (n === 0 && !(C(-1) === 0 && C(0xffffffff) === 0 && C(0x80000000) === 0)) n = 10;
if (n === 0 && !(C(0x7fffffff) === 1)) n = 11;
if (n === 0 && !(C(3.9) === 30 && C(-0.5) === 32 && C(-3.9) === 0 && C(z) === 32)) n = 12;
var o = { mark: markL };
o.f = C;
o.valueOf = throwThis;
if (n === 0 && !(o.f(256) === 23 && o.f(0) === 32)) n = 13;
var ex = C((seq = seq * 10 + 3, 256), (seq = seq * 10 + 4, tail));
var bo = o.f((seq = seq * 10 + 5, 256), (seq = seq * 10 + 6, tail));
if (n === 0 && !(ex === 23 && bo === 23 && seq === 123456)) n = 14;
if (n === 0 && !(tail.th === 0)) throw 'TAIL_COUNT';
if (n === 0 && !(C(p) === 23 && p.ts === 1)) n = 15;
if (n === 0 && !(C(b) === 32 && b.hits === 1)) n = 16;
var k1 = { ord: 0 };
function co1() { "use strict"; this.ord = this.ord * 10 + 3; return 2; }
k1.valueOf = co1;
var t1 = Math.clz32((k1.ord = 1, k1), (k1.ord = k1.ord * 10 + 2, tail));
if (n === 0 && !(t1 === 30 && k1.ord === 123)) n = 17;
var cg = 0;
try { C(g); } catch (e1) { if (e1 === markG) cg = 1; }
if (n === 0 && !(cg === 1)) n = 18;
var cm = 0;
try { C(m); } catch (e2) { if (e2 === markM) cm = 1; }
if (n === 0 && !(cm === 1)) n = 19;
var ct = 0;
try { C(both); } catch (e3) { if (e3 && e3.name === 'TypeError') ct = 1; }
if (n === 0 && !(ct === 1 && both.hits === 2)) n = 20;
var sl = 0;
var lh = 0;
try { C((sl = sl * 10 + 1, L.boom()), (sl = sl * 10 + 2, 1)); } catch (e4) { if (e4 === markL) lh = 1; }
if (n === 0 && !(lh === 1 && sl === 1)) n = 21;
var xh = 0;
var xl = 0;
try { C((xl = xl * 10 + 1, X), (xl = xl * 10 + 2, later)); } catch (e5) { if (e5 === markX) xh = 1; }
if (n === 0 && !(xh === 1 && xl === 12 && later.hits === 0)) n = 22;
if (n !== 0) throw 'MATH_CLZ32_CASE_' + n;
C(256);
"#;

#[test]
fn p8_math_clz32_interpreter() {
    // Main receipt: the entire guest invariant program completes with the
    // exact boxed 23 that distinguishes clz32 from sign/abs/identity, and
    // with bounded fuel and no AOT/JIT-native execution.
    only_method(CLZ32_CASE, "clz32");
    let (tag, bits) = number_bits(CLZ32_CASE);
    assert_eq!(tag, 3, "completion must be a boxed Number");
    assert_eq!(bits, 23.0f64.to_bits(), "clz32(256) exact bits");

    // Host-side vector receipts, each one ordinary ES5 expression.
    expect_bits("clz32", "var C = Math.clz32; C(2);", 30.0);
    expect_bits("clz32", "Math.clz32(256);", 23.0);
    expect_bits("clz32", "Math.clz32(0);", 32.0);
    expect_bits("clz32", "Math.clz32();", 32.0);
    expect_bits("clz32", "Math.clz32(-0.5);", 32.0);
    expect_bits("clz32", "Math.clz32(-1);", 0.0);
    expect_bits("clz32", "Math.clz32(0x7fffffff);", 1.0);
    expect_bits("clz32", "Math.clz32(0 / 0);", 32.0);
}
