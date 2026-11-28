//! JIT-disabled ordinary calls for Math.abs/floor/ceil/round/sqrt.
//!
//! Each script is the MicroQuickJS oracle program for that method only.
//! Return values are boxed Number tags. Tail arguments are evaluated but
//! never converted; guest counters assert the shared order state and a zero
//! tail conversion count. Signed zero uses reciprocal checks
//! in the script and exact IEEE bits on the direct-call receipt. NaN is a
//! category, not a payload. This file does not cover full string ToNumber,
//! JIT, AOT, TypeScript, or argument-bearing `.call`/`.apply`.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

const FUEL_LIMIT: u64 = 16_000_000;

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
        "MATH_UNARY_BUDGET path={} mem_available_kib={kb} fs_available_bytes={available}",
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
    eprintln!("MATH_UNARY_SOURCE bytes={}", source.len());
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    eprintln!("MATH_UNARY_CATALOG ms={}", started.elapsed().as_millis());
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("MATH_UNARY_COMPILED ms={}", started.elapsed().as_millis());
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
    // The ordered tail-conversion guest adds one object conversion on top of the
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
                "MATH_UNARY_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} resources={resources}",
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
        "MATH_UNARY_NUMBER tag={tag} bits={bits:#x} value={}",
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
        "NaN receipts use expect_nan, not payload bits"
    );
    assert_eq!(bits, expected.to_bits(), "{source}");
}

const ABS_CASE: &str = r#"var n = 0;
var z = 1 / (-1 / 0);
var seq = 0;
var markG = {};
var markM = {};
var markL = {};
var tail = { mark: 11, th: 0 };
var p = { ts: 0 };
var b = { hits: 0 };
var g = { mark: markG };
var m = { mark: markM };
var both = { hits: 0 };
var L = { mark: markL };
function retObj() { "use strict"; this.hits = this.hits + 1; return {}; }
function retStr() { "use strict"; return '-8'; }
function throwThis() { "use strict"; throw this.mark; }
p.valueOf = retStr;
p.toString = throwThis;
b.valueOf = retObj;
b.toString = retStr;
m.valueOf = throwThis;
both.valueOf = retObj;
both.toString = retObj;
function tailValue() { "use strict"; this.th = this.th + 1; return 0; }
function tailText() { "use strict"; this.th = this.th + 1; return '-8'; }
tail.valueOf = tailValue;
tail.toString = tailText;
L.boom = throwThis;
Object.defineProperty(g, 'valueOf', { get: throwThis });
var A = Math.abs;
var D = Math.abs((seq = seq * 10 + 1, -5e-324), (seq = seq * 10 + 2, tail));
if (!(A.length === 1)) n = 1;
if (n === 0 && !(D === 5e-324 && seq === 12)) n = 2;
if (n === 0 && !(1 / A(z) === 1 / 0)) n = 3;
if (n === 0 && !(A(-1 / 0) === 1 / 0 && A(1 / 0) === 1 / 0)) n = 4;
if (n === 0 && !(1 / A(0) === 1 / 0)) n = 5;
if (n === 0 && !(A(-4) === 4)) n = 6;
if (n === 0 && !(A() !== A())) n = 7;
if (n === 0 && !(A(0 / 0) !== A(0 / 0))) n = 8;
var o = {};
o.m = A;
if (n === 0 && !(o.m(-4) === 4 && 1 / o.m(z) === 1 / 0)) n = 9;
var ex = A((seq = seq * 10 + 3, -4), (seq = seq * 10 + 4, tail));
var bo = o.m((seq = seq * 10 + 5, -4), (seq = seq * 10 + 6, tail));
if (n === 0 && !(ex === 4 && bo === 4 && seq === 123456)) n = 10;
var k1 = { ord: 0 };
function co1() { "use strict"; this.ord = this.ord * 10 + 3; return -5e-324; }
k1.valueOf = co1;
var t1 = Math.abs((k1.ord = 1, k1), (k1.ord = k1.ord * 10 + 2, tail));
if (n === 0 && !(t1 === 5e-324 && k1.ord === 123)) n = 26;
if (n === 0 && !(tail.th === 0)) throw 'TAIL_COUNT';
if (n === 0 && !(A(p) === 8)) n = 20;
if (n === 0 && !(A(b) === 8 && b.hits === 1)) n = 21;
var cg = 0;
try { A(g); } catch (e1) { if (e1 === markG) cg = 1; }
if (n === 0 && !(cg === 1)) n = 22;
var cm = 0;
try { A(m); } catch (e2) { if (e2 === markM) cm = 1; }
if (n === 0 && !(cm === 1)) n = 23;
var ct = 0;
try { A(both); } catch (e3) { if (e3 && e3.name === 'TypeError') ct = 1; }
if (n === 0 && !(ct === 1 && both.hits === 2)) n = 24;
var sl = 0;
var lh = 0;
try { A((sl = sl * 10 + 1, L.boom()), (sl = sl * 10 + 2, 1)); } catch (e4) { if (e4 === markL) lh = 1; }
if (n === 0 && !(lh === 1 && sl === 1)) n = 25;
n === 0 ? A(-5e-324) : n;
"#;

