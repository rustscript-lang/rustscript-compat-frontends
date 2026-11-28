//! JIT-disabled ordinary calls for Math.imul.
//!
//! The three guest scripts are the MicroQuickJS oracle programs for this
//! method only. Each was first run through the pinned MicroQuickJS `mqjs`
//! build; the oracle receipt is the same guest bytes. imul converts both
//! operands with ToInt32/ToUint32 and returns the low signed 32 bits of the
//! product. Argument expressions are evaluated left to right before the first
//! conversion, the third and later arguments are never converted, and an
//! abrupt first conversion leaves the second conversion uncalled. Final
//! completions are boxed Number tags with exact bits. This file does not
//! cover full string ToNumber, `.call`/`.apply`, property descriptors,
//! TypeScript, JIT, AOT, or the full input domain.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

const FUEL_LIMIT: u64 = 64_000_000;
const PRODUCT: f64 = -131_071.0;

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
        "MATH_IMUL_BUDGET path={} mem_available_kib={kb} fs_available_bytes={available}",
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
    eprintln!("MATH_IMUL_SOURCE bytes={}", source.len());
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    eprintln!("MATH_IMUL_CATALOG ms={}", started.elapsed().as_millis());
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("MATH_IMUL_COMPILED ms={}", started.elapsed().as_millis());
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
                "MATH_IMUL_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} resources={resources}",
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
        "MATH_IMUL_NUMBER tag={tag} bits={bits:#x} value={}",
        f64::from_bits(bits)
    );
    (tag, bits)
}

fn imul_only(source: &str) {
    for other in [
        "abs", "acos", "asin", "atan", "atan2", "ceil", "clz32", "cos", "exp", "floor", "fround",
        "log", "log10", "log2", "max", "min", "pow", "random", "round", "sign", "sin", "sqrt",
        "tan", "trunc",
    ] {
        assert!(
            !source.contains(&format!("Math.{other}")),
            "imul script mentions Math.{other}"
        );
    }
    assert!(!source.contains(".call"), "imul script uses .call");
    assert!(!source.contains(".apply"), "imul script uses .apply");
    for banned in ["0b", "0o", "-0x"] {
        assert!(
            !source.contains(banned),
            "imul script excluded literal {banned}"
        );
    }
}

fn expect_product(source: &str) {
    imul_only(source);
    let (tag, bits) = number_bits(source);
    assert_eq!(tag, 3, "{source}");
    assert_eq!(
        bits,
        PRODUCT.to_bits(),
        "{source} got={bits:#x} expected={:#x}",
        PRODUCT.to_bits()
    );
}

// MicroQuickJS oracle: `var RESULT = ...; RESULT;` with `print(RESULT)`.
// Receipt: -131071 (0xc0fffff000000000).
const VALUE_CASE: &str = r#"var n = 0;
var I = Math.imul;
var o = {};
o.f = I;
if (!(Math.imul(65535, 65535) === -131071)) n = 1;
if (n === 0 && !(Math.imul(0xffffffff, 5) === -5)) n = 2;
if (n === 0 && !(Math.imul(0xfffffffe, 5) === -10)) n = 3;
if (n === 0 && !(Math.imul(4294967296, 7) === 0)) n = 4;
if (n === 0 && !(Math.imul() === 0)) n = 5;
if (n === 0 && !(Math.imul(7) === 0)) n = 6;
if (n === 0 && !(Math.imul(0 / 0, 5) === 0)) n = 7;
if (n === 0 && !(Math.imul(1 / 0, 5) === 0)) n = 8;
if (n === 0 && !(Math.imul(1 / (-1 / 0), 5) === 0)) n = 9;
if (n === 0 && !(Math.imul(5, 1 / 0) === 0)) n = 10;
if (n === 0 && !(Math.imul(5, 1 / (-1 / 0)) === 0)) n = 11;
if (n === 0 && !(Math.imul(5, 0 / 0) === 0)) n = 12;
if (n === 0 && !(Math.imul(2147483647, 2) === -2)) n = 13;
if (n === 0 && !(Math.imul(-2147483648, -1) === -2147483648)) n = 14;
if (n === 0 && !(Math.imul(2147483647, 2147483647) === 1)) n = 15;
if (n === 0 && !(Math.imul(-2147483648, -2147483648) === 0)) n = 16;
if (n === 0 && !(Math.imul(3735928559, 305419896) === 1445054984)) n = 17;
if (n === 0 && !(I(65535, 65535) === -131071)) n = 18;
if (n === 0 && !(o.f(65535, 65535) === -131071)) n = 19;
if (n === 0 && !(Math.imul.length === 2)) n = 20;
var RESULT = n === 0 ? Math.imul(65535, 65535) : n;
RESULT;
"#;

