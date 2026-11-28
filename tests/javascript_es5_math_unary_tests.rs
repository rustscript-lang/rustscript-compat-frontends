//! JIT-disabled ordinary calls for Math.abs/floor/ceil/round/sqrt.
//!
//! Each script is the MicroQuickJS oracle program for that method only.
//! Return values are boxed Number tags. Signed zero uses reciprocal checks
//! in the script and exact IEEE bits on the direct-call receipt. NaN is a
//! category, not a payload. This file does not cover full string ToNumber,
//! JIT, AOT, TypeScript, or argument-bearing `.call`/`.apply`.
use pd_vm_compat_frontends::{javascript_es5_flavor, JAVASCRIPT_ES5_PLUGIN};
use vm::{HostFunctionRegistry, Value, VmStatus};

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
    let output = std::process::Command::new("df")
        .args(["-B1", "/mnt/TEMP"])
        .output()
        .expect("df");
    let df = String::from_utf8(output.stdout).expect("df utf8");
    let available = df
        .lines()
        .last()
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|field| field.parse::<u64>().ok())
        .expect("df available bytes");
    eprintln!("MATH_UNARY_BUDGET mem_available_kib={kb} fs_available_bytes={available}");
    assert!(
        kb.saturating_mul(1024) >= 8_589_934_592,
        "memory free floor below 8589934592 bytes: {kb}kB"
    );
    assert!(
        available >= 8_589_934_592,
        "allocated filesystem free floor below 8589934592 bytes: {available}"
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

const ABS_CASE: &str = r#"(function () {
  var n = 0;
  var z = 1 / (-1 / 0);
  if (!(1 / z === -1 / 0)) n = 1;
  if (n === 0) {
    if (!(Math.abs.length === 1)) n = 2;
  }
  if (n === 0) {
    if (!(1 / Math.abs(z) === 1 / 0)) n = 3;
  }
  if (n === 0) {
    if (!(Math.abs(-5e-324) === 5e-324)) n = 4;
  }
  var fnum = Math.abs;
  if (n === 0) {
    if (!(1 / fnum(z) === 1 / 0 && fnum(-5e-324) === 5e-324)) n = 5;
  }
  var onum = {};
  onum.m = Math.abs;
  if (n === 0) {
    if (!(1 / onum.m(z) === 1 / 0 && onum.m(-5e-324) === 5e-324)) n = 6;
  }
  var seq = 0;
  var tail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 11; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 12; }
  };
  var direct = Math.abs((seq = seq * 10 + 1, -4), (seq = seq * 10 + 2, tail));
  if (n === 0) {
    if (!(seq === 12 && tail.hits === 0 && direct === 4)) n = 30;
  }
  var seqLeft = 0;
  var left = {
    mark: {},
    boom: function () { "use strict"; throw this.mark; }
  };
  var leftHit = 0;
  try {
    Math.abs((seqLeft = seqLeft * 10 + 1, left.boom()), (seqLeft = seqLeft * 10 + 2, 1));
  } catch (e1) {
    if (e1 === left.mark) leftHit = 1;
  }
  if (n === 0) {
    if (!(leftHit === 1 && seqLeft === 1)) n = 31;
  }
  var f = Math.abs;
  var fseq = 0;
  var ftail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 13; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 14; }
  };
  var extracted = f((fseq = fseq * 10 + 1, -4), (fseq = fseq * 10 + 2, ftail));
  if (n === 0) {
    if (!(fseq === 12 && ftail.hits === 0 && extracted === 4)) n = 32;
  }
  var o = {};
  o.m = Math.abs;
  var oseq = 0;
  var otail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 15; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 16; }
  };
  var borrowed = o.m((oseq = oseq * 10 + 1, -4), (oseq = oseq * 10 + 2, otail));
  if (n === 0) {
    if (!(oseq === 12 && otail.hits === 0 && borrowed === 4)) n = 33;
  }
  var live = 0;
  try { tail.valueOf(); } catch (e2) { if (e2 === 11) live = live + 1; }
  try { tail.toString(); } catch (e3) { if (e3 === 12) live = live + 1; }
  if (n === 0) {
    if (!(tail.hits === 2 && live === 2)) n = 34;
  }
  var boxed = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return {}; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '-8'; }
  };
  if (n === 0) {
    if (!(Math.abs(boxed) === 8 && boxed.vo === 1 && boxed.ts === 1)) n = 35;
  }
  var prim = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return -6; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '1'; }
  };
  if (n === 0) {
    if (!(Math.abs(prim) === 6 && prim.vo === 1 && prim.ts === 0)) n = 36;
  }
  var getter = {
    mark: {},
    hits: 0,
    get valueOf() { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtG = 0;
  try { Math.abs(getter); } catch (e4) { if (e4 === getter.mark) caughtG = 1; }
  if (n === 0) {
    if (!(caughtG === 1 && getter.hits === 0)) n = 37;
  }
  var methodObj = {
    mark: {},
    hits: 0,
    valueOf: function () { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtM = 0;
  try { Math.abs(methodObj); } catch (e5) { if (e5 === methodObj.mark) caughtM = 1; }
  if (n === 0) {
    if (!(caughtM === 1 && methodObj.hits === 0)) n = 38;
  }
  var both = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; return {}; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return {}; }
  };
  var typeHits = 0;
  try { Math.abs(both); } catch (e6) { if (e6 && e6.name === 'TypeError') typeHits = 1; }
  if (n === 0) {
    if (!(typeHits === 1 && both.hits === 2)) n = 39;
  }
  if (n !== 0) return n;
  return Math.abs(-5e-324);
})();
"#;