const FLOOR_CASE: &str = r#"var n = 0;
var z = 1 / (-1 / 0);
var seq = 0;
var markG = {};
var markM = {};
var markL = {};
var tail = { mark: 11, th: 0 };
var p = { ts: 0 };
var b = { hits: 0 };
var g = { mark: markG };
var m = { mark: markM };
var both = { hits: 0 };
var L = { mark: markL };
function retObj() { "use strict"; this.hits = this.hits + 1; return {}; }
function retStr() { "use strict"; return '-1.2'; }
function throwThis() { "use strict"; throw this.mark; }
p.valueOf = retStr;
p.toString = throwThis;
b.valueOf = retObj;
b.toString = retStr;
m.valueOf = throwThis;
both.valueOf = retObj;
both.toString = retObj;
function tailValue() { "use strict"; this.th = this.th + 1; return 0; }
function tailText() { "use strict"; this.th = this.th + 1; return '-1.2'; }
tail.valueOf = tailValue;
tail.toString = tailText;
L.boom = throwThis;
Object.defineProperty(g, 'valueOf', { get: throwThis });
var F = Math.floor;
var D = Math.floor((seq = seq * 10 + 1, -1.2), (seq = seq * 10 + 2, tail));
if (!(F.length === 1)) n = 1;
if (n === 0 && !(D === -2 && seq === 12)) n = 2;
if (n === 0 && !(1 / F(z) === -1 / 0)) n = 3;
if (n === 0 && !(F(-1.2) === -2 && F(1.2) === 1)) n = 4;
if (n === 0 && !(1 / F(0) === 1 / 0)) n = 5;
if (n === 0 && !(F(-1 / 0) === -1 / 0 && F(1 / 0) === 1 / 0)) n = 6;
if (n === 0 && !(F(0 / 0) !== F(0 / 0))) n = 7;
if (n === 0 && !(F(-0.5) === -1)) n = 8;
var o = {};
o.m = F;
if (n === 0 && !(o.m(-1.2) === -2 && 1 / o.m(z) === -1 / 0)) n = 9;
var ex = F((seq = seq * 10 + 3, -1.2), (seq = seq * 10 + 4, tail));
var bo = o.m((seq = seq * 10 + 5, -1.2), (seq = seq * 10 + 6, tail));
if (n === 0 && !(ex === -2 && bo === -2 && seq === 123456)) n = 10;
var k1 = { ord: 0 };
function co1() { "use strict"; this.ord = this.ord * 10 + 3; return -1.2; }
k1.valueOf = co1;
var t1 = Math.floor((k1.ord = 1, k1), (k1.ord = k1.ord * 10 + 2, tail));
if (n === 0 && !(t1 === -2 && k1.ord === 123)) n = 26;
if (n === 0 && !(tail.th === 0)) throw 'TAIL_COUNT';
if (n === 0 && !(F(p) === -2)) n = 20;
if (n === 0 && !(F(b) === -2 && b.hits === 1)) n = 21;
var cg = 0;
try { F(g); } catch (e1) { if (e1 === markG) cg = 1; }
if (n === 0 && !(cg === 1)) n = 22;
var cm = 0;
try { F(m); } catch (e2) { if (e2 === markM) cm = 1; }
if (n === 0 && !(cm === 1)) n = 23;
var ct = 0;
try { F(both); } catch (e3) { if (e3 && e3.name === 'TypeError') ct = 1; }
if (n === 0 && !(ct === 1 && both.hits === 2)) n = 24;
var sl = 0;
var lh = 0;
try { F((sl = sl * 10 + 1, L.boom()), (sl = sl * 10 + 2, 1)); } catch (e4) { if (e4 === markL) lh = 1; }
if (n === 0 && !(lh === 1 && sl === 1)) n = 25;
n === 0 ? F(z) : n;
"#;