// MicroQuickJS oracle: receipt -131071. `ev` records left-to-right argument
// expression evaluation; `ord` records the conversion order as 1=left,
// 2=right, 3=tail. `st.t` must stay zero: the extra argument is never
// converted, and a throwing left conversion never reaches the right hook.
const ORDER_CASE: &str = r#"var n = 0;
var st = { l: 0, r: 0, t: 0, ord: 0, ML: {}, MR: {} };
var ev = 0;
function vh() {
  "use strict";
  var s = this.st;
  if (this.slot === 0) s.l = s.l + 1;
  else if (this.slot === 1) s.r = s.r + 1;
  else s.t = s.t + 1;
  s.ord = s.ord * 10 + this.d;
  if (this.mode === 1) return {};
  if (this.mode === 2) throw this.mark;
  if (this.mode === 3) return 0 / 0;
  return this.v;
}
var B1 = { st: st, slot: 0, d: 1, v: 7, mode: 0, valueOf: vh };
var B2 = { st: st, slot: 1, d: 2, v: 3, mode: 0, valueOf: vh };
var B3 = { st: st, slot: 2, d: 3, v: 5, mode: 0, valueOf: vh };
var BL = { st: st, slot: 0, d: 1, v: 0, mode: 2, mark: st.ML, valueOf: vh };
var BR = { st: st, slot: 1, d: 2, v: 3, mode: 0, valueOf: vh };
var NL = { st: st, slot: 0, d: 1, v: 0, mode: 3, valueOf: vh };
var ER = { st: st, slot: 1, d: 2, v: 0, mode: 2, mark: st.MR, valueOf: vh };
st.l = 0; st.r = 0; st.t = 0; st.ord = 0; ev = 0;
var r = Math.imul((ev = ev * 10 + 1, B1), (ev = ev * 10 + 2, B2), (ev = ev * 10 + 3, B3));
if (!(r === 21 && ev === 123 && st.l === 1 && st.r === 1 && st.t === 0 && st.ord === 12)) n = 1;
st.l = 0; st.r = 0; st.t = 0; st.ord = 0; ev = 0;
var caught = 0;
try {
  Math.imul((ev = ev * 10 + 1, BL), (ev = ev * 10 + 2, BR), (ev = ev * 10 + 3, B3));
} catch (e1) {
  if (e1 === st.ML) caught = 1;
}
if (!(caught === 1 && ev === 123 && st.l === 1 && st.r === 0 && st.t === 0 && st.ord === 1)) n = 2;
st.l = 0; st.r = 0; st.t = 0; st.ord = 0; ev = 0;
r = Math.imul(NL, B2);
if (!(r === 0 && 1 / r === 1 / 0 && st.l === 1 && st.r === 1 && st.ord === 12)) n = 3;
st.l = 0; st.r = 0; st.t = 0; st.ord = 0; ev = 0;
caught = 0;
try {
  Math.imul((ev = ev * 10 + 1, B1), (ev = ev * 10 + 2, ER), (ev = ev * 10 + 3, B3));
} catch (e2) {
  if (e2 === st.MR) caught = 1;
}
if (!(caught === 1 && ev === 123 && st.l === 1 && st.r === 1 && st.t === 0 && st.ord === 12)) n = 4;
st.l = 0; st.r = 0; st.t = 0; st.ord = 0; ev = 0;
r = Math.imul(2, 3, (ev = ev * 10 + 2, B3));
if (!(r === 6 && ev === 2 && st.l === 0 && st.r === 0 && st.t === 0)) n = 5;
st.l = 0; st.r = 0; st.t = 0; st.ord = 0; ev = 0;
r = Math.imul(B1, B2);
if (!(r === 21 && st.l === 1 && st.r === 1 && st.t === 0 && st.ord === 12)) n = 6;
var RESULT = n === 0 ? Math.imul(65535, 65535) : n;
RESULT;
"#;