const FLOOR_CASE: &str = r#"(function () {
  var n = 0;
  var z = 1 / (-1 / 0);
  if (!(1 / z === -1 / 0)) n = 1;
  if (n === 0) {
    if (!(Math.floor.length === 1)) n = 2;
  }
  if (n === 0) {
    if (!(Math.floor(-1.2) === -2 && 1 / Math.floor(z) === -1 / 0)) n = 3;
  }
  var fnum = Math.floor;
  if (n === 0) {
    if (!(fnum(-1.2) === -2 && 1 / fnum(z) === -1 / 0)) n = 4;
  }
  var onum = {};
  onum.m = Math.floor;
  if (n === 0) {
    if (!(onum.m(-1.2) === -2 && 1 / onum.m(z) === -1 / 0)) n = 5;
  }
  var seq = 0;
  var tail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 11; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 12; }
  };
  var direct = Math.floor((seq = seq * 10 + 1, -1.2), (seq = seq * 10 + 2, tail));
  if (n === 0) {
    if (!(seq === 12 && tail.hits === 0 && direct === -2)) n = 30;
  }
  var seqLeft = 0;
  var left = {
    mark: {},
    boom: function () { "use strict"; throw this.mark; }
  };
  var leftHit = 0;
  try {
    Math.floor((seqLeft = seqLeft * 10 + 1, left.boom()), (seqLeft = seqLeft * 10 + 2, 1));
  } catch (e1) {
    if (e1 === left.mark) leftHit = 1;
  }
  if (n === 0) {
    if (!(leftHit === 1 && seqLeft === 1)) n = 31;
  }
  var f = Math.floor;
  var fseq = 0;
  var ftail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 13; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 14; }
  };
  var extracted = f((fseq = fseq * 10 + 1, -1.2), (fseq = fseq * 10 + 2, ftail));
  if (n === 0) {
    if (!(fseq === 12 && ftail.hits === 0 && extracted === -2)) n = 32;
  }
  var o = {};
  o.m = Math.floor;
  var oseq = 0;
  var otail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 15; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 16; }
  };
  var borrowed = o.m((oseq = oseq * 10 + 1, -1.2), (oseq = oseq * 10 + 2, otail));
  if (n === 0) {
    if (!(oseq === 12 && otail.hits === 0 && borrowed === -2)) n = 33;
  }
  var live = 0;
  try { tail.valueOf(); } catch (e2) { if (e2 === 11) live = live + 1; }
  try { tail.toString(); } catch (e3) { if (e3 === 12) live = live + 1; }
  if (n === 0) {
    if (!(tail.hits === 2 && live === 2)) n = 34;
  }
  var boxed = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return {}; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '-1.2'; }
  };
  if (n === 0) {
    if (!(Math.floor(boxed) === -2 && boxed.vo === 1 && boxed.ts === 1)) n = 35;
  }
  var prim = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return -1.2; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '1'; }
  };
  if (n === 0) {
    if (!(Math.floor(prim) === -2 && prim.vo === 1 && prim.ts === 0)) n = 36;
  }
  var getter = {
    mark: {},
    hits: 0,
    get valueOf() { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtG = 0;
  try { Math.floor(getter); } catch (e4) { if (e4 === getter.mark) caughtG = 1; }
  if (n === 0) {
    if (!(caughtG === 1 && getter.hits === 0)) n = 37;
  }
  var methodObj = {
    mark: {},
    hits: 0,
    valueOf: function () { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtM = 0;
  try { Math.floor(methodObj); } catch (e5) { if (e5 === methodObj.mark) caughtM = 1; }
  if (n === 0) {
    if (!(caughtM === 1 && methodObj.hits === 0)) n = 38;
  }
  var both = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; return {}; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return {}; }
  };
  var typeHits = 0;
  try { Math.floor(both); } catch (e6) { if (e6 && e6.name === 'TypeError') typeHits = 1; }
  if (n === 0) {
    if (!(typeHits === 1 && both.hits === 2)) n = 39;
  }
  if (n !== 0) return n;
  return Math.floor(z);
})();
"#;