const CEIL_CASE: &str = r#"var n = 0;
var z = 1 / (-1 / 0);
var seq = 0;
var markG = {};
var markM = {};
var markL = {};
var tail = { mark: 11, th: 0 };
var p = { ts: 0 };
var b = { hits: 0 };
var g = { mark: markG };
var m = { mark: markM };
var both = { hits: 0 };
var L = { mark: markL };
function retObj() { "use strict"; this.hits = this.hits + 1; return {}; }
function retStr() { "use strict"; return '-1.2'; }
function throwThis() { "use strict"; throw this.mark; }
p.valueOf = retStr;
p.toString = throwThis;
b.valueOf = retObj;
b.toString = retStr;
m.valueOf = throwThis;
both.valueOf = retObj;
both.toString = retObj;
function tailValue() { "use strict"; this.th = this.th + 1; return 0; }
function tailText() { "use strict"; this.th = this.th + 1; return '-1.2'; }
tail.valueOf = tailValue;
tail.toString = tailText;
L.boom = throwThis;
Object.defineProperty(g, 'valueOf', { get: throwThis });
var F = Math.ceil;
var D = Math.ceil((seq = seq * 10 + 1, -1.2), (seq = seq * 10 + 2, tail));
if (!(F.length === 1)) n = 1;
if (n === 0 && !(D === -1 && seq === 12)) n = 2;
if (n === 0 && !(F(-1.2) === -1 && F(1.2) === 2)) n = 3;
if (n === 0 && !(1 / F(-0.5) === -1 / 0)) n = 4;
if (n === 0 && !(1 / F(z) === -1 / 0)) n = 5;
if (n === 0 && !(1 / F(0) === 1 / 0)) n = 6;
if (n === 0 && !(F(-1 / 0) === -1 / 0 && F(1 / 0) === 1 / 0)) n = 7;
if (n === 0 && !(F(0 / 0) !== F(0 / 0))) n = 8;
var o = {};
o.m = F;
if (n === 0 && !(o.m(-1.2) === -1 && 1 / o.m(-0.5) === -1 / 0)) n = 9;
var ex = F((seq = seq * 10 + 3, -1.2), (seq = seq * 10 + 4, tail));
var bo = o.m((seq = seq * 10 + 5, -1.2), (seq = seq * 10 + 6, tail));
if (n === 0 && !(ex === -1 && bo === -1 && seq === 123456)) n = 10;
var k1 = { ord: 0 };
function co1() { "use strict"; this.ord = this.ord * 10 + 3; return -1.2; }
k1.valueOf = co1;
var t1 = Math.ceil((k1.ord = 1, k1), (k1.ord = k1.ord * 10 + 2, tail));
if (n === 0 && !(t1 === -1 && k1.ord === 123)) n = 26;
if (n === 0 && !(tail.th === 0)) throw 'TAIL_COUNT';
if (n === 0 && !(F(p) === -1)) n = 20;
if (n === 0 && !(F(b) === -1 && b.hits === 1)) n = 21;
var cg = 0;
try { F(g); } catch (e1) { if (e1 === markG) cg = 1; }
if (n === 0 && !(cg === 1)) n = 22;
var cm = 0;
try { F(m); } catch (e2) { if (e2 === markM) cm = 1; }
if (n === 0 && !(cm === 1)) n = 23;
var ct = 0;
try { F(both); } catch (e3) { if (e3 && e3.name === 'TypeError') ct = 1; }
if (n === 0 && !(ct === 1 && both.hits === 2)) n = 24;
var sl = 0;
var lh = 0;
try { F((sl = sl * 10 + 1, L.boom()), (sl = sl * 10 + 2, 1)); } catch (e4) { if (e4 === markL) lh = 1; }
if (n === 0 && !(lh === 1 && sl === 1)) n = 25;
n === 0 ? F(-0.5) : n;
"#;