// MicroQuickJS oracle: receipt -131071. Object conversions: valueOf primitive
// on both sides; valueOf object then an explicit small-string toString on both
// sides (order 1,3,2,4); an abrupt getter; an abrupt valueOf; and two object
// hooks producing TypeError without a right conversion.
const OBJECT_CASE: &str = r#"var n = 0;
var st = { l: 0, r: 0, ord: 0, MG: {}, MM: {} };
function vh() {
  "use strict";
  var s = this.st;
  if (this.slot === 0) s.l = s.l + 1;
  else s.r = s.r + 1;
  s.ord = s.ord * 10 + this.d;
  if (this.mode === 1) return {};
  if (this.mode === 2) throw this.mark;
  return this.v;
}
function th() {
  "use strict";
  var s = this.st;
  if (this.slot === 0) s.l = s.l + 1;
  else s.r = s.r + 1;
  s.ord = s.ord * 10 + this.d + 2;
  return this.tv;
}
var L7 = { st: st, slot: 0, d: 1, v: 7, mode: 0, valueOf: vh };
var P2 = { st: st, slot: 1, d: 2, v: 3, mode: 0, valueOf: vh };
var S1 = { st: st, slot: 0, d: 1, v: 0, tv: '7', mode: 1, valueOf: vh, toString: th };
var S2 = { st: st, slot: 1, d: 2, v: 0, tv: '3', mode: 1, valueOf: vh, toString: th };
var G = { st: st, slot: 0, d: 1, v: 0, mode: 2, mark: st.MG, get valueOf() { "use strict"; var s = this.st; s.l = s.l + 1; throw s.MG; }, toString: th };
var M = { st: st, slot: 0, d: 1, v: 0, mode: 2, mark: st.MM, valueOf: vh, toString: th };
var TY = { st: st, slot: 0, d: 1, v: 0, tv: {}, mode: 1, valueOf: vh, toString: th };
st.l = 0; st.r = 0; st.ord = 0;
var r = Math.imul(L7, P2);
if (!(r === 21 && st.l === 1 && st.r === 1 && st.ord === 12)) n = 1;
st.l = 0; st.r = 0; st.ord = 0;
r = Math.imul(S1, S2);
if (!(r === 21 && st.l === 2 && st.r === 2 && st.ord === 1324)) n = 2;
st.l = 0; st.r = 0; st.ord = 0;
var caught = 0;
try {
  Math.imul(G, P2);
} catch (e1) {
  if (e1 === st.MG) caught = 1;
}
if (!(caught === 1 && st.l === 1 && st.r === 0 && st.ord === 0)) n = 3;
st.l = 0; st.r = 0; st.ord = 0;
caught = 0;
try {
  Math.imul(M, P2);
} catch (e2) {
  if (e2 === st.MM) caught = 1;
}
if (!(caught === 1 && st.l === 1 && st.r === 0 && st.ord === 1)) n = 4;
st.l = 0; st.r = 0; st.ord = 0;
var caughtTy = 0;
try {
  Math.imul(TY, P2);
} catch (e3) {
  if (e3 && e3.name === 'TypeError') caughtTy = 1;
}
if (!(caughtTy === 1 && st.l === 2 && st.r === 0 && st.ord === 13)) n = 5;
var RESULT = n === 0 ? Math.imul(65535, 65535) : n;
RESULT;
"#;

#[test]
fn p8_math_imul_values_interpreter() {
    expect_product(VALUE_CASE);
}

#[test]
fn p8_math_imul_order_interpreter() {
    expect_product(ORDER_CASE);
}

#[test]
fn p8_math_imul_objects_interpreter() {
    expect_product(OBJECT_CASE);
}