const CEIL_CASE: &str = r#"(function () {
  var n = 0;
  var z = 1 / (-1 / 0);
  if (!(1 / z === -1 / 0)) n = 1;
  if (n === 0) {
    if (!(Math.ceil.length === 1)) n = 2;
  }
  if (n === 0) {
    if (!(Math.ceil(-1.2) === -1 && 1 / Math.ceil(-0.5) === -1 / 0)) n = 3;
  }
  var fnum = Math.ceil;
  if (n === 0) {
    if (!(fnum(-1.2) === -1 && 1 / fnum(-0.5) === -1 / 0)) n = 4;
  }
  var onum = {};
  onum.m = Math.ceil;
  if (n === 0) {
    if (!(onum.m(-1.2) === -1 && 1 / onum.m(-0.5) === -1 / 0)) n = 5;
  }
  var seq = 0;
  var tail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 11; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 12; }
  };
  var direct = Math.ceil((seq = seq * 10 + 1, -1.2), (seq = seq * 10 + 2, tail));
  if (n === 0) {
    if (!(seq === 12 && tail.hits === 0 && direct === -1)) n = 30;
  }
  var seqLeft = 0;
  var left = {
    mark: {},
    boom: function () { "use strict"; throw this.mark; }
  };
  var leftHit = 0;
  try {
    Math.ceil((seqLeft = seqLeft * 10 + 1, left.boom()), (seqLeft = seqLeft * 10 + 2, 1));
  } catch (e1) {
    if (e1 === left.mark) leftHit = 1;
  }
  if (n === 0) {
    if (!(leftHit === 1 && seqLeft === 1)) n = 31;
  }
  var f = Math.ceil;
  var fseq = 0;
  var ftail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 13; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 14; }
  };
  var extracted = f((fseq = fseq * 10 + 1, -1.2), (fseq = fseq * 10 + 2, ftail));
  if (n === 0) {
    if (!(fseq === 12 && ftail.hits === 0 && extracted === -1)) n = 32;
  }
  var o = {};
  o.m = Math.ceil;
  var oseq = 0;
  var otail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 15; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 16; }
  };
  var borrowed = o.m((oseq = oseq * 10 + 1, -1.2), (oseq = oseq * 10 + 2, otail));
  if (n === 0) {
    if (!(oseq === 12 && otail.hits === 0 && borrowed === -1)) n = 33;
  }
  var live = 0;
  try { tail.valueOf(); } catch (e2) { if (e2 === 11) live = live + 1; }
  try { tail.toString(); } catch (e3) { if (e3 === 12) live = live + 1; }
  if (n === 0) {
    if (!(tail.hits === 2 && live === 2)) n = 34;
  }
  var boxed = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return {}; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '-1.2'; }
  };
  if (n === 0) {
    if (!(Math.ceil(boxed) === -1 && boxed.vo === 1 && boxed.ts === 1)) n = 35;
  }
  var prim = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return -1.2; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '1'; }
  };
  if (n === 0) {
    if (!(Math.ceil(prim) === -1 && prim.vo === 1 && prim.ts === 0)) n = 36;
  }
  var getter = {
    mark: {},
    hits: 0,
    get valueOf() { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtG = 0;
  try { Math.ceil(getter); } catch (e4) { if (e4 === getter.mark) caughtG = 1; }
  if (n === 0) {
    if (!(caughtG === 1 && getter.hits === 0)) n = 37;
  }
  var methodObj = {
    mark: {},
    hits: 0,
    valueOf: function () { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtM = 0;
  try { Math.ceil(methodObj); } catch (e5) { if (e5 === methodObj.mark) caughtM = 1; }
  if (n === 0) {
    if (!(caughtM === 1 && methodObj.hits === 0)) n = 38;
  }
  var both = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; return {}; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return {}; }
  };
  var typeHits = 0;
  try { Math.ceil(both); } catch (e6) { if (e6 && e6.name === 'TypeError') typeHits = 1; }
  if (n === 0) {
    if (!(typeHits === 1 && both.hits === 2)) n = 39;
  }
  if (n !== 0) return n;
  return Math.ceil(-0.5);
})();
"#;