const ROUND_CASE: &str = r#"var n = 0;
var z = 1 / (-1 / 0);
var seq = 0;
var markG = {};
var markM = {};
var markL = {};
var tail = { mark: 11, th: 0 };
var p = { ts: 0 };
var b = { hits: 0 };
var g = { mark: markG };
var m = { mark: markM };
var both = { hits: 0 };
var L = { mark: markL };
function retObj() { "use strict"; this.hits = this.hits + 1; return {}; }
function retStr() { "use strict"; return '-1.5'; }
function throwThis() { "use strict"; throw this.mark; }
p.valueOf = retStr;
p.toString = throwThis;
b.valueOf = retObj;
b.toString = retStr;
m.valueOf = throwThis;
both.valueOf = retObj;
both.toString = retObj;
function tailValue() { "use strict"; this.th = this.th + 1; return 0; }
function tailText() { "use strict"; this.th = this.th + 1; return '-1.5'; }
tail.valueOf = tailValue;
tail.toString = tailText;
L.boom = throwThis;
Object.defineProperty(g, 'valueOf', { get: throwThis });
var F = Math.round;
var D = Math.round((seq = seq * 10 + 1, -1.5), (seq = seq * 10 + 2, tail));
if (!(F.length === 1)) n = 1;
if (n === 0 && !(D === -1 && seq === 12)) n = 2;
if (n === 0 && !(1 / F(-0.5) === -1 / 0)) n = 3;
if (n === 0 && !(F(-1.5) === -1 && F(-0.5000000000000001) === -1)) n = 4;
if (n === 0 && !(1 / F(0.49999999999999994) === 1 / 0)) n = 5;
if (n === 0 && !(F(2.5) === 3 && F(-2.5) === -2)) n = 6;
if (n === 0 && !(F(4503599627370497) === 4503599627370497)) n = 7;
if (n === 0 && !(1 / F(z) === -1 / 0 && 1 / F(0) === 1 / 0)) n = 8;
if (n === 0 && !(F(-1 / 0) === -1 / 0 && F(1 / 0) === 1 / 0)) n = 11;
if (n === 0 && !(F(0 / 0) !== F(0 / 0))) n = 12;
var o = {};
o.m = F;
if (n === 0 && !(o.m(-1.5) === -1 && 1 / o.m(-0.5) === -1 / 0)) n = 9;
var ex = F((seq = seq * 10 + 3, -1.5), (seq = seq * 10 + 4, tail));
var bo = o.m((seq = seq * 10 + 5, -1.5), (seq = seq * 10 + 6, tail));
if (n === 0 && !(ex === -1 && bo === -1 && seq === 123456)) n = 10;
var k1 = { ord: 0 };
function co1() { "use strict"; this.ord = this.ord * 10 + 3; return -1.5; }
k1.valueOf = co1;
var t1 = Math.round((k1.ord = 1, k1), (k1.ord = k1.ord * 10 + 2, tail));
if (n === 0 && !(t1 === -1 && k1.ord === 123)) n = 26;
if (n === 0 && !(tail.th === 0)) throw 'TAIL_COUNT';
if (n === 0 && !(F(p) === -1)) n = 20;
if (n === 0 && !(F(b) === -1 && b.hits === 1)) n = 21;
var cg = 0;
try { F(g); } catch (e1) { if (e1 === markG) cg = 1; }
if (n === 0 && !(cg === 1)) n = 22;
var cm = 0;
try { F(m); } catch (e2) { if (e2 === markM) cm = 1; }
if (n === 0 && !(cm === 1)) n = 23;
var ct = 0;
try { F(both); } catch (e3) { if (e3 && e3.name === 'TypeError') ct = 1; }
if (n === 0 && !(ct === 1 && both.hits === 2)) n = 24;
var sl = 0;
var lh = 0;
try { F((sl = sl * 10 + 1, L.boom()), (sl = sl * 10 + 2, 1)); } catch (e4) { if (e4 === markL) lh = 1; }
if (n === 0 && !(lh === 1 && sl === 1)) n = 25;
n === 0 ? F(-0.5) : n;
"#;