const ROUND_CASE: &str = r#"(function () {
  var n = 0;
  var z = 1 / (-1 / 0);
  if (!(1 / z === -1 / 0)) n = 1;
  if (n === 0) {
    if (!(Math.round.length === 1)) n = 2;
  }
  if (n === 0) {
    if (!(1 / Math.round(-0.5) === -1 / 0)) n = 3;
  }
  if (n === 0) {
    if (!(Math.round(-1.5) === -1)) n = 4;
  }
  if (n === 0) {
    if (!(Math.round(-0.5000000000000001) === -1)) n = 5;
  }
  if (n === 0) {
    if (!(1 / Math.round(0.49999999999999994) === 1 / 0)) n = 6;
  }
  if (n === 0) {
    if (!(Math.round(4503599627370497) === 4503599627370497)) n = 7;
  }
  var fnum = Math.round;
  if (n === 0) {
    if (!(1 / fnum(-0.5) === -1 / 0 && fnum(-1.5) === -1 && fnum(4503599627370497) === 4503599627370497)) n = 8;
  }
  var onum = {};
  onum.m = Math.round;
  if (n === 0) {
    if (!(1 / onum.m(-0.5) === -1 / 0 && onum.m(-1.5) === -1 && onum.m(4503599627370497) === 4503599627370497)) n = 9;
  }
  var seq = 0;
  var tail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 11; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 12; }
  };
  var direct = Math.round((seq = seq * 10 + 1, -1.5), (seq = seq * 10 + 2, tail));
  if (n === 0) {
    if (!(seq === 12 && tail.hits === 0 && direct === -1)) n = 30;
  }
  var seqLeft = 0;
  var left = {
    mark: {},
    boom: function () { "use strict"; throw this.mark; }
  };
  var leftHit = 0;
  try {
    Math.round((seqLeft = seqLeft * 10 + 1, left.boom()), (seqLeft = seqLeft * 10 + 2, 1));
  } catch (e1) {
    if (e1 === left.mark) leftHit = 1;
  }
  if (n === 0) {
    if (!(leftHit === 1 && seqLeft === 1)) n = 31;
  }
  var f = Math.round;
  var fseq = 0;
  var ftail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 13; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 14; }
  };
  var extracted = f((fseq = fseq * 10 + 1, -1.5), (fseq = fseq * 10 + 2, ftail));
  if (n === 0) {
    if (!(fseq === 12 && ftail.hits === 0 && extracted === -1)) n = 32;
  }
  var o = {};
  o.m = Math.round;
  var oseq = 0;
  var otail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 15; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 16; }
  };
  var borrowed = o.m((oseq = oseq * 10 + 1, -1.5), (oseq = oseq * 10 + 2, otail));
  if (n === 0) {
    if (!(oseq === 12 && otail.hits === 0 && borrowed === -1)) n = 33;
  }
  var live = 0;
  try { tail.valueOf(); } catch (e2) { if (e2 === 11) live = live + 1; }
  try { tail.toString(); } catch (e3) { if (e3 === 12) live = live + 1; }
  if (n === 0) {
    if (!(tail.hits === 2 && live === 2)) n = 34;
  }
  var boxed = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return {}; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '-1.5'; }
  };
  if (n === 0) {
    if (!(Math.round(boxed) === -1 && boxed.vo === 1 && boxed.ts === 1)) n = 35;
  }
  var prim = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return -1.5; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '1'; }
  };
  if (n === 0) {
    if (!(Math.round(prim) === -1 && prim.vo === 1 && prim.ts === 0)) n = 36;
  }
  var getter = {
    mark: {},
    hits: 0,
    get valueOf() { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtG = 0;
  try { Math.round(getter); } catch (e4) { if (e4 === getter.mark) caughtG = 1; }
  if (n === 0) {
    if (!(caughtG === 1 && getter.hits === 0)) n = 37;
  }
  var methodObj = {
    mark: {},
    hits: 0,
    valueOf: function () { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtM = 0;
  try { Math.round(methodObj); } catch (e5) { if (e5 === methodObj.mark) caughtM = 1; }
  if (n === 0) {
    if (!(caughtM === 1 && methodObj.hits === 0)) n = 38;
  }
  var both = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; return {}; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return {}; }
  };
  var typeHits = 0;
  try { Math.round(both); } catch (e6) { if (e6 && e6.name === 'TypeError') typeHits = 1; }
  if (n === 0) {
    if (!(typeHits === 1 && both.hits === 2)) n = 39;
  }
  if (n !== 0) return n;
  return Math.round(-0.5);
})();
"#;

const SQRT_CASE: &str = r#"(function () {
  var n = 0;
  var z = 1 / (-1 / 0);
  if (!(1 / z === -1 / 0)) n = 1;
  if (n === 0) {
    if (!(Math.sqrt.length === 1)) n = 2;
  }
  if (n === 0) {
    if (!(Math.sqrt(-1) !== Math.sqrt(-1))) n = 3;
  }
  if (n === 0) {
    if (!(Math.sqrt(-1 / 0) !== Math.sqrt(-1 / 0))) n = 4;
  }
  if (n === 0) {
    if (!(1 / Math.sqrt(z) === -1 / 0)) n = 5;
  }
  if (n === 0) {
    if (!(Math.sqrt(5e-324) === 2.2227587494850775e-162)) n = 6;
  }
  var fnum = Math.sqrt;
  if (n === 0) {
    if (!(fnum(-2) !== fnum(-2) && 1 / fnum(z) === -1 / 0 && fnum(5e-324) === 2.2227587494850775e-162)) n = 7;
  }
  var onum = {};
  onum.m = Math.sqrt;
  if (n === 0) {
    if (!(onum.m(-3) !== onum.m(-3) && 1 / onum.m(z) === -1 / 0 && onum.m(5e-324) === 2.2227587494850775e-162)) n = 8;
  }
  var seq = 0;
  var tail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 11; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 12; }
  };
  var direct = Math.sqrt((seq = seq * 10 + 1, 9), (seq = seq * 10 + 2, tail));
  if (n === 0) {
    if (!(seq === 12 && tail.hits === 0 && direct === 3)) n = 30;
  }
  var seqLeft = 0;
  var left = {
    mark: {},
    boom: function () { "use strict"; throw this.mark; }
  };
  var leftHit = 0;
  try {
    Math.sqrt((seqLeft = seqLeft * 10 + 1, left.boom()), (seqLeft = seqLeft * 10 + 2, 1));
  } catch (e1) {
    if (e1 === left.mark) leftHit = 1;
  }
  if (n === 0) {
    if (!(leftHit === 1 && seqLeft === 1)) n = 31;
  }
  var f = Math.sqrt;
  var fseq = 0;
  var ftail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 13; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 14; }
  };
  var extracted = f((fseq = fseq * 10 + 1, 9), (fseq = fseq * 10 + 2, ftail));
  if (n === 0) {
    if (!(fseq === 12 && ftail.hits === 0 && extracted === 3)) n = 32;
  }
  var o = {};
  o.m = Math.sqrt;
  var oseq = 0;
  var otail = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; throw 15; },
    toString: function () { "use strict"; this.hits = this.hits + 1; throw 16; }
  };
  var borrowed = o.m((oseq = oseq * 10 + 1, 9), (oseq = oseq * 10 + 2, otail));
  if (n === 0) {
    if (!(oseq === 12 && otail.hits === 0 && borrowed === 3)) n = 33;
  }
  var live = 0;
  try { tail.valueOf(); } catch (e2) { if (e2 === 11) live = live + 1; }
  try { tail.toString(); } catch (e3) { if (e3 === 12) live = live + 1; }
  if (n === 0) {
    if (!(tail.hits === 2 && live === 2)) n = 34;
  }
  var boxed = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return {}; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '9'; }
  };
  if (n === 0) {
    if (!(Math.sqrt(boxed) === 3 && boxed.vo === 1 && boxed.ts === 1)) n = 35;
  }
  var prim = {
    vo: 0,
    ts: 0,
    valueOf: function () { "use strict"; this.vo = this.vo + 1; return 9; },
    toString: function () { "use strict"; this.ts = this.ts + 1; return '1'; }
  };
  if (n === 0) {
    if (!(Math.sqrt(prim) === 3 && prim.vo === 1 && prim.ts === 0)) n = 36;
  }
  var getter = {
    mark: {},
    hits: 0,
    get valueOf() { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtG = 0;
  try { Math.sqrt(getter); } catch (e4) { if (e4 === getter.mark) caughtG = 1; }
  if (n === 0) {
    if (!(caughtG === 1 && getter.hits === 0)) n = 37;
  }
  var methodObj = {
    mark: {},
    hits: 0,
    valueOf: function () { "use strict"; throw this.mark; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return '1'; }
  };
  var caughtM = 0;
  try { Math.sqrt(methodObj); } catch (e5) { if (e5 === methodObj.mark) caughtM = 1; }
  if (n === 0) {
    if (!(caughtM === 1 && methodObj.hits === 0)) n = 38;
  }
  var both = {
    hits: 0,
    valueOf: function () { "use strict"; this.hits = this.hits + 1; return {}; },
    toString: function () { "use strict"; this.hits = this.hits + 1; return {}; }
  };
  var typeHits = 0;
  try { Math.sqrt(both); } catch (e6) { if (e6 && e6.name === 'TypeError') typeHits = 1; }
  if (n === 0) {
    if (!(typeHits === 1 && both.hits === 2)) n = 39;
  }
  if (n !== 0) return n;
  return Math.sqrt(z);
})();
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