const SQRT_CASE: &str = r#"var n = 0;
var z = 1 / (-1 / 0);
var seq = 0;
var markG = {};
var markM = {};
var markL = {};
var tail = { mark: 11, th: 0 };
var p = { ts: 0 };
var b = { hits: 0 };
var g = { mark: markG };
var m = { mark: markM };
var both = { hits: 0 };
var L = { mark: markL };
function retObj() { "use strict"; this.hits = this.hits + 1; return {}; }
function retStr() { "use strict"; return '9'; }
function throwThis() { "use strict"; throw this.mark; }
p.valueOf = retStr;
p.toString = throwThis;
b.valueOf = retObj;
b.toString = retStr;
m.valueOf = throwThis;
both.valueOf = retObj;
both.toString = retObj;
function tailValue() { "use strict"; this.th = this.th + 1; return 0; }
function tailText() { "use strict"; this.th = this.th + 1; return '9'; }
tail.valueOf = tailValue;
tail.toString = tailText;
L.boom = throwThis;
Object.defineProperty(g, 'valueOf', { get: throwThis });
var F = Math.sqrt;
var D = Math.sqrt((seq = seq * 10 + 1, 9), (seq = seq * 10 + 2, tail));
if (!(F.length === 1)) n = 1;
if (n === 0 && !(D === 3 && seq === 12)) n = 2;
if (n === 0 && !(F(-1) !== F(-1) && F(-1 / 0) !== F(-1 / 0))) n = 3;
if (n === 0 && !(1 / F(z) === -1 / 0 && 1 / F(0) === 1 / 0)) n = 4;
if (n === 0 && !(F(1 / 0) === 1 / 0 && F(4) === 2)) n = 5;
if (n === 0 && !(F(5e-324) === 2.2227587494850775e-162)) n = 6;
var o = {};
o.m = F;
if (n === 0 && !(o.m(9) === 3 && 1 / o.m(z) === -1 / 0)) n = 9;
var ex = F((seq = seq * 10 + 3, 9), (seq = seq * 10 + 4, tail));
var bo = o.m((seq = seq * 10 + 5, 9), (seq = seq * 10 + 6, tail));
if (n === 0 && !(ex === 3 && bo === 3 && seq === 123456)) n = 10;
var k1 = { ord: 0 };
function co1() { "use strict"; this.ord = this.ord * 10 + 3; return 9; }
k1.valueOf = co1;
var t1 = Math.sqrt((k1.ord = 1, k1), (k1.ord = k1.ord * 10 + 2, tail));
if (n === 0 && !(t1 === 3 && k1.ord === 123)) n = 26;
if (n === 0 && !(tail.th === 0)) throw 'TAIL_COUNT';
if (n === 0 && !(F(p) === 3)) n = 20;
if (n === 0 && !(F(b) === 3 && b.hits === 1)) n = 21;
var cg = 0;
try { F(g); } catch (e1) { if (e1 === markG) cg = 1; }
if (n === 0 && !(cg === 1)) n = 22;
var cm = 0;
try { F(m); } catch (e2) { if (e2 === markM) cm = 1; }
if (n === 0 && !(cm === 1)) n = 23;
var ct = 0;
try { F(both); } catch (e3) { if (e3 && e3.name === 'TypeError') ct = 1; }
if (n === 0 && !(ct === 1 && both.hits === 2)) n = 24;
var sl = 0;
var lh = 0;
try { F((sl = sl * 10 + 1, L.boom()), (sl = sl * 10 + 2, 1)); } catch (e4) { if (e4 === markL) lh = 1; }
if (n === 0 && !(lh === 1 && sl === 1)) n = 25;
n === 0 ? F(z) : n;
"#;

#[test]
fn p8_math_unary_abs_interpreter() {
    expect_bits("abs", ABS_CASE, 5e-324);
}

#[test]
fn p8_math_unary_floor_interpreter() {
    expect_bits("floor", FLOOR_CASE, -0.0);
}

#[test]
fn p8_math_unary_ceil_interpreter() {
    expect_bits("ceil", CEIL_CASE, -0.0);
}

#[test]
fn p8_math_unary_round_interpreter() {
    expect_bits("round", ROUND_CASE, -0.0);
}

#[test]
fn p8_math_unary_sqrt_interpreter() {
    expect_bits("sqrt", SQRT_CASE, -0.0);
}
